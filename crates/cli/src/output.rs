//! Output formatting for CLI responses.

use std::io::Write;

use tabwriter::TabWriter;
use wallhack_wire::management::{ManagementResponse, management_response};

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
            if p.peers.is_empty() {
                println!("No connected peers.");
            } else {
                let mut tw = TabWriter::new(std::io::stdout());
                let _ = writeln!(tw, "NAME\tADDR\tSTATUS\tLATENCY\tTUN\tLISTEN\tCONNECT");
                for peer in &p.peers {
                    let status = peer.status();
                    let latency = if peer.latency_ms > 0.0 {
                        format!("{:.1}ms", peer.latency_ms)
                    } else {
                        "—".to_string()
                    };
                    let _ = writeln!(
                        tw,
                        "{}\t{}\t{}\t{}\t{}\t{}\t{}",
                        peer.name,
                        peer.addr,
                        status,
                        latency,
                        if peer.tun_capable { "yes" } else { "no" },
                        if peer.listening { "yes" } else { "no" },
                        if peer.connecting { "yes" } else { "no" },
                    );
                }
                let _ = tw.flush();
            }
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
        Some(daemon_notification::Event::PeerDisconnected(pd)) => {
            Some(format!(
                "{} peer \"{}\" disconnected",
                Color::Red.paint("[-]"),
                pd.name
            ))
        }
        Some(daemon_notification::Event::TunnelError(te)) => {
            Some(format!("{} {}", Color::Yellow.paint("[!]"), te.message))
        }
        Some(daemon_notification::Event::ShuttingDown(sd)) => {
            Some(format!(
                "{} daemon shutting down: {}",
                Color::Cyan.paint("[*]"),
                sd.reason
            ))
        }
        _ => None,
    }
}
