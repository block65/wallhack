use std::{
	net::{Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs},
	ops::Deref,
	str::FromStr,
};

use wallhack_core::client::config::ipv6_supported;

use crate::NodeError;

/// Extension trait for `SocketAddr` providing address-family utilities.
pub(crate) trait SocketAddrExt {
	/// Returns the appropriate wildcard bind address for this address family.
	///
	/// Use this when creating a client socket that needs to connect to or bind
	/// in the same address family as `self`.
	fn bind_addr(&self) -> SocketAddr;
}

impl SocketAddrExt for SocketAddr {
	fn bind_addr(&self) -> SocketAddr {
		if self.is_ipv4() {
			(Ipv4Addr::UNSPECIFIED, 0).into()
		} else {
			(Ipv6Addr::UNSPECIFIED, 0).into()
		}
	}
}

pub struct ListenAddr(SocketAddr);

impl FromStr for ListenAddr {
	type Err = NodeError;

	fn from_str(s: &str) -> Result<Self, Self::Err> {
		let normalized = if let Some(port) = s.strip_prefix(':') {
			if ipv6_supported() {
				format!("[::]:{port}")
			} else {
				format!("0.0.0.0:{port}")
			}
		} else {
			s.to_string()
		};

		let addr = normalized
			.to_socket_addrs()
			.map_err(|e| NodeError::Config(format!("invalid address {normalized}: {e}")))?
			.next()
			.ok_or_else(|| NodeError::NoAddresses(normalized))?;

		Ok(ListenAddr(addr))
	}
}

impl Deref for ListenAddr {
	type Target = SocketAddr;

	fn deref(&self) -> &Self::Target {
		&self.0
	}
}

impl From<ListenAddr> for SocketAddr {
	fn from(addr: ListenAddr) -> Self {
		addr.0
	}
}
