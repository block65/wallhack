//! Peer registry for tracking connected nodes.

use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use arc_swap::ArcSwap;
use tokio::sync::{broadcast, mpsc};

use wallhack_wire::data::Capabilities;

use crate::{NodeRole, node_api::NodeApiError};

/// Events emitted when peers connect or disconnect.
#[derive(Debug, Clone)]
pub enum PeerEvent {
    Connected {
        name: String,
        addr: String,
        role: NodeRole,
    },
    Disconnected {
        name: String,
    },
}

/// Which side initiated the connection from the local node's perspective.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionSide {
    /// The peer connected to us (we accepted the connection).
    Accept,
    /// We connected to the peer.
    Connect,
}

impl std::fmt::Display for ConnectionSide {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConnectionSide::Accept => write!(f, "accept"),
            ConnectionSide::Connect => write!(f, "connect"),
        }
    }
}

/// Information about a connected peer.
#[derive(Debug, Clone)]
pub struct PeerInfo {
    /// Registry key — unique per connection. Equals `name` unless
    /// disambiguated (e.g. `foo#3` when another `foo` is already connected).
    pub id: String,
    /// User-provided peer name (from `--name`). May be shared across
    /// multiple connections from the same peer.
    pub name: String,
    /// Remote address of the peer.
    pub addr: String,
    /// What type of node this peer is.
    pub role: NodeRole,
    /// Advertised capabilities from the handshake.
    pub capabilities: Capabilities,
    /// Which side initiated the connection.
    pub side: ConnectionSide,
    /// When the peer connected (monotonic, for uptime/latency calculations).
    pub connect_time: Instant,
    /// When the peer connected (wall clock, seconds since epoch).
    pub connect_time_epoch: u64,
    /// Total bytes transferred through this peer.
    pub bytes_transferred: u64,
    /// Latest measured latency in milliseconds.
    pub latency_ms: Option<f64>,
    /// When latency was last measured.
    pub latency_measured_at: Option<Instant>,
    /// TUN interface name for this peer (entry-side only).
    pub tun_name: Option<String>,
    /// Unique identifier for this connection instance.
    pub connection_id: u64,
}

/// Shared peer registry.
pub type SharedRegistry = Arc<Registry>;

/// Registry of connected peers.
///
/// Uses `ArcSwap` for wait-free reads.
#[derive(Debug)]
pub struct Registry {
    peers: ArcSwap<HashMap<String, PeerInfo>>,
    /// Per-peer control channel senders. Used for heartbeat pings,
    /// API-initiated disconnect, and any future per-peer commands.
    control_channels:
        ArcSwap<HashMap<String, mpsc::Sender<wallhack_wire::control::ControlMessage>>>,
    /// Broadcast channel for peer lifecycle events.
    events_tx: broadcast::Sender<PeerEvent>,
    /// Monotonic counter for assigning unique connection IDs.
    next_connection_id: AtomicU64,
}

impl Default for Registry {
    fn default() -> Self {
        let (events_tx, _) = broadcast::channel(64);
        Self {
            peers: ArcSwap::from_pointee(HashMap::new()),
            control_channels: ArcSwap::from_pointee(HashMap::new()),
            events_tx,
            next_connection_id: AtomicU64::new(0),
        }
    }
}

impl Registry {
    /// Create a new empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a new shared registry.
    #[must_use]
    pub fn shared() -> SharedRegistry {
        Arc::new(Self::new())
    }

    /// Subscribe to peer lifecycle events.
    pub fn subscribe(&self) -> broadcast::Receiver<PeerEvent> {
        self.events_tx.subscribe()
    }

    /// Returns a clone of the peer events sender.
    pub fn events_sender(&self) -> broadcast::Sender<PeerEvent> {
        self.events_tx.clone()
    }

    /// Register a new peer.
    ///
    /// Returns `(peer_id, connection_id)`. The `peer_id` is the actual
    /// registry key — equals `id` normally, or `id#N` if disambiguated.
    /// Use `peer_id` for all subsequent registry operations (heartbeat,
    /// latency updates, unregister).
    pub fn register(
        &self,
        id: String,
        addr: String,
        role: NodeRole,
        capabilities: Capabilities,
        side: ConnectionSide,
    ) -> (String, u64) {
        let connection_id = self.next_connection_id.fetch_add(1, Ordering::Relaxed) + 1;

        // If a peer with the same name already exists, evict it — the new
        // connection supersedes the old one (reconnect scenario). Send a
        // Disconnect so the old transport closes cleanly.
        if self.peers.load().contains_key(&id) {
            tracing::info!("Peer {id:?} reconnected, replacing existing entry");
            self.send_disconnect(&id, "superseded by new connection");
            // Remove from peers map; the old session task will notice its
            // control channel closed and clean up.
            self.unregister(&id);
        }
        let peer_id = id.clone();

        let info = PeerInfo {
            id: peer_id.clone(),
            name: id,
            addr,
            role,
            capabilities,
            side,
            connect_time: Instant::now(),
            connect_time_epoch: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            bytes_transferred: 0,
            latency_ms: None,
            latency_measured_at: None,
            tun_name: None,
            connection_id,
        };
        let event_addr = info.addr.clone();
        let event_role = info.role;
        let return_id = peer_id.clone();
        self.peers.rcu(move |old| {
            let mut new = (**old).clone();
            new.insert(peer_id.clone(), info.clone());
            new
        });
        let _ = self.events_tx.send(PeerEvent::Connected {
            name: return_id.clone(),
            addr: event_addr,
            role: event_role,
        });
        (return_id, connection_id)
    }

    /// Set the TUN interface name for a peer.
    pub fn set_tun_name(&self, id: &str, tun_name: &str) {
        let tun_name = tun_name.to_string();
        self.peers.rcu(|old| {
            let mut new = (**old).clone();
            if let Some(peer) = new.get_mut(id) {
                peer.tun_name = Some(tun_name.clone());
            }
            new
        });
    }

    /// Unregister a peer unconditionally.
    pub fn unregister(&self, id: &str) -> Option<PeerInfo> {
        self.control_channels.rcu(|old| {
            let mut new = (**old).clone();
            new.remove(id);
            new
        });
        let mut removed = None;
        self.peers.rcu(|old| {
            let mut new = (**old).clone();
            removed = new.remove(id);
            new
        });
        if removed.is_some() {
            let _ = self.events_tx.send(PeerEvent::Disconnected {
                name: id.to_string(),
            });
        }
        removed
    }

    /// Unregister a peer only if its connection ID matches.
    ///
    /// Prevents a stale task from evicting a newer connection that
    /// re-registered under the same name between the old task's exit
    /// and its cleanup.
    pub fn unregister_if_current(&self, id: &str, connection_id: u64) -> Option<PeerInfo> {
        let current = self.peers.load().get(id).map(|p| p.connection_id);
        if current != Some(connection_id) {
            tracing::debug!(
                peer = id,
                current = ?current,
                requested = connection_id,
                "skipping stale unregister"
            );
            return None;
        }
        self.unregister(id)
    }

    /// Register a control channel for a peer.
    ///
    /// Stores a clone of the peer's `control_tx` so the registry can send
    /// control messages (e.g. `Disconnect`) to the peer's connection task.
    pub fn register_control(
        &self,
        id: &str,
        tx: &mpsc::Sender<wallhack_wire::control::ControlMessage>,
    ) {
        let tx = tx.clone();
        self.control_channels.rcu(|old| {
            let mut new = (**old).clone();
            new.insert(id.to_string(), tx.clone());
            new
        });
    }

    /// Send a Disconnect message to a peer's connection task.
    ///
    /// Returns `true` if the message was queued, `false` if no control
    /// channel is registered for this peer (already disconnected).
    pub fn send_disconnect(&self, id: &str, reason: &str) -> bool {
        use wallhack_wire::control::{ControlMessage, Disconnect, control_message};

        let channels = self.control_channels.load();
        let Some(tx) = channels.get(id) else {
            return false;
        };
        let msg = ControlMessage {
            message: Some(control_message::Message::Disconnect(Disconnect {
                reason: reason.to_string(),
            })),
        };
        tx.try_send(msg).is_ok()
    }

    /// Update bytes transferred for a peer.
    pub fn add_bytes(&self, id: &str, bytes: u64) {
        self.peers.rcu(|old| {
            let mut new = (**old).clone();
            if let Some(peer) = new.get_mut(id) {
                peer.bytes_transferred = peer.bytes_transferred.saturating_add(bytes);
            }
            new
        });
    }

    /// Update latency measurement for a peer.
    pub fn update_latency(&self, id: &str, latency_ms: f64) {
        self.peers.rcu(|old| {
            let mut new = (**old).clone();
            if let Some(peer) = new.get_mut(id) {
                peer.latency_ms = Some(latency_ms);
                peer.latency_measured_at = Some(Instant::now());
            }
            new
        });
    }

    /// Get info for a specific peer.
    #[must_use]
    pub fn get(&self, id: &str) -> Option<PeerInfo> {
        self.peers.load().get(id).cloned()
    }

    /// List all peers.
    #[must_use]
    pub fn list(&self) -> Vec<PeerInfo> {
        self.peers.load().values().cloned().collect()
    }

    /// Get number of connected peers.
    #[must_use]
    pub fn count(&self) -> usize {
        self.peers.load().len()
    }

    /// Get names of all peers (for iteration without holding lock)
    #[must_use]
    pub fn peer_names(&self) -> Vec<String> {
        self.peers.load().keys().cloned().collect()
    }

    /// Check if a peer's latency is stale (older than threshold).
    #[must_use]
    pub fn is_latency_stale(&self, id: &str, threshold: Duration) -> bool {
        self.peers.load().get(id).is_none_or(|p| {
            p.latency_measured_at
                .is_none_or(|t| t.elapsed() > threshold)
        })
    }

    /// Find a peer by exact address match.
    ///
    /// # Errors
    ///
    /// Returns `PeerNotFound` if no peers have that address.
    pub fn find_by_addr(&self, addr: &str) -> Result<PeerInfo, NodeApiError> {
        let peers = self.peers.load();
        let mut matches = peers.values().filter(|p| p.addr == addr);
        match (matches.next(), matches.next()) {
            (None, _) => Err(NodeApiError::PeerNotFound(addr.to_string())),
            (Some(p), None) => Ok(p.clone()),
            (Some(_p), Some(_)) => Err(NodeApiError::PeerAmbiguous(
                addr.to_string(),
                peers
                    .values()
                    .filter(|q| q.addr == addr)
                    .map(|q| q.name.clone())
                    .collect(),
            )),
        }
    }

    /// Find a peer by name prefix.
    ///
    /// Returns the peer if exactly one peer name starts with the prefix.
    /// Returns an error if no peers match, or if the prefix is ambiguous.
    ///
    /// # Errors
    ///
    /// Returns `PeerNotFound` if no peers match the prefix.
    /// Returns `PeerAmbiguous` if multiple peers match the prefix.
    ///
    /// # Panics
    ///
    /// This function will panic if a match is found but the iterator is empty,
    /// which should never happen given the match guard.
    pub fn find_by_prefix(&self, prefix: &str) -> Result<PeerInfo, NodeApiError> {
        let peers = self.peers.load();
        let matches: Vec<_> = peers
            .values()
            .filter(|p| p.name.starts_with(prefix))
            .cloned()
            .collect();

        match matches.len() {
            0 => Err(NodeApiError::PeerNotFound(prefix.to_string())),
            1 => Ok(matches.into_iter().next().expect("match count is 1")),
            _ => {
                let names = matches.iter().map(|p| p.name.clone()).collect();
                Err(NodeApiError::PeerAmbiguous(prefix.to_string(), names))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_register_unregister() {
        let registry = Registry::new();
        registry.register(
            "peer1".into(),
            "1.2.3.4:5678".into(),
            NodeRole::Exit,
            Capabilities::default(),
            ConnectionSide::Accept,
        );

        assert_eq!(registry.count(), 1);
        assert!(registry.get("peer1").is_some());

        let removed = registry.unregister("peer1");
        assert!(removed.is_some());
        assert_eq!(registry.count(), 0);
    }

    #[test]
    fn test_update_latency() {
        let registry = Registry::new();
        registry.register(
            "peer1".into(),
            "1.2.3.4:5678".into(),
            NodeRole::Exit,
            Capabilities::default(),
            ConnectionSide::Accept,
        );

        registry.update_latency("peer1", 42.5);

        let peer = registry.get("peer1").unwrap();
        assert_eq!(peer.latency_ms, Some(42.5));
        assert!(peer.latency_measured_at.is_some());
    }

    #[test]
    fn test_add_bytes() {
        let registry = Registry::new();
        registry.register(
            "peer1".into(),
            "1.2.3.4:5678".into(),
            NodeRole::Exit,
            Capabilities::default(),
            ConnectionSide::Accept,
        );

        registry.add_bytes("peer1", 100);
        registry.add_bytes("peer1", 50);

        let peer = registry.get("peer1").unwrap();
        assert_eq!(peer.bytes_transferred, 150);
    }

    #[test]
    fn test_register_emits_connected_event() {
        let registry = Registry::new();
        let mut rx = registry.subscribe();

        registry.register(
            "peer1".into(),
            "1.2.3.4:5678".into(),
            NodeRole::Exit,
            Capabilities::default(),
            ConnectionSide::Accept,
        );

        let event = rx.try_recv().unwrap();
        assert!(matches!(event, PeerEvent::Connected { ref name, .. } if name == "peer1"));
    }

    #[test]
    fn test_unregister_emits_disconnected_event() {
        let registry = Registry::new();
        registry.register(
            "peer1".into(),
            "1.2.3.4:5678".into(),
            NodeRole::Exit,
            Capabilities::default(),
            ConnectionSide::Accept,
        );

        let mut rx = registry.subscribe();
        registry.unregister("peer1");

        let event = rx.try_recv().unwrap();
        assert!(matches!(event, PeerEvent::Disconnected { ref name } if name == "peer1"));
    }

    #[test]
    fn test_duplicate_name_disambiguated() {
        let registry = Registry::new();
        let (id1, _) = registry.register(
            "peer1".into(),
            "1.2.3.4:5678".into(),
            NodeRole::Exit,
            Capabilities::default(),
            ConnectionSide::Accept,
        );
        assert_eq!(id1, "peer1");

        // Re-register same name — old entry is evicted, new one takes the name.
        let (id2, _) = registry.register(
            "peer1".into(),
            "1.2.3.4:9999".into(),
            NodeRole::Exit,
            Capabilities::default(),
            ConnectionSide::Accept,
        );
        assert_eq!(id2, "peer1", "reconnect should reuse the name");
        assert_eq!(registry.count(), 1, "old entry should be evicted");

        registry.unregister(&id2);
        assert_eq!(registry.count(), 0);
    }

    #[test]
    fn test_unregister_unknown_does_not_emit() {
        let registry = Registry::new();
        let mut rx = registry.subscribe();

        registry.unregister("ghost");

        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn test_connection_id_prevents_stale_unregister() {
        let registry = Registry::new();
        let (peer_id1, gen1) = registry.register(
            "peer1".into(),
            "1.2.3.4:5678".into(),
            NodeRole::Exit,
            Capabilities::default(),
            ConnectionSide::Accept,
        );
        assert_eq!(peer_id1, "peer1");
        // Re-register same peer — old entry is evicted, new one takes the name.
        let (peer_id2, gen2) = registry.register(
            "peer1".into(),
            "1.2.3.4:9999".into(),
            NodeRole::Exit,
            Capabilities::default(),
            ConnectionSide::Accept,
        );
        assert_eq!(peer_id2, "peer1");
        assert_ne!(gen1, gen2);
        assert_eq!(registry.count(), 1);

        // Old task tries to unregister with stale connection_id — should be a no-op
        // since the entry now belongs to the new connection.
        assert!(registry.unregister_if_current(&peer_id1, gen1).is_none());
        assert_eq!(registry.count(), 1);

        // New task unregisters with current connection_id — should succeed.
        assert!(registry.unregister_if_current(&peer_id2, gen2).is_some());
        assert_eq!(registry.count(), 0);
    }
}
