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
    /// Receiver for dynamic commands (connect, listen, disconnect) sent from
    /// the control API. Modes that support dynamic operations consume this;
    /// modes that do not simply drop it, which causes the sender side to
    /// return `NotSupported`.
    pub cmd_rx:
        Option<tokio::sync::mpsc::Receiver<wallhack_core::control::node_command::NodeCommand>>,
}

/// Derive a peer's role from its advertised capabilities.
///
/// A peer that both listens and connects is a relay; a peer with TUN
/// capability is an entry; otherwise it is an exit node.
pub(crate) fn peer_role_from_capabilities(
    caps: wallhack_wire::data::Capabilities,
) -> wallhack_core::NodeRole {
    if caps.listening && caps.connecting {
        wallhack_core::NodeRole::Relay
    } else if caps.tun_capable {
        wallhack_core::NodeRole::Entry
    } else {
        wallhack_core::NodeRole::Exit
    }
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
        .as_micros() as u64;

    let ping_msg = ControlMessage {
        message: Some(control_message::Message::Ping(wallhack_wire::data::Ping {
            timestamp_us: ts,
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
/// Latency is now updated directly by the control loop Pong handler via
/// the peer registry. Runs until the control channel closes or the
/// returned handle is dropped/aborted.
pub(crate) fn spawn_heartbeat(
    control_tx: tokio::sync::mpsc::Sender<wallhack_wire::control::ControlMessage>,
    peer_name: String,
    peers: Arc<Registry>,
) -> tokio::task::JoinHandle<()> {
    // Register control channel so peer_disconnect can send messages to this peer.
    peers.register_control(&peer_name, &control_tx);

    tokio::spawn(async move {
        // Initial ping so latency is populated immediately after connect.
        if let Err(e) = send_ping(&control_tx).await {
            tracing::debug!("Initial ping failed: {e}");
            return;
        }

        let mut heartbeat = tokio::time::interval(std::time::Duration::from_secs(30));
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        heartbeat.tick().await; // consume first immediate tick

        loop {
            heartbeat.tick().await;
            if let Err(e) = send_ping(&control_tx).await {
                tracing::debug!("Heartbeat ping failed: {e}");
                break;
            }
        }

        // Heartbeat exits when the control channel is closed (peer gone).
        // Unregister here so the registry stays clean regardless of which
        // transport task exits first.
        peers.unregister(&peer_name);
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
            // Entry mode does not support dynamic commands; drop the receiver
            // so the sender side returns NotSupported.
            drop(resources.cmd_rx);
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
            drop(resources.cmd_rx);
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
            drop(resources.cmd_rx);
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
                resources.cmd_rx,
            )
            .await
        }
    }
}
