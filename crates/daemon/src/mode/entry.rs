//! Entry node implementation.
//!
//! The entry node creates a TUN interface and accepts connections from exit or
//! relay nodes. It can either listen for incoming connections (default) or
//! connect to a remote peer. The daemon is headless — no REPL, no TTY.

use std::{collections::HashMap, sync::Arc};

use parking_lot::Mutex;
use subtle::ConstantTimeEq;

use wallhack_core::{
    NodeRole,
    control::{
        handler::HandlerConfig, metrics::Metrics, peers::Registry, routes::SharedRouteTable,
    },
    entry::{actor::TunActor, manager::ConnectionManager},
    server::server::{Server, ServerOptions},
};

#[cfg(feature = "http-api")]
use wallhack_core::control::routes::RouteTable;

use crate::{
    NodeError,
    address_spec::{AddressSpec, ConnectivitySpec, Protocol},
    config::SecurityParams,
    daemon_config::{EntryConfig, GlobalConfig},
};

/// Shared node resources passed through the entry server call stack.
struct EntryResources {
    metrics: Arc<Metrics>,
    peers: Arc<Registry>,
    routes: SharedRouteTable,
    sessions: SessionManager,
}

/// Manages TUN sessions for connected exit nodes.
///
/// Keeps TUN adapters alive between reconnections so exit nodes can reconnect
/// without losing their TUN interface.
#[derive(Clone, Default)]
struct SessionManager {
    sessions: Arc<Mutex<HashMap<String, String>>>,
}

impl SessionManager {
    /// Gets or creates a TUN adapter for the given exit node.
    ///
    /// If the exit node has connected before, returns a clone of their existing
    /// TUN. Otherwise creates a new TUN with stable naming (`tun-{name}`).
    fn get_or_create(&self, name: &str) -> String {
        let mut sessions = self.sessions.lock();

        if let Some(name) = sessions.get(name) {
            tracing::info!("Reusing existing TUN for exit node {}", name);
            return name.clone();
        }

        // Create new TUN with stable name
        let tun_name = format!("tun-{name}");
        tracing::info!("Creating new TUN {} for exit node {}", tun_name, name);
        sessions.insert(name.to_string(), tun_name.clone());
        tun_name
    }

    /// Gets a TUN adapter with auto-generated name (for exit nodes without
    /// identity).
    fn create_anonymous() -> String {
        TunActor::random_iface_name()
    }

    /// Look up the TUN device name for a peer.
    fn get_tun_for_peer(&self, peer: &str) -> Option<String> {
        self.sessions.lock().get(peer).cloned()
    }
}

/// Create a TUN device, retrying on EBUSY to handle the race where the
/// previous connection's `TunActor` hasn't been fully dropped yet.
async fn create_tun_with_retry(name: String) -> Result<TunActor, NodeError> {
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
pub async fn run(
    global: &GlobalConfig,
    cfg: &EntryConfig,
    metrics: Arc<Metrics>,
    peers: Arc<Registry>,
    routes: SharedRouteTable,
) -> Result<(), NodeError> {
    let res = EntryResources {
        sessions: SessionManager::default(),
        metrics,
        peers,
        routes,
    };

    match &cfg.connectivity {
        ConnectivitySpec::Both { .. } => Err(NodeError::Config(
            "entry nodes do not support both --connect and --listen simultaneously".into(),
        )),
        ConnectivitySpec::Listen(spec) => run_entry_listen(global, cfg, spec, res).await,
        ConnectivitySpec::Connect(spec) => run_entry_connect(global, cfg, spec, res.metrics).await,
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
        handler_config: HandlerConfig::new(NodeRole::Entry, crate::built_info::PKG_NAME.to_string(), crate::built_info::PKG_VERSION.to_string()),
        metrics: Some(Arc::clone(&res.metrics)),
        peers: Some(Arc::clone(&res.peers)),
        routes: Some(Arc::clone(&res.routes)),
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
{
    let local_addr = server.local_addr()?;
    let proto = server.protocol_name();
    tracing::info!("Listening on {local_addr} ({proto})");
    tracing::info!("Certificate fingerprint: {}", server.fingerprint());
    if server.psk().is_none() {
        tracing::warn!(
            "No authentication configured. Use --psk <SECRET> to require authentication."
        );
    }
    run_entry_server(
        server,
        res,
        EntryListenOptions {
            max_peers: cfg.max_peers,
            fast_mode: cfg.fast,
        },
    )
    .await
}

/// Run entry node in connect mode.
///
/// DNS resolve once, then retry loop with exponential backoff.
async fn run_entry_connect(
    global: &GlobalConfig,
    cfg: &EntryConfig,
    spec: &AddressSpec,
    metrics: Arc<Metrics>,
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
            &metrics,
            &peers,
            &routes,
            tls,
            api_cfg.user.clone(),
            api_cfg.secret.clone(),
        );
    }

    let peer_addr = endpoint.to_string();
    let security = SecurityParams {
        psk: global.psk.clone(),
        accept_fingerprint: None,
    };
    let fast_mode = cfg.fast;

    match spec.protocol {
        Protocol::Udp => {
            #[cfg(feature = "quic")]
            {
                let client_config =
                    crate::config::build_quic_client_config(global, endpoint, None, &security);
                crate::transport::connect_loop(
                    || {
                        let cfg = client_config.clone();
                        async move {
                            use wallhack_core::client::client::Client;
                            let mut client = wallhack_core::client::quic::QuicClient::try_new(cfg)?;
                            client.connect(NodeRole::Entry).await
                        }
                    },
                    |connect_result| {
                        let m = Arc::clone(&metrics);
                        let pa = peer_addr.clone();
                        async move { run_entry_connected(connect_result, &m, fast_mode, &pa).await }
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
                let client_config =
                    crate::config::build_ws_client_config(global, endpoint, None, &security);
                crate::transport::connect_loop(
                    || {
                        let cfg = client_config.clone();
                        async move {
                            let mut client = wallhack_core::client::ws::WsClient::new(cfg)?;
                            client.connect(NodeRole::Entry).await
                        }
                    },
                    |connect_result| {
                        let m = Arc::clone(&metrics);
                        let pa = peer_addr.clone();
                        async move { run_entry_connected(connect_result, &m, fast_mode, &pa).await }
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

/// Run the entry node session once connected.
async fn run_entry_connected<T: wallhack_core::transport::Transport + 'static>(
    connect_result: wallhack_core::client::client::ConnectResult<T>,
    metrics: &Arc<Metrics>,
    fast_mode: bool,
    peer_addr: &str,
) -> Result<(), NodeError> {
    tracing::info!("Connected to {peer_addr}");

    let transport = connect_result.transport();
    let (instructions_tx, responses_tx) = connect_result.channels().clone();
    let responses_rx = responses_tx.subscribe();
    drop(responses_tx);
    drop(connect_result);

    let name = SessionManager::create_anonymous();
    let actor = create_tun_with_retry(name).await?;
    let (manager, _syn_proxy_state) = ConnectionManager::new(
        actor,
        transport,
        Arc::clone(metrics),
        fast_mode,
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
    fast_mode: bool,
}

/// Generic entry server loop that works with any `Server` implementation.
async fn run_entry_server<S: Server>(
    mut server: S,
    res: EntryResources,
    options: EntryListenOptions,
) -> Result<(), NodeError>
where
    S::Error: std::error::Error + Send + Sync + 'static,
    S::Transport: Send + Sync + 'static,
{
    let EntryResources {
        metrics: _,
        peers,
        routes,
        sessions,
    } = res;
    let EntryListenOptions {
        max_peers,
        fast_mode,
    } = options;
    let server_psk = server.psk().map(String::from);
    let peer_semaphore = Arc::new(tokio::sync::Semaphore::new(
        max_peers.unwrap_or(tokio::sync::Semaphore::MAX_PERMITS),
    ));

    // Main loop: handle incoming connections
    loop {
        match server.accept(NodeRole::Entry).await {
            Ok(Some(accept_result)) => {
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
                let peer_addr = accept_result.peer_addr().to_string();
                let peer = accept_result
                    .exit_hello()
                    .map_or_else(|| peer_addr.clone(), |h| h.name.clone());

                // Register peer in the registry
                conn_peers.register(peer.clone(), peer_addr.clone(), NodeRole::Exit);

                // Create ping channel for this peer
                let mut ping_rx = conn_peers.register_ping_channel(&peer);
                let transport = accept_result.transport();

                // Spawn handler for this connection (each exit node gets its own TUN)
                let conn_psk = server_psk.clone();
                tokio::spawn(async move {
                    // Hold the permit for the lifetime of this connection
                    let _permit = permit;
                    let result = handle_connection(
                        conn_metrics,
                        accept_result,
                        conn_sessions.clone(),
                        &mut ping_rx,
                        &transport,
                        &conn_peers,
                        conn_psk,
                        fast_mode,
                        peer_addr.clone(),
                    )
                    .await;
                    // Unregister peer when connection closes
                    conn_peers.unregister(&peer);
                    // Clean up routes for this peer
                    let removed_routes = conn_routes.remove_by_peer(&peer);
                    for entry in &removed_routes {
                        if let Some(tun) = conn_sessions.get_tun_for_peer(&peer) {
                            let _ = remove_os_route(&entry.cidr.to_string(), &tun);
                        }
                    }
                    if !removed_routes.is_empty() {
                        tracing::info!(
                            "Removed {} route(s) for disconnected peer {peer}",
                            removed_routes.len()
                        );
                    }
                    match result {
                        Ok(_tun_name) => {
                            tracing::info!("Peer disconnected: {peer}");
                        }
                        Err(e) => {
                            tracing::debug!("Connection error for {}: {}", peer, e);
                            tracing::warn!("Peer {peer} disconnected with error: {e}");
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

/// Remove an OS-level route via `ip route del`.
pub(crate) fn remove_os_route(cidr: &str, dev: &str) -> Result<(), String> {
    match std::process::Command::new("ip")
        .args(["route", "del", cidr, "dev", dev])
        .output()
    {
        Ok(output) => {
            if output.status.success() {
                tracing::info!("OS route removed: {cidr} dev {dev}");
                Ok(())
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
                tracing::debug!("Failed to remove OS route: {stderr}");
                Err(stderr)
            }
        }
        Err(e) => {
            tracing::debug!("Failed to run ip route del: {e}");
            Err(e.to_string())
        }
    }
}

// TODO: refactor into a ConnectionContext struct to reduce argument count
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
async fn handle_connection<T: wallhack_core::transport::Transport + 'static>(
    metrics: Arc<Metrics>,
    mut accept_result: wallhack_core::server::server::AcceptResult<T>,
    sessions: SessionManager,
    ping_rx: &mut tokio::sync::mpsc::Receiver<wallhack_core::control::peers::PingRequest>,
    transport: &Arc<T>,
    peers: &Arc<wallhack_core::control::peers::Registry>,
    server_psk: Option<String>,
    fast_mode: bool,
    peer_addr: String,
) -> Result<String, NodeError> {
    // Get ExitNodeHello directly from accept result (already read during accept)
    let peer = if let Some(hello) = accept_result.take_exit_hello() {
        // Validate PSK if configured
        if let Some(ref expected_psk) = server_psk {
            let token_bytes = hello.auth_token.as_bytes();
            let expected_bytes = expected_psk.as_bytes();
            if token_bytes.len() != expected_bytes.len()
                || !bool::from(token_bytes.ct_eq(expected_bytes))
            {
                tracing::warn!("Peer {} failed PSK authentication, dropping", hello.name);
                return Err(NodeError::PskAuth(hello.name));
            }
        }

        tracing::debug!("Peer {} identified (v{})", hello.name, hello.version);
        Some(hello.name)
    } else {
        tracing::debug!("No ExitNodeHello received, using anonymous session");
        None
    };

    // Spawn data tasks AFTER PSK validation (structural guarantee: no data before auth)
    let ((instructions_tx, responses_tx), control_tx) = accept_result.channels();

    // Data task: incoming data (accept uni stream, read data messages)
    let transport_data = Arc::clone(transport);
    let instructions_in = instructions_tx.clone();
    let responses_in = responses_tx.clone();
    tokio::spawn(async move {
        match transport_data.accept_uni().await {
            Ok(Some(mut recv)) => {
                if let Err(e) = wallhack_core::transport::bridge::run_data_in(
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

    // Data task: send instructions to peer (open uni stream, write instructions).
    let transport_out = Arc::clone(transport);
    let instructions_rx = instructions_tx.subscribe();
    tokio::spawn(async move {
        match transport_out.open_uni().await {
            Ok(mut send) => {
                if let Err(e) = wallhack_core::transport::bridge::run_send_instructions(
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

    // Get or create TUN adapter via session manager
    let name = if let Some(ref id) = peer {
        sessions.get_or_create(id)
    } else {
        SessionManager::create_anonymous()
    };

    let actor = create_tun_with_retry(name.clone()).await?;

    // Announce connection after TUN is created
    let peer_display = peer.as_deref().unwrap_or(&peer_addr);
    tracing::info!("Peer connected: {peer_display} ({peer_addr}, tun: {name})");

    let responses_rx = responses_tx.subscribe();
    drop(responses_tx);
    let (manager, _syn_proxy_state) = ConnectionManager::new(
        actor,
        Arc::clone(transport),
        metrics,
        fast_mode,
        instructions_tx.clone(),
        responses_rx,
    );

    // Run the connection manager alongside ping handling
    let mut manager_handle = tokio::spawn(async move { manager.run().await });

    loop {
        tokio::select! {
            result = &mut manager_handle => {
                // Connection ended
                match result {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => return Err(e.into()),
                    Err(e) => return Err(e.into())
                }
                break;
            }
            Some(result_tx) = ping_rx.recv() => {
                match send_ping(&control_tx).await {
                    Ok(ms) => {
                        if let Some(ref id) = peer {
                            peers.update_latency(id, ms);
                        }
                        let _ = result_tx.send(ms);
                    }
                    Err(e) => {
                        tracing::debug!("Ping failed: {e}");
                        drop(result_tx);
                    }
                }
            }
        }
    }

    Ok(name)
}

/// Send a ping via the control stream and measure round-trip time.
async fn send_ping(
    control_tx: &tokio::sync::mpsc::Sender<wallhack_wire::control::ControlMessage>,
) -> Result<f64, NodeError> {
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

    let start = std::time::Instant::now();

    // Send ping via control stream
    control_tx
        .send(ping_msg)
        .await
        .map_err(|_| NodeError::ChannelClosed)?;

    Ok(start.elapsed().as_secs_f64() * 1000.0)
}

#[cfg(feature = "http-api")]
fn start_api(
    api_addr: std::net::SocketAddr,
    metrics: &Arc<Metrics>,
    peers: &Arc<Registry>,
    routes: &SharedRouteTable,
    tls_config: Option<wallhack_core::server::config::TlsConfig>,
    username: String,
    secret: String,
) {
    use wallhack_api::{Auth, State as ApiState};
    use wallhack_core::control::handler::Handler;

    let handler_config = HandlerConfig::new(NodeRole::Entry, crate::built_info::PKG_NAME.to_string(), crate::built_info::PKG_VERSION.to_string());
    let handler = Handler::new(
        handler_config,
        Arc::clone(metrics),
        Arc::clone(peers),
        Arc::clone(routes),
    );
    tracing::info!("REST API listening on {api_addr}");
    tracing::info!("  API username: {username}");
    tracing::info!("  API secret:   {secret}");

    let auth = Auth::new(username, secret);
    let state = ApiState::new(Arc::new(handler), auth);

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
