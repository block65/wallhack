#![warn(unused_extern_crates)]

pub mod built_info {
    #![allow(clippy::needless_raw_string_hashes, clippy::doc_markdown)]
    include!(concat!(env!("OUT_DIR"), "/built.rs"));
}

pub mod address_spec;
pub mod daemon_config;
pub mod dns;
pub mod error;

mod config;
mod mode;
mod net;
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
/// Returns [`NodeError`] for node failures.
pub async fn run_daemon_engine(
    config: DaemonConfig,
    socket_path_override: Option<std::path::PathBuf>,
) -> Result<(), NodeError> {
    let display_version = config
        .binary_version
        .as_deref()
        .unwrap_or(built_info::PKG_VERSION);
    let dirty = if built_info::GIT_DIRTY == Some(true) {
        "-dirty"
    } else {
        ""
    };
    let build_id = match built_info::GIT_COMMIT_HASH_SHORT {
        Some(hash) => format!("{hash}{dirty}"),
        None => format!("{}{dirty}", built_info::BUILT_TIME_UTC),
    };
    tracing::info!(
        "{} {} ({build_id})  {}",
        built_info::PKG_NAME,
        display_version,
        config.mode.name()
    );

    sys::check_entropy_ready();

    let handle = start_node(&config)?;

    // Start IPC listener for the management protocol.
    let socket_path = socket_path_override.unwrap_or_else(wallhack_core::ipc::socket_path);
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
pub fn start_node(config: &DaemonConfig) -> Result<DaemonHandle, NodeError> {
    let role = match &config.mode {
        ModeConfig::Entry(_) => NodeRole::Entry,
        ModeConfig::Exit(_) => NodeRole::Exit,
        ModeConfig::Relay(_) => NodeRole::Relay,
        ModeConfig::Auto(_) => NodeRole::Indeterminate,
    };

    let metrics = Arc::new(Metrics::default());
    let peers = Arc::new(Registry::new());
    let routes = RouteTable::shared();

    let handler = Handler::new(
        HandlerConfig::new(
            role,
            built_info::PKG_NAME.to_string(),
            built_info::PKG_VERSION.to_string(),
        ),
        Arc::clone(&metrics),
        Arc::clone(&peers),
        Arc::clone(&routes),
    );
    let node_state = handler.node_state();
    let node_api: Arc<dyn NodeApi> = Arc::new(handler);

    let (shutdown_tx, _shutdown_rx) = watch::channel(());

    let handle_peers = Arc::clone(&peers);
    let config = config.clone();
    let resources = mode::NodeResources {
        metrics,
        peers,
        routes,
        node_state,
    };
    let task = tokio::spawn(async move { mode::run(&config, resources).await.map_err(Into::into) });

    Ok(DaemonHandle::new(node_api, handle_peers, shutdown_tx, task))
}
