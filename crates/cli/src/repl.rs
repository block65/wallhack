//! Interactive REPL for the wallhack daemon.
//!
//! Uses `reedline` for line editing and history. Commands map to IPC
//! management requests; no new IPC features are introduced.

use reedline::{DefaultPrompt, DefaultPromptSegment, Reedline, Signal};
use tokio::net::UnixStream;
use wallhack_wire::management::management_request;

use crate::{ipc, output};

/// Run the interactive REPL.
///
/// Connects to the daemon's IPC socket, then enters a read-eval-print loop.
/// Reconnects automatically if the socket connection is lost.
///
/// # Errors
///
/// Returns error for fatal failures (e.g. terminal I/O).
#[allow(clippy::missing_panics_doc)] // stream is always Some when unwrap is called
pub async fn run() -> Result<(), Box<dyn std::error::Error>> {
	let history_path = dirs_home().join(".wallhack_history");
	let history: Box<dyn reedline::History> =
		match reedline::FileBackedHistory::with_file(1000, history_path) {
			Ok(h) => Box::new(h),
			Err(_) => {
				// Fall back to in-memory-only history if the file can't be opened.
				Box::new(reedline::FileBackedHistory::with_file(
					1000,
					std::path::PathBuf::from("/dev/null"),
				)?)
			}
		};

	let prompt = DefaultPrompt::new(
		DefaultPromptSegment::Basic("wallhack".to_string()),
		DefaultPromptSegment::Empty,
	);

	let mut line_editor = Reedline::create().with_history(history);

	let mut stream: Option<UnixStream> = None;

	loop {
		match line_editor.read_line(&prompt) {
			Ok(Signal::Success(line)) => {
				let line = line.trim();
				if line.is_empty() {
					continue;
				}

				match line {
					"quit" | "exit" => break,
					"help" | "?" => {
						print_help();
						continue;
					}
					_ => {}
				}

				let Some(request) = parse_command(line) else {
					eprintln!("unknown command: {line}");
					eprintln!("Type 'help' for available commands.");
					continue;
				};

				// Ensure we have a connection, reconnecting if needed.
				if stream.is_none() {
					match ipc::connect().await {
						Ok(s) => stream = Some(s),
						Err(e) => {
							eprintln!("cannot connect to daemon: {e}");
							continue;
						}
					}
				}

				match ipc::send_request(stream.as_mut().unwrap(), request).await {
					Ok(resp) => {
						if let Err(e) = output::print_response(&resp) {
							eprintln!("{e}");
						}
					}
					Err(e) => {
						eprintln!("IPC error: {e}");
						// Drop the broken connection so we reconnect next time.
						stream = None;
					}
				}
			}
			Ok(Signal::CtrlC) => {}
			Ok(Signal::CtrlD) => {
				break;
			}
			Err(e) => {
				eprintln!("readline error: {e}");
				break;
			}
		}
	}

	Ok(())
}

/// Parse a REPL command line into a management request.
fn parse_command(line: &str) -> Option<management_request::Request> {
	let parts: Vec<&str> = line.split_whitespace().collect();
	let cmd = *parts.first()?;

	match cmd {
		"ping" => Some(management_request::Request::Ping(
			wallhack_wire::management::PingRequest {},
		)),
		"status" | "info" => Some(management_request::Request::Status(
			wallhack_wire::management::StatusRequest {},
		)),
		"version" => {
			println!("{}", crate::version::version_short());
			Some(management_request::Request::Ping(
				wallhack_wire::management::PingRequest {},
			))
		}
		"stats" => Some(management_request::Request::Stats(
			wallhack_wire::management::StatsRequest {},
		)),
		"peers" => Some(management_request::Request::Peers(
			wallhack_wire::management::PeersRequest {},
		)),
		"routes" => Some(management_request::Request::Routes(
			wallhack_wire::management::RoutesRequest {},
		)),
		"route" => parse_route_command(&parts),
		"connect" => {
			let addr = parts.get(1)?;
			Some(management_request::Request::Connect(
				wallhack_wire::management::ConnectRequest {
					addr: (*addr).to_string(),
				},
			))
		}
		"listen" => {
			let addr = parts.get(1)?;
			Some(management_request::Request::Listen(
				wallhack_wire::management::ListenRequest {
					addr: (*addr).to_string(),
				},
			))
		}
		"disconnect" => {
			if let Some(peer) = parts.get(1) {
				Some(management_request::Request::DisconnectPeer(
					wallhack_wire::management::DisconnectPeerRequest {
						peer: (*peer).to_string(),
					},
				))
			} else {
				Some(management_request::Request::Disconnect(
					wallhack_wire::management::DisconnectRequest {},
				))
			}
		}
		"shutdown" => Some(management_request::Request::Shutdown(
			wallhack_wire::management::ShutdownRequest {},
		)),
		_ => None,
	}
}

/// Parse route sub-commands: `route add <cidr> [via] <peer>`, `route del <cidr>`.
fn parse_route_command(parts: &[&str]) -> Option<management_request::Request> {
	let sub = *parts.get(1)?;
	match sub {
		"add" => {
			let cidr = (*parts.get(2)?).to_string();
			// Support: `route add <cidr> <peer>` or `route add <cidr> via <peer>`
			let peer_idx = if parts.get(3) == Some(&"via") { 4 } else { 3 };
			let peer = (*parts.get(peer_idx)?).to_string();
			Some(management_request::Request::AddRoute(
				wallhack_wire::management::AddRouteRequest { cidr, peer },
			))
		}
		"del" | "remove" | "rm" => {
			let cidr = (*parts.get(2)?).to_string();
			Some(management_request::Request::RemoveRoute(
				wallhack_wire::management::RemoveRouteRequest { cidr },
			))
		}
		"list" | "ls" => Some(management_request::Request::Routes(
			wallhack_wire::management::RoutesRequest {},
		)),
		_ => None,
	}
}

fn print_help() {
	println!(
		"\
Commands:
  ping                         Ping the daemon
  status / info                Show daemon status
  version                      Show CLI and daemon version
  stats                        Show traffic statistics
  peers                        List connected peers
  routes / route list          List configured routes
  route add <cidr> [via] <peer>  Add a route
  route del <cidr>             Remove a route
  connect <addr>               Connect to a peer
  listen <addr>                Start listening for connections
  disconnect [peer]            Disconnect (upstream or specific peer)
  shutdown                     Shut down the daemon
  help / ?                     Show this help
  quit / exit                  Exit the REPL"
	);
}

/// Return the user's home directory.
fn dirs_home() -> std::path::PathBuf {
	std::env::var("HOME").map_or_else(
		|_| std::path::PathBuf::from("/tmp"),
		std::path::PathBuf::from,
	)
}
