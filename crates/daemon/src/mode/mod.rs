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
    pub route_updates:
        tokio::sync::broadcast::Receiver<wallhack_core::control::routes::RouteUpdate>,
    pub route_updates_tx:
        tokio::sync::broadcast::Sender<wallhack_core::control::routes::RouteUpdate>,
    pub node_state: SharedNodeState,
}

/// Inject a Ping message into the control stream.
pub(crate) async fn send_ping(
    control_tx: &tokio::sync::mpsc::Sender<wallhack_wire::control::ControlMessage>,
) -> Result<(), crate::NodeError> {
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
        .map_err(|_| crate::NodeError::ChannelClosed)
}

/// Spawn a background heartbeat task for any connection.
///
/// Fires an initial ping immediately, then pings every 30 seconds.
/// Consumes latency measurements from the transport control loop and
/// updates the peer registry. Runs until the control channel closes
/// or the returned handle is dropped/aborted.
pub(crate) fn spawn_heartbeat(
    control_tx: tokio::sync::mpsc::Sender<wallhack_wire::control::ControlMessage>,
    latency_rx: Option<tokio::sync::mpsc::Receiver<f64>>,
    peer_name: String,
    peers: Arc<Registry>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        // Initial ping so latency is populated immediately after connect.
        if let Err(e) = send_ping(&control_tx).await {
            tracing::debug!("Initial ping failed: {e}");
            return;
        }

        let mut latency_rx = latency_rx.unwrap_or_else(|| tokio::sync::mpsc::channel(1).1);
        let mut heartbeat = tokio::time::interval(std::time::Duration::from_secs(30));
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        heartbeat.tick().await; // consume first immediate tick

        loop {
            tokio::select! {
                Some(ms) = latency_rx.recv() => {
                    peers.update_latency(&peer_name, ms);
                }
                _ = heartbeat.tick() => {
                    if let Err(e) = send_ping(&control_tx).await {
                        tracing::debug!("Heartbeat ping failed: {e}");
                        break;
                    }
                }
            }
        }
    })
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
                resources.route_updates,
                resources.route_updates_tx,
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
            relay::run(
                &config.global,
                cfg,
                resources.metrics,
                resources.peers,
                resources.node_state,
            )
            .await
        }
        ModeConfig::Auto(cfg) => {
            auto::run(
                &config.global,
                cfg,
                resources.metrics,
                resources.peers,
                resources.routes,
                resources.route_updates,
                resources.route_updates_tx,
                resources.node_state,
            )
            .await
        }
    }
}
