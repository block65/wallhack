//! SYN proxy: probe exit node before committing to a TCP handshake.
//!
//! When a SYN arrives for an unknown port, the poll loop holds it and sends
//! it here for probing. We open a bi-stream to the exit node, send a
//! `SessionInit(TCP)`, and read the `SessionStatus`. If success → mark Open;
//! if refused → mark Closed.

use std::sync::Arc;

use smoltcp::wire::IpVersion;
use wallhack_transport::{BiStream as _, ErasedTransport};
use wallhack_wire::data::{ResponseStatus, SessionInit, SessionProtocol, SessionStatus};

use crate::transport::protocol::{AsyncProtoRead as _, AsyncProtoWrite as _, SESSION_INIT_MTU};

/// Probe the exit node to check if a TCP target is reachable.
///
/// Opens a bi-stream, sends `SessionInit`, reads `SessionStatus`, then closes.
/// Returns `true` if the exit confirmed the connection (open port).
pub async fn probe_tcp_target(transport: &Arc<dyn ErasedTransport>, target_addr: &str) -> bool {
    let result = probe_inner(transport, target_addr).await;
    match result {
        Ok(open) => open,
        Err(e) => {
            tracing::debug!(target_addr, error = %e, "SYN probe failed, treating as closed");
            false
        }
    }
}

async fn probe_inner(
    transport: &Arc<dyn ErasedTransport>,
    target_addr: &str,
) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
    let mut stream = transport.open_bi_erased().await?;

    let init = SessionInit {
        target_addr: target_addr.to_string(),
        source_addr: String::new(), // Not needed for probe
        protocol: SessionProtocol::Tcp as i32,
    };
    stream.write_proto(&init).await?;

    // Signal we're done writing so the exit doesn't wait for more data.
    stream.finish().await?;

    let status: SessionStatus = stream.read_proto(SESSION_INIT_MTU).await?;

    Ok(status.status() == ResponseStatus::Success)
}

/// Extract destination IP and port from a raw IP packet containing a TCP SYN.
///
/// Returns `"ip:port"` formatted string suitable for `SessionInit.target_addr`.
#[must_use]
pub fn parse_syn_target(packet: &[u8]) -> Option<String> {
    let version = IpVersion::of_packet(packet).ok()?;
    match version {
        IpVersion::Ipv4 => parse_ipv4_target(packet),
        IpVersion::Ipv6 => parse_ipv6_target(packet),
    }
}

fn parse_ipv4_target(packet: &[u8]) -> Option<String> {
    if packet.len() < 20 {
        return None;
    }
    let ihl = (packet[0] & 0x0f) as usize * 4;
    if packet.len() < ihl + 4 {
        return None;
    }
    let dst_ip = std::net::Ipv4Addr::new(packet[16], packet[17], packet[18], packet[19]);
    let dst_port = u16::from_be_bytes([packet[ihl + 2], packet[ihl + 3]]);
    Some(format!("{dst_ip}:{dst_port}"))
}

fn parse_ipv6_target(packet: &[u8]) -> Option<String> {
    if packet.len() < 44 {
        return None;
    }
    let octets: [u8; 16] = packet[24..40].try_into().ok()?;
    let dst_ip = std::net::Ipv6Addr::from(octets);
    let dst_port = u16::from_be_bytes([packet[42], packet[43]]);
    Some(format!("[{dst_ip}]:{dst_port}"))
}
