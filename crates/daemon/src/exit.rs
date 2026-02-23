//! Exit node implementation.
//!
//! The exit node processes incoming instructions by making syscalls to the
//! local network. It can either connect to an upstream peer (default) or
//! listen for incoming connections. The daemon is headless — no REPL, no TTY.

use std::{str::FromStr, sync::Arc, time::Duration};

use anyhow::Result;
use tokio::io::AsyncWriteExt;

use wallhack_core::{
	NodeRole,
	client::client::{Client, ConnectResult},
	control::metrics::Metrics,
	exit::{net::SyscallExitAdapter, orchestrator::Orchestrator},
	server::server::Server,
	transport::{
		BiStream, Transport,
		bridge::{SESSION_INIT_MTU, read_length_delimited, write_length_delimited},
	},
};

#[cfg(feature = "quic")]
use wallhack_core::client::{self, config::ClientConfig, config::MtlsConfig};

use crate::{
	WallhackCli,
	cli::{ExitCommand, Protocol, TransportDir},
	net::{SocketAddrExt, parse_listen_addr},
};

/// Initial retry delay for connection attempts (peer not yet listening).
const INITIAL_RETRY_DELAY: Duration = Duration::from_millis(50);
/// Delay before reconnecting after an established session drops.
const RECONNECT_DELAY: Duration = Duration::from_millis(500);
/// Maximum retry delay (caps exponential backoff).
const MAX_RETRY_DELAY: Duration = Duration::from_secs(30);
/// Timeout for UDP response after forwarding packet.
const UDP_RESPONSE_TIMEOUT: Duration = Duration::from_millis(500);

/// Security-related connection parameters.
struct SecurityConfig {
	psk: Option<String>,
	accept_fingerprint: Option<String>,
}

/// Run as an exit node (headless daemon).
///
/// State machine dispatches to mode-specific functions based on the current
/// connect/listen configuration.
///
/// # Errors
///
/// Returns error if orchestrator fails (connection errors are retried).
pub async fn run(global: &WallhackCli, cmd: &ExitCommand, metrics: Arc<Metrics>) -> Result<()> {
	let transport = cmd.transport().map_err(|e| anyhow::anyhow!("{e}"))?;
	let name = cmd.name();
	tracing::info!(
		"wallhack {}  {name}",
		crate::version::built_info::PKG_VERSION
	);
	let security = SecurityConfig {
		psk: global.resolve_psk(),
		accept_fingerprint: cmd.accept_fingerprint.clone(),
	};

	match transport {
		TransportDir::Both { connect, listen } => {
			run_relay_capability_mode(global, &name, &connect, &listen, &metrics).await
		}
		TransportDir::Connect(spec) => {
			run_connect_mode(global, &name, &spec, &metrics, &security).await
		}
		TransportDir::Listen(spec) => run_listen_mode(global, &name, &spec, &metrics).await,
	}
}

/// Run in connect-only mode (standard exit).
async fn run_connect_mode(
	global: &WallhackCli,
	name: &str,
	spec: &crate::cli::AddressSpec,
	metrics: &Arc<Metrics>,
	security: &SecurityConfig,
) -> Result<()> {
	tracing::info!("Connecting to {}...", spec.addr);

	let resolvable = crate::dns::ResolvableAddress::from_str(&spec.addr)?;
	let dns_server = global
		.dns
		.as_ref()
		.map(|s| crate::dns::parse_str_to_addr(s))
		.transpose()?;

	let endpoint = crate::dns::resolve(resolvable, dns_server).await?;

	match spec.protocol {
		Protocol::Udp => {
			#[cfg(feature = "quic")]
			{
				run_quic_exit(global, endpoint, name, metrics, security).await
			}
			#[cfg(not(feature = "quic"))]
			{
				anyhow::bail!("QUIC support not compiled in (enable 'quic' feature)")
			}
		}
		Protocol::Tcp => {
			#[cfg(feature = "websocket")]
			{
				run_ws_exit(global, endpoint, name, metrics, security).await
			}
			#[cfg(not(feature = "websocket"))]
			{
				anyhow::bail!("WebSocket support not compiled in (enable 'websocket' feature)")
			}
		}
	}
}

/// Run with relay capability (both connect and listen).
async fn run_relay_capability_mode(
	global: &WallhackCli,
	name: &str,
	connect_spec: &crate::cli::AddressSpec,
	listen_spec: &crate::cli::AddressSpec,
	metrics: &Arc<Metrics>,
) -> Result<()> {
	tracing::info!("Connecting to {}...", connect_spec.addr);
	let resolvable = crate::dns::ResolvableAddress::from_str(&connect_spec.addr)?;
	let dns_server = global
		.dns
		.as_ref()
		.map(|s| crate::dns::parse_str_to_addr(s))
		.transpose()?;

	let peer_addr = crate::dns::resolve(resolvable, dns_server).await?;

	let listen_addr = parse_listen_addr(&listen_spec.addr)?;

	match connect_spec.protocol {
		Protocol::Udp => {
			#[cfg(feature = "quic")]
			{
				run_quic_relay_capability(global, peer_addr, listen_addr, name, metrics).await
			}
			#[cfg(not(feature = "quic"))]
			{
				anyhow::bail!("QUIC support not compiled in (enable 'quic' feature)")
			}
		}
		Protocol::Tcp => {
			#[cfg(feature = "websocket")]
			{
				run_ws_relay_capability(global, peer_addr, listen_addr, name, metrics).await
			}
			#[cfg(not(feature = "websocket"))]
			{
				anyhow::bail!("WebSocket support not compiled in (enable 'websocket' feature)")
			}
		}
	}
}

/// Run in listen mode.
async fn run_listen_mode(
	global: &WallhackCli,
	_node_name: &str,
	spec: &crate::cli::AddressSpec,
	metrics: &Arc<Metrics>,
) -> Result<()> {
	use wallhack_core::{control::handler::HandlerConfig, server::server::ServerOptions};

	let addr = parse_listen_addr(&spec.addr)?;

	let server_options = ServerOptions {
		handler_config: HandlerConfig::new(NodeRole::Exit),
		metrics: Some(Arc::clone(metrics)),
		peers: None,
		routes: None,
	};

	let server_config = build_server_config(global, addr);

	match spec.protocol {
		Protocol::Udp => {
			#[cfg(feature = "quic")]
			{
				let server = wallhack_core::server::quic::QuicServer::try_new(
					server_config,
					server_options,
				)?;
				let bound = server.local_addr()?;
				tracing::info!("Listening on {bound} ({})", server.protocol_name());
				run_listen_server_loop(server, metrics).await
			}
			#[cfg(not(feature = "quic"))]
			anyhow::bail!("QUIC transport not available (compile with --features quic)")
		}
		Protocol::Tcp => {
			#[cfg(feature = "websocket")]
			{
				let server =
					wallhack_core::server::ws::WsServer::try_new(server_config, server_options)?;
				let bound = server.local_addr()?;
				tracing::info!("Listening on {bound} ({})", server.protocol_name());
				run_listen_server_loop(server, metrics).await
			}
			#[cfg(not(feature = "websocket"))]
			anyhow::bail!("WebSocket transport not available (compile with --features websocket)")
		}
	}
}

/// Server accept loop for listen-only mode.
async fn run_listen_server_loop<S: Server>(mut server: S, metrics: &Arc<Metrics>) -> Result<()>
where
	S::Error: std::error::Error + Send + Sync + 'static,
	S::Transport: Send + Sync + 'static,
{
	loop {
		match server.accept(NodeRole::Exit).await {
			Ok(Some(accept_result)) => {
				tracing::info!("Peer connected: {}", accept_result.peer_addr());
				let transport = accept_result.transport();
				let adapter = SyscallExitAdapter::new();
				let _reaper = adapter.start_reaper(
					std::time::Duration::from_mins(1),
					std::time::Duration::from_mins(5),
				);
				let orchestrator = Orchestrator::new(Arc::new(adapter), Arc::clone(metrics));
				let stream_fut = run_stream_listener(transport);
				let ((instr, resp), control_tx) = accept_result.channels();
				tokio::spawn(async move {
					let _keep_alive = control_tx;
					tokio::select! {
						result = orchestrator.drive(resp.clone(), instr.subscribe()) => {
							if let Err(e) = result {
								tracing::error!("Orchestrator error: {e}");
							}
						}
						result = stream_fut => {
							if let Err(e) = result {
								tracing::error!("Stream handler error: {e}");
							}
						}
					}
				});
			}
			Ok(None) => break,
			Err(e) => {
				tracing::warn!("Accept error: {e}");
			}
		}
	}

	Ok(())
}

/// Drive the exit node orchestrator with a connected client.
///
/// Returns when the connection drops (caller should reconnect).
async fn run_exit_loop<T: wallhack_core::transport::Transport + 'static>(
	connect_result: ConnectResult<T>,
	metrics: &Arc<Metrics>,
	peer_addr: &str,
) -> Result<()> {
	tracing::info!("Connected to {peer_addr}");

	// Create syscall adapter for local network access
	let adapter = SyscallExitAdapter::new();
	let _reaper = adapter.start_reaper(
		std::time::Duration::from_mins(1),
		std::time::Duration::from_mins(5),
	);
	let orchestrator = Orchestrator::new(Arc::new(adapter), Arc::clone(metrics));

	let transport = connect_result.transport();
	let ((instr, resp), mut tasks, _control_tx) = connect_result.into_parts();
	let stream_fut = run_stream_listener(transport);
	let disconnect_fut = tasks.wait_for_disconnect();

	// Pin the long-running futures so we can select over them
	tokio::pin!(stream_fut);
	tokio::pin!(disconnect_fut);
	let drive_fut = orchestrator.drive(resp, instr.subscribe());
	tokio::pin!(drive_fut);

	tokio::select! {
		result = &mut drive_fut => {
			match result {
				Ok(()) => tracing::debug!("Connection closed cleanly"),
				Err(e) => tracing::debug!("Orchestrator error: {e}"),
			}
		}
		result = &mut stream_fut => {
			if let Err(e) = result { tracing::warn!("Stream handler error: {e}"); }
		}
		() = &mut disconnect_fut => {
			tracing::debug!("Connection tasks died - transport disconnected");
		}
	}

	Ok(())
}

async fn run_stream_listener<T: Transport>(transport: std::sync::Arc<T>) -> Result<()>
where
	T::BiStream: 'static,
{
	tracing::trace!("Stream listener started");
	loop {
		let Some(mut stream) = transport.accept_bi().await? else {
			return Ok(());
		};
		tracing::trace!("Accepted bi-stream from entry");
		tokio::spawn(async move {
			if let Err(e) = handle_stream(&mut stream).await {
				tracing::warn!("stream handler failed: {e}");
			}
		});
	}
}

async fn handle_stream<S: BiStream>(stream: &mut S) -> Result<()> {
	let init =
		read_length_delimited::<wallhack_wire::data::SessionInit, _>(stream, SESSION_INIT_MTU)
			.await
			.map_err(|e| anyhow::anyhow!(e))?;
	tracing::trace!(target = %init.target_addr, source = %init.source_addr, protocol = init.protocol, "SessionInit received");
	let target: std::net::SocketAddr = init.target_addr.parse()?;
	let source: Option<std::net::SocketAddr> = if init.source_addr.is_empty() {
		None
	} else {
		Some(init.source_addr.parse()?)
	};
	match init.protocol {
		val if val == wallhack_wire::data::SessionProtocol::Tcp as i32 => {
			match tokio::net::TcpStream::connect(target).await {
				Ok(mut socket) => {
					let status = wallhack_wire::data::SessionStatus {
						status: wallhack_wire::data::ResponseStatus::Success.into(),
						reason: String::new(),
					};
					write_length_delimited(&mut *stream, &status)
						.await
						.map_err(|e| anyhow::anyhow!(e))?;
					let _ = tokio::io::copy_bidirectional(&mut *stream, &mut socket).await?;
				}
				Err(e) => {
					let status_code = match e.kind() {
						std::io::ErrorKind::ConnectionRefused => {
							wallhack_wire::data::ResponseStatus::ConnectionRefused
						}
						_ => wallhack_wire::data::ResponseStatus::HostUnreachable,
					};
					let status = wallhack_wire::data::SessionStatus {
						status: status_code.into(),
						reason: e.to_string(),
					};
					let _ = write_length_delimited(&mut *stream, &status).await;
					return Err(anyhow::anyhow!("connect to {target} failed: {e}"));
				}
			}
		}
		val if val == wallhack_wire::data::SessionProtocol::Udp as i32 => {
			tracing::trace!(target = %target, source = ?source, "Processing UDP session");
			let socket = tokio::net::UdpSocket::bind(match target {
				std::net::SocketAddr::V4(_) => {
					std::net::SocketAddr::from((std::net::Ipv4Addr::UNSPECIFIED, 0))
				}
				std::net::SocketAddr::V6(_) => {
					std::net::SocketAddr::from((std::net::Ipv6Addr::UNSPECIFIED, 0))
				}
			})
			.await?;
			socket.connect(target).await?;
			let mut buf = Vec::new();
			tokio::io::AsyncReadExt::read_to_end(stream, &mut buf).await?;
			tracing::trace!(buf_len = buf.len(), "Read UDP payload from stream");
			if !buf.is_empty() {
				tracing::trace!(target = %target, "Sending UDP to target");
				socket.send(&buf).await?;
				let mut recv_buf = vec![0u8; 65535];
				tracing::trace!("Waiting for UDP response...");
				match tokio::time::timeout(UDP_RESPONSE_TIMEOUT, socket.recv(&mut recv_buf)).await {
					Ok(Ok(size)) => {
						tracing::trace!(size, "Received UDP response");
						stream.write_all(&[0x00]).await?;
						stream.write_all(&recv_buf[..size]).await?;
					}
					Ok(Err(e)) => {
						let status = match e.kind() {
							std::io::ErrorKind::ConnectionRefused => Some(0x01u8),
							std::io::ErrorKind::HostUnreachable => Some(0x02u8),
							std::io::ErrorKind::NetworkUnreachable => Some(0x03u8),
							_ => None,
						};
						if let Some(code) = status {
							tracing::trace!("UDP ICMP error: {e}");
							stream.write_all(&[code]).await?;
						} else {
							tracing::trace!("UDP recv error: {e}");
						}
					}
					Err(_) => {
						tracing::trace!("UDP recv timeout");
					}
				}
				stream.finish().await?;
			}
		}
		_ => {
			tracing::warn!("unsupported session protocol {}", init.protocol);
		}
	}
	Ok(())
}

#[cfg(feature = "quic")]
async fn run_quic_exit(
	global: &WallhackCli,
	endpoint: std::net::SocketAddr,
	name: &str,
	metrics: &Arc<Metrics>,
	security: &SecurityConfig,
) -> Result<()> {
	let client_config = build_quic_client_config(
		global,
		endpoint,
		name.to_string(),
		security.psk.clone(),
		security.accept_fingerprint.clone(),
	);

	let mut retry_delay = INITIAL_RETRY_DELAY;

	let peer_addr = endpoint.to_string();

	loop {
		match async {
			let mut client = client::quic::QuicClient::try_new(client_config.clone())?;
			client.connect(NodeRole::Exit).await
		}
		.await
		{
			Ok(connect_result) => {
				run_exit_loop(connect_result, metrics, &peer_addr).await?;
				tracing::warn!("Connection dropped, reconnecting in {RECONNECT_DELAY:?}...");
				tokio::time::sleep(RECONNECT_DELAY).await;
				retry_delay = INITIAL_RETRY_DELAY;
			}
			Err(e) => {
				if is_nonretryable_error(&e) {
					tracing::warn!("Connection failed (not retrying): {e}");
					return Ok(());
				}
				tracing::debug!("Connection failed: {}, retrying in {:?}", e, retry_delay);
				tracing::warn!("Connection failed: {e}, retrying in {retry_delay:?}...");
				tokio::time::sleep(retry_delay).await;
				retry_delay = (retry_delay * 2).min(MAX_RETRY_DELAY);
			}
		}
	}
}

#[cfg(feature = "websocket")]
async fn run_ws_exit(
	global: &WallhackCli,
	endpoint: std::net::SocketAddr,
	name: &str,
	metrics: &Arc<Metrics>,
	security: &SecurityConfig,
) -> Result<()> {
	use wallhack_core::client::{
		config::ClientConfig,
		ws::{WsClient, WsClientConfig},
	};

	let client_config = WsClientConfig {
		base: ClientConfig {
			addr: endpoint,
			hostname: global.hostname.clone(),
			mtls: None,
			name: Some(name.to_string()),
			psk: security.psk.clone(),
			accept_fingerprint: security.accept_fingerprint.clone(),
			bind: endpoint.bind_addr(),
		},
		path: "/ws".to_string(),
		host_header: global.hostname.clone(),
		use_tls: true,
	};
	let mut retry_delay = INITIAL_RETRY_DELAY;

	let peer_addr = endpoint.to_string();

	loop {
		match async {
			let mut client = WsClient::new(client_config.clone())?;
			client.connect(NodeRole::Exit).await
		}
		.await
		{
			Ok(connect_result) => {
				run_exit_loop(connect_result, metrics, &peer_addr).await?;
				tracing::warn!("Connection dropped, reconnecting in {RECONNECT_DELAY:?}...");
				tokio::time::sleep(RECONNECT_DELAY).await;
				retry_delay = INITIAL_RETRY_DELAY;
			}
			Err(e) => {
				if is_nonretryable_error(&e) {
					tracing::warn!("Connection failed (not retrying): {e}");
					return Ok(());
				}
				tracing::debug!("Connection failed: {}, retrying in {:?}", e, retry_delay);
				tracing::warn!("Connection failed: {e}, retrying in {retry_delay:?}...");
				tokio::time::sleep(retry_delay).await;
				retry_delay = (retry_delay * 2).min(MAX_RETRY_DELAY);
			}
		}
	}
}

#[cfg(feature = "quic")]
fn build_quic_client_config(
	global: &WallhackCli,
	endpoint: std::net::SocketAddr,
	name: String,
	psk: Option<String>,
	accept_fingerprint: Option<String>,
) -> ClientConfig {
	let mtls = match (&global.cert, &global.key) {
		(Some(cert), Some(key)) => Some(MtlsConfig {
			cert_pem_file: cert.clone(),
			key_pem_file: key.clone(),
			ca_roots: global.ca.clone(),
		}),
		_ => None,
	};

	ClientConfig {
		addr: endpoint,
		hostname: global.hostname.clone(),
		mtls,
		name: Some(name),
		psk,
		accept_fingerprint,
		bind: endpoint.bind_addr(),
	}
}

#[cfg(feature = "quic")]
async fn run_quic_relay_capability(
	global: &WallhackCli,
	peer_addr: std::net::SocketAddr,
	listen_addr: std::net::SocketAddr,
	_node_name: &str,
	metrics: &Arc<Metrics>,
) -> Result<()> {
	use wallhack_core::{control::handler::HandlerConfig, server::server::ServerOptions};

	// Connect to peer
	let psk = global.resolve_psk();
	let client_config = build_quic_client_config(global, peer_addr, String::new(), psk, None);
	let mut client = client::quic::QuicClient::try_new(client_config)?;
	let connect_result = client.connect(NodeRole::Exit).await?;

	tracing::info!("Connected to peer {peer_addr}");

	let (relay_instr, relay_resp) = connect_result.channels().clone();

	// Start listening for peer connections
	let server_options = ServerOptions {
		handler_config: HandlerConfig::new(NodeRole::Exit),
		metrics: Some(Arc::clone(metrics)),
		peers: None,
		routes: None,
	};

	let server_config = build_server_config(global, listen_addr);
	let mut server =
		wallhack_core::server::quic::QuicServer::try_new(server_config, server_options)?;
	let bound = server.local_addr()?;
	let proto = server.protocol_name();

	tracing::info!(
		"Relay capability active: connected to {peer_addr}, listening on {bound} ({proto})"
	);

	// Accept and bridge peer connections
	loop {
		match server.accept(NodeRole::Exit).await {
			Ok(Some(accept_result)) => {
				tracing::info!("Peer connected: {}", accept_result.peer_addr());
				bridge_peer(accept_result, &relay_instr, &relay_resp);
			}
			Ok(None) => {
				tracing::info!("Server closed");
				break;
			}
			Err(e) => {
				tracing::warn!("Accept error: {e}");
			}
		}
	}

	Ok(())
}

#[cfg(feature = "websocket")]
async fn run_ws_relay_capability(
	global: &WallhackCli,
	peer_addr: std::net::SocketAddr,
	listen_addr: std::net::SocketAddr,
	node_name: &str,
	metrics: &Arc<Metrics>,
) -> Result<()> {
	use wallhack_core::{
		client::{
			config::ClientConfig,
			ws::{WsClient, WsClientConfig},
		},
		control::handler::HandlerConfig,
		server::server::ServerOptions,
	};

	// Connect to peer
	let psk = global.resolve_psk();
	let client_config = WsClientConfig {
		base: ClientConfig {
			addr: peer_addr,
			hostname: global.hostname.clone(),
			mtls: None,
			name: Some(node_name.to_string()),
			psk,
			bind: peer_addr.bind_addr(),
			..Default::default()
		},
		path: "/ws".to_string(),
		host_header: global.hostname.clone(),
		use_tls: true,
	};

	let mut client = WsClient::new(client_config)?;
	let connect_result = client.connect(NodeRole::Exit).await?;

	tracing::info!("Connected to peer {peer_addr}");

	let (relay_instr, relay_resp) = connect_result.channels().clone();

	// Start listening for peer connections
	let server_options = ServerOptions {
		handler_config: HandlerConfig::new(NodeRole::Exit),
		metrics: Some(Arc::clone(metrics)),
		peers: None,
		routes: None,
	};

	let server_config = build_server_config(global, listen_addr);
	let mut server = wallhack_core::server::ws::WsServer::try_new(server_config, server_options)?;
	let bound = server.local_addr()?;
	let proto = server.protocol_name();

	tracing::info!(
		"Relay capability active: connected to {peer_addr}, listening on {bound} ({proto})"
	);

	// Accept and bridge peer connections
	loop {
		match server.accept(NodeRole::Exit).await {
			Ok(Some(accept_result)) => {
				tracing::info!("Peer connected: {}", accept_result.peer_addr());
				bridge_peer(accept_result, &relay_instr, &relay_resp);
			}
			Ok(None) => {
				tracing::info!("Server closed");
				break;
			}
			Err(e) => {
				tracing::warn!("Accept error: {e}");
			}
		}
	}

	Ok(())
}

/// Bridge a peer connection to relay broadcast channels.
fn bridge_peer<T: wallhack_core::transport::Transport>(
	accept_result: wallhack_core::server::server::AcceptResult<T>,
	relay_instr: &tokio::sync::broadcast::Sender<wallhack_wire::data::EntryNodeInstruction>,
	relay_resp: &tokio::sync::broadcast::Sender<wallhack_wire::data::ExitNodeResponse>,
) {
	tracing::debug!("Bridging peer connection: {}", accept_result.peer_addr());

	let ((peer_instr, peer_resp), control_tx) = accept_result.channels();

	// Bridge this peer to relay broadcast channels
	let relay_instr_clone = relay_instr.clone();
	let mut relay_resp_rx = relay_resp.subscribe();
	let mut peer_instr_rx = peer_instr.subscribe();

	// Forward peer instructions to relay
	tokio::spawn(async move {
		let _keep_alive = control_tx;
		loop {
			match peer_instr_rx.recv().await {
				Ok(instr) => {
					if relay_instr_clone.send(instr).is_err() {
						tracing::warn!("Relay instruction channel closed");
						break;
					}
				}
				Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
				Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
					tracing::warn!("Lagged {n} instructions");
				}
			}
		}
	});

	// Forward relay responses to peer
	let peer_resp_clone = peer_resp.clone();
	tokio::spawn(async move {
		loop {
			match relay_resp_rx.recv().await {
				Ok(resp) => {
					if peer_resp_clone.send(resp).is_err() {
						tracing::warn!("Peer response channel closed");
						break;
					}
				}
				Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
				Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
					tracing::warn!("Lagged {n} responses");
				}
			}
		}
	});
}

fn build_server_config(
	global: &WallhackCli,
	addr: std::net::SocketAddr,
) -> wallhack_core::server::config::ServerConfig {
	let tls = match (&global.cert, &global.key) {
		(Some(cert), Some(key)) => Some(wallhack_core::server::config::TlsConfig {
			cert_pem_file: cert.clone(),
			key_pem_file: key.clone(),
			ca_roots: global.ca.clone(),
		}),
		_ => None,
	};

	wallhack_core::server::config::ServerConfig {
		listen: addr,
		tls,
		psk: global.resolve_psk(),
		max_peers: None,
	}
}

/// Check if an error is terminal and should not be retried.
fn is_nonretryable_error(err: &impl std::fmt::Display) -> bool {
	let msg = err.to_string();
	msg.contains("Fingerprint mismatch")
		|| msg.contains("PSK authentication failed")
		|| msg.contains("certificate")
		|| msg.contains("CertificateRequired")
		|| msg.contains("HandshakeFailure")
}
