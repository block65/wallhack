//! Entry node implementation.
//!
//! The entry node creates a TUN interface and accepts connections from exit or
//! relay nodes. It can either listen for incoming connections (default) or
//! connect to a remote peer. The daemon is headless — no REPL, no TTY.

use std::{collections::HashMap, sync::Arc};

use parking_lot::Mutex;
use wallhack_core::psk::HandshakeExt;

use wallhack_core::{
    NodeRole,
    control::{
        handler::{HandlerConfig, SharedNodeState},
        metrics::Metrics,
        peers::Registry,
        routes::{RouteUpdate, SharedRouteTable},
    },
    entry::{actor::TunActor, manager::ConnectionManager},
    server::server::{Server, ServerOptions},
    transport::{ErasedTransport, Transport},
};

#[cfg(feature = "http-api")]
use wallhack_core::control::routes::RouteTable;

use crate::{
    NodeError,
    address_spec::{AddressSpec, ConnectivitySpec, Protocol},
    config::SecurityParams,
    daemon_config::{EntryConfig, GlobalConfig},
    netlink::{add_os_route, remove_os_route},
};

/// Derive a stable, unique TUN interface name from a peer name.
///
/// Uses FNV-1a (inline, no crate) to hash the peer name into an 8-char hex
/// suffix. Result is always 10 chars — well within Linux's IFNAMSIZ limit.
pub(crate) fn peer_name_to_iface(peer_name: &str) -> String {
    let mut h: u32 = 2_166_136_261;
    for b in peer_name.bytes() {
        h = h.wrapping_mul(16_777_619) ^ u32::from(b);
    }
    format!("wh{h:08x}")
}

/// Shared node resources passed through the entry server call stack.
pub(crate) struct EntryResources {
    pub metrics: Arc<Metrics>,
    pub peers: Arc<Registry>,
    pub routes: SharedRouteTable,
    pub route_updates:
        tokio::sync::broadcast::Receiver<wallhack_core::control::routes::RouteUpdate>,
    pub route_updates_tx:
        tokio::sync::broadcast::Sender<wallhack_core::control::routes::RouteUpdate>,
    pub node_state: SharedNodeState,
    pub sessions: SessionManager,
}

/// Manages TUN sessions for connected exit nodes.
///
/// Keeps TUN adapters alive between reconnections so exit nodes can reconnect
/// without losing their TUN interface.
#[derive(Clone, Default)]
pub(crate) struct SessionManager {
    sessions: Arc<Mutex<HashMap<String, String>>>,
}

impl SessionManager {
    /// Gets or creates a TUN adapter for the given exit node.
    ///
    /// If the exit node has connected before, returns a clone of their existing
    /// TUN. Otherwise creates a new TUN with a stable name derived from the
    /// peer name via FNV-1a hash (fits IFNAMSIZ, unique per peer).
    fn get_or_create(&self, name: &str) -> String {
        let mut sessions = self.sessions.lock();

        if let Some(existing) = sessions.get(name) {
            tracing::info!("Reusing existing TUN for exit node {}", existing);
            return existing.clone();
        }

        let tun_name = peer_name_to_iface(name);
        tracing::info!("Creating new TUN {} for exit node {}", tun_name, name);
        sessions.insert(name.to_string(), tun_name.clone());
        tun_name
    }

    /// Gets a TUN adapter with auto-generated name (for exit nodes without
    /// identity).
    pub(crate) fn create_anonymous() -> String {
        TunActor::random_iface_name()
    }

    /// Look up the TUN device name for a peer.
    fn get_tun_for_peer(&self, peer: &str) -> Option<String> {
        self.sessions.lock().get(peer).cloned()
    }
}

/// Create a TUN device, retrying on EBUSY to handle the race where the
/// previous connection's `TunActor` hasn't been fully dropped yet.
pub(crate) async fn create_tun_with_retry(name: String) -> Result<TunActor, NodeError> {
    let mut attempts = 0;
    loop {
        match TunActor::new(Some(name.clone())) {
            Ok(actor) => return Ok(actor),
            Err(e) if attempts < 3 => {
                attempts += 1;
                tracing::debug!("TUN creation attempt {attempts} failed: {e}, retrying...");
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }
            Err(e) => return Err(e.into()),
        }
    }
}

/// Delay before reconnecting after an established session drops (entry connect mode).
const RECONNECT_DELAY: std::time::Duration = std::time::Duration::from_millis(500);

/// Run as an entry node (headless daemon).
///
/// Creates TUN interface and either listens for peer connections or
/// connects to a remote peer.
///
/// # Errors
///
/// Returns error if server or connection setup fails.
#[allow(clippy::too_many_arguments)]
pub async fn run(
    global: &GlobalConfig,
    cfg: &EntryConfig,
    metrics: Arc<Metrics>,
    peers: Arc<Registry>,
    routes: SharedRouteTable,
    route_updates: tokio::sync::broadcast::Receiver<wallhack_core::control::routes::RouteUpdate>,
    route_updates_tx: tokio::sync::broadcast::Sender<wallhack_core::control::routes::RouteUpdate>,
    node_state: SharedNodeState,
) -> Result<(), NodeError> {
    // Set capabilities that are known at startup (TUN detection).
    node_state.update_capabilities(wallhack_wire::data::Capabilities {
        tun_capable: crate::tun_cap::detect_tun_capable(),
        listening: false,
        connecting: false,
    });

    let res = EntryResources {
        sessions: SessionManager::default(),
        metrics,
        peers,
        routes,
        route_updates,
        route_updates_tx,
        node_state,
    };

    match &cfg.connectivity {
        ConnectivitySpec::Both { .. } => Err(NodeError::Config(
            "entry nodes do not support both connect and listen simultaneously".into(),
        )),
        ConnectivitySpec::Listen(spec) => run_entry_listen(global, cfg, spec, res).await,
        ConnectivitySpec::Connect(spec) => {
            let node_state = res.node_state.clone();
            node_state.set_connected(&spec.addr);
            run_entry_connect(global, cfg, spec, res).await
        }
    }
}

/// Run entry node in listen mode — set up server and accept connections.
async fn run_entry_listen(
    global: &GlobalConfig,
    cfg: &EntryConfig,
    spec: &AddressSpec,
    res: EntryResources,
) -> Result<(), NodeError> {
    let addr: std::net::SocketAddr = spec.addr.parse::<crate::net::ListenAddr>()?.into();
    let psk = global.psk.clone();
    let server_options = ServerOptions {
        handler_config: HandlerConfig::new(
            NodeRole::Entry,
            "wallhack".to_string(),
            global.version.clone(),
        ),
        metrics: Some(Arc::clone(&res.metrics)),
        peers: Some(Arc::clone(&res.peers)),
        routes: Some(Arc::clone(&res.routes)),
        route_updates: Some(res.route_updates_tx.clone()),
        local_handshake: Some(wallhack_wire::data::Handshake {
            capabilities: Some(wallhack_wire::data::Capabilities {
                tun_capable: crate::tun_cap::detect_tun_capable(),
                listening: true,
                connecting: false,
            }),
            name: cfg.name.clone(),
            version: global.version.clone(),
            psk_proof: Vec::new(),
            routes: Vec::new(),
            hint: None,
        }),
    };
    let server_config = crate::config::build_server_config(&global.tls, addr, psk, cfg.max_peers);

    // Start REST API if enabled
    #[cfg(feature = "http-api")]
    if let Some(ref api_cfg) = cfg.api {
        start_api(
            api_cfg.addr,
            &res.metrics,
            &res.peers,
            &res.routes,
            server_config.tls.clone(),
            api_cfg.user.clone(),
            api_cfg.secret.clone(),
            &global.version,
        );
    }

    match spec.protocol {
        Protocol::Udp => {
            #[cfg(feature = "quic")]
            {
                let server =
                    wallhack_core::server::quic::QuicServer::try_new(server_config, server_options)
                        .map_err(|e| NodeError::Transport(Box::new(e)))?;
                start_entry_server(server, res, cfg).await
            }
            #[cfg(not(feature = "quic"))]
            {
                Err(NodeError::TransportUnavailable("quic"))
            }
        }
        Protocol::Tcp => {
            #[cfg(feature = "websocket")]
            {
                let server = wallhack_core::server::ws::WebSocketServer::try_new(
                    server_config,
                    server_options,
                )?;
                start_entry_server(server, res, cfg).await
            }
            #[cfg(not(feature = "websocket"))]
            {
                Err(NodeError::TransportUnavailable("websocket"))
            }
        }
    }
}

/// Announce the server and run the entry server loop.
async fn start_entry_server<S: Server>(
    server: S,
    res: EntryResources,
    cfg: &EntryConfig,
) -> Result<(), NodeError>
where
    S::Error: std::error::Error + Send + Sync + 'static,
    S::Transport: Send + Sync + 'static,
    <S::Transport as Transport>::SendStream: 'static,
    <S::Transport as Transport>::RecvStream: 'static,
    <S::Transport as Transport>::BiStream: 'static,
{
    let local_addr = server.local_addr()?;
    res.node_state.set_listen_addr(local_addr);
    let proto = server.protocol_name();
    tracing::info!("Listening on {local_addr} ({proto})");
    tracing::info!("Certificate fingerprint: {}", server.fingerprint());
    if server.psk().is_none() {
        tracing::warn!(
            "No authentication configured. Set a pre-shared key (PSK) to require authentication."
        );
    } else {
        tracing::info!("PSK authentication configured");
    }
    run_entry_server(
        server,
        res,
        EntryListenOptions {
            max_peers: cfg.max_peers,
        },
    )
    .await
}

/// Run entry node in connect mode.
///
/// DNS resolve once, then retry loop with exponential backoff.
#[allow(clippy::too_many_lines)]
pub(crate) async fn run_entry_connect(
    global: &GlobalConfig,
    cfg: &EntryConfig,
    spec: &AddressSpec,
    res: EntryResources,
) -> Result<(), NodeError> {
    tracing::info!("Connecting to {}...", spec.addr);
    let endpoint =
        crate::transport::resolve_endpoint(&spec.addr, global.dns_server.as_deref()).await?;

    // Start REST API if enabled (peers registry is unused in connect mode)
    #[cfg(feature = "http-api")]
    if let Some(ref api_cfg) = cfg.api {
        let tls = crate::config::build_tls_config(&global.tls);
        let peers = Arc::new(Registry::new());
        let routes = RouteTable::shared();
        start_api(
            api_cfg.addr,
            &res.metrics,
            &peers,
            &routes,
            tls,
            api_cfg.user.clone(),
            api_cfg.secret.clone(),
            &global.version,
        );
    }

    let peer_addr = endpoint.to_string();
    let security = SecurityParams {
        psk: global.psk.clone(),
        accept_fingerprint: None,
    };
    // Advertise the correct capabilities: entry connectors are TUN-capable.
    let entry_handshake = entry_local_handshake(&cfg.name, &global.version);

    let metrics = Arc::clone(&res.metrics);
    let routes = Arc::clone(&res.routes);
    // Note: We need to resubscribe for each connection attempt if we want a fresh stream.
    // route_updates is a Receiver, so we use resubscribe().

    match spec.protocol {
        Protocol::Udp => {
            #[cfg(feature = "quic")]
            {
                let client_config = crate::config::build_quic_client_config(
                    global,
                    endpoint,
                    None,
                    &security,
                    Some(entry_handshake),
                );
                let r_updates = res.route_updates.resubscribe();
                crate::transport::connect_loop(
                    || {
                        let cfg = client_config.clone();
                        async move {
                            use wallhack_core::client::client::Client;
                            let mut client = wallhack_core::client::quic::QuicClient::try_new(cfg)?;
                            client.connect(NodeRole::Entry).await
                        }
                    },
                    move |connect_result| {
                        let e = connect_result.erase();
                        let m = Arc::clone(&metrics);
                        let r = Arc::clone(&routes);
                        let ru = r_updates.resubscribe();
                        let pa = peer_addr.clone();
                        async move {
                            run_entry_connected_erased(e, &m, &pa, Some(r), Some(ru)).await
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
                    None,
                    &security,
                    Some(entry_handshake),
                );
                let r_updates = res.route_updates.resubscribe();
                crate::transport::connect_loop(
                    || {
                        let cfg = client_config.clone();
                        async move {
                            let mut client = wallhack_core::client::ws::WsClient::new(cfg)?;
                            client.connect(NodeRole::Entry).await
                        }
                    },
                    move |connect_result| {
                        let e = connect_result.erase();
                        let m = Arc::clone(&metrics);
                        let r = Arc::clone(&routes);
                        let ru = r_updates.resubscribe();
                        let pa = peer_addr.clone();
                        async move {
                            run_entry_connected_erased(e, &m, &pa, Some(r), Some(ru)).await
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

/// Build the local handshake for an entry connector.
fn entry_local_handshake(name: &str, version: &str) -> wallhack_wire::data::Handshake {
    wallhack_wire::data::Handshake {
        capabilities: Some(wallhack_wire::data::Capabilities {
            tun_capable: crate::tun_cap::detect_tun_capable(),
            listening: false,
            connecting: true,
        }),
        name: name.to_string(),
        version: version.to_string(),
        psk_proof: Vec::new(),
        routes: Vec::new(),
        hint: None,
    }
}

/// Run the entry node session once connected.
///
/// Non-generic: takes the type-erased `ErasedConnectResult` so the async
/// state machine is monomorphized only once regardless of transport type.
pub(crate) async fn run_entry_connected_erased(
    connect_result: wallhack_core::client::client::ErasedConnectResult,
    metrics: &Arc<Metrics>,
    peer_addr: &str,
    routes: Option<SharedRouteTable>,
    route_updates: Option<
        tokio::sync::broadcast::Receiver<wallhack_core::control::routes::RouteUpdate>,
    >,
) -> Result<(), NodeError> {
    use wallhack_core::server::server::DataChannels;

    let wallhack_core::client::client::ErasedConnectResult {
        peer_handshake_rx,
        transport,
        channels,
        tasks: _tasks,
        control_tx: _control_tx,
        peer_addr: _,
    } = connect_result;

    // Wait for the server's handshake to get the peer name
    let peer_name = if let Some(rx) = peer_handshake_rx {
        #[allow(clippy::single_match_else)]
        match rx.await {
            Ok(h) => Some(h.name),
            Err(_) => {
                tracing::warn!("Failed to receive peer handshake from {peer_addr}");
                None
            }
        }
    } else {
        None
    };

    if let Some(ref name) = peer_name {
        tracing::info!("Authenticated peer: {name} at {peer_addr}");
    }

    let DataChannels {
        instructions_tx,
        instructions_rx,
        responses_tx: _responses_tx,
        responses_rx,
    } = channels;
    run_entry_connected_inner(
        transport,
        instructions_tx,
        instructions_rx,
        responses_rx,
        metrics,
        peer_addr,
        peer_name.as_deref(),
        routes,
        route_updates,
    )
    .await
}

/// Non-generic inner: monomorphized once regardless of transport type.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_entry_connected_inner(
    transport: Arc<dyn ErasedTransport>,
    instructions_tx: tokio::sync::mpsc::Sender<wallhack_wire::data::EntryNodeInstruction>,
    instructions_rx: tokio::sync::mpsc::Receiver<wallhack_wire::data::EntryNodeInstruction>,
    responses_rx: tokio::sync::mpsc::Receiver<wallhack_wire::data::ExitNodeResponse>,
    metrics: &Arc<Metrics>,
    peer_addr: &str,
    peer_name: Option<&str>,
    routes: Option<SharedRouteTable>,
    route_updates: Option<
        tokio::sync::broadcast::Receiver<wallhack_core::control::routes::RouteUpdate>,
    >,
) -> Result<(), NodeError> {
    tracing::info!("Connected to {peer_addr}");

    // Spawn outgoing data task: open uni stream, send instructions to peer.
    let transport_out = Arc::clone(&transport);
    tokio::spawn(async move {
        match transport_out.open_uni_erased().await {
            Ok(mut send) => {
                if let Err(e) = wallhack_core::transport::protocol::run_send_instructions(
                    &mut send,
                    instructions_rx,
                )
                .await
                {
                    tracing::debug!("Send-instructions handler finished: {e}");
                }
            }
            Err(e) => tracing::debug!("Failed to open send stream: {e}"),
        }
    });

    let name = peer_name
        .filter(|n| !n.is_empty())
        .map_or_else(SessionManager::create_anonymous, peer_name_to_iface);
    let actor = create_tun_with_retry(name.clone()).await?;

    // Apply existing routes
    #[allow(clippy::collapsible_if)]
    if let Some(r) = &routes {
        if let Some(pn) = peer_name {
            for entry in r.list() {
                if Some(entry.peer.as_str()) == Some(pn) {
                    let _ = add_os_route(&entry.cidr.to_string(), &name);
                }
            }
        }
    }

    // Spawn route update listener
    if let Some(mut updates) = route_updates {
        let tun = name.clone();
        let peer = peer_name.map(std::string::ToString::to_string);
        tokio::spawn(async move {
            loop {
                match updates.recv().await {
                    Ok(wallhack_core::control::routes::RouteUpdate::Add(entry)) => {
                        if Some(entry.peer.as_str()) == peer.as_deref() {
                            let _ = add_os_route(&entry.cidr.to_string(), &tun);
                        }
                    }
                    Ok(wallhack_core::control::routes::RouteUpdate::Remove(entry)) => {
                        if Some(entry.peer.as_str()) == peer.as_deref() {
                            let _ = remove_os_route(&entry.cidr.to_string(), &tun);
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                }
            }
        });
    }

    let (manager, _probe_cache) = ConnectionManager::new(
        actor,
        transport,
        Arc::clone(metrics),
        instructions_tx,
        responses_rx,
    );

    let manager_handle = tokio::spawn(async move { manager.run().await });

    match manager_handle.await {
        Ok(Ok(())) => tracing::info!("Connection closed."),
        Ok(Err(e)) => tracing::warn!("Connection error: {e}"),
        Err(e) => tracing::warn!("Connection task failed: {e}"),
    }

    Ok(())
}

struct EntryListenOptions {
    max_peers: Option<usize>,
}

/// Generic entry server loop that works with any `Server` implementation.
#[allow(clippy::too_many_lines)]
async fn run_entry_server<S: Server>(
    mut server: S,
    res: EntryResources,
    options: EntryListenOptions,
) -> Result<(), NodeError>
where
    S::Error: std::error::Error + Send + Sync + 'static,
    S::Transport: Send + Sync + 'static,
    <S::Transport as Transport>::SendStream: 'static,
    <S::Transport as Transport>::RecvStream: 'static,
    <S::Transport as Transport>::BiStream: Send + 'static,
{
    let EntryResources {
        metrics: _,
        peers,
        routes,
        route_updates,
        route_updates_tx: _,
        node_state: _,
        sessions,
    } = res;
    let EntryListenOptions { max_peers } = options;
    let server_psk = server.psk().map(|s| zeroize::Zeroizing::new(s.to_string()));
    let peer_semaphore = Arc::new(tokio::sync::Semaphore::new(
        max_peers.unwrap_or(tokio::sync::Semaphore::MAX_PERMITS),
    ));

    let mut psk_failures = super::PskFailTracker::new();

    // Main loop: handle incoming connections
    loop {
        match server.accept(NodeRole::Entry).await {
            Ok(Some(mut accept_result)) => {
                // Enforce max peers limit
                let Ok(permit) = Arc::clone(&peer_semaphore).try_acquire_owned() else {
                    tracing::info!(
                        "Rejected connection from {} (max peers reached)",
                        accept_result.peer_addr()
                    );
                    continue;
                };

                let conn_metrics = accept_result.metrics();
                let conn_sessions = sessions.clone();
                let conn_peers = Arc::clone(&peers);
                let conn_routes = Arc::clone(&routes);
                let peer_route_updates = route_updates.resubscribe();
                let peer_addr = accept_result.peer_addr().to_string();

                // Validate handshake before spawning (keeps generic code minimal).
                let identity = match validate_handshake(
                    &mut accept_result,
                    server_psk.as_ref().map(|s| s.as_str()),
                ) {
                    Ok(id) => id,
                    Err(e) => {
                        if matches!(&e, NodeError::PskAuth(_)) {
                            psk_failures.record(&peer_addr);
                        } else {
                            tracing::warn!("Handshake validation failed for {peer_addr}: {e}");
                        }
                        continue;
                    }
                };

                let peer_name = identity.name.as_deref().unwrap_or(&peer_addr).to_string();

                // Register peer in the registry and apply handshake capabilities.
                conn_peers.register(peer_name.clone(), peer_addr.clone(), NodeRole::Exit);
                conn_peers.update_capabilities(&peer_name, &identity.capabilities);

                // Create ping channel for this peer
                #[allow(deprecated)] // TODO: replace with peer events
                let mut ping_rx = conn_peers.register_ping_channel(&peer_name);

                // Extract transport and channels from the generic AcceptResult before
                // spawning so the spawned future is non-generic.
                let transport: Arc<dyn ErasedTransport> = accept_result.transport();
                let latency_rx = accept_result
                    .take_latency_rx()
                    .unwrap_or_else(|| tokio::sync::mpsc::channel(1).1);
                let (channels, control_tx) = accept_result.into_channels();

                // Spawn non-generic handler
                tokio::spawn(async move {
                    // Hold the permit for the lifetime of this connection
                    let _permit = permit;
                    let params = ConnectionParams {
                        metrics: conn_metrics,
                        transport,
                        channels,
                        control_tx,
                        sessions: conn_sessions.clone(),
                        peers: Arc::clone(&conn_peers),
                        routes: Arc::clone(&conn_routes),
                        route_updates: peer_route_updates,
                        peer: identity.name,
                        peer_addr: peer_addr.clone(),
                    };
                    let result = params.run(&mut ping_rx, latency_rx).await;
                    // Unregister peer when connection closes
                    conn_peers.unregister(&peer_name);
                    // Clean up routes for this peer
                    let removed_routes = conn_routes.remove_by_peer(&peer_name);
                    for entry in &removed_routes {
                        if let Some(tun) = conn_sessions.get_tun_for_peer(&peer_name) {
                            let _ = remove_os_route(&entry.cidr.to_string(), &tun);
                        }
                    }
                    if !removed_routes.is_empty() {
                        tracing::info!(
                            "Removed {} route(s) for disconnected peer {peer_name}",
                            removed_routes.len()
                        );
                    }
                    match result {
                        Ok(_tun_name) => {
                            tracing::info!("Peer disconnected: {peer_name}");
                        }
                        Err(e) => {
                            tracing::debug!("Connection error for {}: {}", peer_name, e);
                            tracing::warn!("Peer {peer_name} disconnected with error: {e}");
                        }
                    }
                });
            }
            Ok(None) => {
                tracing::info!("Server closed");
                break;
            }
            Err(e) => {
                tracing::warn!("Accept error: {e}");
            }
        }
    }

    Ok(())
}

/// Arguments for the non-generic connection handler.
struct ConnectionParams {
    metrics: Arc<Metrics>,
    transport: Arc<dyn ErasedTransport>,
    channels: wallhack_core::server::server::DataChannels,
    control_tx: tokio::sync::mpsc::Sender<wallhack_wire::control::ControlMessage>,
    sessions: SessionManager,
    peers: Arc<wallhack_core::control::peers::Registry>,
    routes: SharedRouteTable,
    route_updates: tokio::sync::broadcast::Receiver<RouteUpdate>,
    peer: Option<String>,
    peer_addr: String,
}

/// Validated handshake result containing the peer's name and capabilities.
struct PeerIdentity {
    name: Option<String>,
    capabilities: wallhack_wire::data::Capabilities,
}

/// Validate the peer's handshake (PSK proof + identity) for generic `AcceptResult`.
fn validate_handshake<T: wallhack_core::transport::Transport>(
    accept_result: &mut wallhack_core::server::server::AcceptResult<T>,
    server_psk: Option<&str>,
) -> Result<PeerIdentity, NodeError> {
    let channel_binding = accept_result.channel_binding().copied();
    let Some(hs) = accept_result.take_peer_handshake() else {
        tracing::debug!("No Handshake received, peer unidentified");
        return Ok(PeerIdentity {
            name: None,
            capabilities: wallhack_wire::data::Capabilities::default(),
        });
    };

    if let Some(expected_psk) = server_psk {
        let valid = channel_binding
            .as_ref()
            .is_some_and(|binding| hs.verify_psk_proof(expected_psk.as_bytes(), binding));
        if !valid {
            return Err(NodeError::PskAuth(hs.name));
        }
    }

    let capabilities = hs.capabilities.unwrap_or_default();

    if hs.name.is_empty() {
        tracing::debug!("Peer identified with empty name (v{})", hs.version);
        Ok(PeerIdentity {
            name: None,
            capabilities,
        })
    } else {
        tracing::debug!("Peer {} identified (v{})", hs.name, hs.version);
        Ok(PeerIdentity {
            name: Some(hs.name),
            capabilities,
        })
    }
}

/// Spawn background tasks that bridge transport data streams (entry server side).
///
/// - Incoming task: accepts the peer's uni stream and dispatches data messages.
/// - Outgoing task: opens a uni stream and writes instructions from `instructions_rx`.
pub(crate) fn spawn_data_tasks(
    transport: &Arc<dyn ErasedTransport>,
    instructions_tx: &tokio::sync::mpsc::Sender<wallhack_wire::data::EntryNodeInstruction>,
    responses_tx: &tokio::sync::mpsc::Sender<wallhack_wire::data::ExitNodeResponse>,
    instructions_rx: tokio::sync::mpsc::Receiver<wallhack_wire::data::EntryNodeInstruction>,
) {
    // Incoming data: accept uni stream from exit peer, dispatch data messages.
    let transport_data = Arc::clone(transport);
    let instructions_in = instructions_tx.clone();
    let responses_in = responses_tx.clone();
    tokio::spawn(async move {
        match transport_data.accept_uni_erased().await {
            Ok(Some(mut recv)) => {
                if let Err(e) = wallhack_core::transport::protocol::run_data_in(
                    &mut recv,
                    &instructions_in,
                    &responses_in,
                )
                .await
                {
                    tracing::debug!("Data-in handler finished: {e}");
                }
            }
            Ok(None) => tracing::debug!("Transport closed before data-in stream accepted"),
            Err(e) => tracing::debug!("Failed to accept data-in stream: {e}"),
        }
    });

    // Outgoing data: open uni stream to exit peer, write instructions.
    let transport_out = Arc::clone(transport);
    tokio::spawn(async move {
        match transport_out.open_uni_erased().await {
            Ok(mut send) => {
                if let Err(e) = wallhack_core::transport::protocol::run_send_instructions(
                    &mut send,
                    instructions_rx,
                )
                .await
                {
                    tracing::debug!("Send-instructions handler finished: {e}");
                }
            }
            Err(e) => tracing::debug!("Failed to open send stream: {e}"),
        }
    });
}

/// Run the connection manager alongside ping/latency handling.
#[allow(clippy::too_many_arguments)]
async fn run_connection_loop(
    mut manager_handle: tokio::task::JoinHandle<Result<(), wallhack_core::entry::manager::Error>>,
    control_tx: tokio::sync::mpsc::Sender<wallhack_wire::control::ControlMessage>,
    mut latency_rx: tokio::sync::mpsc::Receiver<f64>,
    ping_rx: &mut tokio::sync::mpsc::Receiver<wallhack_core::control::peers::PingRequest>,
    mut route_updates: tokio::sync::broadcast::Receiver<RouteUpdate>,
    peer: Option<&str>,
    peers: &Arc<Registry>,
    tun_name: &str,
) -> Result<(), NodeError> {
    let mut pending_ping: Option<tokio::sync::oneshot::Sender<f64>> = None;
    let mut heartbeat = tokio::time::interval(std::time::Duration::from_secs(30));
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    // Fire an initial ping so latency is populated immediately after connect.
    if let Err(e) = send_ping(&control_tx).await {
        tracing::debug!("Initial ping failed: {e}");
    }
    // Consume the first tick (fires immediately) so the heartbeat starts
    // 30s after the initial ping, not immediately.
    heartbeat.tick().await;

    loop {
        tokio::select! {
            result = &mut manager_handle => {
                match result {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => return Err(e.into()),
                    Err(e) => return Err(e.into())
                }
                break;
            }
            Some(ms) = latency_rx.recv() => {
                if let Some(id) = peer {
                    peers.update_latency(id, ms);
                }
                if let Some(tx) = pending_ping.take() {
                    let _ = tx.send(ms);
                }
            }
            Some(result_tx) = ping_rx.recv() => {
                match send_ping(&control_tx).await {
                    Ok(()) => {
                        pending_ping = Some(result_tx);
                    }
                    Err(e) => {
                        tracing::debug!("Ping failed: {e}");
                        drop(result_tx);
                    }
                }
            }
            _ = heartbeat.tick() => {
                if let Err(e) = send_ping(&control_tx).await {
                    tracing::debug!("Heartbeat ping failed: {e}");
                }
            }
            update = route_updates.recv() => {
                match update {
                    Ok(RouteUpdate::Add(entry)) => {
                        if Some(entry.peer.as_str()) == peer {
                             let _ = add_os_route(&entry.cidr.to_string(), tun_name);
                        }
                    }
                    Ok(RouteUpdate::Remove(entry)) => {
                        if Some(entry.peer.as_str()) == peer {
                             let _ = remove_os_route(&entry.cidr.to_string(), tun_name);
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                        tracing::warn!("Skipped {skipped} route updates");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                         tracing::debug!("Route update channel closed");
                         break;
                    }
                }
            }
        }
    }
    Ok(())
}

impl ConnectionParams {
    /// Main entry point for the non-generic connection handler.
    pub async fn run(
        self,
        ping_rx: &mut tokio::sync::mpsc::Receiver<wallhack_core::control::peers::PingRequest>,
        latency_rx: tokio::sync::mpsc::Receiver<f64>,
    ) -> Result<String, NodeError> {
        use wallhack_core::server::server::DataChannels;

        let ConnectionParams {
            metrics,
            transport,
            channels:
                DataChannels {
                    instructions_tx,
                    instructions_rx,
                    responses_tx,
                    responses_rx,
                },
            control_tx,
            sessions,
            peers,
            routes,
            route_updates,
            peer,
            peer_addr,
        } = self;

        spawn_data_tasks(&transport, &instructions_tx, &responses_tx, instructions_rx);

        // Get or create TUN adapter via session manager
        let name = if let Some(ref id) = peer {
            sessions.get_or_create(id)
        } else {
            SessionManager::create_anonymous()
        };
        let actor = create_tun_with_retry(name.clone()).await?;

        // Apply existing routes now that we have the TUN interface
        if let Some(peer_name) = &peer {
            let existing = routes.list();
            for entry in existing {
                if &entry.peer == peer_name {
                    let _ = add_os_route(&entry.cidr.to_string(), &name);
                }
            }
        }

        let peer_display = peer.as_deref().unwrap_or(&peer_addr);
        tracing::info!("Peer connected: name={peer_display} addr={peer_addr} tun={name}");

        let (manager, _probe_cache) = ConnectionManager::new(
            actor,
            Arc::clone(&transport),
            Arc::clone(&metrics),
            instructions_tx.clone(),
            responses_rx,
        );

        let manager_handle = tokio::spawn(async move { manager.run().await });
        run_connection_loop(
            manager_handle,
            control_tx,
            latency_rx,
            ping_rx,
            route_updates,
            peer.as_deref(),
            &peers,
            &name,
        )
        .await?;

        Ok(name)
    }
}

/// Inject a Ping message into the control stream.
async fn send_ping(
    control_tx: &tokio::sync::mpsc::Sender<wallhack_wire::control::ControlMessage>,
) -> Result<(), NodeError> {
    use wallhack_wire::control::{ControlMessage, control_message};

    #[allow(clippy::cast_possible_truncation)]
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;

    let ping_msg = ControlMessage {
        message: Some(control_message::Message::Ping(wallhack_wire::data::Ping {
            timestamp_ms: ts,
        })),
    };

    control_tx
        .send(ping_msg)
        .await
        .map_err(|_| NodeError::ChannelClosed)
}

#[cfg(feature = "http-api")]
#[allow(clippy::too_many_arguments)]
fn start_api(
    api_addr: std::net::SocketAddr,
    metrics: &Arc<Metrics>,
    peers: &Arc<Registry>,
    routes: &SharedRouteTable,
    tls_config: Option<wallhack_core::server::config::TlsConfig>,
    username: String,
    secret: String,
    version: &str,
) {
    use wallhack_api::{Auth, State as ApiState};
    use wallhack_core::control::handler::Handler;
    use wallhack_ipc::client::IpcConnection;

    let handler_config =
        HandlerConfig::new(NodeRole::Entry, "wallhack".to_string(), version.to_string());
    let (route_updates, _) = tokio::sync::broadcast::channel(16);
    let handler = Handler::new(
        handler_config,
        Arc::clone(metrics),
        Arc::clone(peers),
        Arc::clone(routes),
        route_updates,
    );
    tracing::info!("REST API listening on {api_addr}");
    tracing::info!("  API username: {username}");
    tracing::info!("  API secret:   {secret}");

    // In-process IPC connection over DuplexStream — same pattern as REPL.
    let (api_client, api_server) = tokio::io::duplex(4096);
    let api: Arc<dyn wallhack_core::node_api::NodeApi> = Arc::new(handler);
    tokio::spawn(async move {
        if let Err(e) = wallhack_core::ipc::handle_connection(api_server, api, None).await {
            tracing::debug!("REST API IPC connection ended: {e}");
        }
    });

    let ipc_conn = IpcConnection::new(api_client);
    let auth = Auth::new(username, secret);
    let state = ApiState::new(ipc_conn, auth);

    tokio::spawn(async move {
        if let Err(e) = wallhack_api::serve(api_addr, state, tls_config).await {
            tracing::error!("REST API error: {e}");
        }
    });
}

#[cfg(test)]
mod tests {
    #[test]
    fn peer_semaphore_default_does_not_panic() {
        // Regression: using usize::MAX exceeded tokio's MAX_PERMITS and panicked.
        let _sem = tokio::sync::Semaphore::new(tokio::sync::Semaphore::MAX_PERMITS);
    }

    #[test]
    fn peer_semaphore_with_limit() {
        let _sem = tokio::sync::Semaphore::new(10);
    }
}
