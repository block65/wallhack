//! Control channel request handler.
//!
//! Handles incoming [`ControlRequest`] messages and produces
//! [`ControlResponse`] messages.

use std::{net::SocketAddr, sync::Arc, time::Instant};

use arc_swap::ArcSwap;
use tokio::sync::mpsc;
use wallhack_wire::{
    control::{
        ControlRequest, ControlResponse, ErrorResponse, PeerInfo, PingResponse, RouteInfo,
        StatsResponse, control_request, control_response,
    },
    data::{Capabilities, NodeRole as ProtoNodeRole, RoleHint},
};

use crate::NodeRole;

use super::{
    log_buffer::LogBuffer, metrics::SharedMetrics, peers::SharedRegistry, routes::SharedRouteTable,
};

/// Reply channel for a single command.
///
/// Uses a standard-library sync channel so the handler side (which may be
/// called from a synchronous context) can block waiting for the reply without
/// needing an async runtime handle.
type ReplySender<T> = std::sync::mpsc::SyncSender<Result<T, crate::node_api::NodeApiError>>;

/// Create a reply channel pair for a node command.
fn reply_channel<T>() -> (
    ReplySender<T>,
    std::sync::mpsc::Receiver<Result<T, crate::node_api::NodeApiError>>,
) {
    std::sync::mpsc::sync_channel(1)
}

/// A command sent from the handler/API layer to the mode task via the
/// control watch channel.
#[derive(Debug)]
pub enum NodeCommand {
    /// Set or clear the role hint.
    Role {
        /// `Some` to set a hint, `None` to clear (auto).
        hint: Option<RoleHint>,
    },
    /// Connect to a remote peer at the given address.
    Connect {
        /// Target address (host, host:port, etc.).
        addr: String,
        /// Channel for sending the result back to the caller.
        reply: ReplySender<crate::node_api::ConnectInfo>,
    },
    /// Start listening for incoming peer connections.
    Listen {
        /// Address to bind.
        addr: SocketAddr,
        /// Channel for sending the result back to the caller.
        reply: ReplySender<crate::node_api::ListenInfo>,
    },
    /// Disconnect from the currently connected peer.
    Disconnect {
        /// Channel for sending the result back to the caller.
        reply: ReplySender<()>,
    },
}

/// Mutable runtime state that can change after construction.
///
/// Stored behind `ArcSwap` for wait-free reads (same pattern as `Registry`).
#[derive(Debug, Clone)]
struct NodeState {
    role: NodeRole,
    capabilities: Capabilities,
    listen_addr: Option<SocketAddr>,
    peer_addr: Option<String>,
}

/// Shared node state handle, cloneable and cheaply updatable.
///
/// Consumers call [`SharedNodeState::update_role`], [`SharedNodeState::update_capabilities`],
/// etc. after negotiation or listening starts so that `wallhack info`
/// reflects the real state of the daemon.
#[derive(Clone, Debug)]
pub struct SharedNodeState(Arc<ArcSwap<NodeState>>);

impl SharedNodeState {
    fn new(role: NodeRole) -> Self {
        Self(Arc::new(ArcSwap::from_pointee(NodeState {
            role,
            capabilities: Capabilities::default(),
            listen_addr: None,
            peer_addr: None,
        })))
    }

    fn load(&self) -> arc_swap::Guard<Arc<NodeState>> {
        self.0.load()
    }

    /// Update the node role (e.g. after auto-negotiation resolves).
    pub fn update_role(&self, role: NodeRole) {
        self.0.rcu(|old| {
            let mut new = (**old).clone();
            new.role = role;
            new
        });
    }

    /// Update the node's own capabilities.
    pub fn update_capabilities(&self, capabilities: Capabilities) {
        self.0.rcu(|old| {
            let mut new = (**old).clone();
            new.capabilities = capabilities;
            new
        });
    }

    /// Record that the node is now listening on `addr`.
    pub fn set_listen_addr(&self, addr: SocketAddr) {
        self.0.rcu(|old| {
            let mut new = (**old).clone();
            new.listen_addr = Some(addr);
            new.capabilities.listening = true;
            new
        });
    }
}

/// Configuration for the control handler.
#[derive(Debug, Clone)]
pub struct HandlerConfig {
    /// The role of this node.
    pub node_role: NodeRole,
    /// Application name.
    pub name: String,
    /// Application version string.
    pub version: String,
}

impl HandlerConfig {
    /// Creates a new handler configuration with the specified role, name, and version.
    #[must_use]
    pub fn new(node_role: NodeRole, name: String, version: String) -> Self {
        Self {
            node_role,
            name,
            version,
        }
    }
}

/// Handler for control channel requests.
///
/// Processes incoming control requests and returns appropriate responses based
/// on the current state of metrics and configuration.
pub struct Handler {
    config: HandlerConfig,
    /// Command channel to the mode task. Carries role changes, connect,
    /// listen, and disconnect commands.
    command_source: mpsc::Sender<NodeCommand>,
    /// Receiver side of the command channel. Extracted once by the daemon
    /// before wrapping Handler in `Arc<dyn NodeApi>`.
    command_sink: std::sync::Mutex<Option<mpsc::Receiver<NodeCommand>>>,
    log_buffer: LogBuffer,
    metrics: SharedMetrics,
    peers: SharedRegistry,
    routes: SharedRouteTable,
    route_updates: tokio::sync::broadcast::Sender<super::routes::RouteUpdate>,
    state: SharedNodeState,
    start_time: Instant,
}

impl Handler {
    /// Creates a new control handler.
    ///
    /// `log_buffer`, when provided, is the shared ring buffer that the tracing
    /// subscriber also writes into — enabling the `logs` API to return recent
    /// daemon output.
    #[must_use]
    pub fn new(
        config: HandlerConfig,
        metrics: SharedMetrics,
        peers: SharedRegistry,
        routes: SharedRouteTable,
        route_updates: tokio::sync::broadcast::Sender<super::routes::RouteUpdate>,
        log_buffer: Option<LogBuffer>,
    ) -> Self {
        let state = SharedNodeState::new(config.node_role);
        let (command_source, command_sink) = mpsc::channel(8);
        Self {
            config,
            command_source,
            command_sink: std::sync::Mutex::new(Some(command_sink)),
            log_buffer: log_buffer.unwrap_or_default(),
            metrics,
            peers,
            routes,
            route_updates,
            state,
            start_time: Instant::now(),
        }
    }

    /// Returns a handle to the shared node state.
    ///
    /// Callers (daemon modes) use this to update role, capabilities, and
    /// listen/connect state after negotiation so that `info()` reports
    /// accurate information.
    #[must_use]
    pub fn node_state(&self) -> SharedNodeState {
        self.state.clone()
    }

    /// Extracts the command receiver. Called once by the daemon before
    /// wrapping Handler in `Arc<dyn NodeApi>`.
    ///
    /// # Panics
    ///
    /// Panics if the mutex is poisoned or if called more than once.
    #[must_use]
    pub fn command_sink(&self) -> mpsc::Receiver<NodeCommand> {
        self.command_sink
            .lock()
            .expect("command_sink mutex poisoned")
            .take()
            .expect("command_sink already taken")
    }

    /// Handles a control request and returns a response.
    ///
    /// # Errors
    ///
    /// Returns an error response if the request is malformed or cannot be
    /// processed.
    #[must_use]
    pub fn handle(&self, request: ControlRequest) -> ControlResponse {
        match request.request {
            Some(control_request::Request::Ping(_)) => self.handle_ping(),
            Some(control_request::Request::Stats(_)) => self.handle_stats(),
            Some(control_request::Request::Peers(_)) => self.handle_peers(),
            Some(control_request::Request::Disconnect(req)) => self.handle_disconnect(&req),
            Some(control_request::Request::RouteAdd(req)) => self.handle_route_add(req),
            Some(control_request::Request::RouteDel(req)) => self.handle_route_del(&req),
            Some(control_request::Request::RouteList(_)) => self.handle_route_list(),
            None => Self::error_response("Empty request"),
        }
    }

    fn handle_ping(&self) -> ControlResponse {
        let uptime = self.start_time.elapsed();
        let state = self.state.load();
        ControlResponse {
            response: Some(control_response::Response::Ping(PingResponse {
                uptime_ms: u64::try_from(uptime.as_millis()).unwrap_or(u64::MAX),
                version: self.config.version.clone(),
                node_role: ProtoNodeRole::from(state.role).into(),
            })),
        }
    }

    fn handle_stats(&self) -> ControlResponse {
        let m = self.metrics.snapshot();
        ControlResponse {
            response: Some(control_response::Response::Stats(StatsResponse {
                bytes_in: m.bytes_in,
                bytes_out: m.bytes_out,
                packets_in: m.packets_in,
                packets_out: m.packets_out,
                active_connections: m.active_connections,
                active_flows: m.active_flows,
                packets_dropped: m.packets_dropped,
            })),
        }
    }

    fn handle_peers(&self) -> ControlResponse {
        let peers = self
            .peers
            .list()
            .into_iter()
            .map(|p| {
                let connect_time = p.connect_time.elapsed();
                PeerInfo {
                    name: p.name,
                    addr: p.addr,
                    role: ProtoNodeRole::from(p.role).into(),
                    connect_time: connect_time.as_secs(),
                    bytes_transferred: p.bytes_transferred,
                    latency_ms: p.latency_ms.unwrap_or(0.0),
                }
            })
            .collect();

        ControlResponse {
            response: Some(control_response::Response::Peers(
                wallhack_wire::control::PeersResponse { peers },
            )),
        }
    }

    fn handle_disconnect(
        &self,
        req: &wallhack_wire::control::DisconnectRequest,
    ) -> ControlResponse {
        let success = self.peers.unregister(&req.peer).is_some();
        ControlResponse {
            response: Some(control_response::Response::Disconnect(
                wallhack_wire::control::DisconnectResponse { success },
            )),
        }
    }

    fn handle_route_add(&self, req: wallhack_wire::control::RouteAddRequest) -> ControlResponse {
        let cidr = match req.cidr.parse() {
            Ok(c) => c,
            Err(e) => {
                return ControlResponse {
                    response: Some(control_response::Response::RouteAdd(
                        wallhack_wire::control::RouteAddResponse {
                            success: false,
                            message: format!("invalid CIDR: {e}"),
                        },
                    )),
                };
            }
        };

        if req.peer.is_empty() {
            return ControlResponse {
                response: Some(control_response::Response::RouteAdd(
                    wallhack_wire::control::RouteAddResponse {
                        success: false,
                        message: "peer is required".to_string(),
                    },
                )),
            };
        }

        let (_, new_entry) = self.routes.add(cidr, req.peer);
        let _ = self
            .route_updates
            .send(super::routes::RouteUpdate::Add(new_entry));

        ControlResponse {
            response: Some(control_response::Response::RouteAdd(
                wallhack_wire::control::RouteAddResponse {
                    success: true,
                    message: String::new(),
                },
            )),
        }
    }

    fn handle_route_del(&self, req: &wallhack_wire::control::RouteDelRequest) -> ControlResponse {
        let cidr = match req.cidr.parse() {
            Ok(c) => c,
            Err(e) => {
                return ControlResponse {
                    response: Some(control_response::Response::RouteDel(
                        wallhack_wire::control::RouteDelResponse {
                            success: false,
                            message: format!("invalid CIDR: {e}"),
                        },
                    )),
                };
            }
        };

        let removed = self.routes.remove(&cidr);
        let success = removed.is_some();
        if let Some(entry) = removed {
            let _ = self
                .route_updates
                .send(super::routes::RouteUpdate::Remove(entry));
        }
        ControlResponse {
            response: Some(control_response::Response::RouteDel(
                wallhack_wire::control::RouteDelResponse {
                    success,
                    message: if success {
                        String::new()
                    } else {
                        "route not found".to_string()
                    },
                },
            )),
        }
    }

    fn handle_route_list(&self) -> ControlResponse {
        let routes = self
            .routes
            .list()
            .into_iter()
            .map(|entry| RouteInfo {
                cidr: entry.cidr.to_string(),
                peer: entry.peer,
                create_time: entry.create_time.elapsed().as_secs(),
            })
            .collect();

        ControlResponse {
            response: Some(control_response::Response::RouteList(
                wallhack_wire::control::RouteListResponse { routes },
            )),
        }
    }

    fn error_response(message: &str) -> ControlResponse {
        ControlResponse {
            response: Some(control_response::Response::Error(ErrorResponse {
                message: message.to_string(),
            })),
        }
    }
}

impl Handler {
    fn do_disconnect(&self, id: &str) {
        self.peers.send_disconnect(id, "disconnected by API");
        let _ = self.peers.unregister(id);
        tracing::info!("Peer disconnected: {id} (via API)");
    }
}

impl crate::node_api::NodeApi for Handler {
    fn peers(&self) -> Vec<crate::node_api::PeerInfo> {
        self.peers
            .list()
            .into_iter()
            .map(|p| crate::node_api::PeerInfo {
                id: p.id,
                name: p.name,
                addr: p.addr,
                role: p.role,
                capabilities: p.capabilities,
                side: p.side,
                status: crate::node_api::PeerStatus::Connected,
                connect_time: p.connect_time_epoch,
                bytes_transferred: p.bytes_transferred,
                latency_ms: p.latency_ms,
                tun_name: p.tun_name,
            })
            .collect()
    }

    fn routes(&self) -> crate::node_api::Result<Vec<crate::node_api::RouteEntry>> {
        Ok(self
            .routes
            .list()
            .into_iter()
            .map(|r| crate::node_api::RouteEntry {
                cidr: r.cidr,
                peer: r.peer,
                create_time: r.create_time,
                auto_managed: r.auto_managed,
            })
            .collect())
    }

    fn metrics(&self) -> crate::node_api::Metrics {
        self.metrics.snapshot()
    }

    fn info(&self) -> crate::node_api::NodeInfo {
        let state = self.state.load();
        crate::node_api::NodeInfo {
            role: state.role,
            peer_addr: state.peer_addr.clone(),
            capabilities: state.capabilities,
            listen_addr: state.listen_addr,
            name: self.config.name.clone(),
            version: self.config.version.clone(),
            uptime_ms: u64::try_from(self.start_time.elapsed().as_millis()).unwrap_or(u64::MAX),
        }
    }

    fn connect(&self, addr: &str) -> crate::node_api::Result<crate::node_api::ConnectInfo> {
        let (reply_sender, reply_receiver) = reply_channel();
        self.command_source
            .try_send(NodeCommand::Connect {
                addr: addr.to_string(),
                reply: reply_sender,
            })
            .map_err(|_| {
                crate::node_api::NodeApiError::NotSupported(
                    "dynamic connect not supported in this mode".into(),
                )
            })?;
        tokio::task::block_in_place(|| reply_receiver.recv()).map_err(|_| {
            crate::node_api::NodeApiError::Internal("mode task dropped reply".into())
        })?
    }

    fn listen(
        &self,
        addr: std::net::SocketAddr,
    ) -> crate::node_api::Result<crate::node_api::ListenInfo> {
        let (reply_sender, reply_receiver) = reply_channel();
        self.command_source
            .try_send(NodeCommand::Listen {
                addr,
                reply: reply_sender,
            })
            .map_err(|_| {
                crate::node_api::NodeApiError::NotSupported(
                    "dynamic listen not supported in this mode".into(),
                )
            })?;
        tokio::task::block_in_place(|| reply_receiver.recv()).map_err(|_| {
            crate::node_api::NodeApiError::Internal("mode task dropped reply".into())
        })?
    }

    fn disconnect(&self) -> crate::node_api::Result<()> {
        let (reply_sender, reply_receiver) = reply_channel();
        self.command_source
            .try_send(NodeCommand::Disconnect {
                reply: reply_sender,
            })
            .map_err(|_| {
                crate::node_api::NodeApiError::NotSupported(
                    "dynamic disconnect not supported in this mode".into(),
                )
            })?;
        tokio::task::block_in_place(|| reply_receiver.recv()).map_err(|_| {
            crate::node_api::NodeApiError::Internal("mode task dropped reply".into())
        })?
    }

    fn add_route(
        &self,
        cidr: crate::Cidr,
        peer: String,
    ) -> crate::node_api::Result<Option<String>> {
        // Resolve peer name by prefix (will error if not found or ambiguous)
        let peer_info = self.peers.find_by_prefix(&peer)?;

        let (_, new_entry) = self.routes.add(cidr, peer_info.name.clone());
        let _ = self
            .route_updates
            .send(super::routes::RouteUpdate::Add(new_entry));

        // Warn if the peer advertises routes but none of them cover the new CIDR.
        // If the peer has no auto-routes at all, it may not advertise routes at all
        // (e.g. an explicit-mode peer), so silence the warning in that case.
        let auto_routes = self.routes.auto_routes_for_peer(&peer_info.name);
        if !auto_routes.is_empty() && !auto_routes.iter().any(|r| r.cidr.contains(&cidr)) {
            let warning = format!(
                "peer {} does not advertise a route covering {cidr}; traffic may not reach the destination",
                peer_info.name,
            );
            return Ok(Some(warning));
        }

        Ok(None)
    }

    fn route_del(&self, cidr: &crate::Cidr) -> crate::node_api::Result<()> {
        if let Some(entry) = self.routes.remove(cidr) {
            let _ = self
                .route_updates
                .send(super::routes::RouteUpdate::Remove(entry));
            Ok(())
        } else {
            Err(crate::node_api::NodeApiError::RouteNotFound(*cidr))
        }
    }

    fn peer_disconnect(&self, peer: String) -> crate::node_api::Result<()> {
        // Try name prefix first, then fall back to exact address match.
        // Used by REPL/CLI where prefix matching is convenient.
        let peer_info = self.peers.find_by_prefix(&peer).or_else(|e| {
            if matches!(e, crate::node_api::NodeApiError::PeerNotFound(_)) {
                self.peers.find_by_addr(&peer)
            } else {
                Err(e)
            }
        })?;

        self.do_disconnect(&peer_info.id);
        Ok(())
    }

    fn peer_disconnect_by_id(&self, id: String) -> crate::node_api::Result<()> {
        // Exact match on registry key. Used by REST API where the id
        // is taken directly from the peers list.
        if self.peers.get(&id).is_none() {
            return Err(crate::node_api::NodeApiError::PeerNotFound(id));
        }
        self.do_disconnect(&id);
        Ok(())
    }

    fn current_role(&self) -> NodeRole {
        self.state.load().role
    }

    fn hint_set(&self, hint: RoleHint) -> crate::node_api::Result<()> {
        let _ = self
            .command_source
            .try_send(NodeCommand::Role { hint: Some(hint) });
        Ok(())
    }

    fn hint_set_auto(&self) -> crate::node_api::Result<()> {
        let _ = self
            .command_source
            .try_send(NodeCommand::Role { hint: None });
        Ok(())
    }

    fn logs(&self, count: u32) -> Vec<String> {
        self.log_buffer.tail(count)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::control::{metrics::Metrics, peers::Registry, routes::RouteTable};

    fn test_handler() -> Handler {
        let metrics = Arc::new(Metrics::default());
        let peers = Arc::new(Registry::new());
        let routes = RouteTable::shared();
        Handler::new(
            HandlerConfig::new(
                NodeRole::Entry,
                "wallhackd".to_string(),
                "0.0.0".to_string(),
            ),
            metrics,
            peers,
            routes,
            tokio::sync::broadcast::channel(16).0,
            None,
        )
    }

    #[test]
    fn test_ping() {
        let handler = test_handler();
        let request = ControlRequest {
            request: Some(control_request::Request::Ping(
                wallhack_wire::control::PingRequest {},
            )),
        };
        let response = handler.handle(request);

        match response.response {
            Some(control_response::Response::Ping(ping)) => {
                assert!(!ping.version.is_empty());
                assert_eq!(ping.node_role, i32::from(ProtoNodeRole::RoleEntry));
            }
            _ => panic!("Expected ping response"),
        }
    }

    #[test]
    fn test_stats() {
        let metrics = Arc::new(Metrics::default());
        let peers = Arc::new(Registry::new());
        let routes = RouteTable::shared();
        metrics.inc_bytes_in(100);
        metrics.inc_packets_out(5);

        let handler = Handler::new(
            HandlerConfig::new(
                NodeRole::Entry,
                "wallhackd".to_string(),
                "0.0.0".to_string(),
            ),
            metrics,
            peers,
            routes,
            tokio::sync::broadcast::channel(16).0,
            None,
        );
        let request = ControlRequest {
            request: Some(control_request::Request::Stats(
                wallhack_wire::control::StatsRequest {},
            )),
        };
        let response = handler.handle(request);

        match response.response {
            Some(control_response::Response::Stats(stats)) => {
                assert_eq!(stats.bytes_in, 100);
                assert_eq!(stats.packets_out, 5);
            }
            _ => panic!("Expected stats response"),
        }
    }

    #[test]
    fn test_empty_request() {
        let handler = test_handler();
        let request = ControlRequest { request: None };
        let response = handler.handle(request);

        match response.response {
            Some(control_response::Response::Error(err)) => {
                assert_eq!(err.message, "Empty request");
            }
            _ => panic!("Expected error response"),
        }
    }

    #[test]
    fn test_route_add_success() {
        let handler = test_handler();
        let request = ControlRequest {
            request: Some(control_request::Request::RouteAdd(
                wallhack_wire::control::RouteAddRequest {
                    cidr: "10.0.0.0/8".to_string(),
                    peer: "exit-1".to_string(),
                },
            )),
        };
        let response = handler.handle(request);

        match response.response {
            Some(control_response::Response::RouteAdd(r)) => {
                assert!(r.success);
                assert!(r.message.is_empty());
            }
            _ => panic!("Expected route add response"),
        }
    }

    #[test]
    fn test_route_add_invalid_cidr() {
        let handler = test_handler();
        let request = ControlRequest {
            request: Some(control_request::Request::RouteAdd(
                wallhack_wire::control::RouteAddRequest {
                    cidr: "not-a-cidr".to_string(),
                    peer: "exit-1".to_string(),
                },
            )),
        };
        let response = handler.handle(request);

        match response.response {
            Some(control_response::Response::RouteAdd(r)) => {
                assert!(!r.success);
                assert!(!r.message.is_empty());
            }
            _ => panic!("Expected route add response"),
        }
    }

    #[test]
    fn test_route_add_missing_peer() {
        let handler = test_handler();
        let request = ControlRequest {
            request: Some(control_request::Request::RouteAdd(
                wallhack_wire::control::RouteAddRequest {
                    cidr: "10.0.0.0/8".to_string(),
                    peer: String::new(),
                },
            )),
        };
        let response = handler.handle(request);

        match response.response {
            Some(control_response::Response::RouteAdd(r)) => {
                assert!(!r.success);
                assert_eq!(r.message, "peer is required");
            }
            _ => panic!("Expected route add response"),
        }
    }

    #[test]
    fn test_route_del() {
        let handler = test_handler();

        // Add first
        let _ = handler.handle(ControlRequest {
            request: Some(control_request::Request::RouteAdd(
                wallhack_wire::control::RouteAddRequest {
                    cidr: "10.0.0.0/8".to_string(),
                    peer: "exit-1".to_string(),
                },
            )),
        });

        // Remove
        let response = handler.handle(ControlRequest {
            request: Some(control_request::Request::RouteDel(
                wallhack_wire::control::RouteDelRequest {
                    cidr: "10.0.0.0/8".to_string(),
                },
            )),
        });

        match response.response {
            Some(control_response::Response::RouteDel(r)) => {
                assert!(r.success);
            }
            _ => panic!("Expected route remove response"),
        }
    }

    #[test]
    fn test_route_del_not_found() {
        let handler = test_handler();
        let response = handler.handle(ControlRequest {
            request: Some(control_request::Request::RouteDel(
                wallhack_wire::control::RouteDelRequest {
                    cidr: "10.0.0.0/8".to_string(),
                },
            )),
        });

        match response.response {
            Some(control_response::Response::RouteDel(r)) => {
                assert!(!r.success);
                assert_eq!(r.message, "route not found");
            }
            _ => panic!("Expected route remove response"),
        }
    }

    #[test]
    fn test_ping_indeterminate_role() {
        let metrics = Arc::new(Metrics::default());
        let peers = Arc::new(Registry::new());
        let routes = RouteTable::shared();
        let handler = Handler::new(
            HandlerConfig::new(
                NodeRole::Indeterminate,
                "wallhackd".to_string(),
                "0.0.0".to_string(),
            ),
            metrics,
            peers,
            routes,
            tokio::sync::broadcast::channel(16).0,
            None,
        );

        let request = ControlRequest {
            request: Some(control_request::Request::Ping(
                wallhack_wire::control::PingRequest {},
            )),
        };
        let response = handler.handle(request);

        match response.response {
            Some(control_response::Response::Ping(ping)) => {
                assert_eq!(
                    ping.node_role,
                    i32::from(ProtoNodeRole::RoleIndeterminate),
                    "indeterminate handler should report RoleIndeterminate"
                );
            }
            _ => panic!("Expected ping response"),
        }
    }

    #[test]
    fn test_info_indeterminate_role() {
        let metrics = Arc::new(Metrics::default());
        let peers = Arc::new(Registry::new());
        let routes = RouteTable::shared();
        let handler = Handler::new(
            HandlerConfig::new(
                NodeRole::Indeterminate,
                "wallhackd".to_string(),
                "0.0.0".to_string(),
            ),
            metrics,
            peers,
            routes,
            tokio::sync::broadcast::channel(16).0,
            None,
        );

        let status = crate::node_api::NodeApi::info(&handler);
        assert_eq!(status.role, NodeRole::Indeterminate);
    }

    #[test]
    fn test_route_list() {
        let handler = test_handler();

        // Add two routes
        let _ = handler.handle(ControlRequest {
            request: Some(control_request::Request::RouteAdd(
                wallhack_wire::control::RouteAddRequest {
                    cidr: "10.0.0.0/8".to_string(),
                    peer: "exit-1".to_string(),
                },
            )),
        });
        let _ = handler.handle(ControlRequest {
            request: Some(control_request::Request::RouteAdd(
                wallhack_wire::control::RouteAddRequest {
                    cidr: "172.16.0.0/12".to_string(),
                    peer: "exit-2".to_string(),
                },
            )),
        });

        let response = handler.handle(ControlRequest {
            request: Some(control_request::Request::RouteList(
                wallhack_wire::control::RouteListRequest {},
            )),
        });

        match response.response {
            Some(control_response::Response::RouteList(r)) => {
                assert_eq!(r.routes.len(), 2);
            }
            _ => panic!("Expected route list response"),
        }
    }

    #[test]
    fn test_info_reflects_node_state_updates() {
        let handler = Handler::new(
            HandlerConfig::new(
                NodeRole::Indeterminate,
                "wallhackd".to_string(),
                "0.0.0".to_string(),
            ),
            Arc::new(Metrics::default()),
            Arc::new(Registry::new()),
            RouteTable::shared(),
            tokio::sync::broadcast::channel(16).0,
            None,
        );

        // Initially indeterminate with no capabilities.
        let status = crate::node_api::NodeApi::info(&handler);
        assert_eq!(status.role, NodeRole::Indeterminate);
        assert!(!status.capabilities.tun_capable);
        assert!(!status.capabilities.listening);
        assert!(status.listen_addr.is_none());

        // Simulate negotiation resolving to Entry.
        let state = handler.node_state();
        state.update_role(NodeRole::Entry);
        state.update_capabilities(Capabilities {
            tun_capable: true,
            listening: false,
            connecting: false,
            interactive: false,
        });

        let status = crate::node_api::NodeApi::info(&handler);
        assert_eq!(status.role, NodeRole::Entry);
        assert!(status.capabilities.tun_capable);

        // Simulate listen address being set.
        let addr: SocketAddr = "0.0.0.0:4433".parse().unwrap();
        state.set_listen_addr(addr);

        let status = crate::node_api::NodeApi::info(&handler);
        assert_eq!(status.listen_addr, Some(addr));
        assert!(status.capabilities.listening);
    }
}
