//! Auto-negotiation mode.
//!
//! Neither `--entry`, `--exit`, nor `--relay` is specified. The node detects
//! its own capabilities, exchanges a `Handshake` with the peer, and
//! independently derives the correct role via the pure `negotiate()` function.
//!
//! **If both `--connect` and `--listen` are provided**: the node is a relay.
//! No negotiation is needed — connectivity alone determines the role.
//!
//! **Single direction**: the node connects or listens, exchanges the
//! handshake, and dispatches to the negotiated role. Indeterminate means the
//! node logs the reason and holds the connection open until it drops.

use std::{sync::Arc, time::Duration};

use wallhack_core::{
    NodeRole,
    client::client::ConnectResult,
    control::{metrics::Metrics, peers::Registry, routes::SharedRouteTable},
    entry::manager::ConnectionManager,
    exit::{net::SyscallExitAdapter, orchestrator::Orchestrator},
    negotiate::{NegotiationResult, negotiate},
    server::server::{Server, ServerOptions},
    transport::protocol,
};
use wallhack_wire::data::{Capabilities, Handshake};

use crate::{
    NodeError,
    address_spec::{AddressSpec, Protocol},
    config::SecurityParams,
    daemon_config::{AutoConfig, GlobalConfig, RelayConfig},
    tun_cap::detect_tun_capable,
};

/// Reconnect delay for auto-connector sessions.
const RECONNECT_DELAY: Duration = Duration::from_millis(500);

/// Run in auto-negotiation mode.
///
/// # Errors
///
/// Returns error if the connection setup fails non-retryably.
pub(crate) async fn run(
    global: &GlobalConfig,
    cfg: &AutoConfig,
    metrics: Arc<Metrics>,
    peers: Arc<Registry>,
    routes: SharedRouteTable,
) -> Result<(), NodeError> {
    let tun_capable = detect_tun_capable();
    tracing::info!(
        "Auto-negotiation: tun_capable={tun_capable}, connect={:?}, listen={:?}",
        cfg.connect,
        cfg.listen,
    );

    match (&cfg.connect, &cfg.listen) {
        (Some(connect), Some(listen)) => {
            // Both connect and listen → relay role (no negotiation needed).
            tracing::info!("Both --connect and --listen: running as relay");
            let relay_cfg = RelayConfig {
                name: cfg.name.clone(),
                connect: connect.clone(),
                listen: listen.clone(),
                accept_fingerprint: cfg.accept_fingerprint.clone(),
            };
            super::relay::run(global, &relay_cfg, metrics).await
        }
        (Some(connect), None) => {
            run_auto_connector(global, cfg, connect, tun_capable, metrics, peers, routes).await
        }
        (None, Some(listen)) => {
            run_auto_listener(global, cfg, listen, tun_capable, metrics, peers, routes).await
        }
        (None, None) => Err(NodeError::Config(
            "auto mode requires --connect or --listen".into(),
        )),
    }
}

/// Build a local `Handshake` for capability advertisement.
fn build_local_handshake(
    name: &str,
    tun_capable: bool,
    listening: bool,
    connecting: bool,
) -> Handshake {
    Handshake {
        capabilities: Some(Capabilities {
            tun_capable,
            listening,
            connecting,
        }),
        name: name.to_string(),
        version: crate::built_info::PKG_VERSION.to_string(),
        psk_proof: Vec::new(),
        routes: Vec::new(),
        hint: None,
    }
}

// ============================================================================
// Connector path
// ============================================================================

/// Auto connector: connect to a peer, negotiate role, run the session.
async fn run_auto_connector(
    global: &GlobalConfig,
    cfg: &AutoConfig,
    spec: &AddressSpec,
    tun_capable: bool,
    metrics: Arc<Metrics>,
    peers: Arc<Registry>,
    _routes: SharedRouteTable,
) -> Result<(), NodeError> {
    let local_hs = build_local_handshake(&cfg.name, tun_capable, false, true);

    tracing::info!("Auto connector: connecting to {}...", spec.addr);
    let endpoint =
        crate::transport::resolve_endpoint(&spec.addr, global.dns_server.as_deref()).await?;
    let peer_addr = endpoint.to_string();

    let security = SecurityParams {
        psk: global.psk.clone(),
        accept_fingerprint: cfg.accept_fingerprint.clone(),
    };

    match spec.protocol {
        Protocol::Udp => {
            #[cfg(feature = "quic")]
            {
                let client_config = crate::config::build_quic_client_config(
                    global,
                    endpoint,
                    Some(cfg.name.clone()),
                    &security,
                    Some(local_hs.clone()),
                );
                let lhs = local_hs;
                crate::transport::connect_loop(
                    || {
                        let cfg = client_config.clone();
                        async move {
                            use wallhack_core::client::client::Client;
                            let mut client = wallhack_core::client::quic::QuicClient::try_new(cfg)?;
                            client.connect(NodeRole::Indeterminate).await
                        }
                    },
                    |connect_result| {
                        let metrics = Arc::clone(&metrics);
                        let peers = Arc::clone(&peers);
                        let pa = peer_addr.clone();
                        let lhs = lhs.clone();
                        async move {
                            run_auto_connect_session(connect_result, &lhs, &pa, metrics, peers)
                                .await
                        }
                    },
                    RECONNECT_DELAY,
                )
                .await
            }
            #[cfg(not(feature = "quic"))]
            Err(NodeError::TransportUnavailable("quic"))
        }
        Protocol::Tcp => {
            #[cfg(feature = "websocket")]
            {
                let client_config = crate::config::build_ws_client_config(
                    global,
                    endpoint,
                    Some(cfg.name.clone()),
                    &security,
                    Some(local_hs.clone()),
                );
                let lhs = local_hs;
                crate::transport::connect_loop(
                    || {
                        let cfg = client_config.clone();
                        async move {
                            let mut client = wallhack_core::client::ws::WsClient::new(cfg)?;
                            client.connect(NodeRole::Indeterminate).await
                        }
                    },
                    |connect_result| {
                        let metrics = Arc::clone(&metrics);
                        let peers = Arc::clone(&peers);
                        let pa = peer_addr.clone();
                        let lhs = lhs.clone();
                        async move {
                            run_auto_connect_session(connect_result, &lhs, &pa, metrics, peers)
                                .await
                        }
                    },
                    RECONNECT_DELAY,
                )
                .await
            }
            #[cfg(not(feature = "websocket"))]
            Err(NodeError::TransportUnavailable("websocket"))
        }
    }
}

/// Drive one auto-connector session: await peer handshake, negotiate, dispatch.
async fn run_auto_connect_session<T: wallhack_core::transport::Transport + 'static>(
    mut connect_result: ConnectResult<T>,
    local_hs: &Handshake,
    peer_addr: &str,
    metrics: Arc<Metrics>,
    peers: Arc<Registry>,
) -> Result<(), NodeError> {
    // Wait for the peer's Handshake (delivered via the control loop oneshot).
    let Some(rx) = connect_result.take_peer_handshake_rx() else {
        tracing::warn!("No peer handshake receiver in ConnectResult");
        return Ok(());
    };
    let peer_hs = match tokio::time::timeout(Duration::from_secs(30), rx).await {
        Ok(Ok(hs)) => hs,
        Ok(Err(_)) => {
            tracing::warn!("Peer handshake channel closed before delivery");
            return Ok(());
        }
        Err(_) => {
            tracing::warn!("Timed out waiting for peer handshake");
            return Ok(());
        }
    };

    let result = negotiate(local_hs, &peer_hs);
    tracing::info!("Auto-negotiation result: {result}");

    match result {
        NegotiationResult::Resolved(NodeRole::Entry) => {
            tracing::info!("Negotiated role: entry");
            // Spawn the outgoing data task (send instructions to exit peer).
            // The client connected with Indeterminate so no outgoing task was
            // spawned; we create it here before calling run_entry_connected.
            let instructions_rx = connect_result.channels().0.subscribe();
            let transport_out = connect_result.transport();
            tokio::spawn(async move {
                match transport_out.open_uni().await {
                    Ok(mut send) => {
                        if let Err(e) =
                            protocol::run_send_instructions(&mut send, instructions_rx).await
                        {
                            tracing::debug!("Auto entry send-instructions finished: {e}");
                        }
                    }
                    Err(e) => tracing::debug!("Auto entry failed to open send stream: {e}"),
                }
            });
            super::entry::run_entry_connected(connect_result, &metrics, false, peer_addr).await
        }
        NegotiationResult::Resolved(NodeRole::Exit) => {
            tracing::info!("Negotiated role: exit");
            // Spawn the outgoing data task (send responses to entry peer).
            let responses_rx = connect_result.channels().1.subscribe();
            let transport_out = connect_result.transport();
            tokio::spawn(async move {
                match transport_out.open_uni().await {
                    Ok(mut send) => {
                        if let Err(e) = protocol::run_send_responses(&mut send, responses_rx).await
                        {
                            tracing::debug!("Auto exit send-responses finished: {e}");
                        }
                    }
                    Err(e) => tracing::debug!("Auto exit failed to open send stream: {e}"),
                }
            });
            run_auto_exit_session(connect_result, peer_addr, &metrics, &peers).await
        }
        NegotiationResult::Resolved(NodeRole::Relay) => {
            // Non-Both connector resolving to relay is unexpected but valid
            // (e.g. both sides advertise relay). Log and hold.
            tracing::warn!("Unexpected relay negotiation for connector-only mode; holding");
            hold_until_disconnect(connect_result).await;
            Ok(())
        }
        NegotiationResult::Resolved(NodeRole::Indeterminate)
        | NegotiationResult::Indeterminate { .. } => {
            tracing::info!("Role is indeterminate: {result}; holding connection");
            hold_until_disconnect(connect_result).await;
            Ok(())
        }
    }
}

/// Hold a `ConnectResult` open until the peer disconnects (or control dies).
async fn hold_until_disconnect<T: wallhack_core::transport::Transport + 'static>(
    connect_result: ConnectResult<T>,
) {
    let (_, mut tasks, _control_tx) = connect_result.into_parts();
    // Only watch incoming and control — the outgoing task is a no-op for
    // Indeterminate connections and completes immediately.
    tokio::select! {
        _ = &mut tasks.incoming => tracing::debug!("Indeterminate: incoming task completed"),
        _ = &mut tasks.control  => tracing::debug!("Indeterminate: control task completed"),
    }
}

/// Run the exit role on an already-connected transport (no `tasks.wait_for_disconnect`).
async fn run_auto_exit_session<T: wallhack_core::transport::Transport + 'static>(
    connect_result: ConnectResult<T>,
    peer_addr: &str,
    metrics: &Arc<Metrics>,
    peers: &Arc<Registry>,
) -> Result<(), NodeError> {
    peers.register(
        peer_addr.to_string(),
        peer_addr.to_string(),
        NodeRole::Entry,
    );

    let adapter = SyscallExitAdapter::new();
    let _reaper = adapter.start_reaper(
        std::time::Duration::from_mins(1),
        std::time::Duration::from_mins(5),
    );
    let orchestrator = Orchestrator::new(Arc::new(adapter), Arc::clone(metrics));

    let transport = connect_result.transport();
    let ((instr, resp), _tasks, _control_tx) = connect_result.into_parts();
    let stream_fut = super::exit::run_stream_listener(transport);
    let drive_fut = orchestrator.drive(resp, instr.subscribe());

    tokio::pin!(stream_fut);
    tokio::pin!(drive_fut);

    tokio::select! {
        result = &mut drive_fut => {
            if let Err(e) = result { tracing::debug!("Auto exit orchestrator: {e}"); }
        }
        result = &mut stream_fut => {
            if let Err(e) = result { tracing::warn!("Auto exit stream handler: {e}"); }
        }
    }

    peers.unregister(peer_addr);
    Ok(())
}

// ============================================================================
// Listener path
// ============================================================================

/// Auto listener: accept connections, negotiate role, dispatch.
async fn run_auto_listener(
    global: &GlobalConfig,
    cfg: &AutoConfig,
    spec: &AddressSpec,
    tun_capable: bool,
    metrics: Arc<Metrics>,
    peers: Arc<Registry>,
    routes: SharedRouteTable,
) -> Result<(), NodeError> {
    let local_hs = build_local_handshake(&cfg.name, tun_capable, true, false);

    let addr: std::net::SocketAddr = spec.addr.parse::<crate::net::ListenAddr>()?.into();
    let server_options = ServerOptions {
        handler_config: wallhack_core::control::handler::HandlerConfig::new(
            NodeRole::Indeterminate,
            crate::built_info::PKG_NAME.to_string(),
            crate::built_info::PKG_VERSION.to_string(),
        ),
        metrics: Some(Arc::clone(&metrics)),
        peers: Some(Arc::clone(&peers)),
        routes: Some(Arc::clone(&routes)),
        local_handshake: Some(local_hs.clone()),
    };
    let server_config =
        crate::config::build_server_config(&global.tls, addr, global.psk.clone(), None);

    match spec.protocol {
        Protocol::Udp => {
            #[cfg(feature = "quic")]
            {
                let server =
                    wallhack_core::server::quic::QuicServer::try_new(server_config, server_options)
                        .map_err(|e| NodeError::Transport(Box::new(e)))?;
                run_auto_accept_loop(server, local_hs, global.psk.clone(), metrics, peers, routes)
                    .await
            }
            #[cfg(not(feature = "quic"))]
            Err(NodeError::TransportUnavailable("quic"))
        }
        Protocol::Tcp => {
            #[cfg(feature = "websocket")]
            {
                let server = wallhack_core::server::ws::WebSocketServer::try_new(
                    server_config,
                    server_options,
                )?;
                run_auto_accept_loop(server, local_hs, global.psk.clone(), metrics, peers, routes)
                    .await
            }
            #[cfg(not(feature = "websocket"))]
            Err(NodeError::TransportUnavailable("websocket"))
        }
    }
}

/// Accept loop for auto-negotiation listener.
async fn run_auto_accept_loop<S: Server>(
    mut server: S,
    local_hs: Handshake,
    server_psk: Option<zeroize::Zeroizing<String>>,
    metrics: Arc<Metrics>,
    peers: Arc<Registry>,
    routes: SharedRouteTable,
) -> Result<(), NodeError>
where
    S::Error: std::error::Error + Send + Sync + 'static,
    S::Transport: Send + Sync + 'static,
{
    let local_addr = server.local_addr()?;
    tracing::info!(
        "Auto listener: listening on {local_addr} ({})",
        server.protocol_name()
    );
    if server.psk().is_none() {
        tracing::warn!(
            "No authentication configured. Use --psk <SECRET> to require authentication."
        );
    }

    loop {
        match server.accept(NodeRole::Indeterminate).await {
            Ok(Some(accept_result)) => {
                let peer_addr = accept_result.peer_addr().to_string();
                let local_hs = local_hs.clone();
                let server_psk = server_psk.clone();
                let metrics = Arc::clone(&metrics);
                let peers = Arc::clone(&peers);
                let routes = Arc::clone(&routes);
                tokio::spawn(async move {
                    if let Err(e) = run_auto_accept_session(
                        accept_result,
                        local_hs,
                        server_psk,
                        metrics,
                        peers,
                        routes,
                        peer_addr,
                    )
                    .await
                    {
                        tracing::warn!("Auto accept session error: {e}");
                    }
                });
            }
            Ok(None) => {
                tracing::info!("Auto listener: server closed");
                break;
            }
            Err(e) => {
                tracing::warn!("Auto listener: accept error: {e}");
            }
        }
    }

    Ok(())
}

/// Drive one accepted auto-listener session.
#[allow(clippy::too_many_lines)] // negotiation dispatch with symmetric entry/exit arms
async fn run_auto_accept_session<T: wallhack_core::transport::Transport + 'static>(
    mut accept_result: wallhack_core::server::server::AcceptResult<T>,
    local_hs: Handshake,
    server_psk: Option<zeroize::Zeroizing<String>>,
    metrics: Arc<Metrics>,
    peers: Arc<Registry>,
    _routes: SharedRouteTable,
    peer_addr: String,
) -> Result<(), NodeError> {
    // PSK validation: verify the peer's proof matches the server PSK.
    if let Some(ref psk) = server_psk {
        let channel_binding = accept_result.channel_binding().copied();
        if let Some(hs) = accept_result.peer_handshake() {
            use wallhack_core::psk::HandshakeExt as _;
            let valid = channel_binding
                .as_ref()
                .is_some_and(|b| hs.verify_psk_proof(psk.as_bytes(), b));
            if !valid {
                tracing::warn!("Peer {peer_addr} failed PSK authentication, dropping");
                return Err(NodeError::PskAuth(
                    accept_result
                        .peer_handshake()
                        .map_or_else(|| peer_addr.clone(), |h| h.name.clone()),
                ));
            }
        }
    }

    let Some(peer_hs) = accept_result.take_peer_handshake() else {
        tracing::warn!("No peer handshake from {peer_addr}; cannot negotiate");
        return Ok(());
    };

    let result = negotiate(&local_hs, &peer_hs);
    tracing::info!("Auto listener {peer_addr}: negotiation result: {result}");

    match result {
        NegotiationResult::Resolved(NodeRole::Entry) => {
            tracing::info!("Negotiated role: entry (listener side)");
            let transport = accept_result.transport();
            let ((instructions_tx, responses_tx), control_tx) = accept_result.channels();

            // Spawn data tasks: incoming (peer→broadcasts) + outgoing (instructions→peer).
            super::entry::spawn_data_tasks(&transport, &instructions_tx, &responses_tx);

            let tun_name = super::entry::SessionManager::create_anonymous();
            let actor = super::entry::create_tun_with_retry(tun_name.clone()).await?;

            tracing::info!("Peer connected: {peer_addr} (tun: {tun_name})");

            let responses_rx = responses_tx.subscribe();
            drop(responses_tx);
            let (manager, _) = ConnectionManager::new(
                actor,
                Arc::clone(&transport),
                Arc::clone(&metrics),
                false,
                instructions_tx,
                responses_rx,
            );
            let _keep_alive = control_tx;
            peers.register(peer_addr.clone(), peer_addr.clone(), NodeRole::Exit);

            let handle = tokio::spawn(async move { manager.run().await });
            match handle.await {
                Ok(Ok(())) => tracing::info!("Auto entry session closed: {peer_addr}"),
                Ok(Err(e)) => tracing::warn!("Auto entry session error {peer_addr}: {e}"),
                Err(e) => tracing::warn!("Auto entry session task failed {peer_addr}: {e}"),
            }
            peers.unregister(&peer_addr);
        }
        NegotiationResult::Resolved(NodeRole::Exit) => {
            tracing::info!("Negotiated role: exit (listener side)");
            let transport = accept_result.transport();
            let ((instructions_tx, responses_tx), control_tx) = accept_result.channels();

            // Spawn data tasks for exit: incoming (peer→broadcasts) + outgoing (responses→peer).
            let transport_in = Arc::clone(&transport);
            let instructions_in = instructions_tx.clone();
            let responses_in = responses_tx.clone();
            tokio::spawn(async move {
                match transport_in.accept_uni().await {
                    Ok(Some(mut recv)) => {
                        if let Err(e) =
                            protocol::run_data_in(&mut recv, &instructions_in, &responses_in).await
                        {
                            tracing::debug!("Auto exit data-in finished: {e}");
                        }
                    }
                    Ok(None) => tracing::debug!("Transport closed before data-in"),
                    Err(e) => tracing::debug!("Failed to accept data-in stream: {e}"),
                }
            });
            let transport_out = Arc::clone(&transport);
            let responses_rx = responses_tx.subscribe();
            tokio::spawn(async move {
                match transport_out.open_uni().await {
                    Ok(mut send) => {
                        if let Err(e) = protocol::run_send_responses(&mut send, responses_rx).await
                        {
                            tracing::debug!("Auto exit send-responses finished: {e}");
                        }
                    }
                    Err(e) => tracing::debug!("Failed to open send stream: {e}"),
                }
            });

            let peer_name = peer_hs.name.clone();
            peers.register(peer_name.clone(), peer_addr.clone(), NodeRole::Entry);

            let adapter = SyscallExitAdapter::new();
            let _reaper = adapter.start_reaper(
                std::time::Duration::from_mins(1),
                std::time::Duration::from_mins(5),
            );
            let orchestrator = Orchestrator::new(Arc::new(adapter), Arc::clone(&metrics));
            let stream_fut = super::exit::run_stream_listener(Arc::clone(&transport));
            let drive_fut = orchestrator.drive(responses_tx, instructions_tx.subscribe());
            let _keep_alive = control_tx;

            tokio::pin!(stream_fut);
            tokio::pin!(drive_fut);

            tokio::select! {
                result = &mut drive_fut => {
                    if let Err(e) = result { tracing::debug!("Auto exit orchestrator: {e}"); }
                }
                result = &mut stream_fut => {
                    if let Err(e) = result { tracing::warn!("Auto exit stream handler: {e}"); }
                }
            }

            peers.unregister(&peer_name);
        }
        NegotiationResult::Resolved(NodeRole::Relay) => {
            tracing::warn!("Unexpected relay negotiation for listener-only mode; holding");
            let (_, control_tx) = accept_result.channels();
            let _keep_alive = control_tx;
            // Hold until transport closes (control_tx keeps the control stream alive).
        }
        NegotiationResult::Resolved(NodeRole::Indeterminate)
        | NegotiationResult::Indeterminate { .. } => {
            tracing::info!("Auto listener {peer_addr}: role is indeterminate: {result}");
            let (_, control_tx) = accept_result.channels();
            let _keep_alive = control_tx;
            // Hold until transport closes.
        }
    }

    Ok(())
}
