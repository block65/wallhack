//! Exit node implementation.
//!
//! The exit node processes incoming instructions by making syscalls to the
//! local network. It can either connect to an upstream peer (default) or
//! listen for incoming connections (reverse tunnel).

use std::{str::FromStr, sync::Arc, time::Duration};

use anyhow::{Context, Result};
use tokio::io::AsyncWriteExt;

use wallhack::{
	NodeRole,
	client::client::{Client, ConnectResult},
	control::metrics::Metrics,
	exit::{net::SyscallExitAdapter, orchestrator::Orchestrator},
	transport::{
		BiStream, Transport,
		bridge::{SESSION_INIT_MTU, read_length_delimited},
	},
};

#[cfg(feature = "quic")]
use wallhack::client::{self, config::ClientConfig, config::MtlsConfig};

use crate::{
	WallhackCli,
	cli::{ExitCommand, Protocol, TransportDir},
};

/// Initial retry delay for connection attempts.
const INITIAL_RETRY_DELAY: Duration = Duration::from_secs(1);
/// Maximum retry delay (caps exponential backoff).
const MAX_RETRY_DELAY: Duration = Duration::from_secs(30);
/// Timeout for UDP response after forwarding packet.
///
/// Each UDP packet opens a QUIC bi-stream and waits for a response.
/// Without a timeout, streams accumulate indefinitely when targets don't
/// respond (common for UDP), eventually hitting QUIC's stream limit.
///
/// 500ms is aggressive but sufficient for:
/// - LAN responses (< 10ms typical)
/// - DNS queries (< 100ms typical)
/// - Most interactive UDP protocols
///
/// For slower protocols, streams queue on entry node (backpressure).
const UDP_RESPONSE_TIMEOUT: Duration = Duration::from_millis(500);

/// Run as an exit node.
///
/// Either connects to an upstream peer or listens for incoming connections
/// (reverse tunnel). Processes instructions using local syscalls.
///
/// # Errors
///
/// Returns error if orchestrator fails (connection errors are retried).
pub async fn run(global: &WallhackCli, cmd: &ExitCommand) -> Result<()> {
	let transport = cmd.transport().map_err(|e| anyhow::anyhow!("{e}"))?;
	let exit_id = cmd.exit_id();

	match transport {
		TransportDir::Connect(spec) => {
			crate::info!("Exit node starting with exit_id: {exit_id}");
			crate::info!("Resolving {}", spec.addr);

			let resolvable = crate::dns::ResolvableAddress::from_str(&spec.addr)?;
			let dns_server = global
				.dns
				.as_ref()
				.map(|s| crate::dns::parse_str_to_addr(s))
				.transpose()?;

			let endpoint = crate::dns::resolve(resolvable, dns_server).await?;
			crate::verbose!("Resolved as {endpoint:?}");

			match spec.protocol {
				Protocol::Udp => {
					#[cfg(feature = "quic")]
					{
						run_quic_exit(global, endpoint, exit_id).await
					}
					#[cfg(not(feature = "quic"))]
					{
						anyhow::bail!("QUIC support not compiled in (enable 'quic' feature)")
					}
				}
				Protocol::Tcp => {
					#[cfg(feature = "websocket")]
					{
						run_ws_exit(global, endpoint, exit_id).await
					}
					#[cfg(not(feature = "websocket"))]
					{
						anyhow::bail!(
							"WebSocket support not compiled in (enable 'websocket' feature)"
						)
					}
				}
			}
		}
		TransportDir::Listen(spec) => run_exit_listen(global, cmd, &spec, exit_id).await,
	}
}

/// Run exit node in listen mode (reverse tunnel).
///
/// Listens for an incoming connection from an entry node, then processes
/// instructions using local syscalls.
async fn run_exit_listen(
	global: &WallhackCli,
	_cmd: &ExitCommand,
	spec: &crate::cli::AddressSpec,
	exit_id: String,
) -> Result<()> {
	use wallhack::{
		control::handler::HandlerConfig,
		server::server::{Server, ServerOptions},
	};

	let addr = parse_listen_addr(&spec.addr)?;
	let metrics = Arc::new(Metrics::default());

	let server_options = ServerOptions {
		handler_config: HandlerConfig::new(NodeRole::Exit),
		metrics: Some(Arc::clone(&metrics)),
		peers: None,
		routes: None,
	};

	let server_config = build_server_config(global, addr);

	crate::info!("Exit node {exit_id} listening on {addr}");

	match spec.protocol {
		Protocol::Udp => {
			#[cfg(feature = "quic")]
			{
				let mut server =
					wallhack::server::quic::QuicServer::try_new(server_config, server_options)?;
				crate::info!("Listening on {addr} (QUIC/UDP)");
				loop {
					match server.accept(NodeRole::Exit).await {
						Ok(Some(accept_result)) => {
							crate::info!(
								"Accepted connection from {}",
								accept_result.client_ident()
							);
							let transport = accept_result.transport();
							let adapter = SyscallExitAdapter::new();
							let orchestrator =
								Orchestrator::new(Arc::new(adapter), Arc::clone(&metrics));
							let stream_fut = run_stream_listener(transport);
							let (instr, resp) = accept_result.channels();
							tokio::select! {
								result = orchestrator.drive(resp.clone(), instr.subscribe()) => {
									if let Err(e) = result {
										crate::error!("Orchestrator error: {e}");
									}
								}
								result = stream_fut => {
									if let Err(e) = result {
										crate::error!("Stream handler error: {e}");
									}
								}
							}
						}
						Ok(None) => break,
						Err(e) => {
							crate::error!("Accept error: {e}");
						}
					}
				}
			}
			#[cfg(not(feature = "quic"))]
			anyhow::bail!("QUIC transport not available (compile with --features quic)")
		}
		Protocol::Tcp => {
			#[cfg(feature = "websocket")]
			{
				let mut server =
					wallhack::server::ws::WsServer::try_new(server_config, server_options)?;
				crate::info!("Listening on {addr} (WebSocket/TCP)");
				loop {
					match server.accept(NodeRole::Exit).await {
						Ok(Some(accept_result)) => {
							crate::info!(
								"Accepted connection from {}",
								accept_result.client_ident()
							);
							let transport = accept_result.transport();
							let adapter = SyscallExitAdapter::new();
							let orchestrator =
								Orchestrator::new(Arc::new(adapter), Arc::clone(&metrics));
							let stream_fut = run_stream_listener(transport);
							let (instr, resp) = accept_result.channels();
							tokio::select! {
								result = orchestrator.drive(resp.clone(), instr.subscribe()) => {
									if let Err(e) = result {
										crate::error!("Orchestrator error: {e}");
									}
								}
								result = stream_fut => {
									if let Err(e) = result {
										crate::error!("Stream handler error: {e}");
									}
								}
							}
						}
						Ok(None) => break,
						Err(e) => {
							crate::error!("Accept error: {e}");
						}
					}
				}
			}
			#[cfg(not(feature = "websocket"))]
			anyhow::bail!("WebSocket transport not available (compile with --features websocket)")
		}
	}

	Ok(())
}

/// Drive the exit node orchestrator with a connected client.
async fn run_exit_loop<T: wallhack::transport::Transport + 'static>(
	connect_result: ConnectResult<T>,
) -> Result<()> {
	crate::info!("Connected to {}", connect_result.client_ident());

	// Create syscall adapter for local network access
	let adapter = SyscallExitAdapter::new();
	let metrics = Arc::new(Metrics::default());

	let orchestrator = Orchestrator::new(Arc::new(adapter), metrics);

	let transport = connect_result.transport();
	let ((instr, resp), mut tasks) = connect_result.into_parts();
	let stream_fut = run_stream_listener(transport);
	let disconnect_fut = tasks.wait_for_disconnect();

	tokio::select! {
		result = orchestrator.drive(resp, instr.subscribe()) => {
			match result {
				Ok(()) => {
					crate::info!("Connection closed cleanly");
					println!("Connection closed, reconnecting...");
				}
				Err(e) => {
					crate::error!("Orchestrator error: {}", e);
					println!("Connection error: {e}, reconnecting...");
				}
			}
		}
		result = stream_fut => {
			if let Err(e) = result {
				crate::error!("Stream handler error: {e}");
			}
		}
		() = disconnect_fut => {
			crate::info!("Connection tasks died - transport disconnected");
			println!("Transport disconnected, reconnecting...");
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
	let init = read_length_delimited::<protobuf::v2::SessionInit, _>(stream, SESSION_INIT_MTU)
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
		val if val == protobuf::v2::SessionProtocol::Tcp as i32 => {
			// Note: source address is informational only, we don't bind to it
			// because it may not exist in our namespace
			let mut socket = tokio::net::TcpStream::connect(target).await?;
			let _ = tokio::io::copy_bidirectional(&mut *stream, &mut socket).await?;
		}
		val if val == protobuf::v2::SessionProtocol::Udp as i32 => {
			// Note: source address is informational only, we don't bind to it
			// because it may not exist in our namespace (same as TCP)
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
			let mut buf = Vec::new();
			tokio::io::AsyncReadExt::read_to_end(stream, &mut buf).await?;
			tracing::trace!(buf_len = buf.len(), "Read UDP payload from stream");
			if !buf.is_empty() {
				tracing::trace!(target = %target, "Sending UDP to target");
				let _ = socket.send_to(&buf, target).await?;
				let mut recv_buf = vec![0u8; 65535];
				tracing::trace!("Waiting for UDP response...");
				// Use timeout to avoid hanging streams that accumulate
				match tokio::time::timeout(UDP_RESPONSE_TIMEOUT, socket.recv_from(&mut recv_buf))
					.await
				{
					Ok(Ok((size, from))) => {
						tracing::trace!(size, from = %from, "Received UDP response");
						stream.write_all(&recv_buf[..size]).await?;
					}
					Ok(Err(e)) => {
						tracing::trace!("UDP recv error: {e}");
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
	exit_id: String,
) -> Result<()> {
	let client_config = build_quic_client_config(global, endpoint, exit_id);
	let mut retry_delay = INITIAL_RETRY_DELAY;

	loop {
		let mut client = client::quic::QuicClient::try_new(client_config.clone())?;

		match client.connect(NodeRole::Exit).await {
			Ok(connect_result) => {
				retry_delay = INITIAL_RETRY_DELAY;
				run_exit_loop(connect_result).await?;
			}
			Err(e) => {
				crate::info!("Connection failed: {}, retrying in {:?}", e, retry_delay);
				println!("Connection failed: {e}, retrying in {retry_delay:?}...");
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
	exit_id: String,
) -> Result<()> {
	use wallhack::client::{
		config::ClientConfig,
		ws::{WsClient, WsClientConfig},
	};

	let client_config = WsClientConfig {
		base: ClientConfig {
			addr: endpoint,
			hostname: global.hostname.clone(),
			mtls: None,
			exit_id: Some(exit_id),
			..Default::default()
		},
		path: "/ws".to_string(),
		host_header: global.hostname.clone(),
		use_tls: global.cert.is_some() || global.key.is_some(),
	};
	let mut retry_delay = INITIAL_RETRY_DELAY;

	loop {
		let mut client = WsClient::new(client_config.clone())?;

		match client.connect(NodeRole::Exit).await {
			Ok(connect_result) => {
				retry_delay = INITIAL_RETRY_DELAY;
				run_exit_loop(connect_result).await?;
			}
			Err(e) => {
				crate::info!("Connection failed: {}, retrying in {:?}", e, retry_delay);
				println!("Connection failed: {e}, retrying in {retry_delay:?}...");
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
	exit_id: String,
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
		exit_id: Some(exit_id),
		..Default::default()
	}
}

fn parse_listen_addr(addr: &str) -> Result<std::net::SocketAddr> {
	let full_addr = if let Some(port) = addr.strip_prefix(':') {
		format!("[::]:{port}")
	} else {
		addr.to_string()
	};

	full_addr
		.parse()
		.with_context(|| format!("Invalid listen address: {full_addr}"))
}

fn build_server_config(
	global: &WallhackCli,
	addr: std::net::SocketAddr,
) -> wallhack::server::config::ServerConfig {
	let tls = match (&global.cert, &global.key) {
		(Some(cert), Some(key)) => Some(wallhack::server::config::TlsConfig {
			cert_pem_file: cert.clone(),
			key_pem_file: key.clone(),
			ca_roots: global.ca.clone(),
		}),
		_ => None,
	};

	wallhack::server::config::ServerConfig { listen: addr, tls }
}
