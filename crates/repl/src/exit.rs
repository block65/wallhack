//! Exit node implementation.
//!
//! The exit node connects to an upstream node (relay or entry) and processes
//! incoming instructions by making syscalls to the local network.

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
	let exit_id = cli.exit_id();

	crate::info!("Exit node starting with exit_id: {}", exit_id);
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
				run_quic_exit(&cli, endpoint, exit_id).await
			}
			#[cfg(not(feature = "quic"))]
			{
				anyhow::bail!("QUIC support not compiled in (enable 'quic' feature)")
			}
		}
		Protocol::Tcp => {
			#[cfg(feature = "websocket")]
			{
				run_ws_exit(&cli, endpoint, exit_id).await
			}
			#[cfg(not(feature = "websocket"))]
			{
				anyhow::bail!("WebSocket support not compiled in (enable 'websocket' feature)")
			}
		}
	}
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
				let (size, from) = socket.recv_from(&mut recv_buf).await?;
				tracing::trace!(size, from = %from, "Received UDP response");
				stream.write_all(&recv_buf[..size]).await?;
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
	cli: &WallhackCli,
	endpoint: std::net::SocketAddr,
	exit_id: String,
) -> Result<()> {
	let client_config = build_quic_client_config(cli, endpoint, exit_id);
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
	cli: &WallhackCli,
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
			hostname: cli.hostname.clone(),
			mtls: None,
			exit_id: Some(exit_id),
			..Default::default()
		},
		path: "/ws".to_string(),
		host_header: cli.hostname.clone(),
		use_tls: cli.cert.is_some() || cli.key.is_some(),
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
	cli: &WallhackCli,
	endpoint: std::net::SocketAddr,
	exit_id: String,
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
		exit_id: Some(exit_id),
		..Default::default()
	}
}
