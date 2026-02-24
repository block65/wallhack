use wallhack_wire::SocketSet;

#[derive(Hash, Eq, PartialEq, Debug, Clone)]
pub enum SessionKey {
    Tcp(SocketSet),
    Udp(SocketSet),
    Icmp((SocketSet, u16)), // (SocketSet, ident)
}

// impl Display for SessionKey {
// 	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
// 		write!(f, "{:?} {:?}", self.proto, self.sockets)
// 	}
// }

// impl SessionKey {
// 	#[must_use]
// 	pub fn new(proto: Protocol, sockets: SocketSet) -> Self {
// 		SessionKey { proto, sockets }
// 	}
// }
