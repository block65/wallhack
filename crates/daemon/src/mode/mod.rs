//! Node mode implementations.
//!
//! Each mode handles one operational role (entry, exit, relay). The unified
//! [`run`] dispatcher routes to the appropriate mode based on the config.

pub(crate) mod entry;
pub(crate) mod exit;
pub(crate) mod relay;

use std::sync::Arc;

use wallhack_core::control::{metrics::Metrics, peers::Registry, routes::SharedRouteTable};

use crate::{
    NodeError,
    daemon_config::{DaemonConfig, ModeConfig},
};

/// Shared resources available to all node modes.
pub(crate) struct NodeResources {
    pub metrics: Arc<Metrics>,
    pub peers: Arc<Registry>,
    pub routes: SharedRouteTable,
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
    }
}
