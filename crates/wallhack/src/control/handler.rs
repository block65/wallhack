//! Control channel request handler.
//!
//! Handles incoming [`ControlRequest`] messages and produces [`ControlResponse`] messages.

use std::{sync::atomic::Ordering, time::Instant};

use protobuf::control::{
	ControlRequest, ControlResponse, ErrorResponse, PingResponse, StatsResponse, control_request,
	control_response,
};

use super::metrics::SharedMetrics;

/// Configuration for the control handler.
#[derive(Debug, Clone)]
pub struct HandlerConfig {
	/// The role of this node (e.g., "host", "agent", "relay").
	pub node_role: String,
	/// Application version string.
	pub version: String,
}

impl Default for HandlerConfig {
	fn default() -> Self {
		Self {
			node_role: "unknown".to_string(),
			version: env!("CARGO_PKG_VERSION").to_string(),
		}
	}
}

/// Handler for control channel requests.
///
/// Processes incoming control requests and returns appropriate responses
/// based on the current state of metrics and configuration.
pub struct Handler {
	config: HandlerConfig,
	metrics: SharedMetrics,
	start_time: Instant,
}

impl Handler {
	/// Creates a new control handler.
	#[must_use]
	pub fn new(config: HandlerConfig, metrics: SharedMetrics) -> Self {
		Self {
			config,
			metrics,
			start_time: Instant::now(),
		}
	}

	/// Handles a control request and returns a response.
	///
	/// # Errors
	///
	/// Returns an error response if the request is malformed or cannot be processed.
	#[must_use]
	pub fn handle(&self, request: ControlRequest) -> ControlResponse {
		match request.request {
			Some(control_request::Request::Ping(_)) => self.handle_ping(),
			Some(control_request::Request::Stats(_)) => self.handle_stats(),
			Some(control_request::Request::Peers(_)) => Self::handle_peers(),
			Some(control_request::Request::Disconnect(req)) => Self::handle_disconnect(req),
			Some(control_request::Request::RouteAdd(req)) => Self::handle_route_add(req),
			Some(control_request::Request::RouteRemove(req)) => Self::handle_route_remove(req),
			None => Self::error_response("Empty request"),
		}
	}

	fn handle_ping(&self) -> ControlResponse {
		let uptime = self.start_time.elapsed();
		ControlResponse {
			response: Some(control_response::Response::Ping(PingResponse {
				uptime_ms: u64::try_from(uptime.as_millis()).unwrap_or(u64::MAX),
				version: self.config.version.clone(),
				node_role: self.config.node_role.clone(),
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
			})),
		}
	}

	fn handle_peers() -> ControlResponse {
		// TODO: Implement peer tracking
		ControlResponse {
			response: Some(control_response::Response::Peers(
				protobuf::control::PeersResponse { peers: vec![] },
			)),
		}
	}

	fn handle_disconnect(_req: protobuf::control::DisconnectRequest) -> ControlResponse {
		// TODO: Implement peer disconnection
		ControlResponse {
			response: Some(control_response::Response::Disconnect(
				protobuf::control::DisconnectResponse { success: false },
			)),
		}
	}

	fn handle_route_add(_req: protobuf::control::RouteAddRequest) -> ControlResponse {
		// TODO: Implement route addition
		ControlResponse {
			response: Some(control_response::Response::RouteAdd(
				protobuf::control::RouteAddResponse { success: false },
			)),
		}
	}

	fn handle_route_remove(_req: protobuf::control::RouteRemoveRequest) -> ControlResponse {
		// TODO: Implement route removal
		ControlResponse {
			response: Some(control_response::Response::RouteRemove(
				protobuf::control::RouteRemoveResponse { success: false },
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

#[cfg(test)]
mod tests {
	use std::sync::Arc;

	use super::*;
	use crate::control::metrics::Metrics;

	fn test_handler() -> Handler {
		let metrics = Arc::new(Metrics::default());
		Handler::new(HandlerConfig::default(), metrics)
	}

	#[test]
	fn test_ping() {
		let handler = test_handler();
		let request = ControlRequest {
			request: Some(control_request::Request::Ping(
				protobuf::control::PingRequest {},
			)),
		};
		let response = handler.handle(request);

		match response.response {
			Some(control_response::Response::Ping(ping)) => {
				assert!(!ping.version.is_empty());
				assert_eq!(ping.node_role, "unknown");
			}
			_ => panic!("Expected ping response"),
		}
	}

	#[test]
	fn test_stats() {
		let metrics = Arc::new(Metrics::default());
		metrics.inc_bytes_in(100);
		metrics.inc_packets_out(5);

		let handler = Handler::new(HandlerConfig::default(), metrics);
		let request = ControlRequest {
			request: Some(control_request::Request::Stats(
				protobuf::control::StatsRequest {},
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
}
