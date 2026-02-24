//! Control channel request handler.
//!
//! Handles incoming [`ControlRequest`] messages and produces
//! [`ControlResponse`] messages.

use std::{sync::atomic::Ordering, time::Instant};

use wallhack_wire::control::{
    ControlRequest, ControlResponse, ErrorResponse, NodeRole as ProtoNodeRole, PeerInfo,
    PingResponse, RouteInfo, StatsResponse, control_request, control_response,
};

use crate::NodeRole;

use super::{metrics::SharedMetrics, peers::SharedRegistry, routes::SharedRouteTable};

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

    #[cfg(test)]
    fn test_config(node_role: NodeRole) -> Self {
        Self {
            node_role,
            name: "test-daemon".to_string(),
            version: "0.0.0-test".to_string(),
        }
    }
}

/// Handler for control channel requests.
///
/// Processes incoming control requests and returns appropriate responses based
/// on the current state of metrics and configuration.
pub struct Handler {
    config: HandlerConfig,
    metrics: SharedMetrics,
    peers: SharedRegistry,
    routes: SharedRouteTable,
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
    ) -> Self {
        Self {
            config,
            metrics,
            peers,
            routes,
            start_time: Instant::now(),
        }
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
        ControlResponse {
            response: Some(control_response::Response::Ping(PingResponse {
                uptime_ms: u64::try_from(uptime.as_millis()).unwrap_or(u64::MAX),
                version: self.config.version.clone(),
                node_role: ProtoNodeRole::from(self.config.node_role).into(),
            })),
        }
    }

    fn handle_stats(&self) -> ControlResponse {
        ControlResponse {
            response: Some(control_response::Response::Stats(StatsResponse {
                bytes_in: self.metrics.bytes_in.load(Ordering::Relaxed),
                bytes_out: self.metrics.bytes_out.load(Ordering::Relaxed),
                packets_in: self.metrics.packets_in.load(Ordering::Relaxed),
                packets_out: self.metrics.packets_out.load(Ordering::Relaxed),
                active_connections: self.metrics.active_connections.load(Ordering::Relaxed),
                active_flows: self.metrics.active_flows.load(Ordering::Relaxed),
                packets_dropped: self.metrics.packets_dropped.load(Ordering::Relaxed),
            })),
        }
    }

    fn handle_peers(&self) -> ControlResponse {
        let peers = self
            .peers
            .list()
            .into_iter()
            .map(|p| {
                let connected_at = p.connected_at.elapsed();
                PeerInfo {
                    name: p.name,
                    addr: p.addr,
                    role: ProtoNodeRole::from(p.role).into(),
                    connected_at: connected_at.as_secs(),
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

        self.routes.add(cidr, req.peer);

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
                added_at_secs: entry.added_at.elapsed().as_secs(),
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
                capability: if p.has_relay_capability {
                    crate::node_api::NodeCapability::Relay
                } else {
                    crate::node_api::NodeCapability::Exit
                },
                status: crate::node_api::PeerStatus::Connected,
                connected_at_secs: p.connected_at.elapsed().as_secs(),
                bytes_transferred: p.bytes_transferred,
                latency_ms: p.latency_ms,
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
                added_at: r.added_at,
            })
            .collect())
    }

    fn metrics(&self) -> crate::node_api::Metrics {
        crate::node_api::Metrics {
            bytes_in: self.metrics.bytes_in.load(Ordering::Relaxed),
            bytes_out: self.metrics.bytes_out.load(Ordering::Relaxed),
            packets_in: self.metrics.packets_in.load(Ordering::Relaxed),
            packets_out: self.metrics.packets_out.load(Ordering::Relaxed),
            active_connections: self.metrics.active_connections.load(Ordering::Relaxed),
            active_flows: self.metrics.active_flows.load(Ordering::Relaxed),
            packets_dropped: self.metrics.packets_dropped.load(Ordering::Relaxed),
        }
    }

    fn status(&self) -> crate::node_api::NodeStatus {
        crate::node_api::NodeStatus {
            role: self.config.node_role,
            connected: false,
            peer_addr: None,
            has_relay_capability: false,
            listen_addr: None,
            name: self.config.name.clone(),
            version: self.config.version.clone(),
            uptime_ms: u64::try_from(self.start_time.elapsed().as_millis()).unwrap_or(u64::MAX),
        }
    }

    fn connect(&self, _addr: std::net::SocketAddr) -> crate::node_api::Result<()> {
        Err(crate::node_api::NodeApiError::NotSupported)
    }

    fn listen(&self, _addr: std::net::SocketAddr) -> crate::node_api::Result<()> {
        Err(crate::node_api::NodeApiError::NotSupported)
    }

    fn disconnect(&self) -> crate::node_api::Result<()> {
        Err(crate::node_api::NodeApiError::NotSupported)
    }

    fn add_route(&self, cidr: crate::Cidr, peer: String) -> crate::node_api::Result<()> {
        // Resolve peer name by prefix (will error if not found or ambiguous)
        let peer_info = self.peers.find_by_prefix(&peer)?;

        self.routes.add(cidr, peer_info.name);
        Ok(())
    }

    fn remove_route(&self, cidr: &crate::Cidr) -> crate::node_api::Result<()> {
        self.routes
            .remove(cidr)
            .map(|_| ())
            .ok_or(crate::node_api::NodeApiError::RouteNotFound(*cidr))
    }

    fn disconnect_peer(&self, peer: String) -> crate::node_api::Result<()> {
        // Resolve peer name by prefix (will error if not found or ambiguous)
        let peer_info = self.peers.find_by_prefix(&peer)?;

        self.peers
            .unregister(&peer_info.name)
            .map(|_| ())
            .ok_or(crate::node_api::NodeApiError::PeerNotFound(peer))
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
        Handler::new(HandlerConfig::new(NodeRole::Entry, "wallhackd".to_string(), "0.0.0".to_string()), metrics, peers, routes)
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

        let handler = Handler::new(HandlerConfig::new(NodeRole::Entry, "wallhackd".to_string(), "0.0.0".to_string()), metrics, peers, routes);
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
}
