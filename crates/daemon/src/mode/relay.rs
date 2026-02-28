//! Relay node implementation.
//!
//! A relay node connects to a peer (entry/relay) and listens for connections
//! (exit nodes). It forwards messages between them without processing.

use std::sync::Arc;

use tokio::sync::broadcast;

use wallhack_core::{
    NodeRole,
    control::{handler::HandlerConfig, metrics::Metrics},
    server::server::{Server, ServerOptions},
};

use crate::{
    NodeError,
    address_spec::{AddressSpec, Protocol},
    config::SecurityParams,
    daemon_config::{GlobalConfig, RelayConfig},
};

/// Run as a relay node.
///
/// Connects to a source peer and listens for peer connections, forwarding
/// messages between them. Retries source connection forever.
///
/// # Errors
///
/// Returns error if server fails (connection errors are retried).
pub async fn run(
    global: &GlobalConfig,
    cfg: &RelayConfig,
    metrics: Arc<Metrics>,
) -> Result<(), NodeError> {
    // Parse listen address
    let addr: std::net::SocketAddr = cfg.listen.addr.parse::<crate::net::ListenAddr>()?.into();

    // Server options with control handler config
    let server_options = ServerOptions {
        handler_config: HandlerConfig::new(
            NodeRole::Relay,
            crate::built_info::PKG_NAME.to_string(),
            crate::built_info::PKG_VERSION.to_string(),
        ),
        metrics: Some(Arc::clone(&metrics)),
        peers: None,
        routes: None,
        local_handshake: Some(wallhack_wire::data::Handshake {
            capabilities: Some(wallhack_wire::data::Capabilities {
                tun_capable: false,
                listening: true,
                connecting: true,
            }),
            name: cfg.name.clone(),
            version: crate::built_info::PKG_VERSION.to_string(),
            psk_proof: Vec::new(),
            routes: Vec::new(),
            hint: None,
        }),
    };

    tracing::info!("Connecting to {}...", cfg.connect.addr);
    let target_addr =
        crate::transport::resolve_endpoint(&cfg.connect.addr, global.dns_server.as_deref()).await?;

    let security = SecurityParams {
        psk: global.psk.clone(),
        accept_fingerprint: cfg.accept_fingerprint.clone(),
    };

    match cfg.connect.protocol {
        Protocol::Udp => {
            #[cfg(feature = "quic")]
            {
                let client_config =
                    crate::config::build_quic_client_config(global, target_addr, None, &security);
                let source_result = crate::transport::connect_with_retry(|| {
                    let cfg = client_config.clone();
                    async move {
                        use wallhack_core::client::client::Client;
                        let mut client = wallhack_core::client::quic::QuicClient::try_new(cfg)?;
                        client.connect(NodeRole::Relay).await
                    }
                })
                .await?;
                let (source_instr, source_resp) = source_result.channels().clone();
                run_listener(
                    global,
                    &cfg.listen,
                    addr,
                    server_options,
                    source_instr,
                    source_resp,
                )
                .await
            }
            #[cfg(not(feature = "quic"))]
            {
                Err(NodeError::TransportUnavailable("quic"))
            }
        }
        Protocol::Tcp => {
            #[cfg(feature = "websocket")]
            {
                let client_config =
                    crate::config::build_ws_client_config(global, target_addr, None, &security);
                let source_result = crate::transport::connect_with_retry(|| {
                    let cfg = client_config.clone();
                    async move {
                        let mut client = wallhack_core::client::ws::WsClient::new(cfg)?;
                        client.connect(NodeRole::Relay).await
                    }
                })
                .await?;
                let (source_instr, source_resp) = source_result.channels().clone();
                run_listener(
                    global,
                    &cfg.listen,
                    addr,
                    server_options,
                    source_instr,
                    source_resp,
                )
                .await
            }
            #[cfg(not(feature = "websocket"))]
            {
                Err(NodeError::TransportUnavailable("websocket"))
            }
        }
    }
}

async fn run_listener(
    global: &GlobalConfig,
    listen_spec: &AddressSpec,
    addr: std::net::SocketAddr,
    server_options: ServerOptions,
    source_instr: broadcast::Sender<wallhack_wire::data::EntryNodeInstruction>,
    source_resp: broadcast::Sender<wallhack_wire::data::ExitNodeResponse>,
) -> Result<(), NodeError> {
    match listen_spec.protocol {
        Protocol::Udp => {
            #[cfg(feature = "quic")]
            {
                run_quic_listener(global, addr, server_options, source_instr, source_resp).await
            }
            #[cfg(not(feature = "quic"))]
            {
                Err(NodeError::TransportUnavailable("quic"))
            }
        }
        Protocol::Tcp => {
            #[cfg(feature = "websocket")]
            {
                run_ws_listener(global, addr, server_options, source_instr, source_resp).await
            }
            #[cfg(not(feature = "websocket"))]
            {
                Err(NodeError::TransportUnavailable("websocket"))
            }
        }
    }
}

#[cfg(feature = "quic")]
async fn run_quic_listener(
    global: &GlobalConfig,
    addr: std::net::SocketAddr,
    server_options: ServerOptions,
    source_instr: broadcast::Sender<wallhack_wire::data::EntryNodeInstruction>,
    source_resp: broadcast::Sender<wallhack_wire::data::ExitNodeResponse>,
) -> Result<(), NodeError> {
    let server_config =
        crate::config::build_server_config(&global.tls, addr, global.psk.clone(), None);
    let mut server =
        wallhack_core::server::quic::QuicServer::try_new(server_config, server_options)
            .map_err(|e| NodeError::Transport(Box::new(e)))?;
    tracing::info!("Listening on {} (QUIC)", server.local_addr()?);

    loop {
        match server.accept(NodeRole::Relay).await {
            Ok(Some(accept_result)) => {
                crate::transport::bridge_channels(accept_result, &source_instr, &source_resp);
            }
            Ok(None) => {
                tracing::info!("Server closed");
                break;
            }
            Err(e) => {
                tracing::warn!("Accept error: {}", e);
            }
        }
    }

    Ok(())
}

#[cfg(feature = "websocket")]
async fn run_ws_listener(
    global: &GlobalConfig,
    addr: std::net::SocketAddr,
    server_options: ServerOptions,
    source_instr: broadcast::Sender<wallhack_wire::data::EntryNodeInstruction>,
    source_resp: broadcast::Sender<wallhack_wire::data::ExitNodeResponse>,
) -> Result<(), NodeError> {
    use wallhack_core::server::ws::WebSocketServer;

    let server_config =
        crate::config::build_server_config(&global.tls, addr, global.psk.clone(), None);
    let mut server = WebSocketServer::try_new(server_config, server_options)
        .map_err(|e| NodeError::Transport(Box::new(e)))?;
    tracing::info!("Listening on {} (WebSocket)", server.local_addr()?);

    loop {
        match server.accept(NodeRole::Relay).await {
            Ok(Some(accept_result)) => {
                crate::transport::bridge_channels(accept_result, &source_instr, &source_resp);
            }
            Ok(None) => {
                tracing::info!("Server closed");
                break;
            }
            Err(e) => {
                tracing::warn!("Accept error: {}", e);
            }
        }
    }

    Ok(())
}
