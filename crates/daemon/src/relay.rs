//! Relay node implementation.
//!
//! A relay node connects upstream to an entry/relay and listens downstream
//! for exit nodes. It forwards messages between them without processing.

use std::{str::FromStr, sync::Arc, time::Duration};

use anyhow::{Context, Result};
use tokio::sync::broadcast;

use wallhack_core::{
	NodeRole,
	client::client::{Client, ConnectResult},
	control::{handler::HandlerConfig, metrics::Metrics},
	server::server::{AcceptResult, Server, ServerOptions},
};

#[cfg(feature = "quic")]
use wallhack_core::{client, server};

use crate::{
	WallhackCli,
	cli::{Protocol, RelayCommand},
	net::{SocketAddrExt, parse_listen_addr},
};

/// Initial retry delay for connection attempts.
const INITIAL_RETRY_DELAY: Duration = Duration::from_millis(50);
/// Maximum retry delay (caps exponential backoff).
const MAX_RETRY_DELAY: Duration = Duration::from_secs(30);

/// Run as a relay node.
///
/// Connects upstream and listens for downstream connections, forwarding
/// messages between them. Retries upstream connection forever.
///
/// # Errors
///
/// Returns error if server fails (connection errors are retried).
pub async fn run(global: &WallhackCli, cmd: &RelayCommand, metrics: Arc<Metrics>) -> Result<()> {
	let name = cmd.name();
	tracing::info!(
		"wallhack {}  {name}",
		crate::version::built_info::PKG_VERSION
	);

	let (connect_spec, listen_spec) = cmd.transport().map_err(|e| anyhow::anyhow!("{e}"))?;

	// Parse listen address
	let addr = parse_listen_addr(&listen_spec.addr)?;

	// Server options with control handler config
	let server_options = ServerOptions {
		handler_config: HandlerConfig::new(NodeRole::Relay),
		metrics: Some(Arc::clone(&metrics)),
		peers: None,
		routes: None,
	};

	tracing::info!("Connecting to {}...", connect_spec.addr);
	let resolvable = crate::dns::ResolvableAddress::from_str(&connect_spec.addr)?;
	tracing::debug!("Resolving {}...", connect_spec.addr);
	let dns_server = global
		.dns
		.as_ref()
		.map(|s| crate::dns::parse_str_to_addr(s))
		.transpose()?;

	let is_hostname = resolvable.hostname.parse::<std::net::IpAddr>().is_err();
	let upstream_addr = crate::dns::resolve(resolvable, dns_server).await?;
	if is_hostname {
		tracing::info!("Resolved {} as {}", connect_spec.addr, upstream_addr);
	}

	let psk = global.resolve_psk();

	match connect_spec.protocol {
		Protocol::Udp => {
			#[cfg(feature = "quic")]
			{
				let upstream_client = connect_quic_upstream(
					global,
					upstream_addr,
					psk.as_deref(),
					cmd.accept_fingerprint.as_deref(),
				)
				.await?;
				let (upstream_instr, upstream_resp) = upstream_client.channels().clone();
				run_downstream(
					global,
					&listen_spec,
					addr,
					server_options,
					upstream_instr,
					upstream_resp,
				)
				.await
			}
			#[cfg(not(feature = "quic"))]
			{
				anyhow::bail!("QUIC support not compiled in (enable 'quic' feature)")
			}
		}
		Protocol::Tcp => {
			#[cfg(feature = "websocket")]
			{
				let upstream_client = connect_ws_upstream(
					global,
					upstream_addr,
					psk.as_deref(),
					cmd.accept_fingerprint.as_deref(),
				)
				.await?;
				let (upstream_instr, upstream_resp) = upstream_client.channels().clone();
				run_downstream(
					global,
					&listen_spec,
					addr,
					server_options,
					upstream_instr,
					upstream_resp,
				)
				.await
			}
			#[cfg(not(feature = "websocket"))]
			{
				anyhow::bail!("WebSocket support not compiled in (enable 'websocket' feature)")
			}
		}
	}
}

async fn run_downstream(
	global: &WallhackCli,
	listen_spec: &crate::cli::AddressSpec,
	addr: std::net::SocketAddr,
	server_options: ServerOptions,
	upstream_instr: broadcast::Sender<wallhack_wire::data::EntryNodeInstruction>,
	upstream_resp: broadcast::Sender<wallhack_wire::data::ExitNodeResponse>,
) -> Result<()> {
	match listen_spec.protocol {
		Protocol::Udp => {
			#[cfg(feature = "quic")]
			{
				run_quic_downstream(global, addr, server_options, upstream_instr, upstream_resp)
					.await
			}
			#[cfg(not(feature = "quic"))]
			{
				anyhow::bail!("QUIC support not compiled in (enable 'quic' feature)")
			}
		}
		Protocol::Tcp => {
			#[cfg(feature = "websocket")]
			{
				run_ws_downstream(global, addr, server_options, upstream_instr, upstream_resp).await
			}
			#[cfg(not(feature = "websocket"))]
			{
				anyhow::bail!("WebSocket support not compiled in (enable 'websocket' feature)")
			}
		}
	}
}

/// Bridge a downstream connection to upstream channels.
fn bridge_downstream<T: wallhack_core::transport::Transport>(
	accept_result: AcceptResult<T>,
	upstream_instr: &broadcast::Sender<wallhack_wire::data::EntryNodeInstruction>,
	upstream_resp: &broadcast::Sender<wallhack_wire::data::ExitNodeResponse>,
) {
	tracing::info!("Downstream connected: {}", accept_result.peer_addr());

	let ((downstream_instr, downstream_resp), control_tx) = accept_result.channels();

	// Bridge this downstream connection to upstream
	let upstream_instr_clone = upstream_instr.clone();
	let mut upstream_resp_rx = upstream_resp.subscribe();
	let mut downstream_instr_rx = downstream_instr.subscribe();

	// Forward downstream instructions to upstream (also holds control_tx to keep control stream alive)
	tokio::spawn(async move {
		let _keep_alive = control_tx;
		loop {
			match downstream_instr_rx.recv().await {
				Ok(instr) => {
					if upstream_instr_clone.send(instr).is_err() {
						tracing::warn!("Upstream instruction channel closed");
						break;
					}
				}
				Err(broadcast::error::RecvError::Closed) => break,
				Err(broadcast::error::RecvError::Lagged(n)) => {
					tracing::warn!("Lagged {} instructions", n);
				}
			}
		}
	});

	// Forward upstream responses to downstream
	let downstream_resp_clone = downstream_resp.clone();
	tokio::spawn(async move {
		loop {
			match upstream_resp_rx.recv().await {
				Ok(resp) => {
					if downstream_resp_clone.send(resp).is_err() {
						tracing::warn!("Downstream response channel closed");
						break;
					}
				}
				Err(broadcast::error::RecvError::Closed) => break,
				Err(broadcast::error::RecvError::Lagged(n)) => {
					tracing::warn!("Lagged {} responses", n);
				}
			}
		}
	});
}

#[cfg(feature = "quic")]
async fn connect_quic_upstream(
	global: &WallhackCli,
	addr: std::net::SocketAddr,
	psk: Option<&str>,
	accept_fingerprint: Option<&str>,
) -> Result<ConnectResult<wallhack_core::transport::quic::QuicTransport>> {
	let client_config = client::config::ClientConfig {
		addr,
		hostname: global.hostname.clone(),
		mtls: None,
		psk: psk.map(std::string::ToString::to_string),
		accept_fingerprint: accept_fingerprint.map(std::string::ToString::to_string),
		bind: addr.bind_addr(),
		..Default::default()
	};

	let mut retry_delay = INITIAL_RETRY_DELAY;

	loop {
		let mut client = client::quic::QuicClient::try_new(client_config.clone())?;

		match client.connect(NodeRole::Relay).await {
			Ok(result) => return Ok(result),
			Err(e) => {
				if crate::is_nonretryable_error(&e) {
					return Err(e).context("connection failed, not retrying");
				}
				tracing::debug!(
					"Upstream connection failed: {}, retrying in {:?}",
					e,
					retry_delay
				);
				tracing::warn!("Connection failed: {e}, retrying in {retry_delay:?}...");
				tokio::time::sleep(retry_delay).await;
				retry_delay = (retry_delay * 2).min(MAX_RETRY_DELAY);
			}
		}
	}
}

#[cfg(feature = "websocket")]
async fn connect_ws_upstream(
	global: &WallhackCli,
	addr: std::net::SocketAddr,
	psk: Option<&str>,
	accept_fingerprint: Option<&str>,
) -> Result<ConnectResult<wallhack_core::transport::ws::WsTransport>> {
	use wallhack_core::client::{
		config::ClientConfig,
		ws::{WsClient, WsClientConfig},
	};

	let client_config = WsClientConfig {
		base: ClientConfig {
			addr,
			hostname: global.hostname.clone(),
			mtls: None,
			psk: psk.map(std::string::ToString::to_string),
			accept_fingerprint: accept_fingerprint.map(std::string::ToString::to_string),
			bind: addr.bind_addr(),
			..Default::default()
		},
		path: "/ws".to_string(),
		host_header: global.hostname.clone(),
		use_tls: true,
	};

	let mut retry_delay = INITIAL_RETRY_DELAY;

	loop {
		let mut client = WsClient::new(client_config.clone())?;

		match client.connect(NodeRole::Relay).await {
			Ok(result) => return Ok(result),
			Err(e) => {
				if crate::is_nonretryable_error(&e) {
					return Err(e).context("connection failed, not retrying");
				}
				tracing::debug!(
					"Upstream connection failed: {}, retrying in {:?}",
					e,
					retry_delay
				);
				tracing::warn!("Connection failed: {e}, retrying in {retry_delay:?}...");
				tokio::time::sleep(retry_delay).await;
				retry_delay = (retry_delay * 2).min(MAX_RETRY_DELAY);
			}
		}
	}
}

#[cfg(feature = "quic")]
async fn run_quic_downstream(
	global: &WallhackCli,
	addr: std::net::SocketAddr,
	server_options: ServerOptions,
	upstream_instr: broadcast::Sender<wallhack_wire::data::EntryNodeInstruction>,
	upstream_resp: broadcast::Sender<wallhack_wire::data::ExitNodeResponse>,
) -> Result<()> {
	let server_config = build_server_config(global, addr);
	let mut server = server::quic::QuicServer::try_new(server_config, server_options)?;
	tracing::info!("Listening on {} (QUIC)", server.local_addr()?);

	loop {
		match server.accept(NodeRole::Relay).await {
			Ok(Some(accept_result)) => {
				bridge_downstream(accept_result, &upstream_instr, &upstream_resp);
			}
			Ok(None) => {
				tracing::info!("Server closed");
				break;
			}
			Err(e) => {
				tracing::warn!("Accept error: {}", e);
			}
		}
	}

	Ok(())
}

#[cfg(feature = "websocket")]
async fn run_ws_downstream(
	global: &WallhackCli,
	addr: std::net::SocketAddr,
	server_options: ServerOptions,
	upstream_instr: broadcast::Sender<wallhack_wire::data::EntryNodeInstruction>,
	upstream_resp: broadcast::Sender<wallhack_wire::data::ExitNodeResponse>,
) -> Result<()> {
	use wallhack_core::server::ws::WsServer;

	let server_config = build_server_config(global, addr);
	let mut server = WsServer::try_new(server_config, server_options)?;
	tracing::info!("Listening on {} (WebSocket)", server.local_addr()?);

	loop {
		match server.accept(NodeRole::Relay).await {
			Ok(Some(accept_result)) => {
				bridge_downstream(accept_result, &upstream_instr, &upstream_resp);
			}
			Ok(None) => {
				tracing::info!("Server closed");
				break;
			}
			Err(e) => {
				tracing::warn!("Accept error: {}", e);
			}
		}
	}

	Ok(())
}

#[cfg(any(feature = "quic", feature = "websocket"))]
fn build_server_config(
	global: &WallhackCli,
	addr: std::net::SocketAddr,
) -> server::config::ServerConfig {
	let tls = match (&global.cert, &global.key) {
		(Some(cert), Some(key)) => Some(server::config::TlsConfig {
			cert_pem_file: cert.clone(),
			key_pem_file: key.clone(),
			ca_roots: global.ca.clone(),
		}),
		_ => None,
	};

	server::config::ServerConfig {
		listen: addr,
		tls,
		psk: global.resolve_psk(),
		max_peers: None,
	}
}
