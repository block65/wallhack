use std::sync::Arc;

use tokio::{
    sync::{mpsc, oneshot},
    task::JoinHandle,
};

use wallhack_transport::ErasedTransport;
use wallhack_wire::{control::ControlMessage, data::Handshake};

use crate::{NodeRole, server::server::DataChannels};

use super::config;

/// Handles to the background tasks that service the connection.
/// When these tasks complete, the connection is dead and reconnection should be attempted.
pub struct ConnectionTasks {
    /// Task handling incoming data from the transport.
    pub incoming: JoinHandle<()>,
    /// Task running the persistent control bidi stream.
    pub control: JoinHandle<()>,
}

impl ConnectionTasks {
    /// Wait for any connection task to complete, indicating the connection is dead.
    pub async fn wait_for_disconnect(&mut self) {
        tokio::select! {
            _ = &mut self.incoming => {
                tracing::debug!("Incoming task completed - connection dead");
            }
            _ = &mut self.control => {
                tracing::debug!("Control task completed - connection dead");
            }
        }
    }
}

pub struct ConnectResult<T: wallhack_transport::Transport + ?Sized> {
    channels: DataChannels,
    peer_addr: String,
    tasks: ConnectionTasks,
    transport: std::sync::Arc<T>,
    /// Channel for injecting messages into the control stream.
    control_tx: mpsc::Sender<ControlMessage>,
    /// Receiver for the server's `Handshake` (delivered via the control loop).
    peer_handshake_rx: Option<oneshot::Receiver<Handshake>>,
}

impl<T: wallhack_transport::Transport + ?Sized> ConnectResult<T> {
    #[must_use]
    pub fn new(
        transport: std::sync::Arc<T>,
        channels: DataChannels,
        peer_addr: String,
        tasks: ConnectionTasks,
        control_tx: mpsc::Sender<ControlMessage>,
        peer_handshake_rx: Option<oneshot::Receiver<Handshake>>,
    ) -> Self {
        Self {
            channels,
            peer_addr,
            tasks,
            transport,
            control_tx,
            peer_handshake_rx,
        }
    }

    #[must_use]
    pub fn into_parts(self) -> (DataChannels, ConnectionTasks, mpsc::Sender<ControlMessage>) {
        (self.channels, self.tasks, self.control_tx)
    }

    #[must_use]
    pub fn peer_addr(&self) -> &str {
        &self.peer_addr
    }

    #[must_use]
    pub fn transport(&self) -> std::sync::Arc<T> {
        std::sync::Arc::clone(&self.transport)
    }

    #[must_use]
    pub fn control_tx(&self) -> &mpsc::Sender<ControlMessage> {
        &self.control_tx
    }

    /// Takes the receiver for the server's `Handshake`.
    ///
    /// Returns `None` if already taken or not provided.
    pub fn take_peer_handshake_rx(&mut self) -> Option<oneshot::Receiver<Handshake>> {
        self.peer_handshake_rx.take()
    }
}

/// Type-erased result of a successful connection.
///
/// Produced by [`ConnectResult::erase()`]. All transport-specific types have
/// been erased to `Arc<dyn ErasedTransport>`, so downstream code is
/// monomorphized only once regardless of the concrete transport.
pub struct ErasedConnectResult {
    pub transport: Arc<dyn ErasedTransport>,
    pub channels: DataChannels,
    pub tasks: ConnectionTasks,
    pub control_tx: mpsc::Sender<ControlMessage>,
    pub peer_handshake_rx: Option<oneshot::Receiver<Handshake>>,
    pub peer_addr: String,
}

impl<T> ConnectResult<T>
where
    T: wallhack_transport::Transport + 'static,
    T::SendStream: 'static,
    T::RecvStream: 'static,
    T::BiStream: 'static,
{
    /// Erase the concrete transport type.
    ///
    /// This is a **sync** operation — no async state machine is created —
    /// so calling it inside a closure before an `async move` block avoids
    /// capturing the generic `ConnectResult<T>` in the future's state machine.
    #[must_use]
    pub fn erase(mut self) -> ErasedConnectResult {
        ErasedConnectResult {
            peer_handshake_rx: self.peer_handshake_rx.take(),
            transport: self.transport as Arc<dyn ErasedTransport>,
            peer_addr: self.peer_addr,
            channels: self.channels,
            tasks: self.tasks,
            control_tx: self.control_tx,
        }
    }
}

pub trait Client {
    type Error: std::error::Error + std::fmt::Debug + Send + Sync + 'static;
    type Transport: wallhack_transport::Transport;

    fn try_new(config: config::ClientConfig) -> Result<Self, Self::Error>
    where
        Self: Sized;

    fn stop(&self) -> Result<(), Self::Error>;

    fn connect(
        &mut self,
        role: NodeRole,
    ) -> impl std::future::Future<Output = Result<ConnectResult<Self::Transport>, Self::Error>> + Send;
}
