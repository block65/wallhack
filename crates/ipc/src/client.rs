//! IPC client for communicating with the wallhack daemon.

use std::{
    io,
    path::{Path, PathBuf},
    pin::Pin,
    sync::atomic::{AtomicU64, Ordering},
    task::{Context, Poll},
};

use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    net::UnixStream,
    sync::{broadcast, mpsc},
    task::JoinHandle,
};
use wallhack_wire::management::{
    DaemonMessage, DaemonNotification, ManagementRequest, ManagementResponse, daemon_message,
    management_request,
};

use crate::framing::{read_length_delimited, write_length_delimited};

/// Maximum size for management messages (4KB, matching daemon).
const MANAGEMENT_MTU: usize = 4096;

/// Default socket filename within the runtime directory.
const SOCKET_NAME: &str = "wallhackd.sock";

/// Environment variable for overriding the socket path (like `DOCKER_HOST`).
const HOST_ENV: &str = "WALLHACK_HOST";

/// Monotonically increasing request ID.
static REQUEST_ID: AtomicU64 = AtomicU64::new(1);

/// A concrete stream type covering all supported IPC transports.
pub enum IpcStream {
    Unix(UnixStream),
    #[cfg(feature = "vsock")]
    Vsock(tokio_vsock::VsockStream),
}

impl AsyncRead for IpcStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match &mut *self {
            Self::Unix(s) => Pin::new(s).poll_read(cx, buf),
            #[cfg(feature = "vsock")]
            Self::Vsock(s) => Pin::new(s).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for IpcStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        match &mut *self {
            Self::Unix(s) => Pin::new(s).poll_write(cx, buf),
            #[cfg(feature = "vsock")]
            Self::Vsock(s) => Pin::new(s).poll_write(cx, buf),
        }
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match &mut *self {
            Self::Unix(s) => Pin::new(s).poll_flush(cx),
            #[cfg(feature = "vsock")]
            Self::Vsock(s) => Pin::new(s).poll_flush(cx),
        }
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match &mut *self {
            Self::Unix(s) => Pin::new(s).poll_shutdown(cx),
            #[cfg(feature = "vsock")]
            Self::Vsock(s) => Pin::new(s).poll_shutdown(cx),
        }
    }
}

/// Parse a host string, stripping `unix://` prefix if present.
#[must_use]
pub fn resolve_host(host: &str) -> PathBuf {
    PathBuf::from(host.strip_prefix("unix://").unwrap_or(host))
}

/// Resolve the default IPC socket path (ignores `vsock://` `WALLHACK_HOST` values).
///
/// When `name` is `Some(n)`, the socket filename is `wallhackd-{n}.sock`.
/// When `name` is `None`, the default `wallhackd.sock` filename is used.
/// Named instances are typically addressed via `-H` or `WALLHACK_HOST` instead.
///
/// Checks (in order):
/// 1. `WALLHACK_HOST` environment variable (unix paths only)
/// 2. `$XDG_RUNTIME_DIR/wallhack/wallhackd[-{name}].sock`
/// 3. `/tmp/wallhack-<user>/wallhackd[-{name}].sock`
/// 4. `$HOME/.wallhack/wallhackd[-{name}].sock`
/// 5. `/tmp/wallhack-shared/wallhackd[-{name}].sock`
#[must_use]
pub fn socket_path(name: Option<&str>) -> PathBuf {
    #[allow(clippy::collapsible_if)]
    if let Ok(host) = std::env::var(HOST_ENV) {
        if !host.starts_with("vsock://") {
            return PathBuf::from(host.strip_prefix("unix://").unwrap_or(&host));
        }
    }
    let filename = match name {
        Some(n) => format!("wallhackd-{n}.sock"),
        None => SOCKET_NAME.to_string(),
    };
    let non_empty = |key| std::env::var(key).ok().filter(|v| !v.is_empty());
    if let Some(runtime_dir) = non_empty("XDG_RUNTIME_DIR") {
        Path::new(&runtime_dir).join("wallhack").join(&filename)
    } else if let Some(user) = non_empty("USER") {
        PathBuf::from(format!("/tmp/wallhack-{user}")).join(&filename)
    } else if let Some(home) = non_empty("HOME") {
        Path::new(&home).join(".wallhack").join(&filename)
    } else {
        PathBuf::from("/tmp/wallhack-shared").join(&filename)
    }
}

/// Connect to the daemon's IPC socket at a specific Unix path.
///
/// # Errors
///
/// Returns an error if the socket doesn't exist or connection is refused.
pub async fn connect_to(path: &Path) -> io::Result<IpcStream> {
    UnixStream::connect(path)
        .await
        .map(IpcStream::Unix)
        .map_err(|e| {
            io::Error::new(
                e.kind(),
                format!("cannot connect to daemon at {}: {e}", path.display()),
            )
        })
}

/// Connect to the daemon, dispatching on `WALLHACK_HOST`.
///
/// Supports:
/// - `vsock://CID:PORT` — virtio-vsock (requires `vsock` feature)
/// - `unix:///path` or bare path — Unix socket
///
/// # Errors
///
/// Returns an error if the connection fails.
pub async fn connect() -> io::Result<IpcStream> {
    #[cfg(feature = "vsock")]
    #[allow(clippy::collapsible_if)]
    if let Ok(host) = std::env::var(HOST_ENV) {
        if let Some(addr) = host.strip_prefix("vsock://") {
            return connect_vsock_str(addr).await;
        }
    }
    connect_to(&socket_path(None)).await
}

#[cfg(feature = "vsock")]
async fn connect_vsock_str(addr: &str) -> io::Result<IpcStream> {
    let (cid_str, port_str) = addr.split_once(':').ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("invalid vsock address '{addr}': expected CID:PORT"),
        )
    })?;
    let cid: u32 = cid_str.parse().map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("invalid vsock CID '{cid_str}'"),
        )
    })?;
    let port: u32 = port_str.parse().map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("invalid vsock port '{port_str}'"),
        )
    })?;
    tokio_vsock::VsockStream::connect(tokio_vsock::VsockAddr::new(cid, port))
        .await
        .map(IpcStream::Vsock)
        .map_err(|e| {
            io::Error::new(
                e.kind(),
                format!("cannot connect to daemon via vsock {cid}:{port}: {e}"),
            )
        })
}

/// Send a management request and read the response.
///
/// # Errors
///
/// Returns an error on I/O or framing failure.
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

    for _ in 0..128 {
        let daemon_msg: DaemonMessage = read_length_delimited(stream, MANAGEMENT_MTU).await?;
        match daemon_msg.message {
            Some(daemon_message::Message::Response(resp)) => return Ok(resp),
            Some(daemon_message::Message::Notification(_)) => {}
            None => return Err(IpcError::EmptyResponse),
        }
    }
    Err(IpcError::TooManyNotifications)
}

// ── Long-lived connection with notification support ──────────────────

/// A long-lived IPC connection that demuxes responses and notifications.
pub struct IpcConnection {
    write_tx: mpsc::Sender<ManagementRequest>,
    response_rx: mpsc::Receiver<ManagementResponse>,
    notifications_tx: broadcast::Sender<DaemonNotification>,
    reader_task: JoinHandle<()>,
    writer_task: JoinHandle<()>,
}

impl IpcConnection {
    /// Create a new `IpcConnection` over the given stream.
    pub fn new(stream: impl AsyncRead + AsyncWrite + Unpin + Send + 'static) -> Self {
        let (reader, writer) = tokio::io::split(stream);

        let (write_tx, write_rx) = mpsc::channel::<ManagementRequest>(16);
        let (response_tx, response_rx) = mpsc::channel::<ManagementResponse>(16);
        let (notifications_tx, _) = broadcast::channel::<DaemonNotification>(64);

        let writer_task = tokio::spawn(Self::writer_task(writer, write_rx));
        let reader_task = {
            let notifications_tx = notifications_tx.clone();
            tokio::spawn(Self::reader_task(reader, response_tx, notifications_tx))
        };

        Self {
            write_tx,
            response_rx,
            notifications_tx,
            reader_task,
            writer_task,
        }
    }

    /// Send a request and await the response.
    ///
    /// # Errors
    ///
    /// Returns an error if the connection is closed.
    pub async fn request(
        &mut self,
        request: management_request::Request,
    ) -> Result<ManagementResponse, IpcError> {
        let request_id = REQUEST_ID.fetch_add(1, Ordering::Relaxed);
        let msg = ManagementRequest {
            request_id,
            request: Some(request),
        };
        self.write_tx
            .send(msg)
            .await
            .map_err(|_| IpcError::ConnectionClosed)?;
        self.response_rx
            .recv()
            .await
            .ok_or(IpcError::ConnectionClosed)
    }

    /// Subscribe to daemon notifications.
    #[must_use]
    pub fn subscribe_notifications(&self) -> broadcast::Receiver<DaemonNotification> {
        self.notifications_tx.subscribe()
    }

    async fn writer_task(
        mut writer: impl AsyncWrite + Unpin,
        mut rx: mpsc::Receiver<ManagementRequest>,
    ) {
        while let Some(msg) = rx.recv().await {
            if let Err(e) = write_length_delimited(&mut writer, &msg).await {
                tracing::trace!(error = %e, "IPC writer ended");
                break;
            }
        }
    }

    async fn reader_task(
        mut reader: impl AsyncRead + Unpin,
        response_tx: mpsc::Sender<ManagementResponse>,
        notifications_tx: broadcast::Sender<DaemonNotification>,
    ) {
        loop {
            let daemon_msg: DaemonMessage =
                match read_length_delimited(&mut reader, MANAGEMENT_MTU).await {
                    Ok(msg) => msg,
                    Err(e) => {
                        tracing::trace!(error = %e, "IPC reader ended");
                        break;
                    }
                };
            #[allow(clippy::collapsible_match)]
            match daemon_msg.message {
                Some(daemon_message::Message::Response(resp)) => {
                    if response_tx.send(resp).await.is_err() {
                        break;
                    }
                }
                Some(daemon_message::Message::Notification(notif)) => {
                    let _ = notifications_tx.send(notif);
                }
                None => {}
            }
        }
    }
}

impl Drop for IpcConnection {
    fn drop(&mut self) {
        self.reader_task.abort();
        self.writer_task.abort();
    }
}

/// Errors from IPC communication.
#[derive(Debug, thiserror::Error)]
pub enum IpcError {
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("daemon sent empty message")]
    EmptyResponse,
    #[error("too many notifications before response")]
    TooManyNotifications,
    #[error("connection closed")]
    ConnectionClosed,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_host_strips_unix_scheme() {
        assert_eq!(
            resolve_host("unix:///var/run/wallhack/wallhackd.sock"),
            PathBuf::from("/var/run/wallhack/wallhackd.sock")
        );
    }

    #[test]
    fn resolve_host_bare_path() {
        assert_eq!(resolve_host("/tmp/my.sock"), PathBuf::from("/tmp/my.sock"));
    }
}
