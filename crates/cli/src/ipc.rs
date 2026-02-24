//! IPC client for communicating with the wallhack daemon.

use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

use tokio::{
    io::{AsyncRead, AsyncWrite},
    net::UnixStream,
};
use wallhack_wire::management::{
    DaemonMessage, ManagementRequest, ManagementResponse, daemon_message, management_request,
};

use crate::framing::{read_length_delimited, write_length_delimited};

/// Maximum size for management messages (4KB, matching daemon).
const MANAGEMENT_MTU: usize = 4096;

/// Default socket filename within the runtime directory.
const SOCKET_NAME: &str = "wallhackd.sock";

/// Monotonically increasing request ID.
static REQUEST_ID: AtomicU64 = AtomicU64::new(1);

/// Resolve the IPC socket path.
///
/// Uses `$XDG_RUNTIME_DIR/wallhack/wallhackd.sock` when available,
/// falling back to `/tmp/wallhack-<uid>/wallhackd.sock`.
#[must_use]
pub fn socket_path() -> PathBuf {
    if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
        std::path::Path::new(&runtime_dir)
            .join("wallhack")
            .join(SOCKET_NAME)
    } else {
        let user = std::env::var("USER").unwrap_or_else(|_| "shared".to_string());
        PathBuf::from(format!("/tmp/wallhack-{user}")).join(SOCKET_NAME)
    }
}

/// Connect to the daemon's IPC socket.
///
/// # Errors
///
/// Returns an error if the socket doesn't exist or connection is refused.
pub async fn connect() -> std::io::Result<UnixStream> {
    let path = socket_path();
    UnixStream::connect(&path).await.map_err(|e| {
        std::io::Error::new(
            e.kind(),
            format!("cannot connect to daemon at {}: {e}", path.display()),
        )
    })
}

/// Send a management request and read the response.
///
/// # Errors
///
/// Returns an error on I/O or framing failure, or if the daemon sends
/// an unexpected message type.
pub async fn send_request(
    stream: &mut (impl AsyncRead + AsyncWrite + Unpin),
    request: management_request::Request,
) -> Result<ManagementResponse, IpcError> {
    let request_id = REQUEST_ID.fetch_add(1, Ordering::Relaxed);
    let msg = ManagementRequest {
        request_id,
        request: Some(request),
    };

    write_length_delimited(stream, &msg).await?;

    let daemon_msg: DaemonMessage = read_length_delimited(stream, MANAGEMENT_MTU).await?;

    match daemon_msg.message {
        Some(daemon_message::Message::Response(resp)) => Ok(resp),
        Some(daemon_message::Message::Notification(_)) => Err(IpcError::UnexpectedNotification),
        None => Err(IpcError::EmptyResponse),
    }
}

/// Errors from IPC communication.
#[derive(Debug, thiserror::Error)]
pub enum IpcError {
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("daemon sent unexpected notification instead of response")]
    UnexpectedNotification,
    #[error("daemon sent empty message")]
    EmptyResponse,
}
