//! Auto-negotiation mode.
//!
//! No explicit role is specified. The node detects its own capabilities,
//! exchanges a `Handshake` with the peer, and independently derives the
//! correct role via the pure `negotiate()` function.
//!
//! **If both connect and listen addresses are provided**: the node is a relay.
//! No negotiation is needed — connectivity alone determines the role.
//!
//! **Single direction**: the node connects or listens, exchanges the
//! handshake, and dispatches to the negotiated role. Indeterminate means the
//! node logs the reason and holds the connection open until it drops.

use std::{sync::Arc, time::Duration};

use wallhack_core::{
    NodeRole,
    control::{
        handler::SharedNodeState,
        metrics::Metrics,
        peers::{ConnectionSide, Registry},
        routes::SharedRouteTable,
    },
    entry::manager::ConnectionManager,
    exit::{net::SyscallExitAdapter, orchestrator::Orchestrator},
    negotiate::{NegotiationResult, negotiate},
    server::server::{DataChannels, Server, ServerOptions},
    transport::{ErasedTransport, Transport, protocol},
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
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run(
    global: &GlobalConfig,
    cfg: &AutoConfig,
    metrics: Arc<Metrics>,
    peers: Arc<Registry>,
    routes: SharedRouteTable,
    route_updates: tokio::sync::broadcast::Receiver<wallhack_core::control::routes::RouteUpdate>,
    route_updates_tx: tokio::sync::broadcast::Sender<wallhack_core::control::routes::RouteUpdate>,
    node_state: SharedNodeState,
) -> Result<(), NodeError> {
    let tun_capable = detect_tun_capable();
    let connect_display = cfg
        .connect
        .as_ref()
        .map_or("none".to_string(), |c| c.addr.clone());
    let listen_display = cfg
        .listen
        .as_ref()
        .map_or("none".to_string(), |l| l.addr.clone());
    tracing::info!(
        "Capabilities: tun={tun_capable}, connect={connect_display}, listen={listen_display}",
    );

    // Set initial capabilities; role stays Indeterminate until negotiation.
    node_state.update_capabilities(Capabilities {
        tun_capable,
        listening: cfg.listen.is_some(),
        connecting: cfg.connect.is_some(),
    });

    match (&cfg.connect, &cfg.listen) {
        (Some(connect), Some(listen)) => {
            // Both connect and listen → relay role (no negotiation needed).
            tracing::info!("Both connect and listen addresses provided: running as relay");
            node_state.update_role(NodeRole::Relay);
            let relay_cfg = RelayConfig {
                name: cfg.name.clone(),
                connect: connect.clone(),
                listen: listen.clone(),
                accept_fingerprint: cfg.accept_fingerprint.clone(),
            };
            super::relay::run(global, &relay_cfg, metrics, node_state).await
        }
        (Some(connect), None) => {
            run_auto_connector(
                global,
                cfg,
                connect,
                tun_capable,
                metrics,
                peers,
                routes,
                route_updates,
                node_state,
            )
            .await
        }
        (None, Some(listen)) => {
            run_auto_listener(
                global,
                cfg,
                listen,
                tun_capable,
                metrics,
                peers,
                routes,
                route_updates,
                route_updates_tx,
                node_state,
            )
            .await
        }
        (None, None) => Err(NodeError::Config(
            "auto mode requires a connect or listen address".into(),
        )),
    }
}

/// Build a local `Handshake` for capability advertisement.
///
/// Always populates `routes` with locally-routable CIDRs so that a peer
/// resolving to Entry can install OS routes automatically.
fn build_local_handshake(
    cfg: &AutoConfig,
    version: &str,
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
        name: cfg.name.clone(),
        version: version.to_string(),
        psk_proof: Vec::new(),
        routes: crate::netlink::enumerate_local_cidrs(),
        hint: cfg.hint,
    }
}

/// Returns `true` if a CIDR is safe to install as a forwarding route.
///
/// Rejects default routes (`prefix_len` == 0), loopback, link-local,
/// unspecified, and multicast destinations.
fn is_routable_cidr(cidr: &wallhack_core::Cidr) -> bool {
    use std::net::IpAddr;
    let addr = cidr.addr();
    cidr.prefix_len() > 0
        && !addr.is_loopback()
        && !match addr {
            IpAddr::V4(a) => a.is_link_local(),
            IpAddr::V6(a) => {
                let o = a.octets();
                o[0] == 0xfe && (o[1] & 0xc0) == 0x80
            }
        }
        && !addr.is_unspecified()
        && !addr.is_multicast()
}

/// Add routes advertised in a peer's handshake to the route table.
///
/// Invalid or non-routable CIDRs are skipped with a log message. Callers are
/// responsible for removing auto-managed routes (via `remove_auto_by_peer`)
/// when the peer disconnects.
fn install_advertised_routes(
    routes: &wallhack_core::control::routes::SharedRouteTable,
    peer_name: &str,
    advertised: &[String],
) {
    let mut installed = 0usize;
    for cidr_str in advertised {
        match cidr_str.parse::<wallhack_core::Cidr>() {
            Ok(cidr) if is_routable_cidr(&cidr) => {
                routes.add_auto(cidr, peer_name.to_string());
                installed += 1;
            }
            Ok(_) => tracing::debug!("Skipping non-routable advertised CIDR: {cidr_str}"),
            Err(e) => tracing::warn!("Ignoring invalid advertised CIDR {cidr_str:?}: {e}"),
        }
    }
    if installed > 0 {
        tracing::info!("Installed {installed} auto route(s) advertised by {peer_name}");
    }
}

// ============================================================================
// Connector path
// ============================================================================

/// Auto connector: connect to a peer, negotiate role, run the session.
#[allow(clippy::too_many_lines, clippy::too_many_arguments)]
async fn run_auto_connector(
    global: &GlobalConfig,
    cfg: &AutoConfig,
    spec: &AddressSpec,
    tun_capable: bool,
    metrics: Arc<Metrics>,
    peers: Arc<Registry>,
    routes: SharedRouteTable,
    route_updates: tokio::sync::broadcast::Receiver<wallhack_core::control::routes::RouteUpdate>,
    node_state: SharedNodeState,
) -> Result<(), NodeError> {
    let local_hs = build_local_handshake(cfg, &global.version, tun_capable, false, true);

    tracing::info!("Auto connector: connecting to {}...", spec.addr);
    let endpoint =
        crate::transport::resolve_endpoint(&spec.addr, global.dns_server.as_deref()).await?;
    let peer_addr = endpoint.to_string();

    let security = SecurityParams {
        psk: global.psk.clone(),
        accept_fingerprint: cfg.accept_fingerprint.clone(),
    };

    // route_updates is a Receiver. We need to pass fresh receivers to the loop.

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
                let ru = route_updates.resubscribe();
                crate::transport::connect_loop(
                    || {
                        let cfg = client_config.clone();
                        async move {
                            use wallhack_core::client::client::Client;
                            let mut client = wallhack_core::client::quic::QuicClient::try_new(cfg)?;
                            client.connect(NodeRole::Indeterminate).await
                        }
                    },
                    move |connect_result| {
                        // erase() is sync — runs before async move captures anything generic
                        let e = connect_result.erase();
                        let metrics = Arc::clone(&metrics);
                        let peers = Arc::clone(&peers);
                        let pa = peer_addr.clone();
                        let lhs = lhs.clone();
                        let ns = node_state.clone();
                        let r = Arc::clone(&routes);
                        let ru = ru.resubscribe();
                        async move {
                            let peer_handshake_rx = e.peer_handshake_rx;
                            let transport = e.transport;
                            let channels = e.channels;
                            let tasks = e.tasks;
                            let control_tx = e.control_tx;
                            run_auto_connect_session_dispatch(
                                peer_handshake_rx,
                                transport,
                                channels,
                                tasks,
                                control_tx,
                                &lhs,
                                &pa,
                                metrics,
                                peers,
                                ns,
                                Some(r),
                                Some(ru),
                            )
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
                let ru = route_updates.resubscribe();
                crate::transport::connect_loop(
                    || {
                        let cfg = client_config.clone();
                        async move {
                            let mut client = wallhack_core::client::ws::WsClient::new(cfg)?;
                            client.connect(NodeRole::Indeterminate).await
                        }
                    },
                    move |connect_result| {
                        // erase() is sync — runs before async move captures anything generic
                        let e = connect_result.erase();
                        let metrics = Arc::clone(&metrics);
                        let peers = Arc::clone(&peers);
                        let pa = peer_addr.clone();
                        let lhs = lhs.clone();
                        let ns = node_state.clone();
                        let r = Arc::clone(&routes);
                        let ru = ru.resubscribe();
                        async move {
                            let peer_handshake_rx = e.peer_handshake_rx;
                            let transport = e.transport;
                            let channels = e.channels;
                            let tasks = e.tasks;
                            let control_tx = e.control_tx;
                            run_auto_connect_session_dispatch(
                                peer_handshake_rx,
                                transport,
                                channels,
                                tasks,
                                control_tx,
                                &lhs,
                                &pa,
                                metrics,
                                peers,
                                ns,
                                Some(r),
                                Some(ru),
                            )
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

/// Non-generic auto-connector dispatch: negotiates role and runs the session.
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
async fn run_auto_connect_session_dispatch(
    peer_handshake_rx: Option<tokio::sync::oneshot::Receiver<Handshake>>,
    transport: Arc<dyn ErasedTransport>,
    channels: DataChannels,
    tasks: wallhack_core::client::client::ConnectionTasks,
    control_tx: tokio::sync::mpsc::Sender<wallhack_wire::control::ControlMessage>,
    local_hs: &Handshake,
    peer_addr: &str,
    metrics: Arc<Metrics>,
    peers: Arc<Registry>,
    node_state: SharedNodeState,
    routes: Option<SharedRouteTable>,
    route_updates: Option<
        tokio::sync::broadcast::Receiver<wallhack_core::control::routes::RouteUpdate>,
    >,
) -> Result<(), NodeError> {
    let DataChannels {
        instructions_tx,
        instructions_rx,
        responses_tx,
        responses_rx,
    } = channels;

    // Wait for the peer's Handshake (delivered via the control loop oneshot).
    let Some(rx) = peer_handshake_rx else {
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

    let negotiated_role = match &result {
        NegotiationResult::Resolved(role) => *role,
        NegotiationResult::Indeterminate { .. } => NodeRole::Indeterminate,
    };
    node_state.update_role(negotiated_role);

    match result {
        NegotiationResult::Resolved(NodeRole::Entry) => {
            tracing::info!(
                "Role resolved: name={} addr={peer_addr} role=entry",
                peer_hs.name,
            );
            node_state.update_role(NodeRole::Entry);

            // Install routes advertised by the exit peer. The inner function
            // applies routes from the table when it creates the TUN, so they
            // must be in the table before we call it.
            let peer_name = peer_hs.name.as_str();
            let routes_for_cleanup = routes.as_ref().map(Arc::clone);
            let tun_name_for_cleanup = if peer_name.is_empty() {
                None
            } else {
                Some(super::entry::peer_name_to_iface(peer_name))
            };
            if let Some(ref r) = routes
                && !peer_name.is_empty()
            {
                install_advertised_routes(r, peer_name, &peer_hs.routes);
            }

            drop(tasks);
            let result = super::entry::run_entry_connected_inner(
                transport,
                instructions_tx,
                instructions_rx,
                responses_rx,
                control_tx,
                &metrics,
                peer_addr,
                Some(peer_name),
                routes,
                route_updates,
            )
            .await;

            // Remove auto-managed routes and their OS entries now that the
            // session has ended.
            if let (Some(r), Some(tun)) = (routes_for_cleanup, tun_name_for_cleanup) {
                let removed = r.remove_auto_by_peer(peer_name);
                if !removed.is_empty() {
                    tracing::info!(
                        "Removing {} auto route(s) for disconnected exit {peer_name}",
                        removed.len()
                    );
                    for entry in &removed {
                        let _ = crate::netlink::remove_os_route(&entry.cidr.to_string(), &tun);
                    }
                }
            }

            result
        }
        NegotiationResult::Resolved(NodeRole::Exit) => {
            tracing::info!(
                "Role resolved: name={} addr={peer_addr} role=exit",
                peer_hs.name,
            );
            node_state.update_role(NodeRole::Exit);
            let peer_name = if peer_hs.name.is_empty() {
                peer_addr.to_string()
            } else {
                peer_hs.name
            };
            // Spawn the outgoing data task (send responses to entry peer).
            let transport_out = Arc::clone(&transport);
            tokio::spawn(async move {
                match transport_out.open_uni_erased().await {
                    Ok(mut send) => {
                        if let Err(e) = protocol::run_send_responses(&mut send, responses_rx).await
                        {
                            tracing::debug!("Auto exit send-responses finished: {e}");
                        }
                    }
                    Err(e) => tracing::debug!("Auto exit failed to open send stream: {e}"),
                }
            });
            drop(tasks);
            run_auto_exit_session_inner(
                transport,
                instructions_rx,
                responses_tx,
                control_tx,
                &peer_name,
                peer_addr,
                &metrics,
                &peers,
            )
            .await
        }
        NegotiationResult::Resolved(NodeRole::Relay) => {
            tracing::warn!("Unexpected relay negotiation for connector-only mode; holding");
            hold_until_disconnect(tasks, control_tx, node_state).await;
            Ok(())
        }
        NegotiationResult::Resolved(NodeRole::Indeterminate)
        | NegotiationResult::Indeterminate { .. } => {
            tracing::info!("Role is indeterminate: {result}; holding connection");
            hold_until_disconnect(tasks, control_tx, node_state).await;
            Ok(())
        }
    }
}

/// Hold connection tasks open until the peer disconnects (or control dies).
async fn hold_until_disconnect(
    mut tasks: wallhack_core::client::client::ConnectionTasks,
    _control_tx: tokio::sync::mpsc::Sender<wallhack_wire::control::ControlMessage>,
    _node_state: SharedNodeState,
) {
    tokio::select! {
        _ = &mut tasks.incoming => {
            tracing::debug!("Indeterminate: incoming task completed");
        }
        _ = &mut tasks.control => {
            tracing::debug!("Indeterminate: control task completed");
        }
    }
}

/// Non-generic exit session handler for the auto-connector path.
#[allow(clippy::too_many_arguments)]
async fn run_auto_exit_session_inner(
    transport: Arc<dyn ErasedTransport>,
    instructions_rx: tokio::sync::mpsc::Receiver<wallhack_wire::data::EntryNodeInstruction>,
    responses_tx: tokio::sync::mpsc::Sender<wallhack_wire::data::ExitNodeResponse>,
    _control_tx: tokio::sync::mpsc::Sender<wallhack_wire::control::ControlMessage>,
    peer_name: &str,
    peer_addr: &str,
    metrics: &Arc<Metrics>,
    peers: &Arc<Registry>,
) -> Result<(), NodeError> {
    peers.register(
        peer_name.to_string(),
        peer_addr.to_string(),
        NodeRole::Entry,
        ConnectionSide::Connect,
    );

    let adapter = SyscallExitAdapter::new();
    let _reaper = adapter.start_reaper(
        std::time::Duration::from_mins(1),
        std::time::Duration::from_mins(5),
    );
    let orchestrator = Orchestrator::new(Arc::new(adapter), Arc::clone(metrics));

    let stream_fut = super::exit::run_stream_listener(transport);
    let drive_fut = orchestrator.drive(responses_tx, instructions_rx);

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

    peers.unregister(peer_name);
    Ok(())
}

// ============================================================================
// Listener path
// ============================================================================

/// Auto listener: accept connections, negotiate role, dispatch.
#[allow(clippy::too_many_arguments)]
async fn run_auto_listener(
    global: &GlobalConfig,
    cfg: &AutoConfig,
    spec: &AddressSpec,
    tun_capable: bool,
    metrics: Arc<Metrics>,
    peers: Arc<Registry>,
    routes: SharedRouteTable,
    route_updates: tokio::sync::broadcast::Receiver<wallhack_core::control::routes::RouteUpdate>,
    route_updates_tx: tokio::sync::broadcast::Sender<wallhack_core::control::routes::RouteUpdate>,
    node_state: SharedNodeState,
) -> Result<(), NodeError> {
    let local_hs = build_local_handshake(cfg, &global.version, tun_capable, true, false);

    let addr: std::net::SocketAddr = spec.addr.parse::<crate::net::ListenAddr>()?.into();
    let server_options = ServerOptions {
        handler_config: wallhack_core::control::handler::HandlerConfig::new(
            NodeRole::Indeterminate,
            "wallhack".to_string(),
            global.version.clone(),
        ),
        metrics: Some(Arc::clone(&metrics)),
        peers: Some(Arc::clone(&peers)),
        routes: Some(Arc::clone(&routes)),
        route_updates: Some(route_updates_tx),
        local_handshake: Some(local_hs.clone()),
    };
    let server_config =
        crate::config::build_server_config(&global.tls, addr, global.psk.clone(), None);

    // route_updates is a Receiver.

    match spec.protocol {
        Protocol::Udp => {
            #[cfg(feature = "quic")]
            {
                let server =
                    wallhack_core::server::quic::QuicServer::try_new(server_config, server_options)
                        .map_err(|e| NodeError::Transport(Box::new(e)))?;
                let bound = server.local_addr()?;
                node_state.set_listen_addr(bound);
                let ru = route_updates.resubscribe();
                let r = Arc::clone(&routes);
                run_auto_accept_loop(
                    server,
                    local_hs,
                    global.psk.clone(),
                    metrics,
                    peers,
                    r,
                    ru,
                    node_state,
                )
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
                let bound = server.local_addr()?;
                node_state.set_listen_addr(bound);
                let ru = route_updates.resubscribe();
                let r = Arc::clone(&routes);
                run_auto_accept_loop(
                    server,
                    local_hs,
                    global.psk.clone(),
                    metrics,
                    peers,
                    r,
                    ru,
                    node_state,
                )
                .await
            }
            #[cfg(not(feature = "websocket"))]
            Err(NodeError::TransportUnavailable("websocket"))
        }
    }
}

/// Accept loop for auto-negotiation listener.
#[allow(clippy::too_many_arguments)]
async fn run_auto_accept_loop<S: Server>(
    mut server: S,
    local_hs: Handshake,
    server_psk: Option<zeroize::Zeroizing<String>>,
    metrics: Arc<Metrics>,
    peers: Arc<Registry>,
    routes: SharedRouteTable,
    route_updates: tokio::sync::broadcast::Receiver<wallhack_core::control::routes::RouteUpdate>,
    node_state: SharedNodeState,
) -> Result<(), NodeError>
where
    S::Error: std::error::Error + Send + Sync + 'static,
    S::Transport: Send + Sync + 'static,
    <S::Transport as Transport>::SendStream: 'static,
    <S::Transport as Transport>::RecvStream: 'static,
    <S::Transport as Transport>::BiStream: Send + 'static,
{
    let local_addr = server.local_addr()?;
    tracing::info!(
        "Auto listener: listening on {local_addr} ({})",
        server.protocol_name()
    );
    if server.psk().is_none() {
        tracing::warn!(
            "No authentication configured. Set a pre-shared key (PSK) to require authentication."
        );
    } else {
        tracing::info!("PSK authentication configured");
    }

    let mut psk_failures = super::PskFailTracker::new();

    loop {
        match server.accept(NodeRole::Indeterminate).await {
            Ok(Some(mut accept_result)) => {
                let peer_addr = accept_result.peer_addr().to_string();

                // PSK validation — must happen before spawning.
                if let Some(ref psk) = server_psk
                    && let Some(hs) = accept_result.peer_handshake()
                {
                    use wallhack_core::psk::HandshakeExt as _;
                    let channel_binding = accept_result.channel_binding().copied();
                    let valid = channel_binding
                        .as_ref()
                        .is_some_and(|b| hs.verify_psk_proof(psk.as_bytes(), b));
                    if !valid {
                        psk_failures.record(&peer_addr);
                        continue;
                    }
                }

                // Extract everything from the generic AcceptResult before spawning
                // so the spawned future is non-generic.
                let peer_hs = accept_result.take_peer_handshake();
                let transport: Arc<dyn ErasedTransport> = accept_result.transport();
                let (
                    DataChannels {
                        instructions_tx,
                        instructions_rx,
                        responses_tx,
                        responses_rx,
                    },
                    control_tx,
                ) = accept_result.into_channels();

                let local_hs = local_hs.clone();
                let metrics = Arc::clone(&metrics);
                let peers = Arc::clone(&peers);
                let routes = Arc::clone(&routes);
                let ru = route_updates.resubscribe();
                let ns = node_state.clone();

                tokio::spawn(async move {
                    if let Err(e) = run_auto_accept_session_inner(
                        transport,
                        instructions_tx,
                        instructions_rx,
                        responses_tx,
                        responses_rx,
                        control_tx,
                        peer_hs,
                        local_hs,
                        metrics,
                        peers,
                        Some(routes),
                        Some(ru),
                        peer_addr,
                        ns,
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

/// Non-generic inner implementation for accepted auto-listener sessions.
///
/// All generic extraction (transport, channels, handshake) happens in the
/// caller before spawning, so this function is monomorphized only once.
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
async fn run_auto_accept_session_inner(
    transport: Arc<dyn ErasedTransport>,
    instructions_tx: tokio::sync::mpsc::Sender<wallhack_wire::data::EntryNodeInstruction>,
    instructions_rx: tokio::sync::mpsc::Receiver<wallhack_wire::data::EntryNodeInstruction>,
    responses_tx: tokio::sync::mpsc::Sender<wallhack_wire::data::ExitNodeResponse>,
    responses_rx: tokio::sync::mpsc::Receiver<wallhack_wire::data::ExitNodeResponse>,
    control_tx: tokio::sync::mpsc::Sender<wallhack_wire::control::ControlMessage>,
    peer_hs: Option<Handshake>,
    local_hs: Handshake,
    metrics: Arc<Metrics>,
    peers: Arc<Registry>,
    routes: Option<SharedRouteTable>,
    route_updates: Option<
        tokio::sync::broadcast::Receiver<wallhack_core::control::routes::RouteUpdate>,
    >,
    peer_addr: String,
    node_state: SharedNodeState,
) -> Result<(), NodeError> {
    let Some(peer_hs) = peer_hs else {
        tracing::warn!("No peer handshake from {peer_addr}; cannot negotiate");
        return Ok(());
    };

    let result = negotiate(&local_hs, &peer_hs);

    let negotiated_role = match &result {
        NegotiationResult::Resolved(role) => *role,
        NegotiationResult::Indeterminate { .. } => NodeRole::Indeterminate,
    };
    node_state.update_role(negotiated_role);

    match result {
        NegotiationResult::Resolved(NodeRole::Entry) => {
            tracing::info!(
                "Role resolved: name={} addr={peer_addr} role=entry",
                peer_hs.name,
            );
            node_state.update_role(NodeRole::Entry);

            // Spawn data tasks: incoming (peer→instructions/responses) + outgoing (instructions→peer).
            super::entry::spawn_data_tasks(
                &transport,
                &instructions_tx,
                &responses_tx,
                instructions_rx,
            );

            let tun_name = if peer_hs.name.is_empty() {
                super::entry::SessionManager::create_anonymous()
            } else {
                super::entry::peer_name_to_iface(&peer_hs.name)
            };
            let actor = super::entry::create_tun_with_retry(tun_name.clone()).await?;

            tracing::info!(
                "Peer connected: name={} addr={peer_addr} tun={tun_name}",
                peer_hs.name,
            );

            // Install routes advertised by the exit peer before applying them
            // to the TUN so the apply block below picks them up in one pass.
            if let Some(ref r) = routes
                && !peer_hs.name.is_empty()
            {
                install_advertised_routes(r, &peer_hs.name, &peer_hs.routes);
            }

            // Apply all routes (user-configured and newly-advertised) to the TUN.
            #[allow(clippy::collapsible_if)]
            if let Some(r) = &routes {
                if !peer_hs.name.is_empty() {
                    for entry in r.list() {
                        if entry.peer == peer_hs.name {
                            let _ =
                                crate::netlink::add_os_route(&entry.cidr.to_string(), &tun_name);
                        }
                    }
                }
            }

            // Spawn route update listener
            if let Some(mut updates) = route_updates {
                let tun = tun_name.clone();
                let peer = if peer_hs.name.is_empty() {
                    None
                } else {
                    Some(peer_hs.name.clone())
                };
                tokio::spawn(async move {
                    tracing::info!(
                        "Route update listener started for peer {} on tun {}",
                        peer.as_deref().unwrap_or("<unknown>"),
                        tun
                    );
                    loop {
                        match updates.recv().await {
                            Ok(wallhack_core::control::routes::RouteUpdate::Add(entry)) => {
                                #[allow(clippy::collapsible_if)]
                                if Some(entry.peer.as_str()) == peer.as_deref() {
                                    if let Err(e) =
                                        crate::netlink::add_os_route(&entry.cidr.to_string(), &tun)
                                    {
                                        tracing::error!("Failed to add OS route: {}", e);
                                    }
                                }
                            }
                            Ok(wallhack_core::control::routes::RouteUpdate::Remove(entry)) =>
                            {
                                #[allow(clippy::collapsible_if)]
                                if Some(entry.peer.as_str()) == peer.as_deref() {
                                    if let Err(e) = crate::netlink::remove_os_route(
                                        &entry.cidr.to_string(),
                                        &tun,
                                    ) {
                                        tracing::error!("Failed to remove OS route: {}", e);
                                    }
                                }
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                                tracing::info!("Route updates channel closed");
                                break;
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                                tracing::warn!(
                                    "Route updates lagged, skipped {} messages",
                                    skipped
                                );
                            }
                        }
                    }
                });
            }

            let (manager, _) = ConnectionManager::new(
                actor,
                Arc::clone(&transport),
                Arc::clone(&metrics),
                instructions_tx,
                responses_rx,
            );
            let keep_alive = control_tx;
            let peer_name = if peer_hs.name.is_empty() {
                peer_addr.clone()
            } else {
                peer_hs.name.clone()
            };
            // The peer connected to us (we accepted), so side=Accept.
            // The peer is an exit/relay node — use role from their handshake capabilities.
            let peer_caps = peer_hs.capabilities.as_ref();
            let peer_role = if peer_caps.is_some_and(|c| c.listening && c.connecting) {
                NodeRole::Relay
            } else {
                NodeRole::Exit
            };
            peers.register(
                peer_name.clone(),
                peer_addr.clone(),
                peer_role,
                ConnectionSide::Accept,
            );

            // Fire an initial ping so latency is populated immediately after accept.
            if let Err(e) = super::entry::send_ping(&keep_alive).await {
                tracing::debug!("Auto accept initial ping failed: {e}");
            }

            let handle = tokio::spawn(async move { manager.run().await });
            match handle.await {
                Ok(Ok(())) => tracing::info!("Auto entry session closed: {peer_name}"),
                Ok(Err(e)) => tracing::warn!("Auto entry session error {peer_name}: {e}"),
                Err(e) => tracing::warn!("Auto entry session task failed {peer_name}: {e}"),
            }
            peers.unregister(&peer_name);

            // Best-effort TUN cleanup after disconnect.
            crate::netlink::delete_tun(&tun_name);

            // Remove auto-managed routes and their OS entries now that the
            // session has ended.
            if let Some(ref r) = routes {
                let removed = r.remove_auto_by_peer(&peer_name);
                if !removed.is_empty() {
                    tracing::info!(
                        "Removing {} auto route(s) for disconnected exit {peer_name}",
                        removed.len()
                    );
                    for entry in &removed {
                        let _ = crate::netlink::remove_os_route(&entry.cidr.to_string(), &tun_name);
                    }
                }
            }
        }
        NegotiationResult::Resolved(NodeRole::Exit) => {
            tracing::info!(
                "Role resolved: name={} addr={peer_addr} role=exit",
                peer_hs.name,
            );
            node_state.update_role(NodeRole::Exit);

            // Spawn data tasks for exit: incoming (peer→broadcasts) + outgoing (responses→peer).
            let transport_in = Arc::clone(&transport);
            let instructions_in = instructions_tx.clone();
            let responses_in = responses_tx.clone();
            tokio::spawn(async move {
                match transport_in.accept_uni_erased().await {
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
            tokio::spawn(async move {
                match transport_out.open_uni_erased().await {
                    Ok(mut send) => {
                        if let Err(e) = protocol::run_send_responses(&mut send, responses_rx).await
                        {
                            tracing::debug!("Auto exit send-responses finished: {e}");
                        }
                    }
                    Err(e) => tracing::debug!("Failed to open send stream: {e}"),
                }
            });

            let peer_name = if peer_hs.name.is_empty() {
                peer_addr.clone()
            } else {
                peer_hs.name.clone()
            };
            // The peer connected to us (we accepted), so side=Accept.
            // We are exit, so the peer that connected is entry.
            peers.register(
                peer_name.clone(),
                peer_addr.clone(),
                NodeRole::Entry,
                ConnectionSide::Accept,
            );

            let adapter = SyscallExitAdapter::new();
            let _reaper = adapter.start_reaper(
                std::time::Duration::from_mins(1),
                std::time::Duration::from_mins(5),
            );
            let orchestrator = Orchestrator::new(Arc::new(adapter), Arc::clone(&metrics));
            let stream_fut = super::exit::run_stream_listener(Arc::clone(&transport));
            let drive_fut = orchestrator.drive(responses_tx, instructions_rx);
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
            let _keep_alive = control_tx;
        }
        NegotiationResult::Resolved(NodeRole::Indeterminate)
        | NegotiationResult::Indeterminate { .. } => {
            tracing::info!("Auto listener {peer_addr}: role is indeterminate: {result}");
            let _keep_alive = control_tx;
        }
    }

    Ok(())
}
