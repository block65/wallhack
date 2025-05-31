use std::fmt::Display;

use icmp_send_instruction::IcmpMessage;

use crate::helpers::{ConversionError, vec_to_sized_array};

include!(concat!(env!("OUT_DIR"), "/tunnel.command.v2.rs"));

impl Display for IpV4Address {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(
			f,
			"{}.{}.{}.{}",
			self.ip[0], self.ip[1], self.ip[2], self.ip[3]
		)
	}
}

impl Display for IpV6Address {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(
			f,
			"[{}:{}:{}:{}:{}:{}:{}:{}]",
			self.ip[0],
			self.ip[1],
			self.ip[2],
			self.ip[3],
			self.ip[4],
			self.ip[5],
			self.ip[6],
			self.ip[7]
		)
	}
}

impl Display for SocketV4Address {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		if let Some(ip) = &self.ip {
			write!(f, "{}:{}", ip, self.port)
		} else {
			write!(f, "<none>:{}", self.port)
		}
	}
}

impl Display for SocketV6Address {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		if let Some(ip) = &self.ip {
			write!(f, "[{}]:{}", ip, self.port)
		} else {
			write!(f, "<none>:{}", self.port)
		}
	}
}

impl Display for SocketAddressPair {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		let Some(pair) = self.pair.as_ref() else {
			return write!(f, "<none>");
		};

		match pair {
			socket_address_pair::Pair::Ipv4(pair) => {
				if let (Some(src), Some(dst)) = (&pair.src_addr, &pair.dst_addr) {
					write!(f, "{src}#{dst}")
				} else {
					write!(f, "<none>")
				}
			}
			socket_address_pair::Pair::Ipv6(socket_v6_address_pair) => {
				if let (Some(src), Some(dst)) = (
					&socket_v6_address_pair.src_addr,
					&socket_v6_address_pair.dst_addr,
				) {
					write!(f, "[{src}]#[{dst}]")
				} else {
					write!(f, "<none>")
				}
			}
		}
	}
}

impl From<(SocketV4Address, SocketV4Address)> for socket_address_pair::Pair {
	fn from(addresses: (SocketV4Address, SocketV4Address)) -> Self {
		socket_address_pair::Pair::Ipv4(addresses.into())
	}
}

impl From<(SocketV4Address, SocketV4Address)> for SocketV4AddressPair {
	fn from(addresses: (SocketV4Address, SocketV4Address)) -> Self {
		SocketV4AddressPair {
			src_addr: Some(addresses.0),
			dst_addr: Some(addresses.1),
		}
	}
}

impl From<(SocketV4Address, SocketV4Address)> for SocketAddressPair {
	fn from(addresses: (SocketV4Address, SocketV4Address)) -> Self {
		SocketAddressPair {
			pair: Some(addresses.into()),
		}
	}
}

impl From<(SocketV6Address, SocketV6Address)> for socket_address_pair::Pair {
	fn from(addresses: (SocketV6Address, SocketV6Address)) -> Self {
		socket_address_pair::Pair::Ipv6(addresses.into())
	}
}

impl From<(SocketV6Address, SocketV6Address)> for SocketV6AddressPair {
	fn from(addresses: (SocketV6Address, SocketV6Address)) -> Self {
		SocketV6AddressPair {
			src_addr: Some(addresses.0),
			dst_addr: Some(addresses.1),
		}
	}
}

impl From<(SocketV6Address, SocketV6Address)> for SocketAddressPair {
	fn from(addresses: (SocketV6Address, SocketV6Address)) -> Self {
		SocketAddressPair {
			pair: Some(addresses.into()),
		}
	}
}

// std::net
// std::net
// std::net
// std::net
impl From<IpV4Address> for std::net::Ipv4Addr {
	fn from(addr: IpV4Address) -> Self {
		vec_to_sized_array::<4>(&addr.ip).into()
	}
}

impl From<IpV6Address> for std::net::Ipv6Addr {
	fn from(addr: IpV6Address) -> Self {
		vec_to_sized_array::<16>(&addr.ip).into()
	}
}

impl From<ip_address::IpAddress> for std::net::IpAddr {
	fn from(addr: ip_address::IpAddress) -> Self {
		match addr {
			ip_address::IpAddress::Ipv4(addr) => {
				std::net::IpAddr::V4(vec_to_sized_array::<4>(&addr.ip).into())
			}
			ip_address::IpAddress::Ipv6(addr) => {
				std::net::IpAddr::V6(vec_to_sized_array::<16>(&addr.ip).into())
			}
		}
	}
}

impl TryFrom<SocketV4Address> for std::net::SocketAddrV4 {
	type Error = ConversionError;

	fn try_from(addr: SocketV4Address) -> Result<Self, Self::Error> {
		let ip_v4_proto = addr.ip.ok_or(Self::Error::MissingIpAddress)?;
		let ip: std::net::Ipv4Addr = ip_v4_proto.into();
		#[allow(clippy::cast_possible_truncation)]
		Ok(std::net::SocketAddrV4::new(ip, addr.port as u16))
	}
}

impl TryFrom<SocketV6Address> for std::net::SocketAddrV6 {
	type Error = ConversionError;

	fn try_from(addr: SocketV6Address) -> Result<Self, Self::Error> {
		let ip_v6_proto = addr.ip.ok_or(Self::Error::MissingIpAddress)?;
		let ip: std::net::Ipv6Addr = ip_v6_proto.into();
		#[allow(clippy::cast_possible_truncation)]
		Ok(std::net::SocketAddrV6::new(
			ip,
			addr.port as u16,
			addr.flowinfo,
			addr.scope_id,
		))
	}
}

impl From<std::net::Ipv4Addr> for IpV4Address {
	fn from(val: std::net::Ipv4Addr) -> Self {
		IpV4Address {
			ip: val.as_octets().to_vec(),
		}
	}
}

impl From<std::net::Ipv6Addr> for IpV6Address {
	fn from(val: std::net::Ipv6Addr) -> Self {
		IpV6Address {
			ip: val.as_octets().to_vec(),
		}
	}
}

// impl From<(std::net::Ipv4Addr, std::net::Ipv4Addr)> for IpAddressPair {
// 	fn from(addresses: (std::net::Ipv4Addr, std::net::Ipv4Addr)) -> Self {
// 		IpAddressPair {
// 			pair: Some(ip_address_pair::Pair::Ipv4(IpV4AddressPair {
// 				src_ip: Some(addresses.0.into()),
// 				dst_ip: Some(addresses.1.into()),
// 			})),
// 		}
// 	}
// }

// impl From<(std::net::Ipv6Addr, std::net::Ipv6Addr)> for IpAddressPair {
// 	fn from(addresses: (std::net::Ipv6Addr, std::net::Ipv6Addr)) -> Self {
// 		IpAddressPair {
// 			pair: Some(ip_address_pair::Pair::Ipv6(IpV6AddressPair {
// 				src_ip: Some(addresses.0.into()),
// 				dst_ip: Some(addresses.1.into()),
// 			})),
// 		}
// 	}
// }

impl From<IpV4Address> for std::net::IpAddr {
	fn from(addr: IpV4Address) -> Self {
		std::net::IpAddr::V4(addr.into())
	}
}

impl From<IpV6Address> for std::net::IpAddr {
	fn from(addr: IpV6Address) -> Self {
		std::net::IpAddr::V6(addr.into())
	}
}

/* impl TryFrom<IpAddressPair> for (std::net::IpAddr, std::net::IpAddr) {
	type Error = ConversionError;

	fn try_from(pair: IpAddressPair) -> Result<Self, Self::Error> {
		let Some(pair) = pair.pair else {
			return Err(Self::Error::MissingIpAddressPair);
		};

		match pair {
			ip_address_pair::Pair::Ipv4(pair) => {
				let src_ip = pair.src_ip.ok_or(Self::Error::MissingIpAddress)?;
				let dst_ip = pair.dst_ip.ok_or(Self::Error::MissingIpAddress)?;
				Ok((src_ip.into(), dst_ip.into()))
			}
			ip_address_pair::Pair::Ipv6(pair) => {
				let src_ip = pair.src_ip.ok_or(Self::Error::MissingIpAddress)?;
				let dst_ip = pair.dst_ip.ok_or(Self::Error::MissingIpAddress)?;
				Ok((src_ip.into(), dst_ip.into()))
			}
		}
	}
} */

// #[prost(message, tag = "2")]
// TcpResponse(super::TcpResponse),
// #[prost(message, tag = "3")]
// UdpResponse(super::UdpResponse),
// #[prost(message, tag = "4")]
// IcmpResponse(super::IcmpResponse),
// #[prost(message, tag = "5")]
// RuntimeError(super::RuntimeErrorResponse),

// /// Nested message and enum types in `TcpResponse`.
// pub mod tcp_response {
//     #[derive(Clone, PartialEq, ::prost::Oneof)]
//     pub enum Response {
//         /// TcpConnectOkResponse connect_ok = 4;
//         #[prost(message, tag = "5")]
//         Connected(super::TcpConnectedResponse),
//         #[prost(message, tag = "6")]
//         SendOk(super::TcpSendOkResponse),
//         #[prost(message, tag = "7")]
//         DataRecv(super::TcpDataRecvResponse),
//         #[prost(message, tag = "8")]
//         ConnectionClosed(super::TcpConnectionClosedResponse),
//         #[prost(message, tag = "9")]
//         ConnectionRefused(super::TcpConnectionRefusedResponse),
//         #[prost(message, tag = "10")]
//         ListenOk(super::TcpListenerOkResponse),
//         #[prost(message, tag = "11")]
//         Listening(super::TcpListenerListeningResponse),
//         #[prost(message, tag = "12")]
//         ListenerConnect(super::TcpListenerConnectResponse),
//         #[prost(message, tag = "13")]
//         ListenerClosed(super::TcpListenerClosedResponse),
//     }
// }

impl Display for TcpDataRecvResponse {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "data:{:#}bytes", self.data.len())
	}
}

impl Display for TcpConnectedResponse {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "connected")
	}
}

impl Display for TcpResponse {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match &self.response {
			Some(tcp_response::Response::DataRecv(res)) => {
				write!(f, "recv:data:{:#}bytes", res.data.len())
			}
			Some(tcp_response::Response::Connected(_)) => {
				write!(f, "connected")
			}
			Some(tcp_response::Response::SendOk(_)) => {
				write!(f, "send:ok")
			}
			Some(tcp_response::Response::ConnectionClosed(_)) => {
				write!(f, "closed")
			}
			Some(tcp_response::Response::ConnectionRefused(_)) => {
				write!(f, "refused")
			}
			Some(tcp_response::Response::Listening(_)) => {
				write!(f, "listening")
			}
			Some(tcp_response::Response::ListenerConnect(_)) => {
				write!(f, "listen:connect")
			}
			Some(tcp_response::Response::ListenerClosed(_)) => {
				write!(f, "listen:closed")
			}
			None => {
				write!(f, "none")
			}
		}
	}
}

impl Display for IcmpResponse {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match &self.response {
			Some(icmp_response::Response::DataRecv(res)) => {
				write!(f, "data:recv:{:#}bytes", res.data.len())
			}
			None => {
				write!(f, "none")
			}
		}
	}
}

impl Display for UdpResponse {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match &self.response {
			Some(udp_response::Response::DataRecv(res)) => {
				write!(f, "data:recv:{:#}bytes", res.data.len())
			}
			None => {
				write!(f, "none")
			}
		}
	}
}

impl Display for RuntimeErrorResponse {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "error:reason:{}", self.reason)
	}
}

impl Display for AgentResponse {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match &self.response {
			Some(agent_response::Response::IcmpResponse(res)) => {
				write!(f, "icmp:response:{res}")
			}
			Some(agent_response::Response::TcpResponse(res)) => {
				write!(f, "tcp:response:{res}")
			}
			Some(agent_response::Response::UdpResponse(res)) => {
				write!(f, "udp:response:{res}")
			}
			Some(agent_response::Response::RuntimeError(res)) => {
				write!(f, "error:{res}")
			}
			None => {
				write!(f, "none")
			}
		}
	}
}

impl Display for IcmpMessage {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self {
			IcmpMessage::IcmpEchoRequest(req) => {
				write!(
					f,
					"echo_request:ident:{},seq_no:{},data:{:#}bytes",
					req.ident,
					req.seq_no,
					req.data.len()
				)
			}
			IcmpMessage::RawPacket(req) => {
				write!(f, "raw:data:{:#}bytes", req.data.len())
			}
		}
	}
}

impl Display for IcmpSendInstruction {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match &self.icmp_message {
			Some(msg) => {
				write!(f, "{msg}")
			}
			None => {
				write!(f, "<none>")
			}
		}
	}
}

impl Display for TcpConnectInstruction {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match &self.pair {
			Some(pair) => write!(f, "pair:{pair}"),
			None => write!(f, "<none>"),
		}
	}
}

impl Display for TcpSendInstruction {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "data:{:#}bytes", self.data.len())
	}
}

impl Display for TcpCloseInstruction {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match &self.pair {
			Some(pair) => write!(f, "pair:{pair}"),
			None => write!(f, "seq_no:<none>"),
		}
	}
}

impl Display for TcpListenInstruction {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match &self.pair {
			Some(pair) => write!(f, "{pair}"),
			None => write!(f, "<none>"),
		}
	}
}

impl Display for TcpListenCloseInstruction {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "close")
	}
}

impl Display for UdpSendInstruction {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "data:{:#}bytes", self.data.len())
	}
}

impl Display for HostInstruction {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match &self.instruction {
			Some(host_instruction::Instruction::IcmpSend(req)) => {
				write!(f, "icmp:request:{req}")
			}
			Some(host_instruction::Instruction::TcpClose(req)) => {
				write!(f, "tcp:close:{req}")
			}
			Some(host_instruction::Instruction::TcpConnect(req)) => {
				write!(f, "tcp:connect:{req}")
			}
			Some(host_instruction::Instruction::TcpSend(req)) => {
				write!(f, "tcp:send:{req}")
			}
			Some(host_instruction::Instruction::TcpListen(req)) => {
				write!(f, "tcp:listen:{req}")
			}
			Some(host_instruction::Instruction::TcpListenClose(req)) => {
				write!(f, "tcp:listen:close:{req}")
			}
			Some(host_instruction::Instruction::UdpSend(req)) => {
				write!(f, "udp:send:{req}")
			}
			None => {
				write!(f, "none")
			}
		}
	}
}

impl Display for TunnelMessage {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match &self.message {
			Some(tunnel_message::Message::HostInstruction(instruction)) => {
				write!(f, "{instruction}")
			}
			Some(tunnel_message::Message::AgentResponse(response)) => {
				write!(f, "{response}")
			}
			None => {
				write!(f, "<none>")
			}
		}
	}
}

impl Display for tunnel_message::Message {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self {
			tunnel_message::Message::HostInstruction(instruction) => {
				write!(f, "instruction:{instruction}")
			}
			tunnel_message::Message::AgentResponse(response) => {
				write!(f, "response:{response}")
			}
		}
	}
}

impl From<HostInstruction> for TunnelMessage {
	fn from(instruction: HostInstruction) -> Self {
		TunnelMessage {
			message: Some(instruction.into()),
		}
	}
}

impl From<AgentResponse> for TunnelMessage {
	fn from(response: AgentResponse) -> Self {
		TunnelMessage {
			message: Some(response.into()),
		}
	}
}

impl From<HostInstruction> for tunnel_message::Message {
	fn from(instruction: HostInstruction) -> Self {
		tunnel_message::Message::HostInstruction(instruction)
	}
}

impl From<AgentResponse> for tunnel_message::Message {
	fn from(response: AgentResponse) -> Self {
		tunnel_message::Message::AgentResponse(response)
	}
}

impl From<host_instruction::Instruction> for HostInstruction {
	fn from(instruction: host_instruction::Instruction) -> Self {
		HostInstruction {
			instruction: Some(instruction),
		}
	}
}

impl From<host_instruction::Instruction> for tunnel_message::Message {
	fn from(instruction: host_instruction::Instruction) -> Self {
		tunnel_message::Message::HostInstruction(instruction.into())
	}
}
