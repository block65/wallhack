//! Entry node implementation.
//!
//! The entry node creates a TUN interface and accepts connections from exit or
//! relay nodes. It can either listen for incoming connections (default) or
//! connect to a remote peer (reverse tunnel). Includes an interactive REPL
//! when stdin is a TTY.

use std::{
	collections::HashMap,
	io::{IsTerminal, Write},
	sync::{
		Arc,
		atomic::{AtomicU64, Ordering},
	},
};

use anyhow::{Context, Result};
use parking_lot::Mutex;
use subtle::ConstantTimeEq;
use tokio::sync::mpsc;

use wallhack::{
	NodeRole,
	control::{
		handler::HandlerConfig,
		metrics::Metrics,
		peers::Registry,
		routes::{RouteTable, SharedRouteTable},
	},
	entry::{actor::TunActor, manager::ConnectionManager},
	server::{
		config::ServerConfig,
		server::{Server, ServerOptions},
	},
};

use crate::{
	WallhackCli,
	cli::{EntryCommand, Protocol, TransportDir},
};

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
	/// TUN. Otherwise creates a new TUN with stable naming (`tun-{name}`).
	fn get_or_create(&self, name: &str) -> std::string::String {
		let mut sessions = self.sessions.lock();

		if let Some(name) = sessions.get(name) {
			tracing::info!("Reusing existing TUN for exit node {}", name);
			return name.clone();
		}

		// Create new TUN with stable name
		let tun_name = format!("tun-{name}");
		tracing::info!("Creating new TUN {} for exit node {}", tun_name, name);
		sessions.insert(name.to_string(), tun_name.clone());
		tun_name
	}

	/// Gets a TUN adapter with auto-generated name (for exit nodes without
	/// identity).
	fn create_anonymous() -> std::string::String {
		TunActor::random_iface_name()
	}

	/// Look up the TUN device name for a peer.
	fn get_tun_for_peer(&self, peer: &str) -> Option<String> {
		self.sessions.lock().get(peer).cloned()
	}
}

/// Create a TUN device, retrying on EBUSY to handle the race where the
/// previous connection's `TunActor` hasn't been fully dropped yet.
async fn create_tun_with_retry(name: String) -> anyhow::Result<TunActor> {
	let mut attempts = 0;
	loop {
		match TunActor::new(Some(name.clone())) {
			Ok(actor) => return Ok(actor),
			Err(e) if attempts < 3 => {
				attempts += 1;
				tracing::debug!("TUN creation attempt {attempts} failed: {e}, retrying...");
				tokio::time::sleep(std::time::Duration::from_millis(500)).await;
			}
			Err(e) => return Err(e.into()),
		}
	}
}

#[cfg(feature = "readline")]
use rustyline::ExternalPrinter;

use crate::repl_common::{PeerRow, Printer, format_duration, print_peer_table, print_ping};

/// Run as an entry node with interactive REPL.
///
/// Creates TUN interface and either listens for downstream connections or
/// connects to a remote peer (reverse tunnel). Runs an interactive REPL for
/// control commands when stdin is a TTY.
///
/// # Errors
///
/// Returns error if server or client setup fails.
pub async fn run(global: &WallhackCli, cmd: &EntryCommand) -> Result<()> {
	crate::repl_common::mark_started();
	let transport = cmd.transport().map_err(|e| anyhow::anyhow!("{e}"))?;
	let sessions = SessionManager::default();
	let metrics = Arc::new(Metrics::default());
	let peers = Arc::new(Registry::new());
	let routes = RouteTable::shared();

	match transport {
		TransportDir::Both { .. } => {
			anyhow::bail!("Entry nodes do not support both --connect and --listen simultaneously")
		}
		TransportDir::Listen(spec) => {
			run_entry_listen(global, cmd, &spec, metrics, peers, routes, sessions).await
		}
		TransportDir::Connect(spec) => {
			run_entry_connect(global, cmd, &spec, metrics, peers, sessions).await
		}
	}
}

/// Run entry node in listen mode — set up server and accept connections.
async fn run_entry_listen(
	global: &WallhackCli,
	cmd: &EntryCommand,
	spec: &crate::cli::AddressSpec,
	metrics: Arc<Metrics>,
	peers: Arc<Registry>,
	routes: SharedRouteTable,
	sessions: SessionManager,
) -> Result<()> {
	let addr = parse_listen_addr(&spec.addr)?;
	let psk = global.resolve_psk();
	let server_options = ServerOptions {
		handler_config: HandlerConfig::new(NodeRole::Entry),
		metrics: Some(Arc::clone(&metrics)),
		peers: Some(Arc::clone(&peers)),
		routes: Some(Arc::clone(&routes)),
	};
	let server_config = build_server_config(global, addr, psk, cmd.max_peers);

	// Start REST API if enabled
	#[cfg(feature = "http-api")]
	if let Some(api_addr) = cmd.api_addr() {
		let (api_user, api_secret) = resolve_api_credentials(cmd, api_addr);
		start_api(
			api_addr,
			&metrics,
			&peers,
			&routes,
			server_config.tls.clone(),
			api_user,
			api_secret,
		);
	}

	match spec.protocol {
		Protocol::Udp => {
			#[cfg(feature = "quic")]
			{
				let server =
					wallhack::server::quic::QuicServer::try_new(server_config, server_options)?;
				crate::info!("Listening on {addr} (QUIC/UDP)");
				crate::info!("Certificate fingerprint: {}", server.fingerprint());
				if server.psk().is_none() {
					crate::warn!(
						"No authentication configured. Use --psk <SECRET> to require authentication."
					);
				}
				run_entry_server(
					server,
					metrics,
					peers,
					routes,
					sessions,
					cmd.max_peers,
					cmd.fast,
				)
				.await
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
				crate::info!("Certificate fingerprint: {}", server.fingerprint());
				if server.psk().is_none() {
					crate::warn!(
						"No authentication configured. Use --psk <SECRET> to require authentication."
					);
				}
				run_entry_server(
					server,
					metrics,
					peers,
					routes,
					sessions,
					cmd.max_peers,
					cmd.fast,
				)
				.await
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

/// Run entry node in connect mode (reverse tunnel).
///
/// Entry connects to a remote peer but still creates TUN and runs REPL.
async fn run_entry_connect(
	global: &WallhackCli,
	cmd: &EntryCommand,
	spec: &crate::cli::AddressSpec,
	metrics: Arc<Metrics>,
	peers: Arc<Registry>,
	_sessions: SessionManager,
) -> Result<()> {
	use std::{str::FromStr, time::Duration};
	use wallhack::client::client::Client;

	const INITIAL_RETRY_DELAY: Duration = Duration::from_secs(1);
	const MAX_RETRY_DELAY: Duration = Duration::from_secs(30);

	// Used only with the `api` feature.
	let _ = (&cmd, &peers);

	crate::info!("Resolving {}", spec.addr);
	let resolvable = crate::dns::ResolvableAddress::from_str(&spec.addr)?;
	let dns_server = global
		.dns
		.as_ref()
		.map(|s| crate::dns::parse_str_to_addr(s))
		.transpose()?;
	let endpoint = crate::dns::resolve(resolvable, dns_server).await?;

	// Start REST API if enabled
	#[cfg(feature = "http-api")]
	if let Some(api_addr) = cmd.api_addr() {
		let tls = build_tls_config(global);
		let routes = RouteTable::shared();
		let (api_user, api_secret) = resolve_api_credentials(cmd, api_addr);
		start_api(
			api_addr, &metrics, &peers, &routes, tls, api_user, api_secret,
		);
	}

	match spec.protocol {
		Protocol::Udp => {
			#[cfg(feature = "quic")]
			{
				let client_config = build_quic_client_config(global, endpoint);
				let mut retry_delay = INITIAL_RETRY_DELAY;

				loop {
					let mut client =
						wallhack::client::quic::QuicClient::try_new(client_config.clone())?;
					match client.connect(NodeRole::Entry).await {
						Ok(connect_result) => {
							retry_delay = INITIAL_RETRY_DELAY;
							handle_entry_connect_result(connect_result, &metrics, cmd.fast).await?;
						}
						Err(e) => {
							if crate::repl_common::is_nonretryable_error(&e) {
								println!("Connection failed (not retrying): {e}");
								return Err(e.into());
							}
							tracing::debug!("Connection failed: {e}, retrying in {retry_delay:?}");
							println!("Connection failed: {e}, retrying in {retry_delay:?}...");
							tokio::time::sleep(retry_delay).await;
							retry_delay = (retry_delay * 2).min(MAX_RETRY_DELAY);
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
				use wallhack::client::ws::{WsClient, WsClientConfig};

				let client_config = WsClientConfig {
					base: wallhack::client::config::ClientConfig {
						addr: endpoint,
						hostname: global.hostname.clone(),
						mtls: None,
						..Default::default()
					},
					path: "/ws".to_string(),
					host_header: global.hostname.clone(),
					use_tls: true,
				};
				let mut retry_delay = INITIAL_RETRY_DELAY;

				loop {
					let mut client = WsClient::new(client_config.clone())?;
					match client.connect(NodeRole::Entry).await {
						Ok(connect_result) => {
							retry_delay = INITIAL_RETRY_DELAY;
							handle_entry_connect_result(connect_result, &metrics, cmd.fast).await?;
						}
						Err(e) => {
							if crate::repl_common::is_nonretryable_error(&e) {
								println!("Connection failed (not retrying): {e}");
								return Err(e.into());
							}
							tracing::debug!("Connection failed: {e}, retrying in {retry_delay:?}");
							println!("Connection failed: {e}, retrying in {retry_delay:?}...");
							tokio::time::sleep(retry_delay).await;
							retry_delay = (retry_delay * 2).min(MAX_RETRY_DELAY);
						}
					}
				}
			}
			#[cfg(not(feature = "websocket"))]
			anyhow::bail!("WebSocket transport not available (compile with --features websocket)")
		}
	}
}

/// Handle a successful entry-side connect result by creating TUN + bridge.
async fn handle_entry_connect_result<T: wallhack::transport::Transport + 'static>(
	connect_result: wallhack::client::client::ConnectResult<T>,
	metrics: &Arc<Metrics>,
	fast_mode: bool,
) -> Result<()> {
	crate::info!("Connected to {}", connect_result.client_ident());

	let name = SessionManager::create_anonymous();
	let actor = create_tun_with_retry(name.clone()).await?;
	let (manager, _syn_proxy_state) = ConnectionManager::new(
		actor,
		connect_result.transport(),
		Arc::clone(metrics),
		fast_mode,
	);
	manager.run().await?;

	Ok(())
}

/// Generic entry server loop that works with any `Server` implementation.
#[allow(clippy::too_many_lines)]
async fn run_entry_server<S: Server>(
	mut server: S,
	metrics: Arc<Metrics>,
	peers: Arc<Registry>,
	routes: SharedRouteTable,
	sessions: SessionManager,
	max_peers: Option<usize>,
	fast_mode: bool,
) -> Result<()>
where
	S::Error: std::error::Error + Send + Sync + 'static,
	S::Transport: Send + Sync + 'static,
{
	let server_psk = server.psk().map(String::from);
	let peer_semaphore = Arc::new(tokio::sync::Semaphore::new(
		max_peers.unwrap_or(tokio::sync::Semaphore::MAX_PERMITS),
	));

	// Channel for REPL commands (input thread -> async loop)
	let (repl_tx, repl_rx) = mpsc::channel::<ReplCommand>(16);

	// Channel for async prints (async loop -> input thread)
	let (print_tx, print_rx) = mpsc::unbounded_channel::<String>();
	let printer = Printer::new(print_tx);

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

	// Connection counter for tracking
	let next_conn_id = AtomicU64::new(1);

	// Main loop: handle both server accepts and REPL commands
	loop {
		tokio::select! {
			// Handle incoming connections
			accept_result = server.accept(NodeRole::Entry) => {
				match accept_result {
					Ok(Some(accept_result)) => {
						// Enforce max peers limit
						let Ok(permit) = Arc::clone(&peer_semaphore).try_acquire_owned() else {
							crate::info!("Max peers reached, rejecting connection from {}", accept_result.client_ident());
							printer.print(format!("Rejected connection from {} (max peers reached)", accept_result.client_ident()));
							continue;
						};

						let conn_id = next_conn_id.fetch_add(1, Ordering::Relaxed);
						let conn_metrics = accept_result.metrics();
						let conn_printer = printer.clone();
						let conn_sessions = sessions.clone();
						let conn_peers = Arc::clone(&peers);
						let conn_routes = Arc::clone(&routes);
						let peer_addr = accept_result.client_ident().to_string();
						let peer = accept_result
							.exit_hello()
							.map_or_else(|| peer_addr.clone(), |h| h.name.clone());

						crate::info!("Connection #{conn_id} from {peer_addr}");
						printer.print(format!("Connection #{conn_id} from {peer_addr}"));

						// Register peer in the registry
						conn_peers.register(peer.clone(), peer_addr, NodeRole::Exit);

						// Create ping channel for this peer
						let mut ping_rx = conn_peers.register_ping_channel(&peer);
						let transport = accept_result.transport();

						// Spawn handler for this connection (each exit node gets its own TUN)
						let conn_psk = server_psk.clone();
						tokio::spawn(async move {
							// Hold the permit for the lifetime of this connection
							let _permit = permit;
							let result = handle_connection(conn_metrics, accept_result, conn_sessions.clone(), &mut ping_rx, &transport, &conn_peers, conn_psk, fast_mode).await;
							// Unregister peer when connection closes
							conn_peers.unregister(&peer);
							// Clean up routes for this peer
							let removed_routes = conn_routes.remove_by_peer(&peer);
							for entry in &removed_routes {
								if let Some(tun) = conn_sessions.get_tun_for_peer(&peer) {
									let _ = remove_os_route(&entry.cidr.to_string(), &tun);
								}
							}
							if !removed_routes.is_empty() {
								conn_printer.print(format!(
									"Removed {} route(s) for disconnected peer {peer}",
									removed_routes.len()
								));
							}
							match result {
								Ok(tun_name) => {
									conn_printer.print(format!("Connection #{conn_id} closed (tun: {tun_name})"));
								}
								Err(e) => {
									tracing::debug!("Connection #{} error: {}", conn_id, e);
									conn_printer.print(format!("Connection #{conn_id} error: {e}"));
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
					Some(ReplCommand::Ping) => {
						print_ping(&printer);
						let peer_names = peers.peer_names();
						if !peer_names.is_empty() {
							for id in &peer_names {
								match peers.ping_peer(id).await {
									Ok(ms) => printer.print(format!("  {id}: {ms:.3}ms")),
									Err(e) => printer.print(format!("  {id}: ping failed ({e})")),
								}
							}
						}
					}
					Some(ReplCommand::Stats) => {
						print_stats(&metrics, &printer);
					}
					Some(ReplCommand::Peers) => {
						print_peers(&peers, &sessions, &printer);
					}
					Some(ReplCommand::RouteAdd(cidr, peer)) => {
						handle_route_add(&cidr, &peer, &routes, &sessions, &printer);
					}
					Some(ReplCommand::RouteRemove(cidr)) => {
						handle_route_remove(&cidr, &routes, &sessions, &printer);
					}
					Some(ReplCommand::RouteList) => {
						handle_route_list(&routes, &sessions, &printer);
					}
					Some(ReplCommand::Disconnect(peer)) => {
						handle_disconnect(&peer, &peers, &printer);
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
	Ping,
	Stats,
	Peers,
	RouteAdd(String, String),
	RouteRemove(String),
	RouteList,
	Disconnect(String),
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
	let mut parts = line.split_whitespace();
	let cmd = parts.next().unwrap_or("").to_lowercase();
	let arg = parts.next().map(String::from);

	match cmd.as_str() {
		"quit" | "exit" | "q" => ReplCommand::Quit,
		"ping" | "p" => ReplCommand::Ping,
		"stats" | "s" => ReplCommand::Stats,
		"peers" | "sessions" | "tuns" | "t" => ReplCommand::Peers,
		"route" => parse_route_subcommand(arg.as_deref(), &mut parts),
		"ip" => match arg.as_deref() {
			Some("route") => {
				let sub = parts.next().map(String::from);
				parse_route_subcommand(sub.as_deref(), &mut parts)
			}
			_ => ReplCommand::Unknown("ip route <add|del|list> ...".to_string()),
		},
		"routes" => ReplCommand::RouteList,
		"disconnect" | "kick" | "kill" => {
			if let Some(peer) = arg {
				ReplCommand::Disconnect(peer)
			} else {
				ReplCommand::Unknown("disconnect <peer>".to_string())
			}
		}
		"help" | "?" => ReplCommand::Help,
		_ => ReplCommand::Unknown(line.to_string()),
	}
}

/// Parse route subcommands (shared between `route ...` and `ip route ...`).
fn parse_route_subcommand(
	sub: Option<&str>,
	parts: &mut std::str::SplitWhitespace<'_>,
) -> ReplCommand {
	match sub {
		Some("add") => {
			let cidr = parts.next();
			// Skip optional "via" keyword
			let next = parts.next();
			let peer = match next {
				Some("via") => parts.next(),
				other => other,
			};
			match (cidr, peer) {
				(Some(c), Some(p)) => ReplCommand::RouteAdd(c.to_string(), p.to_string()),
				_ => ReplCommand::Unknown("route add <cidr> [via] <peer>".to_string()),
			}
		}
		Some("del" | "rm" | "remove") => {
			if let Some(cidr) = parts.next() {
				ReplCommand::RouteRemove(cidr.to_string())
			} else {
				ReplCommand::Unknown("route del <cidr>".to_string())
			}
		}
		Some("list" | "ls") | None => ReplCommand::RouteList,
		_ => ReplCommand::Unknown("route <add|del|list> ...".to_string()),
	}
}

fn print_stats(metrics: &Metrics, printer: &Printer) {
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

fn print_peers(peers: &Arc<Registry>, sessions: &SessionManager, printer: &Printer) {
	let list = peers.list();
	let rows: Vec<PeerRow> = list
		.iter()
		.map(|peer| {
			let latency = peer
				.latency_ms
				.map_or_else(|| "N/A".to_string(), |ms| format!("{ms:.3}ms"));
			PeerRow {
				name: peer.name.clone(),
				role: peer.role.to_string(),
				addr: peer.addr.clone(),
				latency,
				uptime: format_duration(peer.connected_at.elapsed()),
				device: sessions.get_tun_for_peer(&peer.name),
			}
		})
		.collect();
	print_peer_table(printer, &rows);
}

fn handle_route_add(
	cidr: &str,
	peer: &str,
	routes: &SharedRouteTable,
	sessions: &SessionManager,
	printer: &Printer,
) {
	let parsed: wallhack::Cidr = match cidr.parse() {
		Ok(c) => c,
		Err(e) => {
			printer.print(format!("Invalid CIDR '{cidr}': {e}"));
			return;
		}
	};

	let Some(tun_name) = sessions.get_tun_for_peer(peer) else {
		printer.print(format!("No TUN session found for peer '{peer}'"));
		return;
	};

	// Apply OS route first so we can rollback on failure
	if let Err(reason) = apply_os_route(cidr, &tun_name) {
		printer.print(format!(
			"Failed to add route {cidr} via peer {peer}: {reason}"
		));
		return;
	}

	routes.add(parsed, peer.to_string());
	printer.print(format!("Route added: {cidr} via {peer} dev {tun_name}"));
}

fn handle_route_remove(
	cidr: &str,
	routes: &SharedRouteTable,
	sessions: &SessionManager,
	printer: &Printer,
) {
	let parsed: wallhack::Cidr = match cidr.parse() {
		Ok(c) => c,
		Err(e) => {
			printer.print(format!("Invalid CIDR '{cidr}': {e}"));
			return;
		}
	};

	match routes.remove(&parsed) {
		Some(entry) => {
			if let Some(tun) = sessions.get_tun_for_peer(&entry.peer)
				&& let Err(reason) = remove_os_route(cidr, &tun)
			{
				printer.print(format!(
					"Warning: route table updated but OS route removal failed: {reason}"
				));
			}
			printer.print(format!("Route removed: {cidr} (was -> {})", entry.peer));
		}
		None => {
			printer.print(format!("Route not found: {cidr}"));
		}
	}
}

fn handle_route_list(routes: &SharedRouteTable, sessions: &SessionManager, printer: &Printer) {
	let list = routes.list();
	if list.is_empty() {
		printer.print("No routes configured.");
		return;
	}

	printer.print(format!("Routes ({}):", list.len()));

	let mut tw = tabwriter::TabWriter::new(vec![]).padding(2);
	let _ = writeln!(tw, "  DESTINATION\tVIA\tDEVICE\tAGE");
	for entry in &list {
		let tun = sessions
			.get_tun_for_peer(&entry.peer)
			.unwrap_or_else(|| "?".to_string());
		let _ = writeln!(
			tw,
			"  {}\t{}\t{}\t{}",
			entry.cidr,
			entry.peer,
			tun,
			format_duration(entry.added_at.elapsed()),
		);
	}
	let _ = tw.flush();
	let buf = tw.into_inner().unwrap_or_default();
	let output = String::from_utf8_lossy(&buf);
	for line in output.trim_end().lines() {
		printer.print(line.trim_end());
	}
}

fn handle_disconnect(peer: &str, peers: &Arc<Registry>, printer: &Printer) {
	if peers.unregister(peer).is_some() {
		printer.print(format!("Disconnected peer: {peer}"));
	} else {
		printer.print(format!("Peer not found: {peer}"));
	}
}

fn print_help(printer: &Printer) {
	printer.print("Available commands:");
	printer.print("  ping, p                              - Show version and uptime");
	printer.print("  stats, s                             - Show traffic statistics");
	printer.print("  peers                                - List connected peers and sessions");
	printer.print("  route add <cidr> via <peer>          - Add a route");
	printer.print("  route del <cidr>                     - Remove a route");
	printer.print("  route list, routes, ip route         - List all routes");
	printer.print("  disconnect <peer>                    - Disconnect a peer");
	printer.print("  help, ?                              - Show this help message");
	printer.print("  quit, q                              - Exit wallhack");
}

/// Apply an OS-level route via `ip route add`.
fn apply_os_route(cidr: &str, dev: &str) -> Result<(), String> {
	match std::process::Command::new("ip")
		.args(["route", "add", cidr, "dev", dev])
		.output()
	{
		Ok(output) => {
			if output.status.success() {
				tracing::info!("OS route added: {cidr} dev {dev}");
				Ok(())
			} else {
				let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
				tracing::warn!("Failed to add OS route: {stderr}");
				Err(stderr)
			}
		}
		Err(e) => {
			tracing::warn!("Failed to run ip route add: {e}");
			Err(e.to_string())
		}
	}
}

/// Remove an OS-level route via `ip route del`.
fn remove_os_route(cidr: &str, dev: &str) -> Result<(), String> {
	match std::process::Command::new("ip")
		.args(["route", "del", cidr, "dev", dev])
		.output()
	{
		Ok(output) => {
			if output.status.success() {
				tracing::info!("OS route removed: {cidr} dev {dev}");
				Ok(())
			} else {
				let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
				tracing::debug!("Failed to remove OS route: {stderr}");
				Err(stderr)
			}
		}
		Err(e) => {
			tracing::debug!("Failed to run ip route del: {e}");
			Err(e.to_string())
		}
	}
}

// TODO: refactor into a ConnectionContext struct to reduce argument count
#[allow(clippy::too_many_arguments)]
async fn handle_connection<T: wallhack::transport::Transport + 'static>(
	metrics: Arc<Metrics>,
	mut accept_result: wallhack::server::server::AcceptResult<T>,
	sessions: SessionManager,
	ping_rx: &mut tokio::sync::mpsc::Receiver<wallhack::control::peers::PingRequest>,
	transport: &Arc<T>,
	peers: &Arc<wallhack::control::peers::Registry>,
	server_psk: Option<String>,
	fast_mode: bool,
) -> Result<String> {
	// Get ExitNodeHello directly from accept result (already read during accept)
	let peer = if let Some(hello) = accept_result.take_exit_hello() {
		// Validate PSK if configured
		if let Some(ref expected_psk) = server_psk {
			let token_bytes = hello.auth_token.as_bytes();
			let expected_bytes = expected_psk.as_bytes();
			if token_bytes.len() != expected_bytes.len()
				|| !bool::from(token_bytes.ct_eq(expected_bytes))
			{
				tracing::warn!("Peer {} failed PSK authentication, dropping", hello.name);
				anyhow::bail!("PSK authentication failed for peer {}", hello.name);
			}
		}

		crate::info!("Exit node identified: {} (v{})", hello.name, hello.version);
		Some(hello.name)
	} else {
		crate::verbose!("No ExitNodeHello received, using anonymous session");
		None
	};

	// Spawn data tasks AFTER PSK validation (structural guarantee: no data before auth)
	let ((instructions_tx, responses_tx), control_tx) = accept_result.channels();

	// Data task: incoming data (accept uni stream, read data messages)
	let transport_data = Arc::clone(transport);
	let instructions_in = instructions_tx.clone();
	let responses_in = responses_tx.clone();
	tokio::spawn(async move {
		match transport_data.accept_uni().await {
			Ok(Some(mut recv)) => {
				if let Err(e) = wallhack::transport::bridge::run_data_in(
					&mut recv,
					&instructions_in,
					&responses_in,
				)
				.await
				{
					tracing::debug!("Data-in handler finished: {e}");
				}
			}
			Ok(None) => tracing::debug!("Transport closed before data-in stream accepted"),
			Err(e) => tracing::debug!("Failed to accept data-in stream: {e}"),
		}
	});

	// Data task: outgoing instructions (open uni stream, write instructions)
	let transport_out = Arc::clone(transport);
	let instructions_out = instructions_tx.clone();
	tokio::spawn(async move {
		match transport_out.open_uni().await {
			Ok(mut send) => {
				if let Err(e) = wallhack::transport::bridge::run_data_out_instructions(
					&mut send,
					&instructions_out,
				)
				.await
				{
					tracing::debug!("Data-out instructions handler finished: {e}");
				}
			}
			Err(e) => tracing::debug!("Failed to open data-out stream: {e}"),
		}
	});

	// Get or create TUN adapter via session manager
	let name = if let Some(ref id) = peer {
		sessions.get_or_create(id)
	} else {
		SessionManager::create_anonymous()
	};

	let actor = create_tun_with_retry(name.clone()).await?;
	let (manager, _syn_proxy_state) =
		ConnectionManager::new(actor, Arc::clone(transport), metrics, fast_mode);

	// Run the connection manager alongside ping handling
	let mut manager_handle = tokio::spawn(async move { manager.run().await });

	loop {
		tokio::select! {
			result = &mut manager_handle => {
				// Connection ended
				result??;
				break;
			}
			Some(result_tx) = ping_rx.recv() => {
				match send_ping(&control_tx).await {
					Ok(ms) => {
						if let Some(ref id) = peer {
							peers.update_latency(id, ms);
						}
						let _ = result_tx.send(ms);
					}
					Err(e) => {
						tracing::debug!("Ping failed: {e}");
						drop(result_tx);
					}
				}
			}
		}
	}

	Ok(name)
}

/// Send a ping via the control stream and measure round-trip time.
async fn send_ping(
	control_tx: &tokio::sync::mpsc::Sender<protobuf::control_v2::ControlMessage>,
) -> Result<f64> {
	use protobuf::control_v2::{ControlMessage, control_message};

	#[allow(clippy::cast_possible_truncation)]
	let ts = std::time::SystemTime::now()
		.duration_since(std::time::UNIX_EPOCH)
		.unwrap_or_default()
		.as_millis() as u64;

	let ping_msg = ControlMessage {
		message: Some(control_message::Message::Ping(protobuf::v2::Ping {
			timestamp_ms: ts,
		})),
	};

	let start = std::time::Instant::now();

	// Send ping via control stream
	control_tx
		.send(ping_msg)
		.await
		.map_err(|_| anyhow::anyhow!("Control channel closed"))?;

	// NOTE: Pong is handled inline in the control loop. For now we measure
	// the time until the message is queued. A proper implementation would
	// await a pong notification channel, but this is sufficient for a
	// basic latency estimate.
	Ok(start.elapsed().as_secs_f64() * 1000.0)
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
	psk: Option<String>,
	max_peers: Option<usize>,
) -> wallhack::server::config::ServerConfig {
	ServerConfig {
		listen: addr,
		tls: build_tls_config(global),
		psk,
		max_peers,
	}
}

fn build_tls_config(global: &WallhackCli) -> Option<wallhack::server::config::TlsConfig> {
	match (&global.cert, &global.key) {
		(Some(cert), Some(key)) => Some(wallhack::server::config::TlsConfig {
			cert_pem_file: cert.clone(),
			key_pem_file: key.clone(),
			ca_roots: global.ca.clone(),
		}),
		_ => None,
	}
}

/// Resolve API credentials, generating a random secret if not provided.
///
/// Always logs credentials so the user knows how to authenticate.
#[cfg(feature = "http-api")]
fn resolve_api_credentials(cmd: &EntryCommand, api_addr: std::net::SocketAddr) -> (String, String) {
	let username = cmd.api_user.clone().unwrap_or_else(|| "admin".to_string());

	let (secret, generated) = if let Some(s) = &cmd.api_secret {
		(s.clone(), false)
	} else {
		use rand::Rng;
		const CHARSET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
		let mut rng = rand::rng();
		let secret: String = (0..32)
			.map(|_| CHARSET[rng.random_range(0..CHARSET.len())] as char)
			.collect();
		(secret, true)
	};

	crate::info!("REST API listening on {api_addr}");
	crate::info!("  API username: {username}");
	if generated {
		crate::info!("  API secret:   {secret}  (auto-generated)");
	} else {
		crate::info!("  API secret:   {secret}");
	}

	(username, secret)
}

#[cfg(feature = "http-api")]
fn start_api(
	api_addr: std::net::SocketAddr,
	metrics: &Arc<Metrics>,
	peers: &Arc<Registry>,
	routes: &SharedRouteTable,
	tls_config: Option<wallhack::server::config::TlsConfig>,
	username: String,
	secret: String,
) {
	use wallhack::api::{Auth, State as ApiState};

	let handler_config = HandlerConfig::new(NodeRole::Entry);
	let auth = Auth::new(username, secret);
	let state = ApiState::new(
		handler_config,
		Arc::clone(metrics),
		Arc::clone(peers),
		Arc::clone(routes),
		auth,
	);

	tokio::spawn(async move {
		if let Err(e) = wallhack::api::serve(api_addr, state, tls_config).await {
			tracing::error!("REST API error: {e}");
		}
	});
}

#[cfg(feature = "quic")]
fn build_quic_client_config(
	global: &WallhackCli,
	endpoint: std::net::SocketAddr,
) -> wallhack::client::config::ClientConfig {
	let mtls = match (&global.cert, &global.key) {
		(Some(cert), Some(key)) => Some(wallhack::client::config::MtlsConfig {
			cert_pem_file: cert.clone(),
			key_pem_file: key.clone(),
			ca_roots: global.ca.clone(),
		}),
		_ => None,
	};

	wallhack::client::config::ClientConfig {
		addr: endpoint,
		hostname: global.hostname.clone(),
		mtls,
		psk: global.resolve_psk(),
		..Default::default()
	}
}

#[cfg(test)]
mod tests {
	#[test]
	fn peer_semaphore_default_does_not_panic() {
		// Regression: using usize::MAX exceeded tokio's MAX_PERMITS and panicked.
		let _sem = tokio::sync::Semaphore::new(tokio::sync::Semaphore::MAX_PERMITS);
	}

	#[test]
	fn peer_semaphore_with_limit() {
		let _sem = tokio::sync::Semaphore::new(10);
	}
}
