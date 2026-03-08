//! Convert protobuf management responses to LLM-readable plain text.

use std::fmt::Write;

use wallhack_wire::management::{ManagementResponse, management_response};

/// Format a management response as human-readable text for MCP tool output.
pub fn format_response(resp: &ManagementResponse) -> Result<String, String> {
    match &resp.response {
        Some(management_response::Response::Status(s)) => {
            let role = s.role();
            let mut out = String::new();
            let _ = writeln!(out, "role: {role:?}");
            if !s.peer_addr.is_empty() {
                let _ = writeln!(out, "peer addr: {}", s.peer_addr);
            }
            if !s.listen_addr.is_empty() {
                let _ = writeln!(out, "listen addr: {}", s.listen_addr);
            }
            let _ = writeln!(out, "version: {}", s.version);
            let _ = writeln!(out, "uptime: {}", format_uptime(s.uptime_ms));
            let _ = writeln!(
                out,
                "capabilities: tun={} listen={} connect={}",
                s.tun_capable, s.listening, s.connecting,
            );
            Ok(out)
        }
        Some(management_response::Response::Ping(p)) => {
            let role = p.node_role();
            Ok(format!(
                "pong — role: {role:?}, version: {}, uptime: {}",
                p.version,
                format_uptime(p.uptime_ms),
            ))
        }
        Some(management_response::Response::Stats(s)) => Ok(format!(
            "bytes in: {}\nbytes out: {}\npackets in: {}\npackets out: {}\n\
             connections: {}\nflows: {}\ndropped: {}",
            s.bytes_in,
            s.bytes_out,
            s.packets_in,
            s.packets_out,
            s.active_connections,
            s.active_flows,
            s.packets_dropped,
        )),
        Some(management_response::Response::Peers(p)) => {
            if p.peers.is_empty() {
                return Ok("No connected peers.".to_string());
            }
            let mut out = String::new();
            for peer in &p.peers {
                let status = peer.status();
                let role = peer.role();
                let latency = if peer.latency_ms > 0.0 {
                    format!("{:.1}ms", peer.latency_ms)
                } else {
                    "—".to_string()
                };
                let _ = writeln!(
                    out,
                    "{} addr={} role={role:?} status={status:?} latency={latency} \
                     tun={} listen={} connect={}",
                    peer.name, peer.addr, peer.tun_capable, peer.listening, peer.connecting,
                );
            }
            Ok(out)
        }
        Some(management_response::Response::Routes(r)) => {
            if r.routes.is_empty() {
                return Ok("No routes configured.".to_string());
            }
            let mut out = String::new();
            for route in &r.routes {
                let _ = writeln!(out, "{} → {}", route.cidr, route.peer);
            }
            Ok(out)
        }
        Some(management_response::Response::Connect(c)) => {
            Ok(format!("Connected to {} ({})", c.peer_addr, c.protocol))
        }
        Some(management_response::Response::Listen(l)) => {
            let mut out = format!("Listening on {} ({})", l.listen_addr, l.protocol);
            if !l.fingerprint.is_empty() {
                let _ = write!(out, "\nFingerprint: {}", l.fingerprint);
            }
            Ok(out)
        }
        Some(management_response::Response::Ok(_)) => Ok("OK".to_string()),
        Some(management_response::Response::Error(e)) => Err(e.message.clone()),
        None => Err("empty response from daemon".to_string()),
    }
}

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
