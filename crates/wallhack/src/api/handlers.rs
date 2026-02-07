//! HTTP handlers for the REST API.

use std::{convert::Infallible, time::Duration};

use axum::{
	Json,
	extract::{Path, State},
	http::StatusCode,
	response::sse::{Event, KeepAlive, Sse},
};
use serde::{Deserialize, Serialize};
use tokio_stream::{Stream, StreamExt, wrappers::BroadcastStream};

use super::{state::State as ApiState, validation};

/// Metrics snapshot for JSON response.
#[derive(Debug, Serialize)]
pub struct StatsResponse {
	pub bytes_in: u64,
	pub bytes_out: u64,
	pub packets_in: u64,
	pub packets_out: u64,
	pub active_connections: u64,
	pub active_flows: u64,
}

/// Ping response.
#[derive(Debug, Serialize)]
pub struct PingResponse {
	pub uptime_ms: u64,
	pub version: String,
	pub node_role: String,
}

/// Peer info response.
#[derive(Debug, Serialize)]
pub struct PeerResponse {
	pub id: String,
	pub addr: String,
	pub role: String,
	pub connected_at: u64,
	pub bytes_transferred: u64,
	pub latency_ms: Option<f64>,
}

/// Peers list response.
#[derive(Debug, Serialize)]
pub struct PeersResponse {
	pub peers: Vec<PeerResponse>,
}

/// Route add request.
#[derive(Debug, Deserialize)]
pub struct RouteAddRequest {
	pub cidr: String,
	pub peer_id: String,
}

/// Route info for JSON response.
#[derive(Debug, Serialize)]
pub struct RouteResponse {
	pub cidr: String,
	pub peer_id: String,
	pub added_at_secs: u64,
}

/// Routes list response.
#[derive(Debug, Serialize)]
pub struct RoutesResponse {
	pub routes: Vec<RouteResponse>,
}

/// Generic success response.
#[derive(Debug, Serialize)]
pub struct SuccessResponse {
	pub success: bool,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub message: Option<String>,
}

pub async fn health() -> &'static str {
	"ok"
}

pub async fn ping(State(state): State<ApiState>) -> Json<PingResponse> {
	use protobuf::control::{ControlRequest, control_request, control_response};

	let request = ControlRequest {
		request: Some(control_request::Request::Ping(
			protobuf::control::PingRequest {},
		)),
	};

	let response = state.handler.handle(request);

	match response.response {
		Some(control_response::Response::Ping(ping)) => Json(PingResponse {
			uptime_ms: ping.uptime_ms,
			version: ping.version,
			node_role: format!("{:?}", ping.node_role),
		}),
		_ => Json(PingResponse {
			uptime_ms: 0,
			version: "unknown".to_string(),
			node_role: "unknown".to_string(),
		}),
	}
}

pub async fn stats(State(state): State<ApiState>) -> Json<StatsResponse> {
	use protobuf::control::{ControlRequest, control_request, control_response};

	let request = ControlRequest {
		request: Some(control_request::Request::Stats(
			protobuf::control::StatsRequest {},
		)),
	};

	let response = state.handler.handle(request);

	match response.response {
		Some(control_response::Response::Stats(s)) => Json(StatsResponse {
			bytes_in: s.bytes_in,
			bytes_out: s.bytes_out,
			packets_in: s.packets_in,
			packets_out: s.packets_out,
			active_connections: s.active_connections,
			active_flows: s.active_flows,
		}),
		_ => Json(StatsResponse {
			bytes_in: 0,
			bytes_out: 0,
			packets_in: 0,
			packets_out: 0,
			active_connections: 0,
			active_flows: 0,
		}),
	}
}

pub async fn peers(State(state): State<ApiState>) -> Json<PeersResponse> {
	use protobuf::control::{ControlRequest, NodeRole, control_request, control_response};

	let request = ControlRequest {
		request: Some(control_request::Request::Peers(
			protobuf::control::PeersRequest {},
		)),
	};

	let response = state.handler.handle(request);

	match response.response {
		Some(control_response::Response::Peers(p)) => Json(PeersResponse {
			peers: p
				.peers
				.into_iter()
				.map(|peer| {
					let role_name = NodeRole::try_from(peer.role)
						.map_or_else(|_| "unknown".to_string(), |r| r.as_str_name().to_string());
					PeerResponse {
						id: peer.id,
						addr: peer.addr,
						role: role_name,
						connected_at: peer.connected_at,
						bytes_transferred: peer.bytes_transferred,
						latency_ms: if peer.latency_ms > 0.0 {
							Some(peer.latency_ms)
						} else {
							None
						},
					}
				})
				.collect(),
		}),
		_ => Json(PeersResponse { peers: vec![] }),
	}
}

pub async fn disconnect_peer(
	State(state): State<ApiState>,
	Path(peer_id): Path<String>,
) -> (StatusCode, Json<SuccessResponse>) {
	use protobuf::control::{ControlRequest, control_request, control_response};

	if let Err(e) = validation::validate_peer_id(&peer_id) {
		tracing::warn!("Invalid peer_id in disconnect request: {e}");
		return (
			StatusCode::BAD_REQUEST,
			Json(SuccessResponse {
				success: false,
				message: None,
			}),
		);
	}

	let request = ControlRequest {
		request: Some(control_request::Request::Disconnect(
			protobuf::control::DisconnectRequest { peer_id },
		)),
	};

	let response = state.handler.handle(request);

	match response.response {
		Some(control_response::Response::Disconnect(d)) => {
			let status = if d.success {
				StatusCode::OK
			} else {
				StatusCode::NOT_FOUND
			};
			(
				status,
				Json(SuccessResponse {
					success: d.success,
					message: None,
				}),
			)
		}
		_ => (
			StatusCode::INTERNAL_SERVER_ERROR,
			Json(SuccessResponse {
				success: false,
				message: None,
			}),
		),
	}
}

pub async fn add_route(
	State(state): State<ApiState>,
	Json(req): Json<RouteAddRequest>,
) -> (StatusCode, Json<SuccessResponse>) {
	use protobuf::control::{ControlRequest, control_request, control_response};

	if let Err(e) = validation::validate_cidr(&req.cidr) {
		tracing::warn!("Invalid CIDR in route add request: {e}");
		return (
			StatusCode::BAD_REQUEST,
			Json(SuccessResponse {
				success: false,
				message: Some(e.to_string()),
			}),
		);
	}

	if let Err(e) = validation::validate_peer_id(&req.peer_id) {
		tracing::warn!("Invalid peer_id in route add request: {e}");
		return (
			StatusCode::BAD_REQUEST,
			Json(SuccessResponse {
				success: false,
				message: Some(e.to_string()),
			}),
		);
	}

	let request = ControlRequest {
		request: Some(control_request::Request::RouteAdd(
			protobuf::control::RouteAddRequest {
				cidr: req.cidr,
				peer_id: req.peer_id,
			},
		)),
	};

	let response = state.handler.handle(request);

	match response.response {
		Some(control_response::Response::RouteAdd(r)) => {
			let status = if r.success {
				StatusCode::CREATED
			} else {
				StatusCode::BAD_REQUEST
			};
			(
				status,
				Json(SuccessResponse {
					success: r.success,
					message: if r.message.is_empty() {
						None
					} else {
						Some(r.message)
					},
				}),
			)
		}
		_ => (
			StatusCode::INTERNAL_SERVER_ERROR,
			Json(SuccessResponse {
				success: false,
				message: None,
			}),
		),
	}
}

pub async fn delete_route(
	State(state): State<ApiState>,
	Path(cidr): Path<String>,
) -> (StatusCode, Json<SuccessResponse>) {
	use protobuf::control::{ControlRequest, control_request, control_response};

	let cidr = urlencoding::decode(&cidr)
		.map(std::borrow::Cow::into_owned)
		.unwrap_or(cidr);

	if let Err(e) = validation::validate_cidr(&cidr) {
		tracing::warn!("Invalid CIDR in route delete request: {e}");
		return (
			StatusCode::BAD_REQUEST,
			Json(SuccessResponse {
				success: false,
				message: Some(e.to_string()),
			}),
		);
	}

	let request = ControlRequest {
		request: Some(control_request::Request::RouteRemove(
			protobuf::control::RouteRemoveRequest { cidr },
		)),
	};

	let response = state.handler.handle(request);

	match response.response {
		Some(control_response::Response::RouteRemove(r)) => {
			let status = if r.success {
				StatusCode::OK
			} else {
				StatusCode::NOT_FOUND
			};
			(
				status,
				Json(SuccessResponse {
					success: r.success,
					message: if r.message.is_empty() {
						None
					} else {
						Some(r.message)
					},
				}),
			)
		}
		_ => (
			StatusCode::INTERNAL_SERVER_ERROR,
			Json(SuccessResponse {
				success: false,
				message: None,
			}),
		),
	}
}

pub async fn list_routes(State(state): State<ApiState>) -> Json<RoutesResponse> {
	use protobuf::control::{ControlRequest, control_request, control_response};

	let request = ControlRequest {
		request: Some(control_request::Request::RouteList(
			protobuf::control::RouteListRequest {},
		)),
	};

	let response = state.handler.handle(request);

	match response.response {
		Some(control_response::Response::RouteList(r)) => Json(RoutesResponse {
			routes: r
				.routes
				.into_iter()
				.map(|route| RouteResponse {
					cidr: route.cidr,
					peer_id: route.peer_id,
					added_at_secs: route.added_at_secs,
				})
				.collect(),
		}),
		_ => Json(RoutesResponse { routes: vec![] }),
	}
}

/// SSE endpoint for real-time events.
pub async fn events(
	State(state): State<ApiState>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
	let rx = state.events_tx.subscribe();
	let stream = BroadcastStream::new(rx).filter_map(|result| match result {
		Ok(event) => {
			let json = serde_json::to_string(&event).ok()?;
			Some(Ok(Event::default().data(json)))
		}
		Err(_) => None,
	});

	Sse::new(stream).keep_alive(
		KeepAlive::new()
			.interval(Duration::from_secs(15))
			.text("ping"),
	)
}
