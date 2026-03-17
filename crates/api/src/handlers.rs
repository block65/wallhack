//! HTTP handlers for the REST API.

use std::fmt::Write;

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use serde::{Deserialize, Serialize};
use wallhack_wire::management::{
    AddRouteRequest, DisconnectPeerRequest, PeersRequest, RemoveRouteRequest, RoutesRequest,
    StatsRequest, StatusRequest, management_request, management_response,
};

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
    pub name: String,
    pub addr: String,
    pub role: String,
    pub connect_time: String,
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
    pub peer: String,
}

/// Route info for JSON response.
#[derive(Debug, Serialize)]
pub struct RouteResponse {
    pub cidr: String,
    pub peer: String,
    pub create_time: String,
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

pub async fn ping(State(state): State<ApiState>) -> Result<Json<PingResponse>, StatusCode> {
    let resp = state
        .ipc
        .lock()
        .await
        .request(management_request::Request::Status(StatusRequest {}))
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    match resp.response {
        Some(management_response::Response::Status(s)) => {
            let role = format!("{:?}", s.role());
            Ok(Json(PingResponse {
                uptime_ms: s.uptime_ms,
                version: s.version,
                node_role: role,
            }))
        }
        _ => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

pub async fn stats(State(state): State<ApiState>) -> Result<Json<StatsResponse>, StatusCode> {
    let resp = state
        .ipc
        .lock()
        .await
        .request(management_request::Request::Stats(StatsRequest {}))
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    match resp.response {
        Some(management_response::Response::Stats(s)) => Ok(Json(StatsResponse {
            bytes_in: s.bytes_in,
            bytes_out: s.bytes_out,
            packets_in: s.packets_in,
            packets_out: s.packets_out,
            active_connections: s.active_connections,
            active_flows: s.active_flows,
        })),
        _ => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

pub async fn peers(State(state): State<ApiState>) -> Result<Json<PeersResponse>, StatusCode> {
    let resp = state
        .ipc
        .lock()
        .await
        .request(management_request::Request::Peers(PeersRequest {}))
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    match resp.response {
        Some(management_response::Response::Peers(p)) => Ok(Json(PeersResponse {
            peers: p
                .peers
                .into_iter()
                .map(|peer| {
                    let role = wallhack_wire::management::NodeRole::try_from(peer.role)
                        .unwrap_or(wallhack_wire::management::NodeRole::Unspecified);
                    PeerResponse {
                        name: peer.name,
                        addr: peer.addr,
                        role: role.to_string(),
                        connect_time: epoch_to_iso8601(peer.connect_time),
                        bytes_transferred: peer.bytes_transferred,
                        latency_ms: if peer.latency_ms > 0.0 {
                            Some(peer.latency_ms)
                        } else {
                            None
                        },
                    }
                })
                .collect(),
        })),
        _ => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

pub async fn disconnect_peer(
    State(state): State<ApiState>,
    Path(name): Path<String>,
) -> (StatusCode, Json<SuccessResponse>) {
    if let Err(e) = validation::validate_peer_name(&name) {
        tracing::warn!("Invalid peer name in disconnect request: {e}");
        return (
            StatusCode::BAD_REQUEST,
            Json(SuccessResponse {
                success: false,
                message: None,
            }),
        );
    }

    let resp = state
        .ipc
        .lock()
        .await
        .request(management_request::Request::DisconnectPeer(
            DisconnectPeerRequest { peer: name },
        ))
        .await;

    match resp {
        Ok(r) => match r.response {
            Some(management_response::Response::Ok(_)) => (
                StatusCode::OK,
                Json(SuccessResponse {
                    success: true,
                    message: None,
                }),
            ),
            Some(management_response::Response::Error(e)) => {
                let code: i32 = wallhack_wire::management::ErrorCode::PeerNotFound.into();
                let status = if e.code == code {
                    StatusCode::NOT_FOUND
                } else {
                    StatusCode::INTERNAL_SERVER_ERROR
                };
                (
                    status,
                    Json(SuccessResponse {
                        success: false,
                        message: Some(e.message),
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
        },
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

    if let Err(e) = validation::validate_peer_name(&req.peer) {
        tracing::warn!("Invalid peer name in route add request: {e}");
        return (
            StatusCode::BAD_REQUEST,
            Json(SuccessResponse {
                success: false,
                message: Some(e.to_string()),
            }),
        );
    }

    let resp = state
        .ipc
        .lock()
        .await
        .request(management_request::Request::AddRoute(AddRouteRequest {
            cidr: req.cidr,
            peer: req.peer,
        }))
        .await;

    match resp {
        Ok(r) => match r.response {
            Some(management_response::Response::Ok(_)) => (
                StatusCode::CREATED,
                Json(SuccessResponse {
                    success: true,
                    message: None,
                }),
            ),
            Some(management_response::Response::Error(e)) => (
                StatusCode::BAD_REQUEST,
                Json(SuccessResponse {
                    success: false,
                    message: Some(e.message),
                }),
            ),
            _ => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(SuccessResponse {
                    success: false,
                    message: None,
                }),
            ),
        },
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
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

    let resp = state
        .ipc
        .lock()
        .await
        .request(management_request::Request::RemoveRoute(
            RemoveRouteRequest { cidr: cidr_str },
        ))
        .await;

    match resp {
        Ok(r) => match r.response {
            Some(management_response::Response::Ok(_)) => (
                StatusCode::OK,
                Json(SuccessResponse {
                    success: true,
                    message: None,
                }),
            ),
            Some(management_response::Response::Error(e)) => {
                let code: i32 = wallhack_wire::management::ErrorCode::RouteNotFound.into();
                let status = if e.code == code {
                    StatusCode::NOT_FOUND
                } else {
                    StatusCode::INTERNAL_SERVER_ERROR
                };
                (
                    status,
                    Json(SuccessResponse {
                        success: false,
                        message: Some(e.message),
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
        },
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(SuccessResponse {
                success: false,
                message: Some(e.to_string()),
            }),
        ),
    }
}

pub async fn list_routes(
    State(state): State<ApiState>,
) -> Result<Json<RoutesResponse>, StatusCode> {
    let resp = state
        .ipc
        .lock()
        .await
        .request(management_request::Request::Routes(RoutesRequest {}))
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    match resp.response {
        Some(management_response::Response::Routes(r)) => Ok(Json(RoutesResponse {
            routes: r
                .routes
                .into_iter()
                .map(|route| RouteResponse {
                    cidr: route.cidr,
                    peer: route.peer,
                    create_time: epoch_to_iso8601(route.create_time),
                })
                .collect(),
        })),
        _ => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

/// Convert epoch seconds to ISO 8601 UTC string.
fn epoch_to_iso8601(epoch_secs: u64) -> String {
    #[allow(clippy::cast_possible_wrap)] // REASON: epoch seconds fits i64 for millennia
    let dt = time::OffsetDateTime::from_unix_timestamp(epoch_secs as i64)
        .unwrap_or(time::OffsetDateTime::UNIX_EPOCH);
    let mut buf = String::with_capacity(20);
    let _ = write!(
        buf,
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        dt.year(),
        dt.month() as u8,
        dt.day(),
        dt.hour(),
        dt.minute(),
        dt.second(),
    );
    buf
}
