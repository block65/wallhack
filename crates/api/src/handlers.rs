//! HTTP handlers for the REST API.

use std::fmt::Write;

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::sse::{Event, Sse},
};
use serde::{Deserialize, Serialize};
use wallhack_wire::management::{
    ConnectRequest, DisconnectRequest, HintLevel, HintSetAutoRequest, HintSetRequest, InfoRequest,
    ListenRequest, NodeRole, PeerDisconnectRequest, PeersRequest, PingRequest,
    RouteAddRequest as ProtoRouteAddRequest, RouteDelRequest, RoutesRequest, ShutdownRequest,
    StatsRequest, management_request, management_response,
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

/// Node status response.
#[derive(Debug, Serialize)]
pub struct StatusResponse {
    pub name: String,
    pub version: String,
    pub role: String,
    pub uptime_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub peer_addr: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub listen_addr: Option<String>,
    pub capabilities: CapabilitiesResponse,
}

/// Node capability flags.
#[derive(Debug, Serialize)]
pub struct CapabilitiesResponse {
    pub tun_capable: bool,
    pub listening: bool,
    pub connecting: bool,
}

/// Peer info response.
#[derive(Debug, Serialize)]
pub struct PeerResponse {
    pub id: String,
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

/// Connect request body.
#[derive(Debug, Deserialize)]
pub struct ConnectRequestBody {
    pub addr: String,
}

/// Connect response.
#[derive(Debug, Serialize)]
pub struct ConnectResponse {
    pub peer_addr: String,
    pub protocol: String,
}

/// Listen request body.
#[derive(Debug, Deserialize)]
pub struct ListenRequestBody {
    pub addr: String,
}

/// Listen response.
#[derive(Debug, Serialize)]
pub struct ListenResponse {
    pub listen_addr: String,
    pub protocol: String,
    pub fingerprint: String,
}

/// Ping response.
#[derive(Debug, Serialize)]
pub struct PingResponseBody {
    pub uptime_ms: u64,
    pub version: String,
    pub role: String,
}

/// Set hint request body.
#[derive(Debug, Deserialize)]
pub struct HintSetRequestBody {
    pub level: String,
    pub role: String,
}

pub async fn health() -> &'static str {
    "ok"
}

pub async fn events(
    State(state): State<ApiState>,
) -> Sse<impl tokio_stream::Stream<Item = Result<Event, std::convert::Infallible>>> {
    use tokio_stream::{StreamExt as _, wrappers::BroadcastStream};
    use wallhack_core::control::peers::PeerEvent;

    let stream = BroadcastStream::new(state.peer_events.subscribe()).filter_map(|result| {
        match result {
            Ok(PeerEvent::Connected { name, addr, role }) => {
                let data = serde_json::json!({
                    "type": "peer_connected",
                    "name": name,
                    "addr": addr,
                    "role": format!("{role:?}").to_ascii_lowercase(),
                });
                Some(Ok(Event::default()
                    .event("peer_connected")
                    .data(data.to_string())))
            }
            Ok(PeerEvent::Disconnected { name }) => {
                let data = serde_json::json!({
                    "type": "peer_disconnected",
                    "name": name,
                });
                Some(Ok(Event::default()
                    .event("peer_disconnected")
                    .data(data.to_string())))
            }
            Err(_) => None, // lagged — skip
        }
    });

    Sse::new(stream).keep_alive(
        axum::response::sse::KeepAlive::new()
            .interval(std::time::Duration::from_secs(15))
            .text("ping"),
    )
}

pub async fn info(State(state): State<ApiState>) -> Result<Json<StatusResponse>, StatusCode> {
    let resp = state
        .ipc
        .lock()
        .await
        .request(management_request::Request::Info(InfoRequest {}))
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    match resp.response {
        Some(management_response::Response::Info(s)) => {
            let role = s.role().to_string();
            Ok(Json(StatusResponse {
                name: s.package_name,
                version: s.version,
                role,
                uptime_ms: s.uptime_ms,
                peer_addr: if s.peer_addr.is_empty() {
                    None
                } else {
                    Some(s.peer_addr)
                },
                listen_addr: if s.listen_addr.is_empty() {
                    None
                } else {
                    Some(s.listen_addr)
                },
                capabilities: CapabilitiesResponse {
                    tun_capable: s.tun_capable,
                    listening: s.listening,
                    connecting: s.connecting,
                },
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
                        id: peer.id,
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
        .request(management_request::Request::PeerDisconnect(
            PeerDisconnectRequest {
                peer: name,
                exact: true,
            },
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
                let not_found: i32 = wallhack_wire::management::ErrorCode::PeerNotFound.into();
                let ambiguous: i32 = wallhack_wire::management::ErrorCode::PeerAmbiguous.into();
                let status = if e.code == not_found {
                    StatusCode::NOT_FOUND
                } else if e.code == ambiguous {
                    StatusCode::CONFLICT
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
        .request(management_request::Request::RouteAdd(
            ProtoRouteAddRequest {
                cidr: req.cidr,
                peer: req.peer,
            },
        ))
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
        .request(management_request::Request::RouteDel(RouteDelRequest {
            cidr: cidr_str,
        }))
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

pub async fn connect(
    State(state): State<ApiState>,
    Json(req): Json<ConnectRequestBody>,
) -> Result<Json<ConnectResponse>, StatusCode> {
    let resp = state
        .ipc
        .lock()
        .await
        .request(management_request::Request::Connect(ConnectRequest {
            addr: req.addr,
        }))
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    match resp.response {
        Some(management_response::Response::Connect(connect)) => Ok(Json(ConnectResponse {
            peer_addr: connect.peer_addr,
            protocol: connect.protocol,
        })),
        Some(management_response::Response::Error(e)) => {
            tracing::warn!("Connect failed: {}", e.message);
            Err(StatusCode::BAD_REQUEST)
        }
        _ => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

pub async fn listen(
    State(state): State<ApiState>,
    Json(req): Json<ListenRequestBody>,
) -> Result<Json<ListenResponse>, StatusCode> {
    let resp = state
        .ipc
        .lock()
        .await
        .request(management_request::Request::Listen(ListenRequest {
            addr: req.addr,
        }))
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    match resp.response {
        Some(management_response::Response::Listen(listen)) => Ok(Json(ListenResponse {
            listen_addr: listen.listen_addr,
            protocol: listen.protocol,
            fingerprint: listen.fingerprint,
        })),
        Some(management_response::Response::Error(e)) => {
            tracing::warn!("Listen failed: {}", e.message);
            Err(StatusCode::BAD_REQUEST)
        }
        _ => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

pub async fn disconnect(State(state): State<ApiState>) -> (StatusCode, Json<SuccessResponse>) {
    let resp = state
        .ipc
        .lock()
        .await
        .request(management_request::Request::Disconnect(
            DisconnectRequest {},
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
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(SuccessResponse {
                success: false,
                message: None,
            }),
        ),
    }
}

pub async fn ping(State(state): State<ApiState>) -> Result<Json<PingResponseBody>, StatusCode> {
    let resp = state
        .ipc
        .lock()
        .await
        .request(management_request::Request::Ping(PingRequest {
            peer: String::new(),
        }))
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    match resp.response {
        Some(management_response::Response::Ping(ping)) => {
            let role = NodeRole::try_from(ping.node_role).unwrap_or(NodeRole::Unspecified);
            Ok(Json(PingResponseBody {
                uptime_ms: ping.uptime_ms,
                version: ping.version,
                role: role.to_string(),
            }))
        }
        _ => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

pub async fn ping_peer(
    State(state): State<ApiState>,
    Path(peer): Path<String>,
) -> Result<Json<PingResponseBody>, StatusCode> {
    let resp = state
        .ipc
        .lock()
        .await
        .request(management_request::Request::Ping(PingRequest { peer }))
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    match resp.response {
        Some(management_response::Response::Ping(ping)) => {
            let role = NodeRole::try_from(ping.node_role).unwrap_or(NodeRole::Unspecified);
            Ok(Json(PingResponseBody {
                uptime_ms: ping.uptime_ms,
                version: ping.version,
                role: role.to_string(),
            }))
        }
        Some(management_response::Response::Error(e)) => {
            let not_supported: i32 = wallhack_wire::management::ErrorCode::NotSupported.into();
            if e.code == not_supported {
                Err(StatusCode::NOT_IMPLEMENTED)
            } else {
                tracing::warn!("Ping peer failed: {}", e.message);
                Err(StatusCode::NOT_FOUND)
            }
        }
        _ => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

pub async fn shutdown(State(state): State<ApiState>) -> (StatusCode, Json<SuccessResponse>) {
    let resp = state
        .ipc
        .lock()
        .await
        .request(management_request::Request::Shutdown(ShutdownRequest {}))
        .await;

    match resp {
        Ok(_) => (
            StatusCode::OK,
            Json(SuccessResponse {
                success: true,
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

pub async fn hint_set(
    State(state): State<ApiState>,
    Json(req): Json<HintSetRequestBody>,
) -> (StatusCode, Json<SuccessResponse>) {
    let level = match req.level.as_str() {
        "prefer" => HintLevel::Prefer,
        "exclude" => HintLevel::Exclude,
        "fixed" => HintLevel::Fixed,
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(SuccessResponse {
                    success: false,
                    message: Some(format!(
                        "invalid hint level '{}' (expected: prefer, exclude, fixed)",
                        req.level
                    )),
                }),
            );
        }
    };
    let role = match req.role.as_str() {
        "entry" => NodeRole::Entry,
        "exit" => NodeRole::Exit,
        "relay" => NodeRole::Relay,
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(SuccessResponse {
                    success: false,
                    message: Some(format!(
                        "invalid role '{}' (expected: entry, exit, relay)",
                        req.role
                    )),
                }),
            );
        }
    };

    let resp = state
        .ipc
        .lock()
        .await
        .request(management_request::Request::HintSet(HintSetRequest {
            level: level.into(),
            role: role.into(),
        }))
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
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(SuccessResponse {
                success: false,
                message: None,
            }),
        ),
    }
}

pub async fn hint_set_auto(State(state): State<ApiState>) -> (StatusCode, Json<SuccessResponse>) {
    let resp = state
        .ipc
        .lock()
        .await
        .request(management_request::Request::HintSetAuto(
            HintSetAutoRequest {},
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
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(SuccessResponse {
                success: false,
                message: None,
            }),
        ),
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
