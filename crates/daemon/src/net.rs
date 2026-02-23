use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs};

use anyhow::{Context, Result};

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

/// Resolves a listen address string to a `SocketAddr`.
///
/// A bare `:port` is expanded to a full wildcard address: `[::]` on kernels
/// with IPv6 support (dual-stack), `0.0.0.0` on IPv4-only kernels. Explicit
/// addresses (IP literals or `hostname:port`) are resolved via DNS.
pub(crate) fn parse_listen_addr(addr: &str) -> Result<SocketAddr> {
	let full_addr = if let Some(port) = addr.strip_prefix(':') {
		// Bare port: probe IPv6 availability and pick the right wildcard.
		if wallhack_core::client::config::ipv6_supported() {
			format!("[::]:{port}")
		} else {
			format!("0.0.0.0:{port}")
		}
	} else {
		addr.to_string()
	};

	full_addr
		.to_socket_addrs()
		.with_context(|| format!("Invalid listen address: {full_addr}"))?
		.next()
		.ok_or_else(|| anyhow::anyhow!("No addresses resolved for: {full_addr}"))
}
