//! Relay node implementation.
//!
//! A relay node connects to a peer (entry/relay) and listens for connections
//! (exit nodes). It forwards messages between them without processing.
//!
//! When the source peer connection drops, the relay tears down the listener,
//! reconnects, and restarts — connected peers reconnect via their own
//! retry loops.

use std::{sync::Arc, time::Duration};

use wallhack_core::{
    NodeRole,
    client::client::ConnectResult,
    control::{handler::HandlerConfig, metrics::Metrics},
    server::server::{Server, ServerOptions},
};

use crate::{
    NodeError,
    address_spec::{AddressSpec, Protocol},
    config::SecurityParams,
    daemon_config::{GlobalConfig, RelayConfig},
};

/// Delay before reconnecting after the source peer connection drops.
const RECONNECT_DELAY: Duration = Duration::from_millis(500);

fn build_server_options(cfg: &RelayConfig, metrics: Arc<Metrics>) -> ServerOptions {
    ServerOptions {
        handler_config: HandlerConfig::new(
            NodeRole::Relay,
            crate::built_info::PKG_NAME.to_string(),
            crate::built_info::PKG_VERSION.to_string(),
        ),
        metrics: Some(metrics),
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
    }
}

/// Run as a relay node.
///
/// Connects to a source peer and listens for peer connections, forwarding
/// messages between them. Reconnects to the source peer on disconnect.
///
/// # Errors
///
/// Returns error if a non-retryable connection error occurs.
pub async fn run(
    global: &GlobalConfig,
    cfg: &RelayConfig,
    metrics: Arc<Metrics>,
) -> Result<(), NodeError> {
    let addr: std::net::SocketAddr = cfg.listen.addr.parse::<crate::net::ListenAddr>()?.into();
    let server_options = build_server_options(cfg, metrics);

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
                let listen_spec = cfg.listen.clone();
                let global = global.clone();
                crate::transport::connect_loop(
                    || {
                        let cfg = client_config.clone();
                        async move {
                            use wallhack_core::client::client::Client;
                            let mut client = wallhack_core::client::quic::QuicClient::try_new(cfg)?;
                            client.connect(NodeRole::Relay).await
                        }
                    },
                    |connect_result| {
                        let global = global.clone();
                        let listen_spec = listen_spec.clone();
                        let server_options = server_options.clone();
                        async move {
                            run_relay_loop(
                                connect_result,
                                &global,
                                &listen_spec,
                                addr,
                                server_options,
                            )
                            .await
                        }
                    },
                    RECONNECT_DELAY,
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
                let listen_spec = cfg.listen.clone();
                let global = global.clone();
                crate::transport::connect_loop(
                    || {
                        let cfg = client_config.clone();
                        async move {
                            let mut client = wallhack_core::client::ws::WsClient::new(cfg)?;
                            client.connect(NodeRole::Relay).await
                        }
                    },
                    |connect_result| {
                        let global = global.clone();
                        let listen_spec = listen_spec.clone();
                        let server_options = server_options.clone();
                        async move {
                            run_relay_loop(
                                connect_result,
                                &global,
                                &listen_spec,
                                addr,
                                server_options,
                            )
                            .await
                        }
                    },
                    RECONNECT_DELAY,
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

/// Drive the relay with a connected source peer.
///
/// Starts the listener, bridges channels, and returns `Ok(())` when the
/// source peer disconnects so `connect_loop` reconnects.
async fn run_relay_loop<T: wallhack_core::transport::Transport + 'static>(
    connect_result: ConnectResult<T>,
    global: &GlobalConfig,
    listen_spec: &AddressSpec,
    addr: std::net::SocketAddr,
    server_options: ServerOptions,
) -> Result<(), NodeError> {
    let peer_addr = connect_result.peer_addr().to_string();
    tracing::info!("Connected to {peer_addr}");

    let (channels, mut tasks, _control_tx) = connect_result.into_parts();
    let (source_instr, source_resp) = channels;

    let listener_fut = run_listener(
        global,
        listen_spec,
        addr,
        server_options,
        &source_instr,
        &source_resp,
    );
    let disconnect_fut = tasks.wait_for_disconnect();

    tokio::pin!(listener_fut);
    tokio::pin!(disconnect_fut);

    tokio::select! {
        result = &mut listener_fut => {
            match result {
                Ok(()) => tracing::debug!("Listener closed"),
                Err(e) => tracing::warn!("Listener error: {e}"),
            }
        }
        () = &mut disconnect_fut => {
            tracing::warn!("Lost connection to {peer_addr}");
        }
    }

    Ok(())
}

async fn run_listener(
    global: &GlobalConfig,
    listen_spec: &AddressSpec,
    addr: std::net::SocketAddr,
    server_options: ServerOptions,
    source_instr: &tokio::sync::broadcast::Sender<wallhack_wire::data::EntryNodeInstruction>,
    source_resp: &tokio::sync::broadcast::Sender<wallhack_wire::data::ExitNodeResponse>,
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
    source_instr: &tokio::sync::broadcast::Sender<wallhack_wire::data::EntryNodeInstruction>,
    source_resp: &tokio::sync::broadcast::Sender<wallhack_wire::data::ExitNodeResponse>,
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
                crate::transport::bridge_channels(accept_result, source_instr, source_resp);
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
    source_instr: &tokio::sync::broadcast::Sender<wallhack_wire::data::EntryNodeInstruction>,
    source_resp: &tokio::sync::broadcast::Sender<wallhack_wire::data::ExitNodeResponse>,
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
                crate::transport::bridge_channels(accept_result, source_instr, source_resp);
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
