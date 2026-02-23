//! Output formatting for CLI responses.

use wallhack_wire::management::{self, ManagementResponse, management_response};

use crate::ipc::IpcError;

/// Print a management response to stdout.
///
/// # Errors
///
/// Returns an error if the response contains an error from the daemon.
pub fn print_response(resp: &ManagementResponse) -> Result<(), CtlError> {
	match &resp.response {
		Some(management_response::Response::Ping(p)) => {
			let role = role_str(p.node_role());
			println!(
				"pong  role={role}  uptime={}ms  version={}",
				p.uptime_ms, p.version
			);
		}
		Some(management_response::Response::Status(s)) => {
			let role = role_str(s.role());
			let capability = capability_str(s.capability());
			let connected = if s.connected { "yes" } else { "no" };
			println!("role:        {role}");
			println!("connected:   {connected}");
			if !s.peer_addr.is_empty() {
				println!("peer addr:   {}", s.peer_addr);
			}
			println!("capability:  {capability}");
			if !s.listen_addr.is_empty() {
				println!("listen addr: {}", s.listen_addr);
			}
			println!("version:     {}", s.version);
			println!("uptime:      {}ms", s.uptime_ms);
		}
		Some(management_response::Response::Stats(s)) => {
			println!("bytes in:     {}", s.bytes_in);
			println!("bytes out:    {}", s.bytes_out);
			println!("packets in:   {}", s.packets_in);
			println!("packets out:  {}", s.packets_out);
			println!("connections:  {}", s.active_connections);
			println!("flows:        {}", s.active_flows);
			println!("dropped:      {}", s.packets_dropped);
		}
		Some(management_response::Response::Peers(p)) => {
			if p.peers.is_empty() {
				println!("No connected peers.");
			} else {
				println!(
					"{:<20} {:<12} {:<22} {:<10} LATENCY",
					"NAME", "CAPABILITY", "ADDR", "STATUS"
				);
				for peer in &p.peers {
					let cap = capability_str(peer.capability());
					let status = status_str(peer.status());
					let latency = if peer.latency_ms > 0.0 {
						format!("{:.1}ms", peer.latency_ms)
					} else {
						"—".to_string()
					};
					println!(
						"{:<20} {:<12} {:<22} {:<10} {latency}",
						peer.name, cap, peer.addr, status
					);
				}
			}
		}
		Some(management_response::Response::Routes(r)) => {
			if r.routes.is_empty() {
				println!("No routes configured.");
			} else {
				println!("{:<20} {:<20}", "CIDR", "PEER");
				for route in &r.routes {
					println!("{:<20} {:<20}", route.cidr, route.peer);
				}
			}
		}
		Some(management_response::Response::Ok(_)) => {
			println!("OK");
		}
		Some(management_response::Response::Error(e)) => {
			return Err(CtlError::Daemon(e.message.clone()));
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
