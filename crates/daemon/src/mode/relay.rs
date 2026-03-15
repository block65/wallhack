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
    control::{
        handler::{HandlerConfig, SharedNodeState},
        metrics::Metrics,
    },
    server::server::{Server, ServerOptions},
    transport::Transport,
};

use crate::{
    NodeError,
    address_spec::{AddressSpec, Protocol},
    config::SecurityParams,
    daemon_config::{GlobalConfig, RelayConfig},
};

/// Delay before reconnecting after the source peer connection drops.
const RECONNECT_DELAY: Duration = Duration::from_millis(500);

fn build_server_options(cfg: &RelayConfig, version: &str, metrics: Arc<Metrics>) -> ServerOptions {
    ServerOptions {
        handler_config: HandlerConfig::new(
            NodeRole::Relay,
            "wallhack".to_string(),
            version.to_string(),
        ),
        metrics: Some(metrics),
        peers: None,
        routes: None,
        route_updates: None,
        local_handshake: Some(wallhack_wire::data::Handshake {
            capabilities: Some(wallhack_wire::data::Capabilities {
                tun_capable: false,
                listening: true,
                connecting: true,
            }),
            name: cfg.name.clone(),
            version: version.to_string(),
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
#[allow(clippy::too_many_lines)] // symmetric quic/ws dispatch arms
pub async fn run(
    global: &GlobalConfig,
    cfg: &RelayConfig,
    metrics: Arc<Metrics>,
    node_state: SharedNodeState,
) -> Result<(), NodeError> {
    // Relay capabilities are known at startup.
    node_state.update_capabilities(wallhack_wire::data::Capabilities {
        tun_capable: false,
        listening: true,
        connecting: true,
    });
    let addr: std::net::SocketAddr = cfg.listen.addr.parse::<crate::net::ListenAddr>()?.into();
    let server_options = build_server_options(cfg, &global.version, metrics);

    tracing::info!("Connecting to {}...", cfg.connect.addr);
    let target_addr =
        crate::transport::resolve_endpoint(&cfg.connect.addr, global.dns_server.as_deref()).await?;

    let security = SecurityParams {
        psk: global.psk.clone(),
        accept_fingerprint: cfg.accept_fingerprint.clone(),
    };

    // Relay connector advertises both listening and connecting capabilities.
    let relay_connector_hs = wallhack_wire::data::Handshake {
        capabilities: Some(wallhack_wire::data::Capabilities {
            tun_capable: false,
            listening: true,
            connecting: true,
        }),
        name: cfg.name.clone(),
        version: global.version.clone(),
        psk_proof: Vec::new(),
        routes: Vec::new(),
        hint: None,
    };

    match cfg.connect.protocol {
        Protocol::Udp => {
            #[cfg(feature = "quic")]
            {
                let client_config = crate::config::build_quic_client_config(
                    global,
                    target_addr,
                    None,
                    &security,
                    Some(relay_connector_hs.clone()),
                );
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
                        let e = connect_result.erase();
                        let global = global.clone();
                        let listen_spec = listen_spec.clone();
                        let server_options = server_options.clone();
                        async move {
                            run_relay_loop_inner(
                                e.peer_addr,
                                e.transport,
                                e.channels,
                                e.tasks,
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
                let client_config = crate::config::build_ws_client_config(
                    global,
                    target_addr,
                    None,
                    &security,
                    Some(relay_connector_hs),
                );
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
                        let e = connect_result.erase();
                        let global = global.clone();
                        let listen_spec = listen_spec.clone();
                        let server_options = server_options.clone();
                        async move {
                            run_relay_loop_inner(
                                e.peer_addr,
                                e.transport,
                                e.channels,
                                e.tasks,
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
/// Non-generic relay loop: monomorphized once regardless of transport type.
#[allow(clippy::too_many_arguments)]
async fn run_relay_loop_inner(
    peer_addr: String,
    transport: std::sync::Arc<dyn wallhack_core::transport::ErasedTransport>,
    channels: wallhack_core::server::server::DataChannels,
    mut tasks: wallhack_core::client::client::ConnectionTasks,
    global: &GlobalConfig,
    listen_spec: &AddressSpec,
    addr: std::net::SocketAddr,
    server_options: ServerOptions,
) -> Result<(), NodeError> {
    use wallhack_core::{server::server::DataChannels, transport::protocol::run_send_instructions};

    tracing::info!("Connected to {peer_addr}");

    let DataChannels {
        instructions_tx: source_instr_tx,
        instructions_rx: source_instr_rx,
        responses_tx: _source_resp_tx,
        responses_rx: source_resp_rx,
    } = channels;

    // Outgoing: open uni stream to source, send instructions (relay → source).
    let transport_out = std::sync::Arc::clone(&transport);
    tokio::spawn(async move {
        match transport_out.open_uni_erased().await {
            Ok(mut send) => {
                if let Err(e) = run_send_instructions(&mut send, source_instr_rx).await {
                    tracing::debug!("Send-instructions to source finished: {e}");
                }
            }
            Err(e) => tracing::debug!("Failed to open send stream to source: {e}"),
        }
    });

    // Outgoing: accept uni from source (source → relay → peers): this is
    // handled by the client's incoming task spawned in connect(), which
    // dispatches to instructions_tx/responses_tx on the source side.
    // For the relay's own outgoing to source we use run_send_responses
    // on the relay's response channel:
    // NOTE: The incoming task in connect() already handles source→relay direction.

    // Spawn the fan-out task: reads from source_resp_rx, forwards to each connected peer.
    let fanout_register_tx = crate::transport::spawn_fanout_task(source_resp_rx);

    let listener_fut = run_listener(
        global,
        listen_spec,
        addr,
        server_options,
        source_instr_tx,
        fanout_register_tx,
    );

    tokio::pin!(listener_fut);
    let disconnect_fut = tasks.wait_for_disconnect();
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
    source_instr_tx: tokio::sync::mpsc::Sender<wallhack_wire::data::EntryNodeInstruction>,
    fanout_register_tx: tokio::sync::mpsc::UnboundedSender<
        tokio::sync::mpsc::Sender<wallhack_wire::data::ExitNodeResponse>,
    >,
) -> Result<(), NodeError> {
    match listen_spec.protocol {
        Protocol::Udp => {
            #[cfg(feature = "quic")]
            {
                run_quic_listener(
                    global,
                    addr,
                    server_options,
                    source_instr_tx,
                    fanout_register_tx,
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
                run_ws_listener(
                    global,
                    addr,
                    server_options,
                    source_instr_tx,
                    fanout_register_tx,
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

#[cfg(feature = "quic")]
async fn run_quic_listener(
    global: &GlobalConfig,
    addr: std::net::SocketAddr,
    server_options: ServerOptions,
    source_instr_tx: tokio::sync::mpsc::Sender<wallhack_wire::data::EntryNodeInstruction>,
    fanout_register_tx: tokio::sync::mpsc::UnboundedSender<
        tokio::sync::mpsc::Sender<wallhack_wire::data::ExitNodeResponse>,
    >,
) -> Result<(), NodeError> {
    let server_config =
        crate::config::build_server_config(&global.tls, addr, global.psk.clone(), None);
    let server = wallhack_core::server::quic::QuicServer::try_new(server_config, server_options)
        .map_err(|e| NodeError::Transport(Box::new(e)))?;
    tracing::info!("Listening on {} (QUIC)", server.local_addr()?);

    run_relay_accept_loop(server, source_instr_tx, fanout_register_tx).await
}

#[cfg(feature = "websocket")]
async fn run_ws_listener(
    global: &GlobalConfig,
    addr: std::net::SocketAddr,
    server_options: ServerOptions,
    source_instr_tx: tokio::sync::mpsc::Sender<wallhack_wire::data::EntryNodeInstruction>,
    fanout_register_tx: tokio::sync::mpsc::UnboundedSender<
        tokio::sync::mpsc::Sender<wallhack_wire::data::ExitNodeResponse>,
    >,
) -> Result<(), NodeError> {
    use wallhack_core::server::ws::WebSocketServer;

    let server_config =
        crate::config::build_server_config(&global.tls, addr, global.psk.clone(), None);
    let server = WebSocketServer::try_new(server_config, server_options)
        .map_err(|e| NodeError::Transport(Box::new(e)))?;
    tracing::info!("Listening on {} (WebSocket)", server.local_addr()?);

    run_relay_accept_loop(server, source_instr_tx, fanout_register_tx).await
}

/// Generic relay accept loop that works with any `Server` implementation.
async fn run_relay_accept_loop<S: Server>(
    mut server: S,
    source_instr_tx: tokio::sync::mpsc::Sender<wallhack_wire::data::EntryNodeInstruction>,
    fanout_register_tx: tokio::sync::mpsc::UnboundedSender<
        tokio::sync::mpsc::Sender<wallhack_wire::data::ExitNodeResponse>,
    >,
) -> Result<(), NodeError>
where
    S::Error: std::error::Error + Send + Sync + 'static,
    S::Transport: Send + Sync + 'static,
    <S::Transport as Transport>::SendStream: 'static,
    <S::Transport as Transport>::RecvStream: 'static,
    <S::Transport as Transport>::BiStream: Send + 'static,
{
    loop {
        match server.accept(NodeRole::Relay).await {
            Ok(Some(accept_result)) => {
                let erased = accept_result.erase();
                handle_relay_connection(erased, source_instr_tx.clone(), &fanout_register_tx);
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

/// Non-generic handler for erased relay connection results.
fn handle_relay_connection(
    erased: wallhack_core::server::server::ErasedAcceptResult,
    source_instr_tx: tokio::sync::mpsc::Sender<wallhack_wire::data::EntryNodeInstruction>,
    fanout_register_tx: &tokio::sync::mpsc::UnboundedSender<
        tokio::sync::mpsc::Sender<wallhack_wire::data::ExitNodeResponse>,
    >,
) {
    use wallhack_core::{
        server::server::DataChannels,
        transport::protocol::{run_data_in, run_send_responses},
    };

    let peer_addr = erased.peer_addr;
    let transport = erased.transport;
    let (channels, control_tx) = (erased.channels, erased.control_tx);
    let DataChannels {
        instructions_tx,
        instructions_rx,
        responses_tx,
        responses_rx,
    } = channels;

    // Incoming: accept uni stream from peer, dispatch data messages.
    let transport_in = std::sync::Arc::clone(&transport);
    let instr_tx = instructions_tx.clone();
    let resp_tx = responses_tx.clone();
    tokio::spawn(async move {
        match transport_in.accept_uni_erased().await {
            Ok(Some(mut recv)) => {
                if let Err(e) = run_data_in(&mut recv, &instr_tx, &resp_tx).await {
                    tracing::debug!("Relay peer data-in finished: {e}");
                }
            }
            Ok(None) => tracing::debug!("Relay peer transport closed before data-in"),
            Err(e) => tracing::debug!("Relay peer failed to accept data-in: {e}"),
        }
    });

    // Outgoing: open uni stream to peer, send responses.
    let transport_out = transport;
    tokio::spawn(async move {
        match transport_out.open_uni_erased().await {
            Ok(mut send) => {
                if let Err(e) = run_send_responses(&mut send, responses_rx).await {
                    tracing::debug!("Relay peer send-responses finished: {e}");
                }
            }
            Err(e) => tracing::debug!("Relay peer failed to open send stream: {e}"),
        }
    });

    crate::transport::bridge_channels(
        &peer_addr,
        instructions_rx,
        responses_tx,
        control_tx,
        source_instr_tx,
        fanout_register_tx,
    );
}
