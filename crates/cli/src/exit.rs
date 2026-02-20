//! Exit node implementation.
//!
//! The exit node processes incoming instructions by making syscalls to the
//! local network. It can either connect to an upstream peer (default) or
//! listen for incoming connections (reverse tunnel).

use std::{
	str::FromStr,
	sync::{Arc, atomic::Ordering},
	time::{Duration, Instant},
};

use anyhow::{Context, Result};
use tokio::{io::AsyncWriteExt, sync::mpsc};

use wallhack::{
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

use crate::repl_common::{PeerRow, Printer, format_duration, print_peer_table, print_ping};

#[cfg(feature = "readline")]
use rustyline::ExternalPrinter;

/// REPL commands for exit nodes.
enum ExitReplCommand {
	Quit,
	Ping,
	Stats,
	Status,
	Peers,
	Connect(String),
	Listen(String),
	Disconnect,
	Help,
	Unknown(String),
}

/// Security-related connection parameters.
struct SecurityConfig {
	psk: Option<String>,
	accept_fingerprint: Option<String>,
	insecure: bool,
}

/// Action returned from exit loop functions to trigger mode transitions.
enum ExitAction {
	/// User requested quit.
	Quit,
	/// Start listening for peer connections (address spec string).
	StartListen(String),
	/// Start connecting to a peer (address spec string).
	StartConnect(String),
	/// Disconnect from peer.
	StopConnect,
}

/// Setup the REPL once (shared across mode transitions).
fn setup_exit_repl() -> (Option<mpsc::Receiver<ExitReplCommand>>, Option<Printer>) {
	if crate::repl_common::is_interactive() {
		let (tx, rx) = mpsc::channel::<ExitReplCommand>(16);
		let (print_tx, print_rx) = mpsc::unbounded_channel::<String>();
		let printer = Printer::new(print_tx);

		crate::info!("Type 'help' for commands, 'quit' to exit.");

		std::thread::spawn(move || {
			run_exit_repl_input(&tx, print_rx);
		});

		(Some(rx), Some(printer))
	} else {
		crate::info!("Running in headless mode (no REPL).");
		(None, None)
	}
}

/// Run as an exit node.
///
/// Creates a REPL once and runs a state loop that dispatches to mode-specific
/// functions based on the current connect/listen configuration. REPL commands
/// can trigger mode transitions (e.g. adding listen to enable relay capability).
///
/// # Errors
///
/// Returns error if orchestrator fails (connection errors are retried).
pub async fn run(global: &WallhackCli, cmd: &ExitCommand) -> Result<()> {
	crate::repl_common::mark_started();
	let transport = cmd.transport().map_err(|e| anyhow::anyhow!("{e}"))?;
	let name = cmd.name();
	let metrics = Arc::new(Metrics::default());
	let security = SecurityConfig {
		psk: global.resolve_psk(),
		accept_fingerprint: cmd.accept_fingerprint.clone(),
		insecure: cmd.insecure,
	};

	// Parse initial transport config
	let (mut connect_spec, mut listen_spec) = match transport {
		TransportDir::Both { connect, listen } => (Some(connect), Some(listen)),
		TransportDir::Connect(spec) => (Some(spec), None),
		TransportDir::Listen(spec) => (None, Some(spec)),
	};

	// Setup REPL once — shared across mode transitions
	let (mut repl_rx, printer) = setup_exit_repl();

	loop {
		let result = match (&connect_spec, &listen_spec) {
			(Some(c), Some(l)) => {
				crate::info!("Exit node with relay capability as {name}");
				run_relay_capability_mode(
					global,
					&name,
					c,
					l,
					&metrics,
					&mut repl_rx,
					printer.as_ref(),
				)
				.await
			}
			(Some(c), None) => {
				crate::info!("Exit node starting as {name}");
				run_connect_mode(
					global,
					&name,
					c,
					&metrics,
					&mut repl_rx,
					printer.as_ref(),
					&security,
				)
				.await
			}
			(None, Some(l)) => {
				crate::info!("Exit node listening as {name}");
				run_listen_mode(global, l, &metrics, &mut repl_rx, printer.as_ref()).await
			}
			(None, None) => run_idle_mode(&metrics, &mut repl_rx, printer.as_ref()).await,
		};

		let action = match result {
			Ok(action) => action,
			Err(e) => {
				if let Some(p) = printer.as_ref() {
					p.error(e.to_string());
				} else {
					crate::error!("{e}");
				}
				// Reset to idle so the user can try again
				connect_spec = None;
				listen_spec = None;
				continue;
			}
		};

		match action {
			ExitAction::Quit => break,
			ExitAction::StartListen(addr) => {
				listen_spec = Some(crate::cli::AddressSpec::parse(&addr));
			}
			ExitAction::StartConnect(addr) => {
				connect_spec = Some(crate::cli::AddressSpec::parse(&addr));
			}
			ExitAction::StopConnect => {
				connect_spec = None;
			}
		}
	}

	Ok(())
}

/// Run in connect-only mode (standard exit).
async fn run_connect_mode(
	global: &WallhackCli,
	name: &str,
	spec: &crate::cli::AddressSpec,
	metrics: &Arc<Metrics>,
	repl_rx: &mut Option<mpsc::Receiver<ExitReplCommand>>,
	printer: Option<&Printer>,
	security: &SecurityConfig,
) -> Result<ExitAction> {
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
				run_quic_exit(global, endpoint, name, metrics, repl_rx, printer, security).await
			}
			#[cfg(not(feature = "quic"))]
			{
				anyhow::bail!("QUIC support not compiled in (enable 'quic' feature)")
			}
		}
		Protocol::Tcp => {
			#[cfg(feature = "websocket")]
			{
				run_ws_exit(global, endpoint, name, metrics, repl_rx, printer, security).await
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
	_name: &str,
	connect_spec: &crate::cli::AddressSpec,
	listen_spec: &crate::cli::AddressSpec,
	metrics: &Arc<Metrics>,
	repl_rx: &mut Option<mpsc::Receiver<ExitReplCommand>>,
	printer: Option<&Printer>,
) -> Result<ExitAction> {
	crate::info!("Resolving peer to connect to: {}", connect_spec.addr);
	let resolvable = crate::dns::ResolvableAddress::from_str(&connect_spec.addr)?;
	let dns_server = global
		.dns
		.as_ref()
		.map(|s| crate::dns::parse_str_to_addr(s))
		.transpose()?;

	let peer_addr = crate::dns::resolve(resolvable, dns_server).await?;
	crate::info!("Connecting to peer: {peer_addr}");

	let listen_addr = parse_listen_addr(&listen_spec.addr)?;
	crate::info!("Listening for peer connections on: {listen_addr}");

	match connect_spec.protocol {
		Protocol::Udp => {
			#[cfg(feature = "quic")]
			{
				run_quic_relay_capability(global, peer_addr, listen_addr, metrics, repl_rx, printer)
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
				run_ws_relay_capability(global, peer_addr, listen_addr, metrics, repl_rx, printer)
					.await
			}
			#[cfg(not(feature = "websocket"))]
			{
				anyhow::bail!("WebSocket support not compiled in (enable 'websocket' feature)")
			}
		}
	}
}

/// Run in listen-only mode (reverse tunnel) with REPL.
async fn run_listen_mode(
	global: &WallhackCli,
	spec: &crate::cli::AddressSpec,
	metrics: &Arc<Metrics>,
	repl_rx: &mut Option<mpsc::Receiver<ExitReplCommand>>,
	printer: Option<&Printer>,
) -> Result<ExitAction> {
	use wallhack::{
		control::handler::HandlerConfig,
		server::server::{Server, ServerOptions},
	};

	let addr = parse_listen_addr(&spec.addr)?;

	let server_options = ServerOptions {
		handler_config: HandlerConfig::new(NodeRole::Exit),
		metrics: Some(Arc::clone(metrics)),
		peers: None,
		routes: None,
	};

	let server_config = build_server_config(global, addr);

	crate::info!("Exit node listening on {addr}");
	if let Some(p) = printer {
		p.print(format!("Listening on {addr}"));
	}

	match spec.protocol {
		Protocol::Udp => {
			#[cfg(feature = "quic")]
			{
				let server =
					wallhack::server::quic::QuicServer::try_new(server_config, server_options)?;
				run_listen_server_loop(server, metrics, repl_rx, printer, addr).await
			}
			#[cfg(not(feature = "quic"))]
			anyhow::bail!("QUIC transport not available (compile with --features quic)")
		}
		Protocol::Tcp => {
			#[cfg(feature = "websocket")]
			{
				let server =
					wallhack::server::ws::WsServer::try_new(server_config, server_options)?;
				run_listen_server_loop(server, metrics, repl_rx, printer, addr).await
			}
			#[cfg(not(feature = "websocket"))]
			anyhow::bail!("WebSocket transport not available (compile with --features websocket)")
		}
	}
}

/// Server accept loop with REPL integration for listen-only mode.
async fn run_listen_server_loop<S: Server>(
	mut server: S,
	metrics: &Arc<Metrics>,
	repl_rx: &mut Option<mpsc::Receiver<ExitReplCommand>>,
	printer: Option<&Printer>,
	listen_addr: std::net::SocketAddr,
) -> Result<ExitAction>
where
	S::Error: std::error::Error + Send + Sync + 'static,
	S::Transport: Send + Sync + 'static,
{
	loop {
		tokio::select! {
			result = server.accept(NodeRole::Exit) => {
				match result {
					Ok(Some(accept_result)) => {
						crate::info!("Accepted connection from {}", accept_result.peer_addr());
						if let Some(p) = printer {
							p.print(format!("Peer connected: {}", accept_result.peer_addr()));
						}
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
						crate::error!("Accept error: {e}");
					}
				}
			}

			cmd = async {
				match repl_rx {
					Some(rx) => rx.recv().await,
					None => std::future::pending().await,
				}
			} => {
				match cmd {
					Some(ExitReplCommand::Quit) | None => return Ok(ExitAction::Quit),
					Some(ExitReplCommand::Connect(addr)) => return Ok(ExitAction::StartConnect(addr)),
					Some(ExitReplCommand::Listen(_)) => {
						if let Some(p) = printer {
							p.print(format!("Already listening on {listen_addr}"));
						}
					}
					Some(ExitReplCommand::Disconnect) => {
						if let Some(p) = printer {
							p.print("Not connected to any peer.");
						}
					}
					Some(ExitReplCommand::Ping) => {
						if let Some(p) = printer {
							print_ping(p);
						}
					}
					Some(ExitReplCommand::Stats) => {
						if let Some(p) = printer {
							print_exit_stats(metrics, p);
						}
					}
					Some(ExitReplCommand::Status) => {
						if let Some(p) = printer {
							print_listen_status(p, listen_addr);
						}
					}
					Some(ExitReplCommand::Peers) => {
						if let Some(p) = printer {
							print_peer_table(p, &[]);
						}
					}
					Some(ExitReplCommand::Help) => {
						if let Some(p) = printer {
							print_listen_help(p);
						}
					}
					Some(ExitReplCommand::Unknown(cmd)) => {
						if let Some(p) = printer {
							p.print(format!("Unknown command: {cmd}. Type 'help' for available commands."));
						}
					}
				}
			}
		}
	}

	Ok(ExitAction::Quit)
}

/// Run in idle mode (no connection, no listener).
async fn run_idle_mode(
	metrics: &Arc<Metrics>,
	repl_rx: &mut Option<mpsc::Receiver<ExitReplCommand>>,
	printer: Option<&Printer>,
) -> Result<ExitAction> {
	if let Some(p) = printer {
		p.print("Node idle. Use 'connect <addr>' or 'listen <addr>' to start.");
	}

	loop {
		let cmd = match repl_rx {
			Some(rx) => rx.recv().await,
			None => {
				// Headless and idle - nothing to do
				std::future::pending::<Option<ExitReplCommand>>().await
			}
		};

		match cmd {
			Some(ExitReplCommand::Quit) | None => return Ok(ExitAction::Quit),
			Some(ExitReplCommand::Connect(addr)) => return Ok(ExitAction::StartConnect(addr)),
			Some(ExitReplCommand::Listen(addr)) => return Ok(ExitAction::StartListen(addr)),
			Some(ExitReplCommand::Disconnect) => {
				if let Some(p) = printer {
					p.print("Not connected.");
				}
			}
			Some(ExitReplCommand::Status) => {
				if let Some(p) = printer {
					p.print("Node Status: Idle (not connected, not listening)");
				}
			}
			Some(ExitReplCommand::Ping) => {
				if let Some(p) = printer {
					print_ping(p);
				}
			}
			Some(ExitReplCommand::Stats) => {
				if let Some(p) = printer {
					print_exit_stats(metrics, p);
				}
			}
			Some(ExitReplCommand::Peers) => {
				if let Some(p) = printer {
					print_peer_table(p, &[]);
				}
			}
			Some(ExitReplCommand::Help) => {
				if let Some(p) = printer {
					print_idle_help(p);
				}
			}
			Some(ExitReplCommand::Unknown(cmd)) => {
				if let Some(p) = printer {
					p.print(format!(
						"Unknown command: {cmd}. Type 'help' for available commands."
					));
				}
			}
		}
	}
}

/// Drive the exit node orchestrator with a connected client.
///
/// Returns `None` when the connection drops (caller should reconnect),
/// or `Some(action)` when the user requested a mode transition via REPL.
async fn run_exit_loop<T: wallhack::transport::Transport + 'static>(
	connect_result: ConnectResult<T>,
	metrics: &Arc<Metrics>,
	repl_rx: &mut Option<mpsc::Receiver<ExitReplCommand>>,
	printer: Option<&Printer>,
	peer_addr: &str,
) -> Result<Option<ExitAction>> {
	crate::info!("Connected to {}", connect_result.peer_addr());

	if let Some(p) = printer {
		p.print(format!("Connected to {peer_addr}"));
	}

	let connected_at = Instant::now();

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

	// Pin the long-running futures so we can select over them + REPL
	tokio::pin!(stream_fut);
	tokio::pin!(disconnect_fut);
	let drive_fut = orchestrator.drive(resp, instr.subscribe());
	tokio::pin!(drive_fut);

	loop {
		tokio::select! {
			result = &mut drive_fut => {
				let msg = match result {
					Ok(()) => { tracing::debug!("Connection closed cleanly"); "Connection closed, reconnecting...".into() }
					Err(e) => { tracing::debug!("Orchestrator error: {}", e); format!("Connection error: {e}, reconnecting...") }
				};
				if let Some(p) = printer { p.print(msg); } else { crate::info!("{msg}"); }
				return Ok(None);
			}
			result = &mut stream_fut => {
				if let Err(e) = result { crate::error!("Stream handler error: {e}"); }
				return Ok(None);
			}
			() = &mut disconnect_fut => {
				crate::info!("Connection tasks died - transport disconnected");
				let msg = "Transport disconnected, reconnecting...";
				if let Some(p) = printer { p.print(msg); } else { crate::info!("{msg}"); }
				return Ok(None);
			}
			cmd = async {
				match repl_rx {
					Some(rx) => rx.recv().await,
					None => std::future::pending().await,
				}
			} => {
				match cmd {
					Some(ExitReplCommand::Quit) | None => return Ok(Some(ExitAction::Quit)),
					Some(ExitReplCommand::Listen(addr)) => return Ok(Some(ExitAction::StartListen(addr))),
					Some(ExitReplCommand::Connect(_)) => {
						if let Some(p) = printer {
							p.print(format!("Already connected to {peer_addr}. Use 'disconnect' first."));
						}
					}
					Some(ExitReplCommand::Disconnect) => return Ok(Some(ExitAction::StopConnect)),
					Some(ExitReplCommand::Peers) => {
						if let Some(p) = printer {
							let row = exit_peer_row("entry", peer_addr, &format_duration(connected_at.elapsed()));
							print_peer_table(p, &[row]);
						}
					}
					Some(ExitReplCommand::Ping) => {
						if let Some(p) = printer {
							print_ping(p);
						}
					}
					Some(ExitReplCommand::Stats) => {
						if let Some(p) = printer {
							print_exit_stats(metrics, p);
						}
					}
					Some(ExitReplCommand::Status) => {
						if let Some(p) = printer {
							print_exit_status(p, true, peer_addr);
						}
					}
					Some(ExitReplCommand::Help) => {
						if let Some(p) = printer {
							print_connect_help(p);
						}
					}
					Some(ExitReplCommand::Unknown(cmd)) => {
						if let Some(p) = printer {
							p.print(format!("Unknown command: {cmd}. Type 'help' for available commands."));
						}
					}
				}
			}
		}
	}
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
			match tokio::net::TcpStream::connect(target).await {
				Ok(mut socket) => {
					let status = protobuf::v2::SessionStatus {
						status: protobuf::v2::ResponseStatus::Success.into(),
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
							protobuf::v2::ResponseStatus::ConnectionRefused
						}
						_ => protobuf::v2::ResponseStatus::HostUnreachable,
					};
					let status = protobuf::v2::SessionStatus {
						status: status_code.into(),
						reason: e.to_string(),
					};
					let _ = write_length_delimited(&mut *stream, &status).await;
					return Err(anyhow::anyhow!("connect to {target} failed: {e}"));
				}
			}
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
			// Connect so the kernel delivers ICMP errors to this socket
			socket.connect(target).await?;
			let mut buf = Vec::new();
			tokio::io::AsyncReadExt::read_to_end(stream, &mut buf).await?;
			tracing::trace!(buf_len = buf.len(), "Read UDP payload from stream");
			if !buf.is_empty() {
				tracing::trace!(target = %target, "Sending UDP to target");
				socket.send(&buf).await?;
				let mut recv_buf = vec![0u8; 65535];
				tracing::trace!("Waiting for UDP response...");
				// Use timeout to avoid hanging streams that accumulate
				// Status prefix: 0x00=success, 0x01=port unreachable,
				// 0x02=host unreachable, 0x03=network unreachable
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
						// No bytes = timeout (empty stream)
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

/// Handle a REPL command during the connecting/retrying phase.
///
/// Returns `Some(action)` if the command triggers a mode transition, `None` to continue.
fn handle_connecting_repl_cmd(
	cmd: Option<ExitReplCommand>,
	printer: Option<&Printer>,
	metrics: &Arc<Metrics>,
	peer_addr: &str,
) -> Option<ExitAction> {
	match cmd {
		Some(ExitReplCommand::Quit) | None => Some(ExitAction::Quit),
		Some(ExitReplCommand::Listen(addr)) => Some(ExitAction::StartListen(addr)),
		Some(ExitReplCommand::Connect(_)) => {
			if let Some(p) = printer {
				p.print(format!(
					"Already connecting to {peer_addr}. Use 'disconnect' first."
				));
			}
			None
		}
		Some(ExitReplCommand::Disconnect) => Some(ExitAction::StopConnect),
		Some(ExitReplCommand::Peers) => {
			if let Some(p) = printer {
				let row = exit_peer_row("entry", peer_addr, "connecting...");
				print_peer_table(p, &[row]);
			}
			None
		}
		Some(ExitReplCommand::Ping) => {
			if let Some(p) = printer {
				print_ping(p);
			}
			None
		}
		Some(ExitReplCommand::Stats) => {
			if let Some(p) = printer {
				print_exit_stats(metrics, p);
			}
			None
		}
		Some(ExitReplCommand::Status) => {
			if let Some(p) = printer {
				print_exit_status(p, false, peer_addr);
			}
			None
		}
		Some(ExitReplCommand::Help) => {
			if let Some(p) = printer {
				print_connect_help(p);
			}
			None
		}
		Some(ExitReplCommand::Unknown(cmd)) => {
			if let Some(p) = printer {
				p.print(format!(
					"Unknown command: {cmd}. Type 'help' for available commands."
				));
			}
			None
		}
	}
}

#[cfg(feature = "quic")]
async fn run_quic_exit(
	global: &WallhackCli,
	endpoint: std::net::SocketAddr,
	name: &str,
	metrics: &Arc<Metrics>,
	repl_rx: &mut Option<mpsc::Receiver<ExitReplCommand>>,
	printer: Option<&Printer>,
	security: &SecurityConfig,
) -> Result<ExitAction> {
	let client_config = build_quic_client_config(
		global,
		endpoint,
		name.to_string(),
		security.psk.clone(),
		security.accept_fingerprint.clone(),
	);

	if !security.insecure && security.psk.is_none() && security.accept_fingerprint.is_none() {
		crate::warn!(
			"Connecting without authentication or certificate verification. Use --psk <SECRET> or --accept-fingerprint <HASH> for security."
		);
	}

	let mut retry_delay = INITIAL_RETRY_DELAY;

	let peer_addr = endpoint.to_string();

	loop {
		tokio::select! {
			// Handle connection attempts
			result = async {
				let mut client = client::quic::QuicClient::try_new(client_config.clone())?;
				client.connect(NodeRole::Exit).await
			} => {
				match result {
					Ok(connect_result) => {
						if let Some(action) = run_exit_loop(connect_result, metrics, repl_rx, printer, &peer_addr).await? {
							return Ok(action);
						}
						// Connection dropped - backoff before reconnecting to
						// prevent a reconnect storm (e.g. entry TUN EBUSY race).
						// NOTE: Keep this in sync with the WebSocket equivalent
						// in run_ws_exit below.
						let msg = format!("Connection dropped, reconnecting in {retry_delay:?}...");
						if let Some(p) = printer {
							p.print(msg);
						} else {
							crate::info!("{msg}");
						}
						tokio::time::sleep(retry_delay).await;
						retry_delay = (retry_delay * 2).min(MAX_RETRY_DELAY);
					}
					Err(e) => {
						if crate::repl_common::is_nonretryable_error(&e) {
							let msg = format!("Connection failed (not retrying): {e}");
							if let Some(p) = printer {
								p.print(msg);
							} else {
								crate::warn!("{msg}");
							}
							return Ok(ExitAction::StopConnect);
						}
						tracing::debug!("Connection failed: {}, retrying in {:?}", e, retry_delay);
						if let Some(p) = printer {
							p.print(format!("Connection failed: {e}, retrying in {retry_delay:?}..."));
						} else {
							crate::info!("Connection failed: {e}, retrying in {retry_delay:?}...");
						}
						tokio::time::sleep(retry_delay).await;
						retry_delay = (retry_delay * 2).min(MAX_RETRY_DELAY);
					}
				}
			}

			// Handle REPL commands (only fires while connecting/retrying)
			cmd = async {
				match repl_rx {
					Some(rx) => rx.recv().await,
					None => std::future::pending().await,
				}
			} => {
				if let Some(action) = handle_connecting_repl_cmd(cmd, printer, metrics, &peer_addr) {
					return Ok(action);
				}
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
	repl_rx: &mut Option<mpsc::Receiver<ExitReplCommand>>,
	printer: Option<&Printer>,
	security: &SecurityConfig,
) -> Result<ExitAction> {
	use wallhack::client::{
		config::ClientConfig,
		ws::{WsClient, WsClientConfig},
	};

	if !security.insecure && security.psk.is_none() && security.accept_fingerprint.is_none() {
		crate::warn!(
			"Connecting without authentication or certificate verification. Use --psk <SECRET> or --accept-fingerprint <HASH> for security."
		);
	}

	let client_config = WsClientConfig {
		base: ClientConfig {
			addr: endpoint,
			hostname: global.hostname.clone(),
			mtls: None,
			name: Some(name.to_string()),
			psk: security.psk.clone(),
			accept_fingerprint: security.accept_fingerprint.clone(),
			..Default::default()
		},
		path: "/ws".to_string(),
		host_header: global.hostname.clone(),
		use_tls: true,
	};
	let mut retry_delay = INITIAL_RETRY_DELAY;

	let peer_addr = endpoint.to_string();

	loop {
		tokio::select! {
			// Handle connection attempts
			result = async {
				let mut client = WsClient::new(client_config.clone())?;
				client.connect(NodeRole::Exit).await
			} => {
				match result {
					Ok(connect_result) => {
						if let Some(action) = run_exit_loop(connect_result, metrics, repl_rx, printer, &peer_addr).await? {
							return Ok(action);
						}
						// Connection dropped - backoff before reconnecting to
						// prevent a reconnect storm (e.g. entry TUN EBUSY race).
						// NOTE: Keep this in sync with the QUIC equivalent
						// in run_quic_exit above.
						let msg = format!("Connection dropped, reconnecting in {retry_delay:?}...");
						if let Some(p) = printer {
							p.print(msg);
						} else {
							crate::info!("{msg}");
						}
						tokio::time::sleep(retry_delay).await;
						retry_delay = (retry_delay * 2).min(MAX_RETRY_DELAY);
					}
					Err(e) => {
						if crate::repl_common::is_nonretryable_error(&e) {
							let msg = format!("Connection failed (not retrying): {e}");
							if let Some(p) = printer {
								p.print(msg);
							} else {
								crate::warn!("{msg}");
							}
							return Ok(ExitAction::StopConnect);
						}
						tracing::debug!("Connection failed: {}, retrying in {:?}", e, retry_delay);
						if let Some(p) = printer {
							p.print(format!("Connection failed: {e}, retrying in {retry_delay:?}..."));
						} else {
							crate::info!("Connection failed: {e}, retrying in {retry_delay:?}...");
						}
						tokio::time::sleep(retry_delay).await;
						retry_delay = (retry_delay * 2).min(MAX_RETRY_DELAY);
					}
				}
			}

			// Handle REPL commands (only fires while connecting/retrying)
			cmd = async {
				match repl_rx {
					Some(rx) => rx.recv().await,
					None => std::future::pending().await,
				}
			} => {
				if let Some(action) = handle_connecting_repl_cmd(cmd, printer, metrics, &peer_addr) {
					return Ok(action);
				}
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
		..Default::default()
	}
}

#[cfg(feature = "quic")]
#[allow(clippy::too_many_lines)]
async fn run_quic_relay_capability(
	global: &WallhackCli,
	peer_addr: std::net::SocketAddr,
	listen_addr: std::net::SocketAddr,
	metrics: &Arc<Metrics>,
	repl_rx: &mut Option<mpsc::Receiver<ExitReplCommand>>,
	printer: Option<&Printer>,
) -> Result<ExitAction> {
	use wallhack::{
		control::handler::HandlerConfig,
		server::server::{Server, ServerOptions},
	};

	// Connect to peer
	let psk = global.resolve_psk();
	let client_config = build_quic_client_config(global, peer_addr, String::new(), psk, None);
	let mut client = client::quic::QuicClient::try_new(client_config)?;
	let connect_result = client.connect(NodeRole::Exit).await?;

	crate::info!("Connected to peer {peer_addr}");

	let (relay_instr, relay_resp) = connect_result.channels().clone();

	// Start listening for peer connections
	let server_options = ServerOptions {
		handler_config: HandlerConfig::new(NodeRole::Exit),
		metrics: Some(Arc::clone(metrics)),
		peers: None,
		routes: None,
	};

	let server_config = build_server_config(global, listen_addr);
	let mut server = wallhack::server::quic::QuicServer::try_new(server_config, server_options)?;

	crate::info!(
		"Relay capability active: connected to {peer_addr}, listening on :{}",
		listen_addr.port()
	);
	if let Some(p) = printer {
		p.print(format!(
			"Relay capability active (connected to {peer_addr}, listening on :{})",
			listen_addr.port()
		));
	}

	let peer_addr_str = peer_addr.to_string();
	let listen_port = listen_addr.port();
	let connected_at = Instant::now();

	// Accept and bridge peer connections
	loop {
		tokio::select! {
			// Handle peer connections
			result = server.accept(NodeRole::Exit) => {
				match result {
					Ok(Some(accept_result)) => {
						crate::info!("Peer connected: {}", accept_result.peer_addr());
						if let Some(p) = printer {
							p.print(format!("Peer connected: {}", accept_result.peer_addr()));
						}
						bridge_peer(accept_result, &relay_instr, &relay_resp);
					}
					Ok(None) => {
						crate::info!("Server closed");
						break;
					}
					Err(e) => {
						crate::error!("Accept error: {e}");
					}
				}
			}

			// Handle REPL commands
			cmd = async {
				match repl_rx {
					Some(rx) => rx.recv().await,
					None => std::future::pending().await,
				}
			} => {
				match cmd {
					Some(ExitReplCommand::Quit) | None => return Ok(ExitAction::Quit),
					Some(ExitReplCommand::Disconnect) => return Ok(ExitAction::StopConnect),
					Some(ExitReplCommand::Connect(_)) => {
						if let Some(p) = printer {
							p.print(format!("Already connected to {peer_addr_str}. Use 'disconnect' first."));
						}
					}
					Some(ExitReplCommand::Listen(_)) => {
						if let Some(p) = printer {
							p.print(format!("Already listening on :{listen_port}."));
						}
					}
					Some(ExitReplCommand::Peers) => {
						if let Some(p) = printer {
							let row = exit_peer_row("entry", &peer_addr_str, &format_duration(connected_at.elapsed()));
							print_peer_table(p, &[row]);
						}
					}
					Some(ExitReplCommand::Ping) => {
						if let Some(p) = printer {
							print_ping(p);
						}
					}
					Some(ExitReplCommand::Stats) => {
						if let Some(p) = printer {
							print_exit_stats(metrics, p);
						}
					}
					Some(ExitReplCommand::Status) => {
						if let Some(p) = printer {
							print_relay_status(p, &peer_addr_str, listen_port);
						}
					}
					Some(ExitReplCommand::Help) => {
						if let Some(p) = printer {
							print_relay_help(p);
						}
					}
					Some(ExitReplCommand::Unknown(cmd)) => {
						if let Some(p) = printer {
							p.print(format!("Unknown command: {cmd}. Type 'help' for available commands."));
						}
					}
				}
			}
		}
	}

	Ok(ExitAction::Quit)
}

#[cfg(feature = "websocket")]
#[allow(clippy::too_many_lines)]
async fn run_ws_relay_capability(
	global: &WallhackCli,
	peer_addr: std::net::SocketAddr,
	listen_addr: std::net::SocketAddr,
	metrics: &Arc<Metrics>,
	repl_rx: &mut Option<mpsc::Receiver<ExitReplCommand>>,
	printer: Option<&Printer>,
) -> Result<ExitAction> {
	use wallhack::{
		client::{
			config::ClientConfig,
			ws::{WsClient, WsClientConfig},
		},
		control::handler::HandlerConfig,
		server::server::{Server, ServerOptions},
	};

	// Connect to peer
	let psk = global.resolve_psk();
	let client_config = WsClientConfig {
		base: ClientConfig {
			addr: peer_addr,
			hostname: global.hostname.clone(),
			mtls: None,
			name: None,
			psk,
			..Default::default()
		},
		path: "/ws".to_string(),
		host_header: global.hostname.clone(),
		use_tls: true,
	};

	let mut client = WsClient::new(client_config)?;
	let connect_result = client.connect(NodeRole::Exit).await?;

	crate::info!("Connected to peer {peer_addr}");

	let (relay_instr, relay_resp) = connect_result.channels().clone();

	// Start listening for peer connections
	let server_options = ServerOptions {
		handler_config: HandlerConfig::new(NodeRole::Exit),
		metrics: Some(Arc::clone(metrics)),
		peers: None,
		routes: None,
	};

	let server_config = build_server_config(global, listen_addr);
	let mut server = wallhack::server::ws::WsServer::try_new(server_config, server_options)?;

	crate::info!(
		"Relay capability active: connected to {peer_addr}, listening on :{}",
		listen_addr.port()
	);
	if let Some(p) = printer {
		p.print(format!(
			"Relay capability active (connected to {peer_addr}, listening on :{})",
			listen_addr.port()
		));
	}

	let peer_addr_str = peer_addr.to_string();
	let listen_port = listen_addr.port();
	let connected_at = Instant::now();

	// Accept and bridge peer connections
	loop {
		tokio::select! {
			// Handle peer connections
			result = server.accept(NodeRole::Exit) => {
				match result {
					Ok(Some(accept_result)) => {
						crate::info!("Peer connected: {}", accept_result.peer_addr());
						if let Some(p) = printer {
							p.print(format!("Peer connected: {}", accept_result.peer_addr()));
						}
						bridge_peer(accept_result, &relay_instr, &relay_resp);
					}
					Ok(None) => {
						crate::info!("Server closed");
						break;
					}
					Err(e) => {
						crate::error!("Accept error: {e}");
					}
				}
			}

			// Handle REPL commands
			cmd = async {
				match repl_rx {
					Some(rx) => rx.recv().await,
					None => std::future::pending().await,
				}
			} => {
				match cmd {
					Some(ExitReplCommand::Quit) | None => return Ok(ExitAction::Quit),
					Some(ExitReplCommand::Disconnect) => return Ok(ExitAction::StopConnect),
					Some(ExitReplCommand::Connect(_)) => {
						if let Some(p) = printer {
							p.print(format!("Already connected to {peer_addr_str}. Use 'disconnect' first."));
						}
					}
					Some(ExitReplCommand::Listen(_)) => {
						if let Some(p) = printer {
							p.print(format!("Already listening on :{listen_port}."));
						}
					}
					Some(ExitReplCommand::Peers) => {
						if let Some(p) = printer {
							let row = exit_peer_row("entry", &peer_addr_str, &format_duration(connected_at.elapsed()));
							print_peer_table(p, &[row]);
						}
					}
					Some(ExitReplCommand::Ping) => {
						if let Some(p) = printer {
							print_ping(p);
						}
					}
					Some(ExitReplCommand::Stats) => {
						if let Some(p) = printer {
							print_exit_stats(metrics, p);
						}
					}
					Some(ExitReplCommand::Status) => {
						if let Some(p) = printer {
							print_relay_status(p, &peer_addr_str, listen_port);
						}
					}
					Some(ExitReplCommand::Help) => {
						if let Some(p) = printer {
							print_relay_help(p);
						}
					}
					Some(ExitReplCommand::Unknown(cmd)) => {
						if let Some(p) = printer {
							p.print(format!("Unknown command: {cmd}. Type 'help' for available commands."));
						}
					}
				}
			}
		}
	}

	Ok(ExitAction::Quit)
}

/// Bridge a peer connection to relay broadcast channels.
fn bridge_peer<T: wallhack::transport::Transport>(
	accept_result: wallhack::server::server::AcceptResult<T>,
	relay_instr: &tokio::sync::broadcast::Sender<protobuf::v2::EntryNodeInstruction>,
	relay_resp: &tokio::sync::broadcast::Sender<protobuf::v2::ExitNodeResponse>,
) {
	crate::info!("Bridging peer connection: {}", accept_result.peer_addr());

	let ((peer_instr, peer_resp), control_tx) = accept_result.channels();

	// Bridge this peer to relay broadcast channels
	let relay_instr_clone = relay_instr.clone();
	let mut relay_resp_rx = relay_resp.subscribe();
	let mut peer_instr_rx = peer_instr.subscribe();

	// Forward peer instructions to relay (also holds control_tx to keep control stream alive)
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

	wallhack::server::config::ServerConfig {
		listen: addr,
		tls,
		psk: global.resolve_psk(),
		max_peers: None,
	}
}

/// Parse a line into an exit REPL command.
fn parse_exit_repl_command(line: &str) -> ExitReplCommand {
	let mut parts = line.split_whitespace();
	let cmd = parts.next().unwrap_or("").to_lowercase();

	match cmd.as_str() {
		"quit" | "exit" | "q" => ExitReplCommand::Quit,
		"ping" => ExitReplCommand::Ping,
		"stats" | "s" => ExitReplCommand::Stats,
		"status" => ExitReplCommand::Status,
		"peers" | "p" => ExitReplCommand::Peers,
		"connect" | "c" => match parts.next() {
			Some(addr) => ExitReplCommand::Connect(addr.to_string()),
			None => ExitReplCommand::Unknown(
				"connect requires an address (e.g. connect host:6565)".to_string(),
			),
		},
		"listen" | "l" => {
			let default_listen = format!(":{}", wallhack::server::config::DEFAULT_LISTEN_PORT);
			let addr = parts.next().map_or(default_listen, str::to_string);
			ExitReplCommand::Listen(addr)
		}
		"disconnect" => ExitReplCommand::Disconnect,
		"help" | "?" => ExitReplCommand::Help,
		_ => ExitReplCommand::Unknown(line.to_string()),
	}
}

/// Build a peer row for a connected exit node peer.
fn exit_peer_row(role: &str, addr: &str, uptime: &str) -> PeerRow {
	PeerRow {
		name: "-".to_string(),
		role: role.to_string(),
		addr: addr.to_string(),
		latency: "N/A".to_string(),
		uptime: uptime.to_string(),
		device: None,
	}
}

fn print_exit_stats(metrics: &wallhack::control::metrics::Metrics, printer: &Printer) {
	printer.print("Traffic Statistics:");
	printer.print(format!(
		"  Bytes In:     {}",
		crate::repl_common::format_bytes(metrics.bytes_in.load(Ordering::Relaxed))
	));
	printer.print(format!(
		"  Bytes Out:    {}",
		crate::repl_common::format_bytes(metrics.bytes_out.load(Ordering::Relaxed))
	));
	printer.print(format!(
		"  Packets In:   {}",
		metrics.packets_in.load(Ordering::Relaxed)
	));
	printer.print(format!(
		"  Packets Out:  {}",
		metrics.packets_out.load(Ordering::Relaxed)
	));
	printer.print(format!(
		"  Connections:  {}",
		metrics.active_connections.load(Ordering::Relaxed)
	));
	printer.print(format!(
		"  Flows:        {}",
		metrics.active_flows.load(Ordering::Relaxed)
	));
}

fn print_exit_status(printer: &Printer, connected: bool, peer_addr: &str) {
	printer.print("Exit Node Status:");
	printer.print(format!(
		"  Connected:    {}",
		if connected { "Yes" } else { "No" }
	));
	if connected {
		printer.print(format!("  Peer:         {peer_addr}"));
	}
	printer.print("  Relay:        No (standard exit)");
}

fn print_relay_status(printer: &Printer, peer_addr: &str, listen_port: u16) {
	printer.print("Exit Node Status:");
	printer.print("  Connected:    Yes");
	printer.print(format!("  Peer:         {peer_addr}"));
	printer.print(format!("  Listening:    :{listen_port}"));
	printer.print("  Relay:        Yes");
}

fn print_listen_status(printer: &Printer, listen_addr: std::net::SocketAddr) {
	printer.print("Exit Node Status:");
	printer.print("  Connected:    No");
	printer.print(format!("  Listening:    {listen_addr}"));
	printer.print("  Relay:        No (use 'connect <addr>' to enable)");
}

/// Print the help lines common to all exit node modes.
fn print_common_help(printer: &Printer) {
	printer.print("  ping          - Show version and uptime");
	printer.print("  peers, p      - Show peer info");
	printer.print("  stats, s      - Show traffic statistics");
	printer.print("  status        - Show node status");
	printer.print("  help, ?       - Show this help message");
	printer.print("  quit, q       - Exit wallhack");
}

fn print_connect_help(printer: &Printer) {
	printer.print("Available commands:");
	printer.print("  listen [addr] - Start listening for peers (enables relay capability)");
	printer.print("  disconnect    - Disconnect from peer");
	print_common_help(printer);
}

fn print_relay_help(printer: &Printer) {
	printer.print("Available commands:");
	printer.print("  disconnect    - Disconnect from peer (disables relay capability)");
	print_common_help(printer);
}

fn print_listen_help(printer: &Printer) {
	printer.print("Available commands:");
	printer.print("  connect <addr> - Connect to a peer (enables relay capability)");
	print_common_help(printer);
}

fn print_idle_help(printer: &Printer) {
	printer.print("Available commands:");
	printer.print("  connect <addr> - Connect to a peer");
	printer.print("  listen [addr]  - Start listening for peers");
	print_common_help(printer);
}

/// Run the REPL input loop in a blocking thread (with rustyline).
#[cfg(feature = "readline")]
fn run_exit_repl_input(
	tx: &mpsc::Sender<ExitReplCommand>,
	mut print_rx: mpsc::UnboundedReceiver<String>,
) {
	let mut rl = match rustyline::DefaultEditor::new() {
		Ok(rl) => rl,
		Err(e) => {
			crate::error!("Failed to initialize readline: {e}");
			let _ = tx.blocking_send(ExitReplCommand::Quit);
			return;
		}
	};

	let mut printer = rl.create_external_printer().ok();

	std::thread::spawn(move || {
		while let Some(msg) = print_rx.blocking_recv() {
			if let Some(ref mut p) = printer {
				let _ = p.print(msg);
			} else {
				println!("{msg}");
			}
		}
	});

	loop {
		match rl.readline("wallhack> ") {
			Ok(line) => {
				let line = line.trim();
				if line.is_empty() {
					continue;
				}

				let _ = rl.add_history_entry(line);

				let cmd = parse_exit_repl_command(line);
				let is_quit = matches!(cmd, ExitReplCommand::Quit);
				if tx.blocking_send(cmd).is_err() || is_quit {
					break;
				}
			}
			Err(rustyline::error::ReadlineError::Interrupted) => {
				// Continue on Ctrl-C
			}
			Err(rustyline::error::ReadlineError::Eof | _) => {
				let _ = tx.blocking_send(ExitReplCommand::Quit);
				break;
			}
		}
	}
}

/// Run the REPL input loop in a blocking thread (simple stdin, no readline).
#[cfg(not(feature = "readline"))]
fn run_exit_repl_input(
	tx: &mpsc::Sender<ExitReplCommand>,
	mut print_rx: mpsc::UnboundedReceiver<String>,
) {
	use std::io::{BufRead, Write};

	std::thread::spawn(move || {
		while let Some(msg) = print_rx.blocking_recv() {
			println!("{msg}");
		}
	});

	let stdin = std::io::stdin();
	let mut stdout = std::io::stdout();

	loop {
		print!("wallhack> ");
		let _ = stdout.flush();

		let mut line = String::new();
		match stdin.lock().read_line(&mut line) {
			Ok(0) | Err(_) => {
				let _ = tx.blocking_send(ExitReplCommand::Quit);
				break;
			}
			Ok(_) => {
				let line = line.trim();
				if line.is_empty() {
					continue;
				}

				let cmd = parse_exit_repl_command(line);
				let is_quit = matches!(cmd, ExitReplCommand::Quit);
				if tx.blocking_send(cmd).is_err() || is_quit {
					break;
				}
			}
		}
	}
}
