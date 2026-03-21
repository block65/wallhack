#![warn(unused_extern_crates)]

pub mod address_spec;
pub mod daemon_config;
pub mod dns;
pub mod error;

mod config;
mod mode;
mod net;
mod netlink;
mod sys;
mod transport;
mod tun_cap;

pub use error::NodeError;
pub use tun_cap::detect_tun_capable;

use std::sync::Arc;

use daemon_config::{DaemonConfig, ModeConfig};
use tokio::sync::watch;
use wallhack_core::{
    NodeRole,
    control::{
        handler::{Handler, HandlerConfig},
        log_buffer::LogBuffer,
        metrics::Metrics,
        peers::Registry,
        routes::RouteTable,
    },
    daemon::DaemonHandle,
    node_api::NodeApi,
};

// ============================================================================
// Daemon engine entry point
// ============================================================================

/// Run the daemon engine with a structured configuration.
///
/// `socket_path_override` takes precedence over `WALLHACK_HOST` env var and
/// the default socket path (mirrors Docker's `-H` / `DOCKER_HOST`).
///
/// Starts the appropriate node, launches the IPC listener, and blocks
/// until shutdown.
///
/// # Errors
///
/// Returns [`NodeError`] if the node fails to start or the IPC listener errors.
pub async fn run_daemon_engine(
    config: DaemonConfig,
    socket_path_override: Option<std::path::PathBuf>,
    log_buffer: Option<LogBuffer>,
) -> Result<(), NodeError> {
    tracing::info!("wallhack {}  {}", config.global.version, config.mode.name());

    sys::check_entropy_ready();

    let handle = start_node(&config, log_buffer)?;

    // Start IPC listener for the management protocol.
    let socket_path = socket_path_override
        .unwrap_or_else(|| wallhack_core::ipc::socket_path(Some(config.mode.name())));
    let api = handle.api_arc();
    let peer_events = handle.peer_events_sender();
    let shutdown_rx = handle.shutdown_rx();

    let ipc_task = tokio::spawn(async move {
        if let Err(e) =
            wallhack_core::ipc::run_ipc_listener(api, peer_events, &socket_path, shutdown_rx).await
        {
            tracing::error!("IPC listener error: {e}");
        }
    });

    // Start REST API if configured (any mode — not gated on entry).
    #[cfg(feature = "http-api")]
    {
        let api_cfg = match &config.mode {
            ModeConfig::Entry(c) => c.api.clone(),
            ModeConfig::Auto(c) => c.api.clone(),
            _ => None,
        };
        if let Some(api_cfg) = api_cfg {
            mode::entry::start_api_standalone(
                api_cfg,
                handle.api_arc(),
                handle.peer_events_sender(),
                &config.global,
            );
        }
    }

    #[cfg(feature = "vsock")]
    {
        let ipc_api_vsock = handle.api_arc();
        let peer_events_vsock = handle.peer_events_sender();
        let shutdown_rx_vsock = handle.shutdown_rx();
        tokio::spawn(async move {
            if let Err(e) = wallhack_core::ipc::run_vsock_listener(
                ipc_api_vsock,
                peer_events_vsock,
                wallhack_core::ipc::VSOCK_IPC_PORT,
                shutdown_rx_vsock,
            )
            .await
            {
                tracing::warn!("vsock IPC listener unavailable: {e}");
            }
        });
    }

    tokio::select! {
        result = handle.wait() => result.map_err(|e| NodeError::Config(e.to_string())),
        _ = ipc_task => Ok(()),
    }
}

// ============================================================================
// Unified node constructor
// ============================================================================

/// Start a node in the given mode and return a [`DaemonHandle`].
///
/// Spawns the node into a background task. Use [`DaemonHandle::wait`] to
/// block until the node exits, or [`DaemonHandle::shutdown`] to stop it.
///
/// # Errors
///
/// Returns error if node setup fails.
pub fn start_node(
    config: &DaemonConfig,
    log_buffer: Option<LogBuffer>,
) -> Result<DaemonHandle, NodeError> {
    let role = match &config.mode {
        ModeConfig::Entry(_) => NodeRole::Entry,
        ModeConfig::Exit(_) => NodeRole::Exit,
        ModeConfig::Relay(_) => NodeRole::Relay,
        ModeConfig::Auto(cfg) => match &cfg.hint {
            Some(hint) if hint.level == wallhack_wire::data::HintLevel::Fixed as i32 => {
                wallhack_wire::data::NodeRole::try_from(hint.target)
                    .map_or(NodeRole::Indeterminate, NodeRole::from)
            }
            _ => NodeRole::Indeterminate,
        },
    };

    let metrics = Arc::new(Metrics::default());
    let peers = Arc::new(Registry::new());
    let routes = RouteTable::shared();
    let (route_update_tx, route_update_rx) = tokio::sync::broadcast::channel(16);

    let handler = Handler::new(
        HandlerConfig::new(
            role,
            config.mode.name().to_string(),
            config.global.version.clone(),
        ),
        Arc::clone(&metrics),
        Arc::clone(&peers),
        Arc::clone(&routes),
        route_update_tx.clone(),
        log_buffer,
    );
    let node_state = handler.node_state();
    let node_api: Arc<dyn NodeApi> = Arc::new(handler);

    let (shutdown_tx, _shutdown_rx) = watch::channel(());

    let peers_for_handle = Arc::clone(&peers);
    let config = config.clone();
    let resources = mode::NodeResources {
        metrics,
        peers,
        routes,
        route_updates: route_update_rx,
        route_updates_tx: route_update_tx,
        node_state,
    };
    let task = tokio::spawn(async move { mode::run(&config, resources).await.map_err(Into::into) });

    Ok(DaemonHandle::new(
        node_api,
        peers_for_handle,
        shutdown_tx,
        task,
    ))
}
