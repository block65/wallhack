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
use tokio::sync::{broadcast, mpsc, oneshot};

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

/// Request to ping a peer, with a channel to send the result back.
pub type PingRequest = oneshot::Sender<f64>;

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
    /// Name of the peer (user-provided or auto-generated).
    pub name: String,
    /// Remote address of the peer.
    pub addr: String,
    /// What type of node this peer is.
    pub role: NodeRole,
    /// Advertised capabilities from the handshake.
    pub capabilities: Capabilities,
    /// Which side initiated the connection.
    pub side: ConnectionSide,
    /// When the peer connected.
    pub connected_at: Instant,
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
    /// Channels to request pings from connection handlers.
    ping_channels: ArcSwap<HashMap<String, mpsc::Sender<PingRequest>>>,
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
            ping_channels: ArcSwap::from_pointee(HashMap::new()),
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
    /// Returns a connection ID that must be passed to
    /// [`unregister_if_current`] to prevent a stale task from evicting
    /// a newer connection that re-registered under the same name.
    pub fn register(&self, id: String, addr: String, role: NodeRole, side: ConnectionSide) -> u64 {
        let connection_id = self.next_connection_id.fetch_add(1, Ordering::Relaxed) + 1;
        // TOCTOU: another thread could register the same id between this
        // check and the rcu insert. Harmless — a duplicate Connected event
        // is better than a missed one, and callers don't race on the same id.
        let is_new = !self.peers.load().contains_key(&id);
        let info = PeerInfo {
            name: id.clone(),
            addr,
            role,
            capabilities: Capabilities::default(),
            side,
            connected_at: Instant::now(),
            bytes_transferred: 0,
            latency_ms: None,
            latency_measured_at: None,
            tun_name: None,
            connection_id,
        };
        let event_addr = info.addr.clone();
        let event_role = info.role;
        let event_name = id.clone();
        self.peers.rcu(move |old| {
            let mut new = (**old).clone();
            new.insert(id.clone(), info.clone());
            new
        });
        if is_new {
            let _ = self.events_tx.send(PeerEvent::Connected {
                name: event_name,
                addr: event_addr,
                role: event_role,
            });
        }
        connection_id
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

    /// Update capability fields for a peer from a received `Handshake` message.
    pub fn update_capabilities(&self, id: &str, capabilities: &Capabilities) {
        self.peers.rcu(|old| {
            let mut new = (**old).clone();
            if let Some(peer) = new.get_mut(id) {
                peer.capabilities = *capabilities;
            }
            new
        });
    }

    /// Unregister a peer unconditionally.
    pub fn unregister(&self, id: &str) -> Option<PeerInfo> {
        self.ping_channels.rcu(|old| {
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

    /// Register a ping channel for a peer's connection handler.
    ///
    /// Returns the receiver that the connection handler should listen on.
    #[deprecated(note = "will be replaced by peer events")]
    pub fn register_ping_channel(&self, id: &str) -> mpsc::Receiver<PingRequest> {
        let (tx, rx) = mpsc::channel(1);
        self.ping_channels.rcu(|old| {
            let mut new = (**old).clone();
            new.insert(id.to_string(), tx.clone());
            new
        });
        rx
    }

    /// Ping a peer and return latency in milliseconds.
    ///
    /// Sends a request to the peer's connection handler, which performs the
    /// actual ping/pong exchange over the tunnel transport.
    ///
    /// # Errors
    ///
    /// Returns error if the peer doesn't exist or ping fails.
    #[deprecated(note = "will be replaced by peer events")]
    pub async fn ping_peer(
        &self,
        id: &str,
    ) -> Result<f64, Box<dyn std::error::Error + Send + Sync>> {
        let tx = {
            let channels = self.ping_channels.load();
            channels
                .get(id)
                .ok_or_else(|| format!("No ping channel for peer: {id}"))?
                .clone()
        };

        let (result_tx, result_rx) = oneshot::channel();
        tx.send(result_tx)
            .await
            .map_err(|_| "Peer connection closed")?;

        let latency = result_rx.await.map_err(|_| "Ping timed out")?;
        self.update_latency(id, latency);
        Ok(latency)
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
            ConnectionSide::Accept,
        );

        let mut rx = registry.subscribe();
        registry.unregister("peer1");

        let event = rx.try_recv().unwrap();
        assert!(matches!(event, PeerEvent::Disconnected { ref name } if name == "peer1"));
    }

    #[test]
    fn test_duplicate_register_does_not_emit() {
        let registry = Registry::new();
        registry.register(
            "peer1".into(),
            "1.2.3.4:5678".into(),
            NodeRole::Exit,
            ConnectionSide::Accept,
        );

        let mut rx = registry.subscribe();
        registry.register(
            "peer1".into(),
            "1.2.3.4:9999".into(),
            NodeRole::Exit,
            ConnectionSide::Accept,
        );

        assert!(rx.try_recv().is_err());
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
        let gen1 = registry.register(
            "peer1".into(),
            "1.2.3.4:5678".into(),
            NodeRole::Exit,
            ConnectionSide::Accept,
        );
        // Re-register same peer (new connection arrived before old task cleaned up).
        let gen2 = registry.register(
            "peer1".into(),
            "1.2.3.4:9999".into(),
            NodeRole::Exit,
            ConnectionSide::Accept,
        );
        assert_ne!(gen1, gen2);

        // Old task tries to unregister with stale connection_id — should be a no-op.
        assert!(registry.unregister_if_current("peer1", gen1).is_none());
        assert_eq!(registry.count(), 1);

        // New task unregisters with current connection_id — should succeed.
        assert!(registry.unregister_if_current("peer1", gen2).is_some());
        assert_eq!(registry.count(), 0);
    }
}
