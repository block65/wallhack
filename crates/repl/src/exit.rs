//! Exit node implementation.
//!
//! The exit node connects to an upstream node (relay or entry) and processes
//! incoming instructions by making syscalls to the local network.

use std::{str::FromStr, sync::Arc, time::Duration};

use anyhow::{Context, Result};

use wallhack::{
	agent::{net::SyscallAgentAdapter, orchestrator::Orchestrator},
	client::client::{Client, ClientRole, ConnectResult},
	control::metrics::Metrics,
};

#[cfg(feature = "quic")]
use wallhack::client::{self, config::ClientConfig, config::MtlsConfig};

use crate::{WallhackCli, cli::Protocol};

/// Initial retry delay for connection attempts.
const INITIAL_RETRY_DELAY: Duration = Duration::from_secs(1);
/// Maximum retry delay (caps exponential backoff).
const MAX_RETRY_DELAY: Duration = Duration::from_secs(30);

/// Run as an exit node.
///
/// Connects to upstream and processes instructions using local syscalls.
/// Retries connection forever until successful.
/// Note: Exit nodes don't run a control server since they connect as clients.
/// Control is available via the entry/relay node they connect to.
///
/// # Errors
///
/// Returns error if orchestrator fails (connection errors are retried).
pub async fn run(cli: WallhackCli) -> Result<()> {
	let connect_spec = cli.connect_spec().context("Exit node requires --connect")?;
	let agent_id = cli.agent_id();

	crate::info!("Exit node starting with agent_id: {}", agent_id);
	crate::info!("Resolving {}", connect_spec.addr);

	// Parse and resolve target address
	let resolvable = crate::dns::ResolvableAddress::from_str(&connect_spec.addr)?;
	let dns_server = cli
		.dns
		.as_ref()
		.map(|s| crate::dns::parse_str_to_addr(s))
		.transpose()?;

	let endpoint = crate::dns::resolve(resolvable, dns_server).await?;
	crate::verbose!("Resolved as {:?}", endpoint);

	match connect_spec.protocol {
		Protocol::Udp => {
			#[cfg(feature = "quic")]
			{
				run_quic_exit(&cli, endpoint, agent_id).await
			}
			#[cfg(not(feature = "quic"))]
			{
				anyhow::bail!("QUIC support not compiled in (enable 'quic' feature)")
			}
		}
		Protocol::Tcp => {
			#[cfg(feature = "websocket")]
			{
				run_ws_exit(&cli, endpoint, agent_id).await
			}
			#[cfg(not(feature = "websocket"))]
			{
				anyhow::bail!("WebSocket support not compiled in (enable 'websocket' feature)")
			}
		}
	}
}

/// Drive the exit node orchestrator with a connected client.
async fn run_exit_loop(connect_result: ConnectResult) -> Result<()> {
	crate::info!("Connected to {}", connect_result.client_ident());

	// Create syscall adapter for local network access
	let adapter = SyscallAgentAdapter::new();
	let metrics = Arc::new(Metrics::default());

	// Create orchestrator
	let orchestrator = Orchestrator::new(Arc::new(adapter), metrics);

	// Split into channels and connection tasks
	let ((instr, resp), mut tasks) = connect_result.into_parts();

	// Run orchestrator and monitor connection health concurrently.
	// If either the orchestrator exits OR the connection tasks die, we reconnect.
	let orchestrator_fut = orchestrator.drive(resp, instr.subscribe());
	let disconnect_fut = tasks.wait_for_disconnect();

	tokio::select! {
		result = orchestrator_fut => {
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
		_ = disconnect_fut => {
			crate::info!("Connection tasks died - transport disconnected");
			println!("Transport disconnected, reconnecting...");
		}
	}

	Ok(())
}

#[cfg(feature = "quic")]
async fn run_quic_exit(
	cli: &WallhackCli,
	endpoint: std::net::SocketAddr,
	agent_id: String,
) -> Result<()> {
	let client_config = build_quic_client_config(cli, endpoint, agent_id);
	let mut retry_delay = INITIAL_RETRY_DELAY;

	loop {
		let mut client = client::quic::QuicClient::try_new(client_config.clone())?;

		match client.connect(ClientRole::Agent).await {
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
	cli: &WallhackCli,
	endpoint: std::net::SocketAddr,
	agent_id: String,
) -> Result<()> {
	use wallhack::client::{
		config::ClientConfig,
		ws::{WsClient, WsClientConfig},
	};

	let client_config = WsClientConfig {
		base: ClientConfig {
			addr: endpoint,
			hostname: cli.hostname.clone(),
			mtls: None,
			agent_id: Some(agent_id),
			..Default::default()
		},
		path: "/ws".to_string(),
		host_header: cli.hostname.clone(),
		use_tls: cli.cert.is_some() || cli.key.is_some(),
	};
	let mut retry_delay = INITIAL_RETRY_DELAY;

	loop {
		let mut client = WsClient::new(client_config.clone())?;

		match client.connect(ClientRole::Agent).await {
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
	cli: &WallhackCli,
	endpoint: std::net::SocketAddr,
	agent_id: String,
) -> ClientConfig {
	let mtls = match (&cli.cert, &cli.key) {
		(Some(cert), Some(key)) => Some(MtlsConfig {
			cert_pem_file: cert.clone(),
			key_pem_file: key.clone(),
			ca_roots: cli.ca.clone(),
		}),
		_ => None,
	};

	ClientConfig {
		addr: endpoint,
		hostname: cli.hostname.clone(),
		mtls,
		agent_id: Some(agent_id),
		..Default::default()
	}
}
