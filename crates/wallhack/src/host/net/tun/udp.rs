use protobuf::SocketSet;
use smoltcp::{phy::ChecksumCapabilities, wire::IpAddress};

use super::tcp::BuildOutcome;

#[derive(Debug, Clone)]
pub struct UdpFlow;

pub type UdpFlowHashKey = SocketSet;

pub fn emit_udp_segment(
	socket_set: SocketSet,
	payload: &[u8],
	packet_buf: &mut [u8],
) -> Option<BuildOutcome> {
	let (src_ip, dst_ip, src_port, dst_port) = match socket_set {
		SocketSet::Ipv4((s, d)) => (
			IpAddress::Ipv4(*s.ip()),
			IpAddress::Ipv4(*d.ip()),
			s.port(),
			d.port(),
		),
		SocketSet::Ipv6((s, d)) => (
			IpAddress::Ipv6(*s.ip()),
			IpAddress::Ipv6(*d.ip()),
			s.port(),
			d.port(),
		),
	};

	let udp_repr = smoltcp::wire::UdpRepr { src_port, dst_port };

	let udp_segment_len = udp_repr.header_len();

	if packet_buf.len() < udp_segment_len {
		tracing::error!(
			"Packet buffer too small for UDP segment. Needed: {}, Available: {}",
			udp_segment_len,
			packet_buf.len()
		);
		return None;
	}

	let mut udp_packet =
		smoltcp::wire::UdpPacket::new_unchecked(&mut packet_buf[..udp_segment_len]);
	udp_repr.emit(
		&mut udp_packet,
		&src_ip,
		&dst_ip,
		payload.len(),
		|buf| buf.copy_from_slice(payload),
		&ChecksumCapabilities::default(),
	);

	Some(BuildOutcome::Udp(udp_segment_len))
}
