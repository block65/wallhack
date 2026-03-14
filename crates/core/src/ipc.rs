//! IPC listener for the management protocol.
//!
//! Accepts connections on a Unix domain socket, reads [`ManagementRequest`]
//! messages, dispatches to [`NodeApi`], and writes [`DaemonMessage`] responses.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context, Result};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    net::UnixListener,
    sync::{broadcast, mpsc, watch},
};
use wallhack_transport::TransportError;
use wallhack_wire::management::{
    self, ConnectResponse, DaemonMessage, DaemonNotification, ErrorCode, ErrorResponse,
    ListenResponse, ManagementRequest, ManagementResponse, OkResponse, PeerConnected,
    PeerDisconnected, PeersResponse, PingResponse, RoutesResponse, StatsResponse, StatusResponse,
    daemon_message, daemon_notification, management_request, management_response,
};

use crate::{
    control::peers::PeerEvent,
    node_api::{NodeApi, NodeApiError},
    transport::protocol::{AsyncProtoRead as _, AsyncProtoWrite as _, CONTROL_MTU},
};

/// Default socket filename within the runtime directory.
const SOCKET_NAME: &str = "wallhackd.sock";

/// vsock port for the IPC listener (guest-side, host connects to this).
pub const VSOCK_IPC_PORT: u32 = 4434;

/// Environment variable for overriding the socket path (like `DOCKER_HOST`).
const HOST_ENV: &str = "WALLHACK_HOST";

/// Resolve the IPC socket path.
///
/// Checks (in order):
/// 1. `WALLHACK_HOST` environment variable
/// 2. `$XDG_RUNTIME_DIR/wallhack/wallhackd.sock`
/// 3. `/tmp/wallhack-<user>/wallhackd.sock`
/// 4. `$HOME/.wallhack/wallhackd.sock`
/// 5. `/tmp/wallhack-shared/wallhackd.sock`
#[must_use]
pub fn socket_path() -> PathBuf {
    if let Ok(host) = std::env::var(HOST_ENV) {
        return PathBuf::from(host.strip_prefix("unix://").unwrap_or(&host));
    }
    if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
        Path::new(&runtime_dir).join("wallhack").join(SOCKET_NAME)
    } else if let Ok(user) = std::env::var("USER") {
        PathBuf::from(format!("/tmp/wallhack-{user}")).join(SOCKET_NAME)
    } else if let Ok(home) = std::env::var("HOME") {
        Path::new(&home).join(".wallhack").join(SOCKET_NAME)
    } else {
        PathBuf::from("/tmp/wallhack-shared").join(SOCKET_NAME)
    }
}

/// Run the IPC listener on the given Unix socket path.
///
/// Accepts connections in a loop, spawning a task per connection. Each
/// connection reads [`ManagementRequest`] frames, dispatches to `node_api`,
/// and writes back [`DaemonMessage`] frames.
///
/// The listener runs until `shutdown_rx` fires or the task is cancelled.
///
/// # Errors
///
/// Returns an error if the socket cannot be bound.
pub async fn run_ipc_listener(
    node_api: Arc<dyn NodeApi>,
    peer_events: broadcast::Sender<PeerEvent>,
    path: &Path,
    mut shutdown_rx: watch::Receiver<()>,
) -> Result<()> {
    // Ensure parent directory exists.
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating socket directory {}", parent.display()))?;
    }

    // Remove stale socket if it exists.
    if path.exists() {
        std::fs::remove_file(path)
            .with_context(|| format!("removing stale socket {}", path.display()))?;
    }

    let listener = UnixListener::bind(path)
        .with_context(|| format!("binding IPC socket {}", path.display()))?;

    tracing::info!(path = %path.display(), "IPC listener started");

    loop {
        tokio::select! {
            biased;
            _ = shutdown_rx.changed() => {
                tracing::info!("IPC listener shutting down");
                break;
            }
            result = listener.accept() => {
                let (stream, _addr) = result.context("accepting IPC connection")?;
                let api = Arc::clone(&node_api);
                let events_rx = peer_events.subscribe();
                tokio::spawn(async move {
                    if let Err(e) = handle_connection(stream, api, Some(events_rx)).await {
                        tracing::debug!(error = %e, "IPC connection ended");
                    }
                });
            }
        }
    }

    // Clean up socket file.
    // TODO: use platform-agnostic named pipe on Windows (UnixListener is Unix-only)
    let _ = std::fs::remove_file(path);

    Ok(())
}

/// Run a vsock IPC listener on the given port.
///
/// Binds on `VMADDR_CID_ANY` so any host CID can connect. Accepts connections
/// in a loop, spawning `handle_connection` per client. Runs until `shutdown_rx`
/// fires.
///
/// # Errors
///
/// Returns an error if the vsock port cannot be bound.
#[cfg(feature = "vsock")]
pub async fn run_vsock_listener(
    node_api: Arc<dyn NodeApi>,
    peer_events: broadcast::Sender<PeerEvent>,
    port: u32,
    mut shutdown_rx: watch::Receiver<()>,
) -> Result<()> {
    use tokio_vsock::{VMADDR_CID_ANY, VsockAddr, VsockListener};

    let listener = VsockListener::bind(VsockAddr::new(VMADDR_CID_ANY, port))
        .with_context(|| format!("binding vsock port {port}"))?;

    tracing::info!(port, "vsock IPC listener started");

    loop {
        tokio::select! {
            biased;
            _ = shutdown_rx.changed() => {
                tracing::info!("vsock IPC listener shutting down");
                break;
            }
            result = listener.accept() => {
                match result {
                    Ok((stream, addr)) => {
                        tracing::debug!(cid = addr.cid(), port = addr.port(), "vsock IPC connection");
                        let api = Arc::clone(&node_api);
                        let events_rx = peer_events.subscribe();
                        tokio::spawn(async move {
                            if let Err(e) = handle_connection(stream, api, Some(events_rx)).await {
                                tracing::debug!(error = %e, "vsock IPC connection ended");
                            }
                        });
                    }
                    Err(e) => tracing::warn!(error = %e, "vsock accept error"),
                }
            }
        }
    }

    Ok(())
}

/// Handle a single IPC connection.
///
/// Reads requests, dispatches to `node_api`, and writes back responses.
/// If `peer_events` is provided, peer lifecycle notifications are also
/// pushed to the client as `DaemonMessage::Notification` frames.
pub async fn handle_connection(
    stream: impl AsyncRead + AsyncWrite + Unpin + Send + 'static,
    node_api: Arc<dyn NodeApi>,
    peer_events: Option<broadcast::Receiver<PeerEvent>>,
) -> Result<(), TransportError> {
    let (mut reader, mut writer) = tokio::io::split(stream);

    // Channel for serialising writes from both request handler and
    // notification forwarder through a single writer task.
    let (write_tx, mut write_rx) = mpsc::channel::<DaemonMessage>(32);

    // Writer task: drains the channel and writes frames.
    let writer_task = tokio::spawn(async move {
        while let Some(msg) = write_rx.recv().await {
            if let Err(e) = writer.write_proto(&msg).await {
                tracing::trace!(error = %e, "IPC write ended");
                break;
            }
        }
    });

    // Notification forwarder task (if subscribed).
    let notify_tx = write_tx.clone();
    let notify_task = peer_events.map(|mut rx| {
        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(event) => {
                        let notification = DaemonNotification::from(event);
                        let msg = DaemonMessage {
                            message: Some(daemon_message::Message::Notification(notification)),
                        };
                        if notify_tx.send(msg).await.is_err() {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::debug!(missed = n, "IPC notification subscriber lagged");
                        // Reusing TunnelError as a generic warning — no
                        // dedicated proto message for internal advisories yet.
                        let warning = DaemonMessage {
                            message: Some(daemon_message::Message::Notification(
                                DaemonNotification {
                                    event: Some(daemon_notification::Event::TunnelError(
                                        management::TunnelError {
                                            message: format!("missed {n} peer notification(s)"),
                                        },
                                    )),
                                },
                            )),
                        };
                        if notify_tx.send(warning).await.is_err() {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        })
    });

    // Request loop: read requests, dispatch, send responses.
    let result = loop {
        let request: ManagementRequest = match reader.read_proto(CONTROL_MTU).await {
            Ok(req) => req,
            Err(e) => {
                tracing::trace!(error = %e, "IPC read ended");
                break Ok(());
            }
        };

        let response = dispatch_request(&request, &*node_api);
        let msg = DaemonMessage {
            message: Some(daemon_message::Message::Response(response)),
        };

        if write_tx.send(msg).await.is_err() {
            break Ok(());
        }
    };

    // Tear down: drop our sender so the writer drains and exits.
    drop(write_tx);
    if let Some(task) = notify_task {
        task.abort();
    }
    let _ = writer_task.await;

    result
}

/// Map a [`ManagementRequest`] to a [`ManagementResponse`] via [`NodeApi`].
#[allow(clippy::too_many_lines)] // refactor candidate
fn dispatch_request(request: &ManagementRequest, api: &dyn NodeApi) -> ManagementResponse {
    let request_id = request.request_id;

    let response = match &request.request {
        Some(management_request::Request::Ping(req)) => {
            if req.peer.is_empty() {
                // Ping the daemon itself
                let status = api.status();
                management_response::Response::Ping(PingResponse {
                    uptime_ms: status.uptime_ms,
                    version: status.version,
                    node_role: management::NodeRole::from(status.role).into(),
                })
            } else {
                // Peer pinging is not yet implemented
                return ManagementResponse {
                    request_id,
                    response: Some(management_response::Response::Error(ErrorResponse {
                        code: ErrorCode::NotSupported.into(),
                        message: "peer ping not yet implemented".to_string(),
                    })),
                };
            }
        }

        Some(management_request::Request::Status(_)) => {
            let s = api.status();
            management_response::Response::Status(StatusResponse {
                role: management::NodeRole::from(s.role).into(),
                connected: s.connected,
                peer_addr: s.peer_addr.unwrap_or_default(),
                listen_addr: s.listen_addr.map_or_else(String::new, |a| a.to_string()),
                version: s.version,
                uptime_ms: s.uptime_ms,
                package_name: s.name,
                tun_capable: s.capabilities.tun_capable,
                listening: s.capabilities.listening,
                connecting: s.capabilities.connecting,
            })
        }

        Some(management_request::Request::Stats(_)) => {
            let m = api.metrics();
            management_response::Response::Stats(StatsResponse {
                bytes_in: m.bytes_in,
                bytes_out: m.bytes_out,
                packets_in: m.packets_in,
                packets_out: m.packets_out,
                active_connections: m.active_connections,
                active_flows: m.active_flows,
                packets_dropped: m.packets_dropped,
            })
        }

        Some(management_request::Request::Peers(_)) => {
            let peers = api.peers();
            management_response::Response::Peers(PeersResponse {
                peers: peers.into_iter().map(management::PeerInfo::from).collect(),
            })
        }

        Some(management_request::Request::Routes(_)) => match api.routes() {
            Ok(routes) => management_response::Response::Routes(RoutesResponse {
                routes: routes
                    .into_iter()
                    .map(management::RouteEntry::from)
                    .collect(),
            }),
            Err(e) => error_response(&e),
        },

        Some(management_request::Request::AddRoute(req)) => match req.cidr.parse() {
            Ok(cidr) => match api.add_route(cidr, req.peer.clone()) {
                Ok(()) => management_response::Response::Ok(OkResponse {}),
                Err(e) => error_response(&e),
            },
            Err(_) => management_response::Response::Error(ErrorResponse {
                code: ErrorCode::InvalidAddress.into(),
                message: format!("invalid CIDR: {}", req.cidr),
            }),
        },

        Some(management_request::Request::RemoveRoute(req)) => match req.cidr.parse() {
            Ok(cidr) => match api.remove_route(&cidr) {
                Ok(()) => management_response::Response::Ok(OkResponse {}),
                Err(e) => error_response(&e),
            },
            Err(_) => management_response::Response::Error(ErrorResponse {
                code: ErrorCode::InvalidAddress.into(),
                message: format!("invalid CIDR: {}", req.cidr),
            }),
        },

        Some(management_request::Request::DisconnectPeer(req)) => {
            match api.disconnect_peer(req.peer.clone()) {
                Ok(()) => management_response::Response::Ok(OkResponse {}),
                Err(e) => error_response(&e),
            }
        }

        Some(management_request::Request::Connect(req)) => match api.connect(&req.addr) {
            Ok(info) => management_response::Response::Connect(ConnectResponse {
                peer_addr: info.peer_addr,
                protocol: info.protocol,
            }),
            Err(e) => error_response(&e),
        },

        Some(management_request::Request::Listen(req)) => match req.addr.parse() {
            Ok(addr) => match api.listen(addr) {
                Ok(info) => management_response::Response::Listen(ListenResponse {
                    listen_addr: info.listen_addr.to_string(),
                    protocol: info.protocol,
                    fingerprint: info.fingerprint,
                }),
                Err(e) => error_response(&e),
            },
            Err(_) => management_response::Response::Error(ErrorResponse {
                code: ErrorCode::InvalidAddress.into(),
                message: format!("invalid address: {}", req.addr),
            }),
        },

        Some(management_request::Request::Disconnect(_)) => match api.disconnect() {
            Ok(()) => management_response::Response::Ok(OkResponse {}),
            Err(e) => error_response(&e),
        },

        Some(management_request::Request::Shutdown(_)) => {
            // Shutdown is handled by the caller via DaemonHandle, not NodeApi.
            // Return Ok here — the daemon layer should intercept ShutdownRequest
            // before it reaches dispatch, or handle it after dispatch returns.
            management_response::Response::Ok(OkResponse {})
        }

        Some(management_request::Request::SetHint(req)) => {
            let level = wallhack_wire::data::HintLevel::try_from(req.level).unwrap_or_default();
            let target = wallhack_wire::data::NodeRole::try_from(req.role).unwrap_or_default();
            let hint = wallhack_wire::data::RoleHint {
                level: level.into(),
                target: target.into(),
            };
            match api.set_hint(hint) {
                Ok(()) => management_response::Response::Ok(OkResponse {}),
                Err(e) => error_response(&e),
            }
        }

        Some(management_request::Request::ClearHints(_)) => match api.clear_hints() {
            Ok(()) => management_response::Response::Ok(OkResponse {}),
            Err(e) => error_response(&e),
        },

        None => management_response::Response::Error(ErrorResponse {
            code: ErrorCode::Internal.into(),
            message: "empty request".to_string(),
        }),
    };

    ManagementResponse {
        request_id,
        response: Some(response),
    }
}

// ── Conversion helpers ──────────────────────────────────────────────

impl From<PeerEvent> for DaemonNotification {
    fn from(event: PeerEvent) -> Self {
        let event = match event {
            PeerEvent::Connected {
                name,
                addr,
                role: _,
            } => daemon_notification::Event::PeerConnected(PeerConnected {
                peer: Some(management::PeerInfo {
                    name,
                    addr,
                    status: management::PeerStatus::Connected.into(),
                    ..Default::default()
                }),
            }),
            PeerEvent::Disconnected { name } => {
                daemon_notification::Event::PeerDisconnected(PeerDisconnected {
                    name,
                    reason: String::new(),
                })
            }
        };
        DaemonNotification { event: Some(event) }
    }
}

fn error_response(e: &NodeApiError) -> management_response::Response {
    let (code, message) = match e {
        NodeApiError::PeerNotFound(p) => (ErrorCode::PeerNotFound, format!("peer not found: {p}")),
        NodeApiError::PeerAmbiguous(prefix, peers) => {
            let peers_str = peers.join(", ");
            (
                ErrorCode::PeerAmbiguous,
                format!("peer name '{prefix}' is ambiguous: matches {peers_str}"),
            )
        }
        NodeApiError::RouteNotFound(c) => {
            (ErrorCode::RouteNotFound, format!("route not found: {c}"))
        }
        NodeApiError::NotSupported(msg) => (ErrorCode::NotSupported, msg.clone()),
        NodeApiError::InvalidAddress(a) => {
            (ErrorCode::InvalidAddress, format!("invalid address: {a}"))
        }
        NodeApiError::AlreadyConnected => {
            (ErrorCode::AlreadyConnected, "already connected".to_string())
        }
        NodeApiError::AlreadyListening => {
            (ErrorCode::AlreadyListening, "already listening".to_string())
        }
        NodeApiError::NotConnected => (ErrorCode::NotConnected, "not connected".to_string()),
        NodeApiError::Internal(msg) => (ErrorCode::Internal, msg.clone()),
    };
    management_response::Response::Error(ErrorResponse {
        code: code.into(),
        message,
    })
}

/// Maps domain `NodeRole` to management proto `NodeRole`.
impl From<crate::NodeRole> for management::NodeRole {
    fn from(role: crate::NodeRole) -> Self {
        match role {
            crate::NodeRole::Indeterminate => management::NodeRole::Indeterminate,
            crate::NodeRole::Entry => management::NodeRole::Entry,
            crate::NodeRole::Relay => management::NodeRole::Relay,
            crate::NodeRole::Exit => management::NodeRole::Exit,
        }
    }
}

impl From<crate::node_api::PeerStatus> for management::PeerStatus {
    fn from(status: crate::node_api::PeerStatus) -> Self {
        match status {
            crate::node_api::PeerStatus::Connected => management::PeerStatus::Connected,
            crate::node_api::PeerStatus::Disconnected => management::PeerStatus::Disconnected,
        }
    }
}

impl From<crate::node_api::PeerInfo> for management::PeerInfo {
    fn from(p: crate::node_api::PeerInfo) -> Self {
        management::PeerInfo {
            name: p.name,
            addr: p.addr,
            status: management::PeerStatus::from(p.status).into(),
            connected_at_secs: p.connected_at_secs,
            bytes_transferred: p.bytes_transferred,
            latency_ms: p.latency_ms.unwrap_or(0.0),
            tun_capable: p.capabilities.tun_capable,
            listening: p.capabilities.listening,
            connecting: p.capabilities.connecting,
            role: management::NodeRole::from(p.role).into(),
        }
    }
}

impl From<crate::node_api::RouteEntry> for management::RouteEntry {
    fn from(r: crate::node_api::RouteEntry) -> Self {
        let elapsed = r.added_at.elapsed();
        let added_at_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |now| now.as_secs().saturating_sub(elapsed.as_secs()));
        management::RouteEntry {
            cidr: r.cidr.to_string(),
            peer: r.peer,
            added_at_secs,
            auto_managed: r.auto_managed,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_role_management_proto_round_trip() {
        let cases = [
            (
                crate::NodeRole::Indeterminate,
                management::NodeRole::Indeterminate,
            ),
            (crate::NodeRole::Entry, management::NodeRole::Entry),
            (crate::NodeRole::Relay, management::NodeRole::Relay),
            (crate::NodeRole::Exit, management::NodeRole::Exit),
        ];
        for (domain, expected_proto) in cases {
            let proto = management::NodeRole::from(domain);
            assert_eq!(proto, expected_proto, "domain {domain} -> proto mismatch");
        }
    }
}
