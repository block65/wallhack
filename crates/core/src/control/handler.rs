//! Control channel request handler.
//!
//! Handles incoming [`ControlRequest`] messages and produces
//! [`ControlResponse`] messages.

use std::{net::SocketAddr, sync::Arc, time::Instant};

use arc_swap::ArcSwap;
use tokio::sync::watch;
use wallhack_wire::{
    control::{
        ControlRequest, ControlResponse, ErrorResponse, PeerInfo, PingResponse, RouteInfo,
        StatsResponse, control_request, control_response,
    },
    data::{Capabilities, NodeRole as ProtoNodeRole, RoleHint},
};

use crate::NodeRole;

use super::{metrics::SharedMetrics, peers::SharedRegistry, routes::SharedRouteTable};

/// Mutable runtime state that can change after construction.
///
/// Stored behind `ArcSwap` for wait-free reads (same pattern as `Registry`).
#[derive(Debug, Clone)]
struct NodeState {
    role: NodeRole,
    capabilities: Capabilities,
    listen_addr: Option<SocketAddr>,
    connected: bool,
    peer_addr: Option<String>,
}

/// Shared node state handle, cloneable and cheaply updatable.
///
/// Consumers call [`SharedNodeState::update_role`], [`SharedNodeState::update_capabilities`],
/// etc. after negotiation or listening starts so that `wallhack info` /
/// `wallhack_status` reflects the real state of the daemon.
#[derive(Clone, Debug)]
pub struct SharedNodeState(Arc<ArcSwap<NodeState>>);

impl SharedNodeState {
    fn new(role: NodeRole) -> Self {
        Self(Arc::new(ArcSwap::from_pointee(NodeState {
            role,
            capabilities: Capabilities::default(),
            listen_addr: None,
            connected: false,
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

    /// Record that the node has connected to a peer.
    pub fn set_connected(&self, peer_addr: &str) {
        let addr = peer_addr.to_string();
        self.0.rcu(|old| {
            let mut new = (**old).clone();
            new.connected = true;
            new.peer_addr = Some(addr.clone());
            new.capabilities.connecting = true;
            new
        });
    }

    /// Record that the node has disconnected from its peer.
    pub fn set_disconnected(&self) {
        self.0.rcu(|old| {
            let mut new = (**old).clone();
            new.connected = false;
            new.peer_addr = None;
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
    /// Sender for hint changes. The mode task watches the receiver and
    /// re-evaluates when a new hint arrives. `None` means no hint is active.
    hint_tx: watch::Sender<Option<RoleHint>>,
    metrics: SharedMetrics,
    peers: SharedRegistry,
    routes: SharedRouteTable,
    route_updates: tokio::sync::broadcast::Sender<super::routes::RouteUpdate>,
    state: SharedNodeState,
    start_time: Instant,
}

impl Handler {
    /// Creates a new control handler.
    #[must_use]
    pub fn new(
        config: HandlerConfig,
        metrics: SharedMetrics,
        peers: SharedRegistry,
        routes: SharedRouteTable,
        route_updates: tokio::sync::broadcast::Sender<super::routes::RouteUpdate>,
    ) -> Self {
        let state = SharedNodeState::new(config.node_role);
        let (hint_tx, _) = watch::channel(None);
        Self {
            config,
            hint_tx,
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
    /// listen/connect state after negotiation so that `status()` reports
    /// accurate information.
    #[must_use]
    pub fn node_state(&self) -> SharedNodeState {
        self.state.clone()
    }

    /// Returns a receiver that fires when the runtime hint changes.
    #[must_use]
    pub fn hint_rx(&self) -> watch::Receiver<Option<RoleHint>> {
        self.hint_tx.subscribe()
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
            Some(control_request::Request::RouteRemove(req)) => self.handle_route_remove(&req),
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

    fn handle_route_remove(
        &self,
        req: &wallhack_wire::control::RouteRemoveRequest,
    ) -> ControlResponse {
        let cidr = match req.cidr.parse() {
            Ok(c) => c,
            Err(e) => {
                return ControlResponse {
                    response: Some(control_response::Response::RouteRemove(
                        wallhack_wire::control::RouteRemoveResponse {
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
            response: Some(control_response::Response::RouteRemove(
                wallhack_wire::control::RouteRemoveResponse {
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

impl crate::node_api::NodeApi for Handler {
    fn peers(&self) -> Vec<crate::node_api::PeerInfo> {
        self.peers
            .list()
            .into_iter()
            .map(|p| crate::node_api::PeerInfo {
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

    fn status(&self) -> crate::node_api::NodeStatus {
        let state = self.state.load();
        crate::node_api::NodeStatus {
            role: state.role,
            connected: state.connected,
            peer_addr: state.peer_addr.clone(),
            capabilities: state.capabilities,
            listen_addr: state.listen_addr,
            name: self.config.name.clone(),
            version: self.config.version.clone(),
            uptime_ms: u64::try_from(self.start_time.elapsed().as_millis()).unwrap_or(u64::MAX),
        }
    }

    fn connect(&self, _addr: &str) -> crate::node_api::Result<crate::node_api::ConnectInfo> {
        Err(crate::node_api::NodeApiError::NotSupported(
            "dynamic connect not yet implemented — specify --connect at startup".into(),
        ))
    }

    fn listen(
        &self,
        _addr: std::net::SocketAddr,
    ) -> crate::node_api::Result<crate::node_api::ListenInfo> {
        Err(crate::node_api::NodeApiError::NotSupported(
            "dynamic listen not yet implemented — specify --listen at startup".into(),
        ))
    }

    fn disconnect(&self) -> crate::node_api::Result<()> {
        Err(crate::node_api::NodeApiError::NotSupported(
            "dynamic disconnect not yet implemented".into(),
        ))
    }

    fn add_route(&self, cidr: crate::Cidr, peer: String) -> crate::node_api::Result<()> {
        // Resolve peer name by prefix (will error if not found or ambiguous)
        let peer_info = self.peers.find_by_prefix(&peer)?;

        let (_, new_entry) = self.routes.add(cidr, peer_info.name);
        let _ = self
            .route_updates
            .send(super::routes::RouteUpdate::Add(new_entry));
        Ok(())
    }

    fn remove_route(&self, cidr: &crate::Cidr) -> crate::node_api::Result<()> {
        if let Some(entry) = self.routes.remove(cidr) {
            let _ = self
                .route_updates
                .send(super::routes::RouteUpdate::Remove(entry));
            Ok(())
        } else {
            Err(crate::node_api::NodeApiError::RouteNotFound(*cidr))
        }
    }

    fn disconnect_peer(&self, peer: String) -> crate::node_api::Result<()> {
        let all_peers: Vec<_> = self.peers.list().iter().map(|p| p.name.clone()).collect();
        tracing::debug!(
            requested = %peer,
            registered = ?all_peers,
            count = all_peers.len(),
            "disconnect_peer: lookup"
        );

        // Try name prefix first, then fall back to exact address match.
        let peer_info = self.peers.find_by_prefix(&peer).or_else(|e| {
            tracing::debug!(requested = %peer, error = %e, "disconnect_peer: find_by_prefix failed, trying find_by_addr");
            if matches!(e, crate::node_api::NodeApiError::PeerNotFound(_)) {
                self.peers.find_by_addr(&peer)
            } else {
                Err(e)
            }
        })?;

        tracing::debug!(
            found = %peer_info.name,
            "disconnect_peer: found peer, unregistering"
        );

        // Peer was found; unregister may return None if concurrently removed
        // by the session task — that still counts as a successful disconnect.
        let _ = self.peers.unregister(&peer_info.name);
        Ok(())
    }

    fn current_role(&self) -> NodeRole {
        self.state.load().role
    }

    fn set_hint(&self, hint: RoleHint) -> crate::node_api::Result<()> {
        self.hint_tx.send_replace(Some(hint));
        Ok(())
    }

    fn clear_hints(&self) -> crate::node_api::Result<()> {
        self.hint_tx.send_replace(None);
        Ok(())
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
    fn test_route_remove() {
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
            request: Some(control_request::Request::RouteRemove(
                wallhack_wire::control::RouteRemoveRequest {
                    cidr: "10.0.0.0/8".to_string(),
                },
            )),
        });

        match response.response {
            Some(control_response::Response::RouteRemove(r)) => {
                assert!(r.success);
            }
            _ => panic!("Expected route remove response"),
        }
    }

    #[test]
    fn test_route_remove_not_found() {
        let handler = test_handler();
        let response = handler.handle(ControlRequest {
            request: Some(control_request::Request::RouteRemove(
                wallhack_wire::control::RouteRemoveRequest {
                    cidr: "10.0.0.0/8".to_string(),
                },
            )),
        });

        match response.response {
            Some(control_response::Response::RouteRemove(r)) => {
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
    fn test_status_indeterminate_role() {
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
        );

        let status = crate::node_api::NodeApi::status(&handler);
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
    fn test_status_reflects_node_state_updates() {
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
        );

        // Initially indeterminate with no capabilities.
        let status = crate::node_api::NodeApi::status(&handler);
        assert_eq!(status.role, NodeRole::Indeterminate);
        assert!(!status.capabilities.tun_capable);
        assert!(!status.capabilities.listening);
        assert!(status.listen_addr.is_none());
        assert!(!status.connected);

        // Simulate negotiation resolving to Entry.
        let state = handler.node_state();
        state.update_role(NodeRole::Entry);
        state.update_capabilities(Capabilities {
            tun_capable: true,
            listening: false,
            connecting: false,
        });

        let status = crate::node_api::NodeApi::status(&handler);
        assert_eq!(status.role, NodeRole::Entry);
        assert!(status.capabilities.tun_capable);

        // Simulate listen address being set.
        let addr: SocketAddr = "0.0.0.0:4433".parse().unwrap();
        state.set_listen_addr(addr);

        let status = crate::node_api::NodeApi::status(&handler);
        assert_eq!(status.listen_addr, Some(addr));
        assert!(status.capabilities.listening);

        // Simulate connection.
        state.set_connected("1.2.3.4:5678");

        let status = crate::node_api::NodeApi::status(&handler);
        assert!(status.connected);
        assert_eq!(status.peer_addr.as_deref(), Some("1.2.3.4:5678"));

        // Simulate disconnection.
        state.set_disconnected();

        let status = crate::node_api::NodeApi::status(&handler);
        assert!(!status.connected);
        assert!(status.peer_addr.is_none());
    }
}
