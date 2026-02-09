use smoltcp::wire::{
	Icmpv4DstUnreachable, Icmpv4Packet, Icmpv4Repr, Icmpv6DstUnreachable, Icmpv6Packet, Icmpv6Repr,
	IpAddress, IpProtocol, Ipv4Packet, Ipv4Repr, Ipv6Packet, Ipv6Repr,
};

/// Build a raw ICMP Destination Unreachable IP packet.
///
/// Constructs the full IP packet (outer header + ICMP payload) suitable for
/// injection into a TUN device. The ICMP payload contains the original IP
/// header and the first 8 bytes of the triggering UDP datagram per RFC 792.
///
/// Returns `None` if the address is not IPv4 or IPv6 (shouldn't happen).
#[must_use]
pub fn build_icmp_dest_unreachable(
	reason: IcmpUnreachableReason,
	client_ip: IpAddress,
	target_ip: IpAddress,
	target_port: u16,
	client_port: u16,
	original_payload: &[u8],
) -> Option<Vec<u8>> {
	match (client_ip, target_ip) {
		(IpAddress::Ipv4(client), IpAddress::Ipv4(target)) => Some(build_icmpv4(
			reason,
			client,
			target,
			target_port,
			client_port,
			original_payload,
		)),
		(IpAddress::Ipv6(client), IpAddress::Ipv6(target)) => Some(build_icmpv6(
			reason,
			client,
			target,
			target_port,
			client_port,
			original_payload,
		)),
		_ => None,
	}
}

/// Reason for ICMP unreachable, abstracting over v4/v6.
#[derive(Debug, Clone, Copy)]
pub enum IcmpUnreachableReason {
	Port,
	Host,
	Net,
}

/// First 8 bytes of the UDP header that triggered the error (RFC 792).
fn build_udp_header_bytes(src_port: u16, dst_port: u16, payload_len: usize) -> [u8; 8] {
	let udp_len = u16::try_from(8 + payload_len).unwrap_or(u16::MAX);
	let mut buf = [0u8; 8];
	buf[0..2].copy_from_slice(&src_port.to_be_bytes());
	buf[2..4].copy_from_slice(&dst_port.to_be_bytes());
	buf[4..6].copy_from_slice(&udp_len.to_be_bytes());
	// checksum = 0 (not needed for the ICMP error payload)
	buf
}

fn build_icmpv4(
	reason: IcmpUnreachableReason,
	client: smoltcp::wire::Ipv4Address,
	target: smoltcp::wire::Ipv4Address,
	target_port: u16,
	client_port: u16,
	original_payload: &[u8],
) -> Vec<u8> {
	let icmp_reason = match reason {
		IcmpUnreachableReason::Port => Icmpv4DstUnreachable::PortUnreachable,
		IcmpUnreachableReason::Host => Icmpv4DstUnreachable::HostUnreachable,
		IcmpUnreachableReason::Net => Icmpv4DstUnreachable::NetUnreachable,
	};

	let udp_header = build_udp_header_bytes(client_port, target_port, original_payload.len());

	// The "original" IP header that was in the triggering packet
	let inner_ip = Ipv4Repr {
		src_addr: client,
		dst_addr: target,
		next_header: IpProtocol::Udp,
		payload_len: 8 + original_payload.len(),
		hop_limit: 64,
	};

	let icmp_repr = Icmpv4Repr::DstUnreachable {
		reason: icmp_reason,
		header: inner_ip,
		data: &udp_header,
	};

	// Outer IP header: from the target back to the client
	let icmp_len = icmp_repr.buffer_len();
	let outer_ip = Ipv4Repr {
		src_addr: target,
		dst_addr: client,
		next_header: IpProtocol::Icmp,
		payload_len: icmp_len,
		hop_limit: 64,
	};

	let total_len = outer_ip.buffer_len() + icmp_len;
	let mut buf = vec![0u8; total_len];

	// Emit outer IP header
	let mut ip_packet = Ipv4Packet::new_unchecked(&mut buf);
	outer_ip.emit(
		&mut ip_packet,
		&smoltcp::phy::ChecksumCapabilities::default(),
	);

	// Emit ICMP payload
	let mut icmp_packet = Icmpv4Packet::new_unchecked(&mut buf[outer_ip.buffer_len()..]);
	icmp_repr.emit(
		&mut icmp_packet,
		&smoltcp::phy::ChecksumCapabilities::default(),
	);

	buf
}

fn build_icmpv6(
	reason: IcmpUnreachableReason,
	client: smoltcp::wire::Ipv6Address,
	target: smoltcp::wire::Ipv6Address,
	target_port: u16,
	client_port: u16,
	original_payload: &[u8],
) -> Vec<u8> {
	let icmp_reason = match reason {
		IcmpUnreachableReason::Port => Icmpv6DstUnreachable::PortUnreachable,
		IcmpUnreachableReason::Host => Icmpv6DstUnreachable::AddrUnreachable,
		IcmpUnreachableReason::Net => Icmpv6DstUnreachable::NoRoute,
	};

	let udp_header = build_udp_header_bytes(client_port, target_port, original_payload.len());

	// The "original" IP header that was in the triggering packet
	let inner_ip = Ipv6Repr {
		src_addr: client,
		dst_addr: target,
		next_header: IpProtocol::Udp,
		payload_len: 8 + original_payload.len(),
		hop_limit: 64,
	};

	let icmp_repr = Icmpv6Repr::DstUnreachable {
		reason: icmp_reason,
		header: inner_ip,
		data: &udp_header,
	};

	// Outer IP header: from the target back to the client
	let icmp_len = icmp_repr.buffer_len();
	let outer_ip = Ipv6Repr {
		src_addr: target,
		dst_addr: client,
		next_header: IpProtocol::Icmpv6,
		payload_len: icmp_len,
		hop_limit: 64,
	};

	let total_len = outer_ip.buffer_len() + icmp_len;
	let mut buf = vec![0u8; total_len];

	// Emit outer IPv6 header
	let mut ip_packet = Ipv6Packet::new_unchecked(&mut buf);
	outer_ip.emit(&mut ip_packet);

	// Emit ICMPv6 payload
	let mut icmp_packet = Icmpv6Packet::new_unchecked(&mut buf[outer_ip.buffer_len()..]);
	icmp_repr.emit(
		&target,
		&client,
		&mut icmp_packet,
		&smoltcp::phy::ChecksumCapabilities::default(),
	);

	buf
}
