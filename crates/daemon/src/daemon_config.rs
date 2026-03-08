//! Structured configuration for the daemon engine.
//!
//! These types decouple the daemon library from CLI parsing. The CLI crate
//! builds a [`DaemonConfig`] from command-line arguments and passes it in.

use std::{net::SocketAddr, path::PathBuf, time::Duration};

use crate::address_spec::{AddressSpec, ConnectivitySpec};

/// Top-level daemon configuration.
#[derive(Debug, Clone)]
pub struct DaemonConfig {
    pub global: GlobalConfig,
    pub mode: ModeConfig,
}

/// Global settings shared across all node modes.
#[derive(Debug, Clone)]
pub struct GlobalConfig {
    pub tls: TlsParams,
    pub hostname: Option<String>,
    pub dns_server: Option<String>,
    pub timeout: Duration,
    pub psk: Option<zeroize::Zeroizing<String>>,
    /// Canonical version string, e.g. `0.6.2 (abc1234-dirty)`.
    /// Computed once at startup, used in banner, IPC, and handshake.
    pub version: String,
}

/// TLS certificate/key paths.
#[derive(Debug, Clone, Default)]
pub struct TlsParams {
    pub cert: Option<PathBuf>,
    pub key: Option<PathBuf>,
    pub ca: Option<PathBuf>,
}

/// Which node mode to run.
#[derive(Debug, Clone)]
pub enum ModeConfig {
    Entry(EntryConfig),
    Exit(ExitConfig),
    Relay(RelayConfig),
    /// Role is determined automatically via handshake negotiation.
    Auto(AutoConfig),
}

impl ModeConfig {
    #[must_use]
    pub fn name(&self) -> &str {
        match self {
            Self::Entry(c) => &c.name,
            Self::Exit(c) => &c.name,
            Self::Relay(c) => &c.name,
            Self::Auto(c) => &c.name,
        }
    }
}

/// Entry node configuration.
#[derive(Debug, Clone)]
pub struct EntryConfig {
    pub name: String,
    pub connectivity: ConnectivitySpec,
    pub api: Option<ApiConfig>,
    pub max_peers: Option<usize>,
    pub fast: bool,
}

/// Exit node configuration.
#[derive(Debug, Clone)]
pub struct ExitConfig {
    pub name: String,
    pub connectivity: ConnectivitySpec,
    pub accept_fingerprint: Option<String>,
}

/// Relay node configuration.
#[derive(Debug, Clone)]
pub struct RelayConfig {
    pub name: String,
    pub connect: AddressSpec,
    pub listen: AddressSpec,
    pub accept_fingerprint: Option<String>,
}

/// Auto-negotiation mode configuration.
///
/// Role is derived from the handshake exchange. With both `connect` and
/// `listen` set, the node runs as a relay immediately (no negotiation needed).
#[derive(Debug, Clone)]
pub struct AutoConfig {
    pub name: String,
    pub listen: Option<AddressSpec>,
    pub connect: Option<AddressSpec>,
    pub accept_fingerprint: Option<String>,
    pub hint: Option<wallhack_wire::data::RoleHint>,
}

/// REST API configuration for entry nodes.
#[derive(Debug, Clone)]
pub struct ApiConfig {
    pub addr: SocketAddr,
    pub user: String,
    pub secret: String,
}
