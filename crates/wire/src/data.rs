use std::fmt::Display;

use icmp_send_instruction::IcmpMessage;

use crate::helpers::{ConversionError, vec_to_sized_array};

// Suppress clippy warnings from auto-generated prost code
#[allow(clippy::doc_markdown, clippy::must_use_candidate)]
mod generated {
    include!(concat!(env!("OUT_DIR"), "/wallhack.data.rs"));
}
pub use generated::*;

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

impl Display for TcpDataRecvResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "data:{:#}bytes", self.data.len())?;
        if self.fin {
            write!(f, ",fin")?;
        }
        Ok(())
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
                write!(f, "recv:data:{:#}bytes", res.data.len())?;
                if res.fin {
                    write!(f, ",fin")?;
                }
                Ok(())
            }
            Some(tcp_response::Response::Connected(_)) => {
                write!(f, "connected")
            }
            Some(tcp_response::Response::Ok(_)) => {
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

impl Display for ExitNodeResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.response {
            Some(exit_node_response::Response::IcmpResponse(res)) => {
                write!(f, "icmp:response:{res}")
            }
            Some(exit_node_response::Response::TcpResponse(res)) => {
                write!(f, "tcp:response:{res}")
            }
            Some(exit_node_response::Response::UdpResponse(res)) => {
                write!(f, "udp:response:{res}")
            }
            Some(exit_node_response::Response::RuntimeError(res)) => {
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
        write!(f, "data:{:#}bytes", self.data.len())?;
        if self.fin {
            write!(f, ",fin")?;
        }
        Ok(())
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

impl Display for EntryNodeInstruction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.instruction {
            Some(entry_node_instruction::Instruction::IcmpSend(req)) => {
                write!(f, "icmp:request:{req}")
            }
            Some(entry_node_instruction::Instruction::TcpClose(req)) => {
                write!(f, "tcp:close:{req}")
            }
            Some(entry_node_instruction::Instruction::TcpConnect(req)) => {
                write!(f, "tcp:connect:{req}")
            }
            Some(entry_node_instruction::Instruction::TcpSend(req)) => {
                write!(f, "tcp:send:{req}")
            }
            Some(entry_node_instruction::Instruction::TcpListen(req)) => {
                write!(f, "tcp:listen:{req}")
            }
            Some(entry_node_instruction::Instruction::TcpListenClose(req)) => {
                write!(f, "tcp:listen:close:{req}")
            }
            Some(entry_node_instruction::Instruction::UdpSend(req)) => {
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
            Some(tunnel_message::Message::EntryNodeInstruction(instruction)) => {
                write!(f, "{instruction}")
            }
            Some(tunnel_message::Message::ExitNodeResponse(response)) => {
                write!(f, "{response}")
            }
            Some(tunnel_message::Message::RawPacket(raw_packet)) => {
                write!(f, "{:02x?}", raw_packet.data.len())
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
            tunnel_message::Message::EntryNodeInstruction(instruction) => {
                write!(f, "instruction:{instruction}")
            }
            tunnel_message::Message::ExitNodeResponse(response) => {
                write!(f, "response:{response}")
            }
            tunnel_message::Message::RawPacket(raw_packet) => {
                write!(f, "raw:{:02x?}", raw_packet.data.len())
            }
        }
    }
}

impl From<EntryNodeInstruction> for TunnelMessage {
    fn from(instruction: EntryNodeInstruction) -> Self {
        TunnelMessage {
            message: Some(instruction.into()),
        }
    }
}

impl From<ExitNodeResponse> for TunnelMessage {
    fn from(response: ExitNodeResponse) -> Self {
        TunnelMessage {
            message: Some(response.into()),
        }
    }
}

impl From<EntryNodeInstruction> for tunnel_message::Message {
    fn from(instruction: EntryNodeInstruction) -> Self {
        tunnel_message::Message::EntryNodeInstruction(instruction)
    }
}

impl From<ExitNodeResponse> for tunnel_message::Message {
    fn from(response: ExitNodeResponse) -> Self {
        tunnel_message::Message::ExitNodeResponse(response)
    }
}

impl From<entry_node_instruction::Instruction> for EntryNodeInstruction {
    fn from(instruction: entry_node_instruction::Instruction) -> Self {
        EntryNodeInstruction {
            instruction: Some(instruction),
        }
    }
}

impl From<entry_node_instruction::Instruction> for tunnel_message::Message {
    fn from(instruction: entry_node_instruction::Instruction) -> Self {
        tunnel_message::Message::EntryNodeInstruction(instruction.into())
    }
}
