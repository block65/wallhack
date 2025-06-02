use protobuf::SocketSet;
use smoltcp::wire::{IcmpRepr, Icmpv4Message, Icmpv4Packet, Icmpv4Repr};

#[derive(Debug, Clone)]
pub struct IcmpFlow {
	pub ident: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct IcmpFlowHashKey {
	pub pair: SocketSet,
	pub ident: u16,
}

pub fn icmp_repr<'a>(
	flow: &IcmpFlow,
	socket_set: SocketSet,
	icmp_data: &'a Vec<u8>,
) -> Option<IcmpRepr<'a>> {
	match socket_set {
		SocketSet::Ipv4(_) => {
			// the caller should check, we assume its valid by now
			let parsed = Icmpv4Packet::new_unchecked(icmp_data);

			let icmp_repr = match parsed.msg_type() {
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
					ident: flow.ident as u16,
					seq_no: parsed.echo_seq_no(),
					data: parsed.data(),
				},
				Icmpv4Message::Unknown(_) => {
					tracing::warn!("unknown ICMPv4 message type: {:?}", parsed.msg_type());
					return None;
				}
				_ => todo!(),
			};
			Some(IcmpRepr::Ipv4(icmp_repr))
		}
		SocketSet::Ipv6(_) => {
			// let parsed = Ipv6Packet::new_unchecked(icmp_data);

			// let icmp_repr = match parsed.flow_label() {
			// 	// Icmpv6Message::DstUnreachable => todo!(),
			// 	// Icmpv6Message::Redirect => todo!(),
			// 	// Icmpv6Message::EchoRequest => todo!(),
			// 	// Icmpv6Message::RouterAdvert => todo!(),
			// 	// Icmpv6Message::RouterSolicit => todo!(),
			// 	// Icmpv6Message::TimeExceeded => todo!(),
			// 	// Icmpv6Message::ParamProblem => todo!(),
			// 	// Icmpv6Message::Timestamp => todo!(),
			// 	// Icmpv6Message::TimestampReply => todo!(),
			// 	// Icmpv6Message::Unknown(_) => todo!(),
			// 	Icmpv6Message::EchoReply => Icmpv6Repr::EchoReply {
			// 		ident: flow.ident as u16,
			// 		seq_no: parsed.(),
			// 		data: parsed.data(),
			// 	},
			// 	Icmpv6Message::Unknown(_) => {
			// 		tracing::warn!("unknown ICMPv6 message type: {:?}", parsed.msg_type());
			// 		return None;
			// 	}
			// 	_ => todo!(),
			// };
			// Some(IcmpRepr::Ipv6(icmp_repr))
			None
		}
	}
}
