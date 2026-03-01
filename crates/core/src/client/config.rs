use std::{net::SocketAddr, path::PathBuf};

use socket2::{Domain, Socket, Type};
use wallhack_wire::data::Handshake;
use zeroize::Zeroizing;

use crate::server::config::DEFAULT_LISTEN_PORT;

pub const DEFAULT_BIND_PORT: u16 = 0;
pub const DEFAULT_CONNECT_ADDRESS: std::net::Ipv6Addr = std::net::Ipv6Addr::LOCALHOST;

/// Returns `true` if the kernel supports the IPv6 address family.
///
/// Asks the kernel to create an `AF_INET6` socket object. No port is allocated;
/// this is purely a kernel capability check.
#[must_use]
pub fn ipv6_supported() -> bool {
    Socket::new(Domain::IPV6, Type::DGRAM, None).is_ok()
}

fn default_bind_addr() -> SocketAddr {
    if ipv6_supported() {
        (std::net::Ipv6Addr::UNSPECIFIED, DEFAULT_BIND_PORT).into()
    } else {
        (std::net::Ipv4Addr::UNSPECIFIED, DEFAULT_BIND_PORT).into()
    }
}

#[derive(Debug, Clone)]
pub struct ClientConfig {
    /// URL to connect to
    pub addr: SocketAddr,

    /// Override hostname used for certificate verification
    pub hostname: Option<String>,

    /// MTLS Client config
    pub mtls: Option<MtlsConfig>,

    /// Bind address for UDP socket
    pub bind: SocketAddr,

    /// Name for this peer (exit nodes only).
    /// If set, sent to peer via `Handshake` message.
    pub name: Option<String>,

    /// Pre-shared key for tunnel authentication. Zeroized on drop.
    pub psk: Option<Zeroizing<String>>,

    /// Expected server certificate fingerprint (TOFU).
    pub accept_fingerprint: Option<String>,

    /// Local handshake capabilities advertised to the peer.
    /// If set, its `capabilities` field is used instead of the client defaults.
    pub local_handshake: Option<Handshake>,
}

#[derive(Debug, Clone)]
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
            bind: default_bind_addr(),
            name: None,
            psk: None,
            accept_fingerprint: None,
            local_handshake: None,
        }
    }
}
