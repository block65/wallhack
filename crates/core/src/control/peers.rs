//! Peer registry for tracking connected nodes.

use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

use arc_swap::ArcSwap;
use tokio::sync::{mpsc, oneshot};

use crate::{NodeRole, node_api::NodeApiError};

/// Request to ping a peer, with a channel to send the result back.
pub type PingRequest = oneshot::Sender<f64>;

/// Information about a connected peer.
#[derive(Debug, Clone)]
pub struct PeerInfo {
    /// Name of the peer (user-provided or auto-generated).
    pub name: String,
    /// Remote address of the peer.
    pub addr: String,
    /// What type of node this peer is.
    pub role: NodeRole,
    /// Whether this peer has relay capability (connect + listen).
    pub has_relay_capability: bool,
    /// When the peer connected.
    pub connected_at: Instant,
    /// Total bytes transferred through this peer.
    pub bytes_transferred: u64,
    /// Latest measured latency in milliseconds.
    pub latency_ms: Option<f64>,
    /// When latency was last measured.
    pub latency_measured_at: Option<Instant>,
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
}

impl Default for Registry {
    fn default() -> Self {
        Self {
            peers: ArcSwap::from_pointee(HashMap::new()),
            ping_channels: ArcSwap::from_pointee(HashMap::new()),
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

    /// Register a new peer.
    pub fn register(&self, id: String, addr: String, role: NodeRole) {
        let info = PeerInfo {
            name: id.clone(),
            addr,
            role,
            has_relay_capability: false,
            connected_at: Instant::now(),
            bytes_transferred: 0,
            latency_ms: None,
            latency_measured_at: None,
        };
        self.peers.rcu(move |old| {
            let mut new = (**old).clone();
            new.insert(id.clone(), info.clone());
            new
        });
    }

    /// Update relay capability for a peer.
    pub fn set_relay_capability(&self, id: &str, has_capability: bool) {
        self.peers.rcu(|old| {
            let mut new = (**old).clone();
            if let Some(peer) = new.get_mut(id) {
                peer.has_relay_capability = has_capability;
            }
            new
        });
    }

    /// Unregister a peer.
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
        removed
    }

    /// Register a ping channel for a peer's connection handler.
    ///
    /// Returns the receiver that the connection handler should listen on.
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
        registry.register("peer1".into(), "1.2.3.4:5678".into(), NodeRole::Exit);

        assert_eq!(registry.count(), 1);
        assert!(registry.get("peer1").is_some());

        let removed = registry.unregister("peer1");
        assert!(removed.is_some());
        assert_eq!(registry.count(), 0);
    }

    #[test]
    fn test_update_latency() {
        let registry = Registry::new();
        registry.register("peer1".into(), "1.2.3.4:5678".into(), NodeRole::Exit);

        registry.update_latency("peer1", 42.5);

        let peer = registry.get("peer1").unwrap();
        assert_eq!(peer.latency_ms, Some(42.5));
        assert!(peer.latency_measured_at.is_some());
    }

    #[test]
    fn test_add_bytes() {
        let registry = Registry::new();
        registry.register("peer1".into(), "1.2.3.4:5678".into(), NodeRole::Exit);

        registry.add_bytes("peer1", 100);
        registry.add_bytes("peer1", 50);

        let peer = registry.get("peer1").unwrap();
        assert_eq!(peer.bytes_transferred, 150);
    }
}
