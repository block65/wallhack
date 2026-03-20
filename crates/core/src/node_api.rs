//! Internal node API for control clients.
//!
//! This trait provides a common interface for all node types (entry, exit, relay).
//! Clients like REPL and REST API consume this trait instead of directly accessing
//! internal state.

use std::net::SocketAddr;

use wallhack_wire::data::{Capabilities, RoleHint};

use crate::{Cidr, NodeRole};

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
    /// Unique registry key (equals `name` unless disambiguated).
    pub id: String,
    /// User-provided peer name (from `--name`).
    pub name: String,
    /// Remote address of the peer.
    pub addr: String,
    /// The peer's negotiated role.
    pub role: NodeRole,
    /// Advertised capabilities from the handshake.
    pub capabilities: Capabilities,
    /// Which side initiated the connection.
    pub side: crate::control::peers::ConnectionSide,
    /// Connection status.
    pub status: PeerStatus,
    /// When the peer connected (seconds since epoch).
    pub connect_time: u64,
    /// Total bytes transferred through this peer.
    pub bytes_transferred: u64,
    /// Latest measured latency in milliseconds.
    pub latency_ms: Option<f64>,
    /// TUN interface name for this peer (entry-side only, `None` otherwise).
    pub tun_name: Option<String>,
}

/// Route table entry mapping CIDR to peer.
#[derive(Debug, Clone)]
pub struct RouteEntry {
    /// Destination network.
    pub cidr: Cidr,
    /// Name of the peer responsible for this route.
    pub peer: String,
    /// When the route was added.
    pub create_time: std::time::Instant,
    /// True if auto-installed from a peer's handshake advertisement.
    pub auto_managed: bool,
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
    pub packets_dropped: u64,
}

/// Overall node info.
#[derive(Debug, Clone)]
pub struct NodeInfo {
    /// Node's role.
    pub role: NodeRole,
    /// Peer address (if connected).
    pub peer_addr: Option<String>,
    /// Advertised capabilities.
    pub capabilities: Capabilities,
    /// Listen address (if listening).
    pub listen_addr: Option<SocketAddr>,
    /// Application name.
    pub name: String,
    /// Application version.
    pub version: String,
    /// Uptime in milliseconds.
    pub uptime_ms: u64,
}

/// Result of a successful connect operation.
#[derive(Debug, Clone)]
pub struct ConnectInfo {
    /// Resolved peer address.
    pub peer_addr: String,
    /// Transport protocol used (e.g. "QUIC", "WebSocket").
    pub protocol: String,
}

/// Result of a successful listen operation.
#[derive(Debug, Clone)]
pub struct ListenInfo {
    /// Actual bound address (may differ from requested if port was 0).
    pub listen_addr: SocketAddr,
    /// Transport protocol used (e.g. "QUIC", "WebSocket").
    pub protocol: String,
    /// Certificate fingerprint (SHA-256).
    pub fingerprint: String,
}

/// Error types for node API operations.
#[derive(Debug, thiserror::Error)]
pub enum NodeApiError {
    #[error("peer not found: {0}")]
    PeerNotFound(String),
    #[error("peer name is ambiguous: {0} (matches: {1:?})")]
    PeerAmbiguous(String, Vec<String>),
    #[error("route not found: {0}")]
    RouteNotFound(Cidr),
    #[error("{0}")]
    NotSupported(String),
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
    /// For exit nodes with relay capability: returns accepted peer connections.
    /// For standard exit nodes: returns empty (no peers).
    fn peers(&self) -> Vec<PeerInfo>;

    /// Get routing table entries.
    ///
    /// Only supported on entry nodes. Returns error for exit/relay nodes.
    fn routes(&self) -> Result<Vec<RouteEntry>>;

    /// Get traffic and connection metrics.
    fn metrics(&self) -> Metrics;

    /// Get overall node info.
    fn info(&self) -> NodeInfo;

    /// Connect to a peer.
    ///
    /// `addr` is a raw address string that may be a hostname, IP, or
    /// `host:port`. Implementations are responsible for applying default
    /// ports and DNS resolution.
    ///
    /// Only supported in exit mode. Returns error for entry nodes.
    /// If already connected, returns `AlreadyConnected` error.
    fn connect(&self, addr: &str) -> Result<ConnectInfo>;

    /// Start listening for peer connections.
    ///
    /// Only supported in exit mode. Returns error for entry nodes.
    /// If already listening, returns `AlreadyListening` error.
    /// Enables relay capability when combined with connect.
    fn listen(&self, addr: SocketAddr) -> Result<ListenInfo>;

    /// Disconnect from the connected peer.
    ///
    /// Only supported in exit mode. Returns error for entry nodes.
    /// If relay capability is active, disables it (loses relay capability).
    fn disconnect(&self) -> Result<()>;

    /// Add a route mapping CIDR to peer.
    ///
    /// Only supported on entry nodes. Returns error for exit/relay nodes.
    /// Peer must be directly connected.
    ///
    /// Returns `Ok(Some(warning))` when the route was added but the peer's
    /// advertised routes do not cover the requested CIDR, meaning traffic
    /// may be silently dropped. Returns `Ok(None)` on clean success.
    fn add_route(&self, cidr: Cidr, peer: String) -> Result<Option<String>>;

    /// Delete a route by CIDR.
    ///
    /// Only supported on entry nodes. Returns error for exit/relay nodes.
    fn route_del(&self, cidr: &Cidr) -> Result<()>;

    /// Disconnect a specific peer by name prefix or address.
    ///
    /// Supports prefix matching for REPL/CLI convenience.
    fn peer_disconnect(&self, peer: String) -> Result<()>;

    /// Disconnect a specific peer by exact registry id.
    ///
    /// Used by the REST API where the id comes directly from the peers list.
    fn peer_disconnect_by_id(&self, id: String) -> Result<()>;

    /// Get the current negotiated role.
    fn current_role(&self) -> NodeRole;

    /// Apply a role hint at runtime.
    ///
    /// Triggers re-negotiation if the node is in auto mode.
    /// `role <target>` in the REPL is shorthand for `hint_set(Fixed, target)`.
    fn hint_set(&self, hint: RoleHint) -> Result<()>;

    /// Remove all hints (both startup and runtime).
    fn hint_set_auto(&self) -> Result<()>;

    /// Retrieve recent daemon log lines.
    ///
    /// Returns the most recent `count` lines from the in-memory log buffer.
    /// If `count` is 0, returns all buffered lines.
    fn logs(&self, count: u32) -> Vec<String>;
}
