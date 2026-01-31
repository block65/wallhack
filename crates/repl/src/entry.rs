//! Entry node implementation.
//!
//! The entry node creates a TUN interface and listens for incoming connections
//! from relay or exit nodes. It captures packets from the TUN and sends them
//! through the tunnel. Includes an interactive REPL for control.

use std::{
	collections::HashMap,
	io::IsTerminal,
	sync::{Arc, atomic::Ordering},
};

use anyhow::{Context, Result};
use parking_lot::Mutex;
use tokio::sync::mpsc;

use wallhack::{
	NodeRole,
	control::{handler::HandlerConfig, metrics::Metrics},
	entry::{actor::TunActor, manager::ConnectionManager},
	server::{
		config::ServerConfig,
		server::{Server, ServerOptions},
	},
};

use crate::{WallhackCli, cli::Protocol};

/// Manages TUN sessions for connected exit nodes.
///
/// Keeps TUN adapters alive between reconnections so exit nodes can reconnect
/// without losing their TUN interface.
#[derive(Clone, Default)]
struct SessionManager {
	sessions: Arc<Mutex<HashMap<String, String>>>,
}

impl SessionManager {
	/// Gets or creates a TUN adapter for the given exit node.
	///
	/// If the exit node has connected before, returns a clone of their existing
	/// TUN. Otherwise creates a new TUN with stable naming (`tun-{exit_id}`).
	fn get_or_create(&self, exit_id: &str) -> std::string::String {
		let mut sessions = self.sessions.lock();

		if let Some(name) = sessions.get(exit_id) {
			tracing::info!("Reusing existing TUN for exit node {}", exit_id);
			return name.clone();
		}

		// Create new TUN with stable name
		let tun_name = format!("tun-{exit_id}");
		tracing::info!("Creating new TUN {} for exit node {}", tun_name, exit_id);
		sessions.insert(exit_id.to_string(), tun_name.clone());
		tun_name
	}

	/// Gets a TUN adapter with auto-generated name (for exit nodes without
	/// identity).
	fn create_anonymous() -> std::string::String {
		TunActor::random_iface_name()
	}

	/// Returns a list of (`exit_id`, `tun_name`) pairs for all active sessions.
	fn list(&self) -> Vec<(String, String)> {
		self.sessions
			.lock()
			.iter()
			.map(|(id, name)| (id.clone(), name.clone()))
			.collect()
	}
}

#[cfg(feature = "readline")]
use rustyline::ExternalPrinter;

/// Wrapper for printing to terminal without disrupting readline.
#[derive(Clone)]
struct Printer {
	tx: mpsc::UnboundedSender<String>,
}

impl Printer {
	fn print(&self, msg: impl Into<String>) {
		let _ = self.tx.send(msg.into());
	}
}

/// Run as an entry node with interactive REPL.
///
/// Creates TUN interface and listens for downstream connections. Runs an
/// interactive REPL for control commands.
///
/// # Errors
///
/// Returns error if server or orchestrator fails.
pub async fn run(cli: WallhackCli) -> Result<()> {
	// Parse listen address with protocol
	let listen_spec = cli.listen_spec();
	let addr = parse_listen_addr(&listen_spec.addr)?;

	// Session manager keeps TUNs alive across reconnections
	let sessions = SessionManager::default();

	// Shared metrics across all connections and control
	let metrics = Arc::new(Metrics::default());

	// Server options with control handler config
	let server_options = ServerOptions {
		handler_config: HandlerConfig::new(NodeRole::Entry),
		metrics: Some(Arc::clone(&metrics)),
	};

	// Build server config
	let server_config = build_server_config(&cli, addr);

	// Run with appropriate transport based on protocol
	match listen_spec.protocol {
		Protocol::Udp => {
			#[cfg(feature = "quic")]
			{
				let server =
					wallhack::server::quic::QuicServer::try_new(server_config, server_options)?;
				crate::info!("Listening on {addr} (QUIC/UDP)");
				run_entry_server(cli, server, metrics, sessions).await
			}
			#[cfg(not(feature = "quic"))]
			{
				anyhow::bail!("QUIC transport not available (compile with --features quic)");
			}
		}
		Protocol::Tcp => {
			#[cfg(feature = "websocket")]
			{
				let server =
					wallhack::server::ws::WsServer::try_new(server_config, server_options)?;
				crate::info!("Listening on {addr} (WebSocket/TCP)");
				run_entry_server(cli, server, metrics, sessions).await
			}
			#[cfg(not(feature = "websocket"))]
			{
				anyhow::bail!(
					"WebSocket transport not available (compile with --features websocket)"
				);
			}
		}
	}
}

/// Generic entry server loop that works with any Server implementation.
async fn run_entry_server<S: Server>(
	_cli: WallhackCli,
	mut server: S,
	metrics: Arc<Metrics>,
	sessions: SessionManager,
) -> Result<()>
where
	S::Error: std::error::Error + Send + Sync + 'static,
	S::Transport: Send + Sync + 'static,
{
	// Channel for REPL commands (input thread -> async loop)
	let (repl_tx, repl_rx) = mpsc::channel::<ReplCommand>(16);

	// Channel for async prints (async loop -> input thread)
	let (print_tx, print_rx) = mpsc::unbounded_channel::<String>();
	let printer = Printer { tx: print_tx };

	// Only spawn REPL if stdin is a terminal (skip in headless/Docker mode)
	let interactive = std::io::stdin().is_terminal();
	let mut repl_rx = if interactive {
		println!("Type 'help' for commands, 'quit' to exit.\n");
		let repl_metrics = Arc::clone(&metrics);
		std::thread::spawn(move || {
			run_repl_input(&repl_tx, repl_metrics, print_rx);
		});
		Some(repl_rx)
	} else {
		println!("Running in headless mode (no REPL).\n");
		// Drop the sender so REPL doesn't block
		drop(repl_tx);
		drop(print_rx);
		None
	};

	// Main loop: handle both server accepts and REPL commands
	loop {
		tokio::select! {
			// Handle incoming connections
			accept_result = server.accept(NodeRole::Entry) => {
				match accept_result {
					Ok(Some(mut accept_result)) => {
						let conn_metrics = accept_result.metrics();
						let conn_printer = printer.clone();
						let conn_sessions = sessions.clone();
						let hello_rx = accept_result.take_hello_rx();

						crate::info!("Accepted connection from {}", accept_result.client_ident());
						printer.print(format!("Connection from {}", accept_result.client_ident()));

						// Spawn handler for this connection (each exit node gets its own
						// TUN)
						tokio::spawn(async move {
							match handle_connection(conn_metrics, accept_result, hello_rx, conn_sessions).await {
								Ok(tun_name) => {
									conn_printer.print(format!("Connection closed (tun: {tun_name})"));
								}
								Err(e) => {
									crate::error!("Connection error: {}", e);
									conn_printer.print(format!("Connection error: {e}"));
								}
							}
						});
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

			// Handle REPL commands (only if interactive)
			cmd = async {
				match &mut repl_rx {
					Some(rx) => rx.recv().await,
					None => std::future::pending().await,
				}
			} => {
				match cmd {
					Some(ReplCommand::Quit) | None => {
						printer.print("Shutting down...");
						break;
					}
					Some(ReplCommand::Stats) => {
						print_stats(&metrics, &printer);
					}
					Some(ReplCommand::Sessions) => {
						print_sessions(&sessions, &printer);
					}
					Some(ReplCommand::Help) => {
						print_help(&printer);
					}
					Some(ReplCommand::Unknown(cmd)) => {
						printer.print(format!("Unknown command: {cmd}. Type 'help' for available commands."));
					}
				}
			}
		}
	}

	Ok(())
}

/// REPL commands that can be sent from the input thread.
enum ReplCommand {
	Quit,
	Stats,
	Sessions,
	Help,
	Unknown(String),
}

/// Run the REPL input loop in a blocking thread (with rustyline).
#[cfg(feature = "readline")]
fn run_repl_input(
	tx: &mpsc::Sender<ReplCommand>,
	_metrics: Arc<Metrics>,
	mut print_rx: mpsc::UnboundedReceiver<String>,
) {
	let mut rl = match rustyline::DefaultEditor::new() {
		Ok(rl) => rl,
		Err(e) => {
			eprintln!("Failed to initialize readline: {e}");
			let _ = tx.blocking_send(ReplCommand::Quit);
			return;
		}
	};

	// Create external printer for async output, falling back to println if
	// unavailable
	let mut printer = rl.create_external_printer().ok();

	// Spawn thread to handle print requests
	std::thread::spawn(move || {
		while let Some(msg) = print_rx.blocking_recv() {
			if let Some(ref mut p) = printer {
				let _ = p.print(msg);
			} else {
				// Fallback if external printer couldn't be created (e.g. non-TTY env)
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

				let cmd = parse_repl_command(line);
				let is_quit = matches!(cmd, ReplCommand::Quit);
				if tx.blocking_send(cmd).is_err() || is_quit {
					break;
				}
			}
			Err(rustyline::error::ReadlineError::Interrupted) => {
				// continue;
			}
			Err(rustyline::error::ReadlineError::Eof | _) => {
				let _ = tx.blocking_send(ReplCommand::Quit);
				break;
			}
		}
	}
}

/// Run the REPL input loop in a blocking thread (simple stdin, no readline).
#[cfg(not(feature = "readline"))]
fn run_repl_input(
	tx: &mpsc::Sender<ReplCommand>,
	_metrics: Arc<Metrics>,
	mut print_rx: mpsc::UnboundedReceiver<String>,
) {
	use std::io::{BufRead, Write};

	// Spawn thread to handle print requests (just println without readline
	// coordination)
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
			Ok(0) => {
				// EOF
				let _ = tx.blocking_send(ReplCommand::Quit);
				break;
			}
			Ok(_) => {
				let line = line.trim();
				if line.is_empty() {
					continue;
				}

				let cmd = parse_repl_command(line);
				let is_quit = matches!(cmd, ReplCommand::Quit);
				if tx.blocking_send(cmd).is_err() || is_quit {
					break;
				}
			}
			Err(_) => {
				let _ = tx.blocking_send(ReplCommand::Quit);
				break;
			}
		}
	}
}

/// Parse a line into a REPL command.
fn parse_repl_command(line: &str) -> ReplCommand {
	match line
		.split_whitespace()
		.next()
		.unwrap_or("")
		.to_lowercase()
		.as_str()
	{
		"quit" | "exit" | "q" => ReplCommand::Quit,
		"stats" | "s" => ReplCommand::Stats,
		"sessions" | "tuns" | "t" => ReplCommand::Sessions,
		"help" | "?" => ReplCommand::Help,
		_ => ReplCommand::Unknown(line.to_string()),
	}
}

fn print_stats(metrics: &Metrics, printer: &Printer) {
	printer.print("Traffic Statistics:");
	printer.print(format!(
		"  Bytes In:     {}",
		format_bytes(metrics.bytes_in.load(Ordering::Relaxed))
	));
	printer.print(format!(
		"  Bytes Out:    {}",
		format_bytes(metrics.bytes_out.load(Ordering::Relaxed))
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

fn print_sessions(sessions: &SessionManager, printer: &Printer) {
	let list = sessions.list();
	if list.is_empty() {
		printer.print("No active sessions.");
	} else {
		printer.print(format!("Active sessions ({}):", list.len()));
		for (exit_id, tun_name) in &list {
			printer.print(format!("  {exit_id} -> {tun_name}"));
		}
	}
}

fn print_help(printer: &Printer) {
	printer.print("Available commands:");
	printer.print("  stats, s       - Show traffic statistics");
	printer.print("  sessions, t    - List active exit node sessions");
	printer.print("  help, ?        - Show this help message");
	printer.print("  quit, q        - Exit wallhack");
}

fn format_bytes(bytes: u64) -> String {
	let units = ["B", "KB", "MB", "GB", "TB", "PB"];
	#[allow(clippy::cast_precision_loss)]
	let mut value = bytes as f64;
	let mut i = 0;

	// We suppress the warning here, once, for the initial cast.
	while value >= 1024.0 && i < units.len() - 1 {
		value /= 1024.0;
		i += 1;
	}

	// Use integer formatting for simple Bytes, float for everything else
	if i == 0 {
		format!("{} {}", bytes, units[0])
	} else {
		format!("{:.2} {}", value, units[i])
	}
}

/// Timeout for waiting for `ExitNodeHello` message.
const HELLO_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

async fn handle_connection<T: wallhack::transport::Transport + 'static>(
	metrics: Arc<Metrics>,
	accept_result: wallhack::server::server::AcceptResult<T>,
	hello_rx: Option<tokio::sync::oneshot::Receiver<protobuf::v2::ExitNodeHello>>,
	sessions: SessionManager,
) -> Result<String> {
	// Wait for ExitNodeHello to get exit node identity for session management
	let exit_id = if let Some(rx) = hello_rx {
		match tokio::time::timeout(HELLO_TIMEOUT, rx).await {
			Ok(Ok(hello)) => {
				crate::info!(
					"Exit node identified: {} (v{})",
					hello.exit_id,
					hello.version
				);
				Some(hello.exit_id)
			}
			Ok(Err(_)) => {
				crate::verbose!("ExitNodeHello channel closed before receiving message");
				None
			}
			Err(_) => {
				crate::verbose!("Timeout waiting for ExitNodeHello, using anonymous session");
				None
			}
		}
	} else {
		None
	};

	// Get or create TUN adapter via session manager
	let name = if let Some(ref id) = exit_id {
		sessions.get_or_create(id)
	} else {
		SessionManager::create_anonymous()
	};

	let actor = TunActor::new(Some(name.clone()))?;
	let manager = ConnectionManager::new(actor, accept_result.transport(), metrics);
	manager.run().await?;

	Ok(name)
}

fn parse_listen_addr(addr: &str) -> Result<std::net::SocketAddr> {
	// Check if it starts with ':' and capture the part after it in 'port'
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
	cli: &WallhackCli,
	addr: std::net::SocketAddr,
) -> wallhack::server::config::ServerConfig {
	let tls = match (&cli.cert, &cli.key) {
		(Some(cert), Some(key)) => Some(wallhack::server::config::TlsConfig {
			cert_pem_file: cert.clone(),
			key_pem_file: key.clone(),
			ca_roots: cli.ca.clone(),
		}),
		_ => None,
	};

	ServerConfig { listen: addr, tls }
}
