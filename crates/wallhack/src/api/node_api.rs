//! Internal node API for control clients.
//!
//! This trait provides a common interface for all node types (entry, exit, relay).
//! Clients like REPL and REST API consume this trait instead of directly accessing
//! internal state.

use std::net::SocketAddr;

use crate::{Cidr, NodeRole};

/// Mode indicating whether a node has relay capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeCapability {
	/// Standard exit node (no listen capability).
	Exit,
	/// Exit node with relay capability (has both connect + listen).
	Relay,
}

impl std::fmt::Display for NodeCapability {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self {
			NodeCapability::Exit => write!(f, "EXIT"),
			NodeCapability::Relay => write!(f, "RELAY"),
		}
	}
}

/// Status of a peer connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerStatus {
	/// Peer is connected and active.
	Connected,
	/// Peer is disconnected.
	Disconnected,
}

impl std::fmt::Display for PeerStatus {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self {
			PeerStatus::Connected => write!(f, "Connected"),
			PeerStatus::Disconnected => write!(f, "Disconnected"),
		}
	}
}

/// Information about a directly connected peer.
#[derive(Debug, Clone)]
pub struct PeerInfo {
	/// Unique identifier for the peer.
	pub id: String,
	/// Remote address of the peer.
	pub addr: String,
	/// Whether this peer has relay capability.
	pub capability: NodeCapability,
	/// Connection status.
	pub status: PeerStatus,
	/// When the peer connected (seconds since epoch).
	pub connected_at_secs: u64,
	/// Total bytes transferred through this peer.
	pub bytes_transferred: u64,
	/// Latest measured latency in milliseconds.
	pub latency_ms: Option<f64>,
}

/// Route table entry mapping CIDR to peer.
#[derive(Debug, Clone)]
pub struct RouteEntry {
	/// Destination network.
	pub cidr: Cidr,
	/// Peer responsible for this route.
	pub peer_id: String,
}

/// Traffic and connection metrics.
#[derive(Debug, Clone, Default)]
pub struct Metrics {
	pub bytes_in: u64,
	pub bytes_out: u64,
	pub packets_in: u64,
	pub packets_out: u64,
	pub active_connections: u64,
	pub active_flows: u64,
}

/// Overall node status information.
#[derive(Debug, Clone)]
pub struct NodeStatus {
	/// Node's role.
	pub role: NodeRole,
	/// Whether node is connected to a peer (for exit/relay).
	pub connected: bool,
	/// Peer address (if connected).
	pub peer_addr: Option<String>,
	/// Whether node has relay capability.
	pub has_relay_capability: bool,
	/// Listen address (if listening).
	pub listen_addr: Option<SocketAddr>,
	/// Application version.
	pub version: String,
	/// Uptime in milliseconds.
	pub uptime_ms: u64,
}

/// Error types for node API operations.
#[derive(Debug, thiserror::Error)]
pub enum NodeApiError {
	#[error("peer not found: {0}")]
	PeerNotFound(String),
	#[error("route not found: {0}")]
	RouteNotFound(Cidr),
	#[error("operation not supported on this node type")]
	NotSupported,
	#[error("invalid address: {0}")]
	InvalidAddress(String),
	#[error("already connected")]
	AlreadyConnected,
	#[error("already listening")]
	AlreadyListening,
	#[error("not connected")]
	NotConnected,
	#[error("internal error: {0}")]
	Internal(String),
}

pub type Result<T> = std::result::Result<T, NodeApiError>;

/// Common API for all node types.
///
/// This trait provides a unified interface for querying and controlling nodes.
/// Different node types (entry, exit, relay) implement this trait with
/// appropriate subsets of functionality.
pub trait NodeApi: Send + Sync {
	/// Get list of directly connected peers.
	///
	/// For entry nodes: returns all connected exit/relay nodes.
	/// For exit nodes with relay capability: returns downstream connected nodes.
	/// For standard exit nodes: returns empty (no peers).
	fn peers(&self) -> Vec<PeerInfo>;

	/// Get routing table entries.
	///
	/// Only supported on entry nodes. Returns error for exit/relay nodes.
	fn routes(&self) -> Result<Vec<RouteEntry>>;

	/// Get traffic and connection metrics.
	fn metrics(&self) -> Metrics;

	/// Get overall node status.
	fn status(&self) -> NodeStatus;

	/// Connect to an upstream peer.
	///
	/// Only supported on exit nodes. Returns error for entry nodes.
	/// If already connected, returns `AlreadyConnected` error.
	fn connect(&self, addr: SocketAddr) -> Result<()>;

	/// Start listening for downstream connections.
	///
	/// Only supported on exit nodes. Returns error for entry nodes.
	/// If already listening, returns `AlreadyListening` error.
	/// Enables relay capability when combined with connect.
	fn listen(&self, addr: SocketAddr) -> Result<()>;

	/// Disconnect from upstream peer.
	///
	/// Only supported on exit nodes. Returns error for entry nodes.
	/// If relay capability is active, disables it (loses relay capability).
	fn disconnect(&self) -> Result<()>;

	/// Add a route mapping CIDR to peer.
	///
	/// Only supported on entry nodes. Returns error for exit/relay nodes.
	/// Peer must be directly connected.
	fn add_route(&self, cidr: Cidr, peer_id: String) -> Result<()>;

	/// Remove a route by CIDR.
	///
	/// Only supported on entry nodes. Returns error for exit/relay nodes.
	fn remove_route(&self, cidr: &Cidr) -> Result<()>;

	/// Disconnect a specific peer.
	///
	/// Only supported on entry nodes. Returns error for exit/relay nodes.
	fn disconnect_peer(&self, peer_id: String) -> Result<()>;
}
