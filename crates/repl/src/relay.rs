//! Relay node implementation.
//!
//! A relay node connects upstream to an entry/relay and listens downstream
//! for exit nodes. It forwards messages between them without processing.

use std::{str::FromStr, sync::Arc, time::Duration};

use anyhow::{Context, Result};
use tokio::sync::broadcast;

use wallhack::{
	client::client::{Client, ClientRole, ConnectResult},
	control::{handler::HandlerConfig, metrics::Metrics},
	server::server::{AcceptResult, Server, ServerOptions, ServerRole},
};

#[cfg(feature = "quic")]
use wallhack::{client, server};

use crate::{WallhackCli, cli::Protocol};

/// Initial retry delay for connection attempts.
const INITIAL_RETRY_DELAY: Duration = Duration::from_secs(1);
/// Maximum retry delay (caps exponential backoff).
const MAX_RETRY_DELAY: Duration = Duration::from_secs(30);

/// Run as a relay node.
///
/// Connects upstream and listens for downstream connections, forwarding
/// messages between them. Retries upstream connection forever.
/// Control commands are handled on the same connection via bidirectional streams.
///
/// # Errors
///
/// Returns error if server fails (connection errors are retried).
pub async fn run(cli: WallhackCli) -> Result<()> {
	let connect_spec = cli
		.connect_spec()
		.context("Relay node requires --connect")?;

	let listen_spec = cli.listen_spec();

	// Parse listen address
	let addr = parse_listen_addr(&listen_spec.addr)?;

	// Shared metrics across all connections and control
	let metrics = Arc::new(Metrics::default());

	// Server options with control handler config
	let server_options = ServerOptions {
		handler_config: HandlerConfig {
			node_role: "relay".to_string(),
			..Default::default()
		},
		metrics: Some(Arc::clone(&metrics)),
	};

	// Resolve upstream target
	crate::info!("Resolving upstream: {}", connect_spec.addr);
	let resolvable = crate::dns::ResolvableAddress::from_str(&connect_spec.addr)?;
	let dns_server = cli
		.dns
		.as_ref()
		.map(|s| crate::dns::parse_str_to_addr(s))
		.transpose()?;

	let upstream_addr = crate::dns::resolve(resolvable, dns_server).await?;

	// Connect to upstream with retry based on protocol
	crate::info!(
		"Connecting to upstream: {} ({:?})",
		upstream_addr,
		connect_spec.protocol
	);
	println!("Connecting to upstream: {upstream_addr}");

	let upstream_client = match connect_spec.protocol {
		Protocol::Udp => {
			#[cfg(feature = "quic")]
			{
				connect_quic_upstream(&cli, upstream_addr).await?
			}
			#[cfg(not(feature = "quic"))]
			{
				anyhow::bail!("QUIC support not compiled in (enable 'quic' feature)")
			}
		}
		Protocol::Tcp => {
			#[cfg(feature = "websocket")]
			{
				connect_ws_upstream(&cli, upstream_addr).await?
			}
			#[cfg(not(feature = "websocket"))]
			{
				anyhow::bail!("WebSocket support not compiled in (enable 'websocket' feature)")
			}
		}
	};

	let (upstream_instr, upstream_resp) = upstream_client.channels().clone();
	crate::info!("Connected to upstream");

	// TODO: Monitor upstream_client connection health and reconnect on failure

	// Start downstream listener based on protocol
	crate::info!(
		"Listening for downstream on {} ({:?})",
		addr,
		listen_spec.protocol
	);

	match listen_spec.protocol {
		Protocol::Udp => {
			#[cfg(feature = "quic")]
			{
				run_quic_downstream(&cli, addr, server_options, upstream_instr, upstream_resp).await
			}
			#[cfg(not(feature = "quic"))]
			{
				anyhow::bail!("QUIC support not compiled in (enable 'quic' feature)")
			}
		}
		Protocol::Tcp => {
			#[cfg(feature = "websocket")]
			{
				run_ws_downstream(&cli, addr, server_options, upstream_instr, upstream_resp).await
			}
			#[cfg(not(feature = "websocket"))]
			{
				anyhow::bail!("WebSocket support not compiled in (enable 'websocket' feature)")
			}
		}
	}
}

/// Bridge a downstream connection to upstream channels.
fn bridge_downstream(
	accept_result: AcceptResult,
	upstream_instr: &broadcast::Sender<protobuf::v2::HostInstruction>,
	upstream_resp: &broadcast::Sender<protobuf::v2::AgentResponse>,
) {
	crate::info!("Downstream connected: {}", accept_result.client_ident());

	let (downstream_instr, downstream_resp) = accept_result.channels();

	// Bridge this downstream connection to upstream
	let upstream_instr_clone = upstream_instr.clone();
	let mut upstream_resp_rx = upstream_resp.subscribe();
	let mut downstream_instr_rx = downstream_instr.subscribe();

	// Forward downstream instructions to upstream
	tokio::spawn(async move {
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
	cli: &WallhackCli,
	addr: std::net::SocketAddr,
) -> Result<ConnectResult> {
	let client_config = client::config::ClientConfig {
		addr,
		hostname: cli.hostname.clone(),
		mtls: None, // TODO: add mTLS support
		..Default::default()
	};

	let mut retry_delay = INITIAL_RETRY_DELAY;

	loop {
		let mut client = client::quic::QuicClient::try_new(client_config.clone())?;

		match client.connect(ClientRole::Host).await {
			Ok(result) => return Ok(result),
			Err(e) => {
				crate::info!(
					"Upstream connection failed: {}, retrying in {:?}",
					e,
					retry_delay
				);
				println!("Connection failed: {e}, retrying in {retry_delay:?}...");
				tokio::time::sleep(retry_delay).await;
				retry_delay = (retry_delay * 2).min(MAX_RETRY_DELAY);
			}
		}
	}
}

#[cfg(feature = "websocket")]
async fn connect_ws_upstream(
	cli: &WallhackCli,
	addr: std::net::SocketAddr,
) -> Result<ConnectResult> {
	use wallhack::client::{
		config::ClientConfig,
		ws::{WsClient, WsClientConfig},
	};

	let client_config = WsClientConfig {
		base: ClientConfig {
			addr,
			hostname: cli.hostname.clone(),
			mtls: None,
			..Default::default()
		},
		path: "/ws".to_string(),
		host_header: cli.hostname.clone(),
		use_tls: cli.cert.is_some() || cli.key.is_some(),
	};

	let mut retry_delay = INITIAL_RETRY_DELAY;

	loop {
		let mut client = WsClient::new(client_config.clone())?;

		match client.connect(ClientRole::Host).await {
			Ok(result) => return Ok(result),
			Err(e) => {
				crate::info!(
					"Upstream connection failed: {}, retrying in {:?}",
					e,
					retry_delay
				);
				println!("Connection failed: {e}, retrying in {retry_delay:?}...");
				tokio::time::sleep(retry_delay).await;
				retry_delay = (retry_delay * 2).min(MAX_RETRY_DELAY);
			}
		}
	}
}

#[cfg(feature = "quic")]
async fn run_quic_downstream(
	cli: &WallhackCli,
	addr: std::net::SocketAddr,
	server_options: ServerOptions,
	upstream_instr: broadcast::Sender<protobuf::v2::HostInstruction>,
	upstream_resp: broadcast::Sender<protobuf::v2::AgentResponse>,
) -> Result<()> {
	let server_config = build_quic_server_config(cli, addr);
	let mut server = server::quic::QuicServer::try_new(server_config, server_options)?;

	loop {
		match server.accept(ServerRole::Host).await {
			Ok(Some(accept_result)) => {
				bridge_downstream(accept_result, &upstream_instr, &upstream_resp);
			}
			Ok(None) => {
				crate::info!("Server closed");
				break;
			}
			Err(e) => {
				crate::error!("Accept error: {}", e);
			}
		}
	}

	Ok(())
}

#[cfg(feature = "websocket")]
async fn run_ws_downstream(
	cli: &WallhackCli,
	addr: std::net::SocketAddr,
	server_options: ServerOptions,
	upstream_instr: broadcast::Sender<protobuf::v2::HostInstruction>,
	upstream_resp: broadcast::Sender<protobuf::v2::AgentResponse>,
) -> Result<()> {
	use wallhack::server::{config::ServerConfig, ws::WsServer};

	let tls = match (&cli.cert, &cli.key) {
		(Some(cert), Some(key)) => Some(wallhack::server::config::TlsConfig {
			cert_pem_file: cert.clone(),
			key_pem_file: key.clone(),
			ca_roots: cli.ca.clone(),
		}),
		_ => None,
	};

	let server_config = ServerConfig { listen: addr, tls };
	let mut server = WsServer::try_new(server_config, server_options)?;

	loop {
		match server.accept(ServerRole::Host).await {
			Ok(Some(accept_result)) => {
				bridge_downstream(accept_result, &upstream_instr, &upstream_resp);
			}
			Ok(None) => {
				crate::info!("Server closed");
				break;
			}
			Err(e) => {
				crate::error!("Accept error: {}", e);
			}
		}
	}

	Ok(())
}

fn parse_listen_addr(addr: &str) -> Result<std::net::SocketAddr> {
	// Handle short form like ":6565" -> "[::]:6565"
	let full_addr = if let Some(port) = addr.strip_prefix(':') {
		format!("[::]:{port}")
	} else {
		addr.to_string()
	};

	full_addr
		.parse()
		.with_context(|| format!("Invalid listen address: {full_addr}"))
}

#[cfg(feature = "quic")]
fn build_quic_server_config(
	cli: &WallhackCli,
	addr: std::net::SocketAddr,
) -> server::config::ServerConfig {
	let tls = match (&cli.cert, &cli.key) {
		(Some(cert), Some(key)) => Some(server::config::TlsConfig {
			cert_pem_file: cert.clone(),
			key_pem_file: key.clone(),
			ca_roots: cli.ca.clone(),
		}),
		_ => None,
	};

	server::config::ServerConfig { listen: addr, tls }
}
