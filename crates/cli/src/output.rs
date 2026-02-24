//! Output formatting for CLI responses.

use std::io::Write;

use tabwriter::TabWriter;
use wallhack_wire::management::{self, ManagementResponse, management_response};

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
            let role = role_str(s.role());
            let capability = capability_str(s.capability());
            let uptime = format_uptime(s.uptime_ms);

            println!("{:<18} {}", "role:", role);
            if !s.peer_addr.is_empty() {
                println!("{:<18} {}", "peer addr:", s.peer_addr);
            }
            println!("{:<18} {}", "capability:", capability);
            if !s.listen_addr.is_empty() {
                println!("{:<18} {}", "listen addr:", s.listen_addr);
            }
            println!("{:<18} {} {}", "version:", s.package_name, s.version);
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
                let _ = writeln!(tw, "NAME\tCAPABILITY\tADDR\tSTATUS\tLATENCY");
                for peer in &p.peers {
                    let cap = capability_str(peer.capability());
                    let status = status_str(peer.status());
                    let latency = if peer.latency_ms > 0.0 {
                        format!("{:.1}ms", peer.latency_ms)
                    } else {
                        "—".to_string()
                    };
                    let _ = writeln!(
                        tw,
                        "{}\t{}\t{}\t{}\t{}",
                        peer.name, cap, peer.addr, status, latency
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

fn role_str(role: management::NodeRole) -> &'static str {
    match role {
        management::NodeRole::Entry => "entry",
        management::NodeRole::Exit => "exit",
        management::NodeRole::Unspecified => "unknown",
    }
}

fn capability_str(cap: management::NodeCapability) -> &'static str {
    match cap {
        management::NodeCapability::Exit => "exit",
        management::NodeCapability::Relay => "relay",
        management::NodeCapability::Unspecified => "unknown",
    }
}

fn status_str(status: management::PeerStatus) -> &'static str {
    match status {
        management::PeerStatus::Connected => "connected",
        management::PeerStatus::Disconnected => "disconnected",
        management::PeerStatus::Unspecified => "unknown",
    }
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
