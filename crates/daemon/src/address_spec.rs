//! Docker-style address spec parsing.
//!
//! Parses addresses in the form `host:port/protocol` where the protocol
//! suffix (`/tcp`, `/udp`) is optional and defaults to UDP.

use std::str::FromStr;

/// Network protocol for transport selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Protocol {
	/// UDP transport (QUIC) - default, better performance
	#[default]
	Udp,
	/// TCP transport (WebSocket) - for proxy traversal
	Tcp,
}

/// Parsed address with protocol.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddressSpec {
	/// The address without protocol suffix
	pub addr: String,
	/// The transport protocol
	pub protocol: Protocol,
}

impl AddressSpec {
	/// Creates an `AddressSpec` that listens on all interfaces (":port") with UDP.
	#[must_use]
	pub fn listen_all(port: u16) -> Self {
		Self {
			addr: format!(":{port}"),
			protocol: Protocol::Udp,
		}
	}

	/// Append the default port if `addr` has no port specified.
	fn apply_default_port(addr: &str) -> String {
		let has_port = if addr.starts_with('[') {
			addr.contains("]:")
		} else {
			addr.contains(':')
		};

		if has_port {
			addr.to_string()
		} else {
			format!(
				"{}:{}",
				addr,
				wallhack_core::server::config::DEFAULT_LISTEN_PORT
			)
		}
	}
}

/// The idiomatic way to parse strings in Rust.
impl FromStr for AddressSpec {
	type Err = String;

	fn from_str(s: &str) -> Result<Self, Self::Err> {
		// Validation example:
		if s.is_empty() {
			return Err("address spec cannot be empty".to_string());
		}

		let (base, protocol) = if let Some(stripped) = s.strip_suffix("/tcp") {
			(stripped, Protocol::Tcp)
		} else if let Some(stripped) = s.strip_suffix("/udp") {
			(stripped, Protocol::Udp)
		} else {
			(s, Protocol::Udp)
		};

		Ok(Self {
			addr: Self::apply_default_port(base),
			protocol,
		})
	}
}

/// Provides automatic conversion for methods expecting `Into<AddressSpec>`.
///
/// # Panics
/// Panics if the string is invalid. Use `s.parse::<AddressSpec>()` for fallible parsing.
impl From<&str> for AddressSpec {
	fn from(s: &str) -> Self {
		s.parse().expect("Failed to convert string to AddressSpec")
	}
}

/// A resolved connectivity specification.
#[derive(Debug, Clone)]
pub enum ConnectivitySpec {
	/// Node listens for incoming connections.
	Listen(AddressSpec),
	/// Node connects to a remote peer.
	Connect(AddressSpec),
	/// Node has both connect and listen (relay capability).
	Both {
		connect: AddressSpec,
		listen: AddressSpec,
	},
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn test_idiomatic_parsing() {
		let spec: AddressSpec = "localhost:8080/tcp".parse().unwrap();
		assert_eq!(spec.protocol, Protocol::Tcp);
		assert_eq!(spec.addr, "localhost:8080");
	}

	#[test]
	#[allow(deprecated)]
	fn test_legacy_parsing() {
		let spec = "localhost:8080/udp".parse::<AddressSpec>().unwrap();
		assert_eq!(spec.protocol, Protocol::Udp);
	}
}
