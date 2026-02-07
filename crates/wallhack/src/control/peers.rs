//! Peer registry for tracking connected nodes.

use std::{
	collections::HashMap,
	sync::Arc,
	time::{Duration, Instant},
};

use parking_lot::RwLock;

use crate::NodeRole;

/// Information about a connected peer.
#[derive(Debug, Clone)]
pub struct PeerInfo {
	/// Unique identifier for the peer.
	pub id: String,
	/// Remote address of the peer.
	pub addr: String,
	/// What type of node this peer is.
	pub role: NodeRole,
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
#[derive(Debug, Default)]
pub struct Registry {
	peers: RwLock<HashMap<String, PeerInfo>>,
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
			id: id.clone(),
			addr,
			role,
			connected_at: Instant::now(),
			bytes_transferred: 0,
			latency_ms: None,
			latency_measured_at: None,
		};
		self.peers.write().insert(id, info);
	}

	/// Unregister a peer.
	pub fn unregister(&self, id: &str) -> Option<PeerInfo> {
		self.peers.write().remove(id)
	}

	/// Update bytes transferred for a peer.
	pub fn add_bytes(&self, id: &str, bytes: u64) {
		if let Some(peer) = self.peers.write().get_mut(id) {
			peer.bytes_transferred = peer.bytes_transferred.saturating_add(bytes);
		}
	}

	/// Update latency measurement for a peer.
	pub fn update_latency(&self, id: &str, latency_ms: f64) {
		if let Some(peer) = self.peers.write().get_mut(id) {
			peer.latency_ms = Some(latency_ms);
			peer.latency_measured_at = Some(Instant::now());
		}
	}

	/// Get info for a specific peer.
	#[must_use]
	pub fn get(&self, id: &str) -> Option<PeerInfo> {
		self.peers.read().get(id).cloned()
	}

	/// List all peers.
	#[must_use]
	pub fn list(&self) -> Vec<PeerInfo> {
		self.peers.read().values().cloned().collect()
	}

	/// Get number of connected peers.
	#[must_use]
	pub fn count(&self) -> usize {
		self.peers.read().len()
	}

	/// Get IDs of all peers (for iteration without holding lock)
	#[must_use]
	pub fn peer_ids(&self) -> Vec<String> {
		self.peers.read().keys().cloned().collect()
	}

	/// Check if a peer's latency is stale (older than threshold).cargo fmt
	#[must_use]
	pub fn is_latency_stale(&self, id: &str, threshold: Duration) -> bool {
		self.peers.read().get(id).is_none_or(|p| {
			p.latency_measured_at
				.is_none_or(|t| t.elapsed() > threshold)
		})
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
