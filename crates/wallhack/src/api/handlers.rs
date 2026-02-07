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
	let status = state.node_api.status();

	Json(PingResponse {
		uptime_ms: status.uptime_ms,
		version: status.version,
		node_role: format!("{:?}", status.role),
	})
}

pub async fn stats(State(state): State<ApiState>) -> Json<StatsResponse> {
	let metrics = state.node_api.metrics();

	Json(StatsResponse {
		bytes_in: metrics.bytes_in,
		bytes_out: metrics.bytes_out,
		packets_in: metrics.packets_in,
		packets_out: metrics.packets_out,
		active_connections: metrics.active_connections,
		active_flows: metrics.active_flows,
	})
}

pub async fn peers(State(state): State<ApiState>) -> Json<PeersResponse> {
	let peers = state.node_api.peers();

	Json(PeersResponse {
		peers: peers
			.into_iter()
			.map(|peer| PeerResponse {
				id: peer.id,
				addr: peer.addr,
				role: match peer.capability {
					super::node_api::NodeCapability::Exit => "exit".to_string(),
					super::node_api::NodeCapability::Relay => "relay".to_string(),
				},
				connected_at: peer.connected_at_secs,
				bytes_transferred: peer.bytes_transferred,
				latency_ms: peer.latency_ms,
			})
			.collect(),
	})
}

pub async fn disconnect_peer(
	State(state): State<ApiState>,
	Path(peer_id): Path<String>,
) -> (StatusCode, Json<SuccessResponse>) {
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

	match state.node_api.disconnect_peer(peer_id) {
		Ok(()) => (
			StatusCode::OK,
			Json(SuccessResponse {
				success: true,
				message: None,
			}),
		),
		Err(super::node_api::NodeApiError::PeerNotFound(_)) => (
			StatusCode::NOT_FOUND,
			Json(SuccessResponse {
				success: false,
				message: None,
			}),
		),
		Err(_) => (
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

	let cidr = match req.cidr.parse() {
		Ok(c) => c,
		Err(e) => {
			return (
				StatusCode::BAD_REQUEST,
				Json(SuccessResponse {
					success: false,
					message: Some(format!("Invalid CIDR: {e}")),
				}),
			);
		}
	};

	match state.node_api.add_route(cidr, req.peer_id) {
		Ok(()) => (
			StatusCode::CREATED,
			Json(SuccessResponse {
				success: true,
				message: None,
			}),
		),
		Err(e) => (
			StatusCode::BAD_REQUEST,
			Json(SuccessResponse {
				success: false,
				message: Some(e.to_string()),
			}),
		),
	}
}

pub async fn delete_route(
	State(state): State<ApiState>,
	Path(cidr_str): Path<String>,
) -> (StatusCode, Json<SuccessResponse>) {
	let cidr_str = urlencoding::decode(&cidr_str)
		.map(std::borrow::Cow::into_owned)
		.unwrap_or(cidr_str);

	if let Err(e) = validation::validate_cidr(&cidr_str) {
		tracing::warn!("Invalid CIDR in route delete request: {e}");
		return (
			StatusCode::BAD_REQUEST,
			Json(SuccessResponse {
				success: false,
				message: Some(e.to_string()),
			}),
		);
	}

	let cidr = match cidr_str.parse() {
		Ok(c) => c,
		Err(e) => {
			return (
				StatusCode::BAD_REQUEST,
				Json(SuccessResponse {
					success: false,
					message: Some(format!("Invalid CIDR: {e}")),
				}),
			);
		}
	};

	match state.node_api.remove_route(&cidr) {
		Ok(()) => (
			StatusCode::OK,
			Json(SuccessResponse {
				success: true,
				message: None,
			}),
		),
		Err(super::node_api::NodeApiError::RouteNotFound(_)) => (
			StatusCode::NOT_FOUND,
			Json(SuccessResponse {
				success: false,
				message: Some("Route not found".to_string()),
			}),
		),
		Err(e) => (
			StatusCode::INTERNAL_SERVER_ERROR,
			Json(SuccessResponse {
				success: false,
				message: Some(e.to_string()),
			}),
		),
	}
}

pub async fn list_routes(State(state): State<ApiState>) -> Json<RoutesResponse> {
	let routes = state.node_api.routes().unwrap_or_default();

	Json(RoutesResponse {
		routes: routes
			.into_iter()
			.map(|route| RouteResponse {
				cidr: route.cidr.to_string(),
				peer_id: route.peer_id,
				added_at_secs: 0, // Not available in current NodeApi
			})
			.collect(),
	})
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
