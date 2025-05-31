use protobuf::SocketSet;
use smoltcp::{
	phy::ChecksumCapabilities,
	wire::{Icmpv4Message, Icmpv4Packet, Icmpv4Repr},
};

use super::tcp::BuildOutcome;

#[derive(Debug, Clone)]
pub struct IcmpFlow {
	pub echo_ident: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct IcmpFlowHashKey {
	pub pair: SocketSet,
	pub echo_ident: u16,
}

pub fn emit_icmp_segment(
	flow: &IcmpFlow,
	socket_set: SocketSet,
	icmp_data: &[u8],
	buf: &mut [u8],
) -> Option<BuildOutcome> {
	match socket_set {
		SocketSet::Ipv4(_) => {
			// the caller should check, we assume its valid by now
			let icmp_pkt = Icmpv4Packet::new_unchecked(icmp_data);

			let icmp_repr = match icmp_pkt.msg_type() {
				// Icmpv4Message::DstUnreachable => todo!(),
				// Icmpv4Message::Redirect => todo!(),
				// Icmpv4Message::EchoRequest => todo!(),
				// Icmpv4Message::RouterAdvert => todo!(),
				// Icmpv4Message::RouterSolicit => todo!(),
				// Icmpv4Message::TimeExceeded => todo!(),
				// Icmpv4Message::ParamProblem => todo!(),
				// Icmpv4Message::Timestamp => todo!(),
				// Icmpv4Message::TimestampReply => todo!(),
				// Icmpv4Message::Unknown(_) => todo!(),
				Icmpv4Message::EchoReply => Icmpv4Repr::EchoReply {
					#[allow(clippy::cast_possible_truncation)]
					ident: flow.echo_ident as u16, // icmp_pkt.echo_ident(), // WARN: this will be the one from the OS agent side
					seq_no: icmp_pkt.echo_seq_no(),
					data: icmp_pkt.data(),
				},
				Icmpv4Message::Unknown(_) => {
					tracing::warn!("unknown ICMPv4 message type: {:?}", icmp_pkt.msg_type());
					return None;
				}
				_ => todo!(),
			};

			let icmp_buffer_len = icmp_repr.buffer_len();

			let mut icmp_pkt_out = Icmpv4Packet::new_unchecked(&mut buf[..icmp_buffer_len]);
			icmp_repr.emit(&mut icmp_pkt_out, &ChecksumCapabilities::default());

			Some(BuildOutcome::Icmp(icmp_buffer_len))
		}
		SocketSet::Ipv6(_) => {
			tracing::warn!("ICMPv6 not yet implemented");
			None
		}
	}
}
