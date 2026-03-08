//! Node mode implementations.
//!
//! Each mode handles one operational role (entry, exit, relay, or auto).
//! The unified [`run`] dispatcher routes to the appropriate mode based on the
//! config.

pub(crate) mod auto;
pub(crate) mod entry;
pub(crate) mod exit;
pub(crate) mod relay;

use std::sync::Arc;

use tokio::sync::watch;
use wallhack_core::control::{
    handler::SharedRole, metrics::Metrics, peers::Registry, routes::SharedRouteTable,
};
use wallhack_wire::data::RoleHint;

use crate::{
    NodeError,
    daemon_config::{DaemonConfig, ModeConfig},
};

/// Shared resources available to all node modes.
pub(crate) struct NodeResources {
    pub metrics: Arc<Metrics>,
    pub peers: Arc<Registry>,
    pub routes: SharedRouteTable,
    pub shared_role: SharedRole,
    pub hint_rx: watch::Receiver<Option<RoleHint>>,
}

/// Dispatch to the appropriate node mode based on the config.
///
/// # Errors
///
/// Returns error if the selected mode fails.
pub(crate) async fn run(config: &DaemonConfig, resources: NodeResources) -> Result<(), NodeError> {
    match &config.mode {
        ModeConfig::Entry(cfg) => {
            entry::run(
                &config.global,
                cfg,
                resources.metrics,
                resources.peers,
                resources.routes,
            )
            .await
        }
        ModeConfig::Exit(cfg) => {
            exit::run(&config.global, cfg, resources.metrics, resources.peers).await
        }
        ModeConfig::Relay(cfg) => relay::run(&config.global, cfg, resources.metrics).await,
        ModeConfig::Auto(cfg) => {
            auto::run(
                &config.global,
                cfg,
                resources.metrics,
                resources.peers,
                resources.routes,
                resources.shared_role,
                resources.hint_rx,
            )
            .await
        }
    }
}
