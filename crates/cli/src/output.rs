//! Output formatting for CLI responses.

use std::{fmt::Write as FmtWrite, io::Write};

use tabwriter::TabWriter;
use wallhack_wire::management::{
    ManagementResponse, PeerInfo as WirePeerInfo, management_response,
};

#[cfg(feature = "repl")]
use {
    tokio::sync::broadcast,
    wallhack_wire::management::{DaemonNotification, daemon_notification},
};

use crate::ipc::IpcError;

/// Format an uptime duration in milliseconds into a human-readable string.
fn format_uptime(ms: u64) -> String {
    let secs = ms / 1000;
    if secs < 60 {
        return format!("{secs}s");
    }
    let mins = secs / 60;
    let secs = secs % 60;
    if mins < 60 {
        return format!("{mins}m {secs}s");
    }
    let hours = mins / 60;
    let mins = mins % 60;
    if hours < 24 {
        return format!("{hours}h {mins}m");
    }
    let days = hours / 24;
    let hours = hours % 24;
    format!("{days}d {hours}h")
}

/// Escape a string for embedding in a JSON value.
fn json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Print the peers list as JSON to stdout.
///
/// Shape matches the REST API `/peers` response plus a `tun_name` field.
pub fn print_peers_json(peers: &[WirePeerInfo]) {
    use wallhack_wire::management::ConnectionSide;

    let mut out = String::from("{\"peers\":[");
    for (i, peer) in peers.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        let role = peer.role();
        let side = match peer.side() {
            ConnectionSide::Accept => "accept",
            ConnectionSide::Connect => "connect",
            ConnectionSide::Unspecified => "unknown",
        };
        let latency = if peer.latency_ms > 0.0 {
            format!("{}", peer.latency_ms)
        } else {
            "null".to_string()
        };
        let tun_name = if peer.tun_name.is_empty() {
            "null".to_string()
        } else {
            json_str(&peer.tun_name)
        };
        out.push('{');
        let _ = write!(
            out,
            "\"name\":{},\"addr\":{},\"role\":{},\"side\":{},\"connected_at_secs\":{},\"bytes_transferred\":{},\"latency_ms\":{},\"tun_name\":{}",
            json_str(&peer.name),
            json_str(&peer.addr),
            json_str(&format!("{role}")),
            json_str(side),
            peer.connected_at_secs,
            peer.bytes_transferred,
            latency,
            tun_name,
        );
        out.push('}');
    }
    out.push_str("]}");
    println!("{out}");
}

/// Print the peers table to stdout.
fn print_peers_table(peers: &[wallhack_wire::management::PeerInfo]) {
    use wallhack_wire::management::ConnectionSide;

    if peers.is_empty() {
        println!("No connected peers.");
        return;
    }

    let mut tw = TabWriter::new(std::io::stdout());
    let _ = writeln!(tw, "NAME\tADDR\tROLE\tSIDE\tLATENCY\tTUN");
    for peer in peers {
        let role = peer.role();
        let side = match peer.side() {
            ConnectionSide::Accept => "accept",
            ConnectionSide::Connect => "connect",
            ConnectionSide::Unspecified => "?",
        };
        let latency = if peer.latency_ms > 0.0 {
            format!("{:.1}ms", peer.latency_ms)
        } else {
            "\u{2014}".to_string()
        };
        let tun = if peer.tun_name.is_empty() {
            "\u{2014}".to_string()
        } else {
            peer.tun_name.clone()
        };
        let _ = writeln!(
            tw,
            "{}\t{}\t{role}\t{side}\t{latency}\t{tun}",
            peer.name, peer.addr,
        );
    }
    let _ = tw.flush();
}

/// Print a management response to stdout.
///
/// # Errors
///
/// Returns an error if the response contains an error from the daemon.
pub fn print_response(resp: &ManagementResponse) -> Result<(), CtlError> {
    match &resp.response {
        Some(management_response::Response::Status(s)) => {
            let role = s.role();
            let uptime = format_uptime(s.uptime_ms);

            println!("{:<18} {}", "role:", role);
            if !s.peer_addr.is_empty() {
                println!("{:<18} {}", "peer addr:", s.peer_addr);
            }
            if !s.listen_addr.is_empty() {
                println!("{:<18} {}", "listen addr:", s.listen_addr);
            }
            println!("{:<18} {} {}", "version:", s.package_name, s.version);
            println!("{:<18} wallhack {}", "cli:", env!("CARGO_PKG_VERSION"));
            println!("{:<18} {}", "uptime:", uptime);
        }
        Some(management_response::Response::Stats(s)) => {
            let mut tw = TabWriter::new(std::io::stdout());
            let _ = writeln!(tw, "bytes in:\t{}", s.bytes_in);
            let _ = writeln!(tw, "bytes out:\t{}", s.bytes_out);
            let _ = writeln!(tw, "packets in:\t{}", s.packets_in);
            let _ = writeln!(tw, "packets out:\t{}", s.packets_out);
            let _ = writeln!(tw, "connections:\t{}", s.active_connections);
            let _ = writeln!(tw, "flows:\t{}", s.active_flows);
            let _ = writeln!(tw, "dropped:\t{}", s.packets_dropped);
            let _ = tw.flush();
        }
        Some(management_response::Response::Peers(p)) => {
            print_peers_table(&p.peers);
        }
        Some(management_response::Response::Routes(r)) => {
            if r.routes.is_empty() {
                println!("No routes configured.");
            } else {
                let mut tw = TabWriter::new(std::io::stdout());
                let _ = writeln!(tw, "CIDR\tPEER");
                for route in &r.routes {
                    let _ = writeln!(tw, "{}\t{}", route.cidr, route.peer);
                }
                let _ = tw.flush();
            }
        }
        Some(management_response::Response::Connect(c)) => {
            println!("Connected to {} ({})", c.peer_addr, c.protocol);
        }
        Some(management_response::Response::Listen(l)) => {
            println!("Listening on {} ({})", l.listen_addr, l.protocol);
            if !l.fingerprint.is_empty() {
                println!("Fingerprint: {}", l.fingerprint);
            }
        }
        Some(management_response::Response::Ok(_)) => {
            println!("OK");
        }
        Some(management_response::Response::Error(e)) => {
            return Err(CtlError::Daemon(e.message.clone()));
        }
        Some(management_response::Response::Ping(_)) => {
            // Ping response is handled by daemon; not used by CLI currently.
            return Err(CtlError::Daemon(
                "unexpected ping response from daemon".to_string(),
            ));
        }
        None => {
            return Err(CtlError::EmptyResponse);
        }
    }
    Ok(())
}

/// Errors from the control CLI.
#[derive(Debug, thiserror::Error)]
pub enum CtlError {
    #[error("{0}")]
    Ipc(#[from] IpcError),
    #[error("daemon error: {0}")]
    Daemon(String),
    #[error("empty response from daemon")]
    EmptyResponse,
}

/// Forward daemon notifications to a callback.
///
/// Runs until the broadcast channel closes. Designed to be spawned as a task.
/// The callback receives formatted notification strings (e.g. for
/// `ExternalPrinter::sender().send()`).
#[cfg(feature = "repl")]
pub async fn forward_notifications(
    rx: &mut broadcast::Receiver<DaemonNotification>,
    mut emit: impl FnMut(String),
) {
    use nu_ansi_term::Color;

    loop {
        match rx.recv().await {
            Ok(notif) => {
                if let Some(line) = format_notification(&notif) {
                    emit(line);
                }
            }
            Err(broadcast::error::RecvError::Lagged(n)) => {
                emit(format!(
                    "{} missed {n} notification(s)",
                    Color::Yellow.paint("[!]")
                ));
            }
            Err(broadcast::error::RecvError::Closed) => break,
        }
    }
}

#[cfg(feature = "repl")]
fn format_notification(notif: &DaemonNotification) -> Option<String> {
    use nu_ansi_term::Color;

    match &notif.event {
        Some(daemon_notification::Event::PeerConnected(pc)) => {
            let peer = pc.peer.as_ref()?;
            Some(format!(
                "{} peer \"{}\" connected ({})",
                Color::Green.paint("[+]"),
                peer.name,
                peer.addr
            ))
        }
        Some(daemon_notification::Event::PeerDisconnected(pd)) => Some(format!(
            "{} peer \"{}\" disconnected",
            Color::Red.paint("[-]"),
            pd.name
        )),
        Some(daemon_notification::Event::TunnelError(te)) => {
            Some(format!("{} {}", Color::Yellow.paint("[!]"), te.message))
        }
        Some(daemon_notification::Event::ShuttingDown(sd)) => Some(format!(
            "{} daemon shutting down: {}",
            Color::Cyan.paint("[*]"),
            sd.reason
        )),
        Some(daemon_notification::Event::RoleChanged(rc)) => {
            let new_role = wallhack_wire::management::NodeRole::try_from(rc.new_role)
                .unwrap_or(wallhack_wire::management::NodeRole::Unspecified);
            Some(format!(
                "{} role changed to {}",
                Color::Cyan.paint("[*]"),
                new_role
            ))
        }
        _ => None,
    }
}
