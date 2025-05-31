use std::{net::SocketAddr, path::PathBuf};

use crate::server::config::DEFAULT_LISTEN_PORT;

pub const DEFAULT_BIND_PORT: u16 = 0;
pub const DEFAULT_BIND_ADDRESS: std::net::Ipv6Addr = std::net::Ipv6Addr::UNSPECIFIED;

pub const DEFAULT_CONNECT_ADDRESS: std::net::Ipv6Addr = std::net::Ipv6Addr::LOCALHOST;

#[derive(Debug)]
pub struct ClientConfig {
	/// URL to connect to
	pub addr: SocketAddr,

	/// Override hostname used for certificate verification
	pub hostname: Option<String>,

	/// MTLS Client config
	pub mtls: Option<MtlsConfig>,

	/// Bind address for UDP socket
	pub bind: SocketAddr,
}

#[derive(Debug)]
pub struct MtlsConfig {
	/// Path to the client certificate
	pub cert_pem_file: PathBuf,

	/// Path to the client private key
	pub key_pem_file: PathBuf,

	/// Path to the CA certificate - optional for client auth if the M in your
	/// definition of MTLS stands for Monolateral
	pub ca_roots: Option<PathBuf>,
}

impl Default for ClientConfig {
	fn default() -> Self {
		Self {
			addr: (DEFAULT_CONNECT_ADDRESS, DEFAULT_LISTEN_PORT).into(),
			hostname: None,
			mtls: None,
			bind: (DEFAULT_BIND_ADDRESS, DEFAULT_BIND_PORT).into(),
		}
	}
}
