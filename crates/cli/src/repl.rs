//! Interactive REPL for the wallhack daemon.
//!
//! Uses `reedline` for line editing and history. Commands map to IPC
//! management requests; no new IPC features are introduced.

use reedline::{DefaultPrompt, DefaultPromptSegment, ExternalPrinter, Reedline, Signal};
use tokio::io::{AsyncRead, AsyncWrite};
use wallhack_wire::management::management_request;

use crate::{ipc, output};

/// Run the interactive REPL.
///
/// Takes an already-connected stream to the daemon (in-process duplex or
/// external Unix socket). Commands are sent as IPC requests over that stream.
/// The `printer` is used to safely print log output without corrupting the prompt.
///
/// # Errors
///
/// Returns error for fatal failures (e.g. terminal I/O).
pub async fn run(
    mut stream: impl AsyncRead + AsyncWrite + Unpin,
    printer: ExternalPrinter<String>,
) -> Result<(), Box<dyn std::error::Error>> {
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

    let mut line_editor = Reedline::create()
        .with_history(history)
        .with_external_printer(printer);

    println!("{}", crate::version::version_short());
    println!("Type 'help' for available commands.");

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
                    "version" => {
                        println!("{}", crate::version::version_short());
                        continue;
                    }
                    _ => {}
                }

                let Some(request) = parse_command(line) else {
                    eprintln!(
                        "error: unknown command: {line} (type 'help' for available commands)"
                    );
                    continue;
                };

                match ipc::send_request(&mut stream, request).await {
                    Ok(resp) => {
                        if let Err(e) = output::print_response(&resp) {
                            eprintln!("error: {e}");
                        }
                    }
                    Err(e) => {
                        eprintln!("error: {e}");
                        break;
                    }
                }
            }
            Ok(Signal::CtrlC) => {}
            Ok(Signal::CtrlD) => {
                break;
            }
            Err(e) => {
                eprintln!("error: {e}");
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
        "ping" => {
            let peer = parts
                .get(1)
                .map(std::string::ToString::to_string)
                .unwrap_or_default();
            Some(management_request::Request::Ping(
                wallhack_wire::management::PingRequest { peer },
            ))
        }
        "info" => Some(management_request::Request::Status(
            wallhack_wire::management::StatusRequest {},
        )),
        "stats" => Some(management_request::Request::Stats(
            wallhack_wire::management::StatsRequest {},
        )),
        "peers" => Some(management_request::Request::Peers(
            wallhack_wire::management::PeersRequest {},
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
    let sub = parts.get(1).copied().unwrap_or("list");
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
        "del" | "remove" => {
            let cidr = (*parts.get(2)?).to_string();
            Some(management_request::Request::RemoveRoute(
                wallhack_wire::management::RemoveRouteRequest { cidr },
            ))
        }
        "list" | "" => Some(management_request::Request::Routes(
            wallhack_wire::management::RoutesRequest {},
        )),
        _ => None,
    }
}

fn print_help() {
    use std::io::Write;
    use tabwriter::TabWriter;

    let mut tw = TabWriter::new(std::io::stdout());
    let _ = writeln!(tw, "Commands:");
    let _ = writeln!(tw, "  ping [<peer>]\tPing the daemon or a peer");
    let _ = writeln!(tw, "  info\tShow daemon info");
    let _ = writeln!(tw, "  version\tShow version");
    let _ = writeln!(tw, "  stats\tShow traffic statistics");
    let _ = writeln!(tw, "  peers\tList connected peers");
    let _ = writeln!(tw, "  route\tList configured routes");
    let _ = writeln!(tw, "  route add <cidr> <peer>\tAdd a route");
    let _ = writeln!(tw, "  route del <cidr>\tRemove a route");
    let _ = writeln!(tw, "  connect <addr>\tConnect to a peer");
    let _ = writeln!(tw, "  listen <addr>\tStart listening for connections");
    let _ = writeln!(tw, "  disconnect [peer]\tDisconnect peer");
    let _ = writeln!(tw, "  shutdown\tShut down the daemon");
    let _ = writeln!(tw, "  help / ?\tShow this help");
    let _ = writeln!(tw, "  quit \tQuit the REPL");
    let _ = tw.flush();
}

/// Return the user's home directory.
fn dirs_home() -> std::path::PathBuf {
    std::env::var("HOME").map_or_else(
        |_| std::path::PathBuf::from("/tmp"),
        std::path::PathBuf::from,
    )
}
