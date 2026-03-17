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
        peers::{ConnectionSide, Registry},
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

/// Lightweight shutdown signal: dropping the sender wakes all receivers.
///
/// Avoids a `tokio-util` dependency for `CancellationToken`.
type ShutdownSignal = tokio::sync::watch::Receiver<()>;

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
                interactive: false,
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
    peers: Arc<Registry>,
    node_state: SharedNodeState,
) -> Result<(), NodeError> {
    // Relay capabilities are known at startup.
    node_state.update_capabilities(wallhack_wire::data::Capabilities {
        tun_capable: false,
        listening: true,
        connecting: true,
        interactive: false,
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
            interactive: false,
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
                let peers_quic = Arc::clone(&peers);
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
                        let peers = Arc::clone(&peers_quic);
                        async move {
                            run_relay_loop_inner(
                                e.peer_addr,
                                e.transport,
                                e.channels,
                                e.tasks,
                                e.control_tx,
                                e.peer_handshake_rx,
                                &global,
                                &listen_spec,
                                addr,
                                server_options,
                                peers,
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
                let peers_ws = Arc::clone(&peers);
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
                        let peers = Arc::clone(&peers_ws);
                        async move {
                            run_relay_loop_inner(
                                e.peer_addr,
                                e.transport,
                                e.channels,
                                e.tasks,
                                e.control_tx,
                                e.peer_handshake_rx,
                                &global,
                                &listen_spec,
                                addr,
                                server_options,
                                peers,
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
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
async fn run_relay_loop_inner(
    peer_addr: String,
    transport: std::sync::Arc<dyn wallhack_core::transport::ErasedTransport>,
    channels: wallhack_core::server::server::DataChannels,
    mut tasks: wallhack_core::client::client::ConnectionTasks,
    // Retain control_tx for the full session lifetime — dropping it kills the
    // control stream and causes the source to see the relay as disconnected.
    _source_control_tx: tokio::sync::mpsc::Sender<wallhack_wire::control::ControlMessage>,
    peer_handshake_rx: Option<tokio::sync::oneshot::Receiver<wallhack_wire::data::Handshake>>,
    global: &GlobalConfig,
    listen_spec: &AddressSpec,
    addr: std::net::SocketAddr,
    server_options: ServerOptions,
    peers: Arc<Registry>,
) -> Result<(), NodeError> {
    use wallhack_core::{server::server::DataChannels, transport::protocol::run_send_responses};

    tracing::info!("Connected to {peer_addr}");

    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(());

    // Resolve peer handshake for name and capabilities.
    let peer_handshake = if let Some(rx) = peer_handshake_rx {
        match tokio::time::timeout(std::time::Duration::from_secs(10), rx).await {
            Ok(Ok(hs)) => Some(hs),
            _ => None,
        }
    } else {
        None
    };
    let peer_name = peer_handshake
        .as_ref()
        .filter(|h| !h.name.is_empty())
        .map_or_else(|| peer_addr.clone(), |h| h.name.clone());
    let peer_role = peer_handshake
        .as_ref()
        .and_then(|h| h.capabilities)
        .map_or(NodeRole::Exit, super::peer_role_from_capabilities);

    // Register the source peer so it appears in `wallhack peers`.
    peers.register(
        peer_name.clone(),
        peer_addr.clone(),
        peer_role,
        ConnectionSide::Connect,
    );

    let DataChannels {
        instructions_tx: _source_instr_tx,
        instructions_rx: source_instr_rx,
        responses_tx: source_resp_tx,
        responses_rx: source_resp_rx,
    } = channels;

    // Outgoing: open uni stream to source, send exit-peer responses (relay → entry).
    // The connect() incoming task already handles entry→relay instructions via
    // source_instr_tx; here we send the collected exit responses back to the entry.
    let transport_resp = std::sync::Arc::clone(&transport);
    tokio::spawn(async move {
        match transport_resp.open_uni_erased().await {
            Ok(mut send) => {
                if let Err(e) = run_send_responses(&mut send, source_resp_rx).await {
                    tracing::debug!("Send-responses to source finished: {e}");
                }
            }
            Err(e) => tracing::debug!("Failed to open send stream to source: {e}"),
        }
    });

    // Spawn the instruction fan-out task: reads from source_instr_rx and
    // forwards each instruction to all connected exit peers.
    let fanout_register_tx = crate::transport::spawn_fanout_task(source_instr_rx);

    // Watch channel for exit peer transports — the source→peer bidi bridge
    // reads the latest registered peer transport to route accepted bidi streams.
    let (peer_transport_tx, peer_transport_rx) = tokio::sync::watch::channel::<
        Option<Arc<dyn wallhack_core::transport::ErasedTransport>>,
    >(None);

    // Source→peer bidi bridge: single accept loop on the source transport.
    // When a bidi stream arrives from the source, opens a matching bidi to
    // the current peer and splices them together.
    let source_transport_bi = Arc::clone(&transport);
    let mut shutdown_bidi = shutdown_rx.clone();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                result = source_transport_bi.accept_bi_erased() => {
                    match result {
                        Ok(Some(source_stream)) => {
                            let current_peer = peer_transport_rx.borrow().clone();
                            let Some(peer) = current_peer else {
                                tracing::debug!("bidi bridge: no peer connected, dropping stream");
                                continue;
                            };
                            tokio::spawn(async move {
                                match peer.open_bi_erased().await {
                                    Ok(peer_stream) => {
                                        if let Err(e) = wallhack_core::transport::splice_bi(
                                            source_stream,
                                            peer_stream,
                                        ).await {
                                            tracing::debug!("bidi bridge (source→peer) ended: {e}");
                                        }
                                    }
                                    Err(e) => tracing::debug!("bidi bridge: failed to open peer stream: {e}"),
                                }
                            });
                        }
                        Ok(None) => break,
                        Err(e) => {
                            tracing::debug!("bidi bridge: source accept_bi error: {e}");
                        }
                    }
                }
                _ = shutdown_bidi.changed() => break,
            }
        }
    });

    let listener_fut = run_listener(
        global,
        listen_spec,
        addr,
        server_options,
        source_resp_tx,
        fanout_register_tx,
        peer_transport_tx,
        Arc::clone(&transport),
        Arc::clone(&peers),
        shutdown_rx,
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

    // Dropping shutdown_tx wakes all bridge tasks holding a shutdown_rx clone.
    drop(shutdown_tx);

    peers.unregister(&peer_name);

    Ok(())
}

#[allow(clippy::too_many_arguments)] // REASON: threading transport + shutdown signal through listener stack
async fn run_listener(
    global: &GlobalConfig,
    listen_spec: &AddressSpec,
    addr: std::net::SocketAddr,
    server_options: ServerOptions,
    source_resp_tx: tokio::sync::mpsc::Sender<wallhack_wire::data::ExitNodeResponse>,
    fanout_register_tx: tokio::sync::mpsc::UnboundedSender<
        tokio::sync::mpsc::Sender<wallhack_wire::data::EntryNodeInstruction>,
    >,
    peer_transport_tx: tokio::sync::watch::Sender<
        Option<Arc<dyn wallhack_core::transport::ErasedTransport>>,
    >,
    source_transport: std::sync::Arc<dyn wallhack_core::transport::ErasedTransport>,
    peers: Arc<Registry>,
    shutdown: ShutdownSignal,
) -> Result<(), NodeError> {
    match listen_spec.protocol {
        Protocol::Udp => {
            #[cfg(feature = "quic")]
            {
                run_quic_listener(
                    global,
                    addr,
                    server_options,
                    source_resp_tx,
                    fanout_register_tx,
                    peer_transport_tx,
                    source_transport,
                    peers,
                    shutdown,
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
                    source_resp_tx,
                    fanout_register_tx,
                    peer_transport_tx,
                    source_transport,
                    peers,
                    shutdown,
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
#[allow(clippy::too_many_arguments)] // REASON: forwarding from run_listener
async fn run_quic_listener(
    global: &GlobalConfig,
    addr: std::net::SocketAddr,
    server_options: ServerOptions,
    source_resp_tx: tokio::sync::mpsc::Sender<wallhack_wire::data::ExitNodeResponse>,
    fanout_register_tx: tokio::sync::mpsc::UnboundedSender<
        tokio::sync::mpsc::Sender<wallhack_wire::data::EntryNodeInstruction>,
    >,
    peer_transport_tx: tokio::sync::watch::Sender<
        Option<Arc<dyn wallhack_core::transport::ErasedTransport>>,
    >,
    source_transport: std::sync::Arc<dyn wallhack_core::transport::ErasedTransport>,
    peers: Arc<Registry>,
    shutdown: ShutdownSignal,
) -> Result<(), NodeError> {
    let server_config =
        crate::config::build_server_config(&global.tls, addr, global.psk.clone(), None);
    let server = wallhack_core::server::quic::QuicServer::try_new(server_config, server_options)
        .map_err(|e| NodeError::Transport(Box::new(e)))?;
    tracing::info!("Listening on {} (QUIC)", server.local_addr()?);

    run_relay_accept_loop(
        server,
        source_resp_tx,
        fanout_register_tx,
        peer_transport_tx,
        source_transport,
        peers,
        shutdown,
    )
    .await
}

#[cfg(feature = "websocket")]
#[allow(clippy::too_many_arguments)] // REASON: forwarding from run_listener
async fn run_ws_listener(
    global: &GlobalConfig,
    addr: std::net::SocketAddr,
    server_options: ServerOptions,
    source_resp_tx: tokio::sync::mpsc::Sender<wallhack_wire::data::ExitNodeResponse>,
    fanout_register_tx: tokio::sync::mpsc::UnboundedSender<
        tokio::sync::mpsc::Sender<wallhack_wire::data::EntryNodeInstruction>,
    >,
    peer_transport_tx: tokio::sync::watch::Sender<
        Option<Arc<dyn wallhack_core::transport::ErasedTransport>>,
    >,
    source_transport: std::sync::Arc<dyn wallhack_core::transport::ErasedTransport>,
    peers: Arc<Registry>,
    shutdown: ShutdownSignal,
) -> Result<(), NodeError> {
    use wallhack_core::server::ws::WebSocketServer;

    let server_config =
        crate::config::build_server_config(&global.tls, addr, global.psk.clone(), None);
    let server = WebSocketServer::try_new(server_config, server_options)
        .map_err(|e| NodeError::Transport(Box::new(e)))?;
    tracing::info!("Listening on {} (WebSocket)", server.local_addr()?);

    run_relay_accept_loop(
        server,
        source_resp_tx,
        fanout_register_tx,
        peer_transport_tx,
        source_transport,
        peers,
        shutdown,
    )
    .await
}

/// Generic relay accept loop that works with any `Server` implementation.
#[allow(clippy::too_many_arguments)] // REASON: forwarding from protocol-specific listeners
async fn run_relay_accept_loop<S: Server>(
    mut server: S,
    source_resp_tx: tokio::sync::mpsc::Sender<wallhack_wire::data::ExitNodeResponse>,
    fanout_register_tx: tokio::sync::mpsc::UnboundedSender<
        tokio::sync::mpsc::Sender<wallhack_wire::data::EntryNodeInstruction>,
    >,
    peer_transport_tx: tokio::sync::watch::Sender<
        Option<Arc<dyn wallhack_core::transport::ErasedTransport>>,
    >,
    source_transport: std::sync::Arc<dyn wallhack_core::transport::ErasedTransport>,
    peers: Arc<Registry>,
    shutdown: ShutdownSignal,
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
                handle_relay_connection(
                    erased,
                    source_resp_tx.clone(),
                    &fanout_register_tx,
                    &peer_transport_tx,
                    &source_transport,
                    &peers,
                    &shutdown,
                );
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
    source_resp_tx: tokio::sync::mpsc::Sender<wallhack_wire::data::ExitNodeResponse>,
    fanout_register_tx: &tokio::sync::mpsc::UnboundedSender<
        tokio::sync::mpsc::Sender<wallhack_wire::data::EntryNodeInstruction>,
    >,
    peer_transport_tx: &tokio::sync::watch::Sender<
        Option<Arc<dyn wallhack_core::transport::ErasedTransport>>,
    >,
    source_transport: &std::sync::Arc<dyn wallhack_core::transport::ErasedTransport>,
    peers: &Arc<Registry>,
    shutdown: &ShutdownSignal,
) {
    use wallhack_core::{
        server::server::DataChannels,
        transport::protocol::{run_data_in, run_send_instructions},
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

    // Register the bridged peer so it appears in `wallhack peers`.
    peers.register(
        peer_addr.clone(),
        peer_addr.clone(),
        NodeRole::Relay,
        ConnectionSide::Accept,
    );

    // Incoming: accept uni stream from exit peer, dispatch data messages.
    // Exit peers send ExitNodeResponses which are dispatched via responses_tx.
    let peer_transport_uni = std::sync::Arc::clone(&transport);
    let instr_tx = instructions_tx.clone();
    let resp_tx = responses_tx.clone();
    tokio::spawn(async move {
        match peer_transport_uni.accept_uni_erased().await {
            Ok(Some(mut recv)) => {
                if let Err(e) = run_data_in(&mut recv, &instr_tx, &resp_tx).await {
                    tracing::debug!("Relay peer data-in finished: {e}");
                }
            }
            Ok(None) => tracing::debug!("Relay peer transport closed before data-in"),
            Err(e) => tracing::debug!("Relay peer failed to accept data-in: {e}"),
        }
    });

    // Outgoing: open uni stream to exit peer, send instructions from the entry.
    // instructions_rx receives instructions distributed by the fan-out task.
    let peer_transport_instr = std::sync::Arc::clone(&transport);
    let peer_addr_cleanup = peer_addr.clone();
    let peers_cleanup = Arc::clone(peers);
    tokio::spawn(async move {
        match peer_transport_instr.open_uni_erased().await {
            Ok(mut send) => {
                if let Err(e) = run_send_instructions(&mut send, instructions_rx).await {
                    tracing::debug!("Relay peer send-instructions finished: {e}");
                }
            }
            Err(e) => tracing::debug!("Relay peer failed to open send stream: {e}"),
        }
        // Unregister peer when the outgoing stream closes (connection gone).
        peers_cleanup.unregister(&peer_addr_cleanup);
    });

    // Register this peer's transport for bidi bridging.
    // The source→peer accept loop (spawned in run_relay_loop_inner) reads
    // the latest peer transport from the watch channel.
    let _ = peer_transport_tx.send(Some(Arc::clone(&transport)));

    // Peer→source bidi bridge: accept bidi from this peer, open bidi to source, splice.
    let peer_transport_bidi = transport;
    let source_transport_bidi = Arc::clone(source_transport);
    let mut shutdown_bidi = shutdown.clone();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                result = peer_transport_bidi.accept_bi_erased() => {
                    match result {
                        Ok(Some(peer_stream)) => {
                            let source = Arc::clone(&source_transport_bidi);
                            tokio::spawn(async move {
                                match source.open_bi_erased().await {
                                    Ok(source_stream) => {
                                        if let Err(e) = wallhack_core::transport::splice_bi(
                                            peer_stream,
                                            source_stream,
                                        ).await {
                                            tracing::debug!("bidi bridge (peer→source) ended: {e}");
                                        }
                                    }
                                    Err(e) => tracing::debug!("bidi bridge: failed to open source stream: {e}"),
                                }
                            });
                        }
                        Ok(None) => break,
                        Err(e) => {
                            tracing::debug!("bidi bridge: peer accept_bi error: {e}");
                        }
                    }
                }
                _ = shutdown_bidi.changed() => break,
            }
        }
    });

    crate::transport::relay_bridge_channels(
        &peer_addr,
        instructions_tx,
        responses_rx,
        control_tx,
        source_resp_tx,
        fanout_register_tx,
    );
}
