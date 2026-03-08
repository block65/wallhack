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

use wallhack_core::control::{
    handler::SharedNodeState, metrics::Metrics, peers::Registry, routes::SharedRouteTable,
};

use crate::{
    NodeError,
    daemon_config::{DaemonConfig, ModeConfig},
};

/// Deduplicates repeated PSK authentication failure logs per source IP.
///
/// Keys on IP only (strips port) so reconnects from the same host with
/// different ephemeral ports are correctly deduplicated.
/// Logs on the first failure and at power-of-two counts (1, 2, 4, 8, …).
pub(crate) struct PskFailTracker {
    counts: std::collections::HashMap<std::net::IpAddr, u32>,
}

impl PskFailTracker {
    pub fn new() -> Self {
        Self {
            counts: std::collections::HashMap::new(),
        }
    }

    /// Record a failure for `addr` (ip:port). Logs with dedup (first + powers of two).
    pub fn record(&mut self, addr: &str) {
        let ip = addr
            .parse::<std::net::SocketAddr>()
            .map(|sa| sa.ip())
            .or_else(|_| addr.parse::<std::net::IpAddr>())
            .unwrap_or(std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED));
        let count = self.counts.entry(ip).or_insert(0);
        *count += 1;
        if *count == 1 || count.is_power_of_two() {
            tracing::warn!("PSK authentication failed for {ip} (x{count})");
        }
    }
}

/// Shared resources available to all node modes.
pub(crate) struct NodeResources {
    pub metrics: Arc<Metrics>,
    pub peers: Arc<Registry>,
    pub routes: SharedRouteTable,
    pub node_state: SharedNodeState,
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
                resources.node_state,
            )
            .await
        }
        ModeConfig::Exit(cfg) => {
            exit::run(
                &config.global,
                cfg,
                resources.metrics,
                resources.peers,
                resources.node_state,
            )
            .await
        }
        ModeConfig::Relay(cfg) => {
            relay::run(&config.global, cfg, resources.metrics, resources.node_state).await
        }
        ModeConfig::Auto(cfg) => {
            auto::run(
                &config.global,
                cfg,
                resources.metrics,
                resources.peers,
                resources.routes,
                resources.node_state,
            )
            .await
        }
    }
}
