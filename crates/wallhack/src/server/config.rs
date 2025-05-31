use std::path::PathBuf;

#[cfg(test)]
use std::net::SocketAddr;

#[derive(Debug, Clone)]
pub struct TlsConfig {
	/// Path to the PEM file containing the server certificate.
	pub cert_pem_file: PathBuf,

	/// Path to the PEM file containing the server private key.
	pub key_pem_file: PathBuf,

	/// Enables MTLS (mutual TLS) authentication.
	pub ca_roots: Option<PathBuf>,
}

pub const DEFAULT_LISTEN_PORT: u16 = 6565;
pub const DEFAULT_LISTEN_ADDRESS: std::net::Ipv6Addr = std::net::Ipv6Addr::UNSPECIFIED;

#[derive(Debug)]
pub struct ServerConfig {
	/// Specifies the local address and port for the server to listen on.
	pub listen: std::net::SocketAddr,

	pub tls: Option<TlsConfig>,
}

#[cfg(test)]
impl Default for ServerConfig {
	fn default() -> Self {
		Self {
			listen: SocketAddr::new(std::net::Ipv6Addr::LOCALHOST.into(), DEFAULT_LISTEN_PORT),
			tls: None,
		}
	}
}

#[cfg(not(test))]
impl Default for ServerConfig {
	fn default() -> Self {
		Self {
			listen: (DEFAULT_LISTEN_ADDRESS, DEFAULT_LISTEN_PORT).into(),
			tls: None,
		}
	}
}

impl From<std::net::SocketAddr> for ServerConfig {
	fn from(addr: std::net::SocketAddr) -> Self {
		Self {
			listen: addr,
			..Default::default()
		}
	}
}
