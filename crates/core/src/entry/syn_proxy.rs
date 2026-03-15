//! SYN probe: check exit-node reachability before committing to a TCP handshake.
//!
//! When a SYN arrives for an unknown (host, port), the poll loop holds it and
//! sends it here for probing. We open a bi-stream to the exit node, send a
//! `TcpStreamHeader`, and read the `TcpStreamStatus` to determine reachability.

use std::sync::Arc;

use smoltcp::wire::IpVersion;
use wallhack_transport::{BiStream as _, ErasedTransport};
use wallhack_wire::data::{ResponseStatus, SessionProtocol, TcpStreamHeader, TcpStreamStatus};

use crate::transport::protocol::{
    AsyncProtoRead as _, AsyncProtoWrite as _, TCP_STREAM_HEADER_MTU,
};

/// Result of probing a TCP target via the exit node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeResult {
    /// Exit connected successfully — port is listening.
    Open,
    /// Exit got `ECONNREFUSED` — host alive, port not listening.
    Closed,
    /// Exit got `EHOSTUNREACH` / `ENETUNREACH` — host doesn't exist.
    Unreachable,
    /// Transport error or timeout — tunnel issue, don't cache.
    TransportError,
}

/// Probe the exit node to check if a TCP target is reachable.
///
/// Opens a bi-stream, sends `TcpStreamHeader`, reads `TcpStreamStatus`, then closes.
pub async fn probe_tcp_target(
    transport: &Arc<dyn ErasedTransport>,
    target_addr: &str,
) -> ProbeResult {
    let result = probe_inner(transport, target_addr).await;
    match result {
        Ok(status) => match status {
            ResponseStatus::Success => ProbeResult::Open,
            ResponseStatus::ConnectionRefused => ProbeResult::Closed,
            _ => {
                tracing::debug!(target_addr, ?status, "SYN probe: host/network unreachable");
                ProbeResult::Unreachable
            }
        },
        Err(e) => {
            tracing::debug!(target_addr, error = %e, "SYN probe: transport error");
            ProbeResult::TransportError
        }
    }
}

async fn probe_inner(
    transport: &Arc<dyn ErasedTransport>,
    target_addr: &str,
) -> Result<ResponseStatus, Box<dyn std::error::Error + Send + Sync>> {
    let mut stream = transport.open_bi_erased().await?;

    let header = TcpStreamHeader {
        target_addr: target_addr.to_string(),
        source_addr: String::new(), // Not needed for probe
        protocol: SessionProtocol::Tcp as i32,
    };
    stream.write_proto(&header).await?;

    // Signal we're done writing so the exit doesn't wait for more data.
    stream.finish().await?;

    let status: TcpStreamStatus = stream.read_proto(TCP_STREAM_HEADER_MTU).await?;

    Ok(status.status())
}

/// Extract destination IP and port from a raw IP packet containing a TCP SYN.
///
/// Returns `"ip:port"` formatted string suitable for `TcpStreamHeader.target_addr`.
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
