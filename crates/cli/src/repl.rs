//! Interactive REPL for the wallhack daemon.
//!
//! Uses `reedline` for line editing and history. Commands map to IPC
//! management requests; no new IPC features are introduced.

use reedline::{DefaultPrompt, DefaultPromptSegment, ExternalPrinter, Reedline, Signal};
use wallhack_wire::management::{self, management_request};

use crate::{ipc::IpcConnection, output};

/// Run the interactive REPL.
///
/// Takes an [`IpcConnection`] to the daemon (in-process duplex or external
/// Unix socket). The `printer` is used to safely print log output and
/// notifications without corrupting the prompt.
///
/// # Errors
///
/// Returns error for fatal failures (e.g. terminal I/O).
pub async fn run(
    mut conn: IpcConnection,
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

    println!("{}", crate::version::version());
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
                        println!("{}", crate::version::version());
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

                match conn.request(request).await {
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
        "info" => Some(management_request::Request::Info(
            wallhack_wire::management::InfoRequest {},
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
                Some(management_request::Request::PeerDisconnect(
                    wallhack_wire::management::PeerDisconnectRequest {
                        peer: (*peer).to_string(),
                        exact: false,
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
        "role" => parse_role_command(&parts),
        "hint" => parse_hint_command(&parts),
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
            Some(management_request::Request::RouteAdd(
                wallhack_wire::management::RouteAddRequest { cidr, peer },
            ))
        }
        "remove" => {
            let cidr = (*parts.get(2)?).to_string();
            Some(management_request::Request::RouteRemove(
                wallhack_wire::management::RouteRemoveRequest { cidr },
            ))
        }
        "list" | "" => Some(management_request::Request::Routes(
            wallhack_wire::management::RoutesRequest {},
        )),
        _ => None,
    }
}

/// Parse `role` command: `role` (show) or `role <entry|exit|relay>` (set fixed).
fn parse_role_command(parts: &[&str]) -> Option<management_request::Request> {
    match parts.get(1).copied() {
        None => {
            // `role` alone → show current role via info.
            Some(management_request::Request::Info(
                wallhack_wire::management::InfoRequest {},
            ))
        }
        Some(target) => {
            // `role <target>` → shorthand for `hint fixed <target>`.
            let role = parse_role_name(target)?;
            Some(management_request::Request::HintSet(
                management::HintSetRequest {
                    level: management::HintLevel::Fixed.into(),
                    role: role.into(),
                },
            ))
        }
    }
}

/// Parse `hint` command: `hint auto` or `hint <level> <role>`.
fn parse_hint_command(parts: &[&str]) -> Option<management_request::Request> {
    let sub = parts.get(1).copied()?;
    match sub {
        "auto" | "clear" => Some(management_request::Request::HintAuto(
            management::HintAutoRequest {},
        )),
        "prefer" | "exclude" | "fixed" => {
            let role_name = parts.get(2).copied()?;
            let role = parse_role_name(role_name)?;
            let level = match sub {
                "prefer" => management::HintLevel::Prefer,
                "exclude" => management::HintLevel::Exclude,
                "fixed" => management::HintLevel::Fixed,
                _ => unreachable!(),
            };
            Some(management_request::Request::HintSet(
                management::HintSetRequest {
                    level: level.into(),
                    role: role.into(),
                },
            ))
        }
        _ => None,
    }
}

/// Parse a role name string into a management `NodeRole`.
fn parse_role_name(s: &str) -> Option<management::NodeRole> {
    match s {
        "entry" => Some(management::NodeRole::Entry),
        "exit" => Some(management::NodeRole::Exit),
        "relay" => Some(management::NodeRole::Relay),
        _ => None,
    }
}

fn print_help() {
    use std::io::Write;
    use tabwriter::TabWriter;

    let mut tw = TabWriter::new(std::io::stdout());
    let _ = writeln!(tw, "Commands:");
    let _ = writeln!(tw, "  ping [<peer>]\tPing a peer");
    let _ = writeln!(tw, "  info\tShow daemon info");
    let _ = writeln!(tw, "  version\tShow version");
    let _ = writeln!(tw, "  stats\tShow traffic statistics");
    let _ = writeln!(tw, "  peers\tList connected peers");
    let _ = writeln!(tw, "  route\tList configured routes");
    let _ = writeln!(tw, "  route add <cidr> <peer>\tAdd a route");
    let _ = writeln!(tw, "  route remove <cidr>\tRemove a route");
    let _ = writeln!(tw, "  connect <addr>\tConnect to a peer");
    let _ = writeln!(tw, "  listen <addr>\tStart listening for connections");
    let _ = writeln!(tw, "  disconnect [peer]\tDisconnect peer");
    let _ = writeln!(tw, "  role\tShow current role");
    let _ = writeln!(tw, "  role <entry|exit|relay>\tSet role hint");
    let _ = writeln!(
        tw,
        "  hint <prefer|exclude|fixed> <role>\tApply a role hint"
    );
    let _ = writeln!(tw, "  hint auto\tReturn to capability-based negotiation");
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
