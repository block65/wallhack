//! Consolidated configuration builders for all node modes.
//!
//! Eliminates the duplicated `build_*_config` functions that were scattered
//! across entry, exit, and relay modules.

use std::net::SocketAddr;

use wallhack_core::server::config::{ServerConfig, TlsConfig};

use crate::daemon_config::{GlobalConfig, TlsParams};

#[cfg(feature = "quic")]
use wallhack_core::client::config::{ClientConfig, MtlsConfig};

/// Security-related connection parameters for peer connections.
pub(crate) struct SecurityParams {
    pub psk: Option<String>,
    pub accept_fingerprint: Option<String>,
}

/// Build a [`TlsConfig`] from TLS parameters.
///
/// Returns `None` if either cert or key is not provided.
pub(crate) fn build_tls_config(tls: &TlsParams) -> Option<TlsConfig> {
    match (&tls.cert, &tls.key) {
        (Some(cert), Some(key)) => Some(TlsConfig {
            cert_pem_file: cert.clone(),
            key_pem_file: key.clone(),
            ca_roots: tls.ca.clone(),
        }),
        _ => None,
    }
}

/// Build an [`MtlsConfig`] from TLS parameters.
///
/// Returns `None` if either cert or key is not provided.
#[cfg(feature = "quic")]
pub(crate) fn build_mtls_config(tls: &TlsParams) -> Option<MtlsConfig> {
    match (&tls.cert, &tls.key) {
        (Some(cert), Some(key)) => Some(MtlsConfig {
            cert_pem_file: cert.clone(),
            key_pem_file: key.clone(),
            ca_roots: tls.ca.clone(),
        }),
        _ => None,
    }
}

/// Build a [`ServerConfig`] for any node mode.
pub(crate) fn build_server_config(
    tls: &TlsParams,
    addr: SocketAddr,
    psk: Option<String>,
    max_peers: Option<usize>,
) -> ServerConfig {
    ServerConfig {
        listen: addr,
        tls: build_tls_config(tls),
        psk,
        max_peers,
    }
}

/// Build a QUIC [`ClientConfig`] with full security options.
#[cfg(feature = "quic")]
pub(crate) fn build_quic_client_config(
    global: &GlobalConfig,
    endpoint: SocketAddr,
    name: Option<String>,
    security: &SecurityParams,
) -> ClientConfig {
    use crate::net::SocketAddrExt;

    ClientConfig {
        addr: endpoint,
        hostname: global.hostname.clone(),
        mtls: build_mtls_config(&global.tls),
        name,
        psk: security.psk.clone(),
        accept_fingerprint: security.accept_fingerprint.clone(),
        bind: endpoint.bind_addr(),
    }
}

/// Build a WebSocket client config with full security options.
#[cfg(feature = "websocket")]
pub(crate) fn build_ws_client_config(
    global: &GlobalConfig,
    endpoint: SocketAddr,
    name: Option<String>,
    security: &SecurityParams,
) -> wallhack_core::client::ws::WsClientConfig {
    use crate::net::SocketAddrExt;

    wallhack_core::client::ws::WsClientConfig {
        base: wallhack_core::client::config::ClientConfig {
            addr: endpoint,
            hostname: global.hostname.clone(),
            mtls: None,
            name,
            psk: security.psk.clone(),
            accept_fingerprint: security.accept_fingerprint.clone(),
            bind: endpoint.bind_addr(),
        },
        path: "/ws".to_string(),
        host_header: global.hostname.clone(),
        use_tls: true,
    }
}
