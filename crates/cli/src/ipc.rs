//! IPC client for communicating with the wallhack daemon.

use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

use tokio::{
    io::{AsyncRead, AsyncWrite},
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
    } else if let Ok(user) = std::env::var("USER") {
        PathBuf::from(format!("/tmp/wallhack-{user}")).join(SOCKET_NAME)
    } else if let Ok(home) = std::env::var("HOME") {
        std::path::Path::new(&home)
            .join(".wallhack")
            .join(SOCKET_NAME)
    } else {
        PathBuf::from("/tmp/wallhack-shared").join(SOCKET_NAME)
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
/// Skips any interleaved notifications (discards them). For long-lived
/// connections that need notifications, use [`IpcConnection`] instead.
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

    // Loop until we get a response, skipping up to 128 interleaved notifications.
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

// ── Long-lived connection with notification support ─────────────────

/// A long-lived IPC connection that demuxes responses and notifications.
///
/// The reader task runs in the background, dispatching responses to callers
/// and broadcasting notifications to subscribers.
pub struct IpcConnection {
    write_tx: mpsc::Sender<ManagementRequest>,
    response_rx: mpsc::Receiver<ManagementResponse>,
    notifications_tx: broadcast::Sender<DaemonNotification>,
    reader_task: JoinHandle<()>,
    writer_task: JoinHandle<()>,
}

impl IpcConnection {
    /// Create a new `IpcConnection` over the given stream.
    ///
    /// Spawns background reader and writer tasks.
    pub fn new(stream: impl AsyncRead + AsyncWrite + Unpin + Send + 'static) -> Self {
        let (reader, writer) = tokio::io::split(stream);

        let (write_tx, write_rx) = mpsc::channel::<ManagementRequest>(16);
        let (response_tx, response_rx) = mpsc::channel::<ManagementResponse>(16);
        let (notifications_tx, _) = broadcast::channel::<DaemonNotification>(64);

        let writer_task = tokio::spawn(Self::writer_task(writer, write_rx));

        let reader_notifications_tx = notifications_tx.clone();
        let reader_task = tokio::spawn(Self::reader_task(
            reader,
            response_tx,
            reader_notifications_tx,
        ));

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

            // Guard form would require cloning resp (moved into send)
            #[allow(clippy::collapsible_match)]
            match daemon_msg.message {
                Some(daemon_message::Message::Response(resp)) => {
                    if response_tx.send(resp).await.is_err() {
                        break;
                    }
                }
                Some(daemon_message::Message::Notification(notif)) => {
                    // Best-effort: if no subscribers, the send fails silently.
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
