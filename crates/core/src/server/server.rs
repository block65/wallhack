use std::sync::Arc;

use tokio::sync::mpsc;
use wallhack_transport::{ErasedTransport, Transport};
use wallhack_wire::{
    control::ControlMessage,
    data::{EntryNodeInstruction, ExitNodeResponse, Handshake},
};

use crate::{
    NodeRole,
    control::{
        handler::HandlerConfig, metrics::SharedMetrics, peers::SharedRegistry,
        routes::SharedRouteTable,
    },
};

use super::config;

/// Capacity for data-plane mpsc channels (instructions and responses).
pub const DATA_CHANNEL_CAPACITY: usize = 1024;

/// Data-plane channels for a connection.
///
/// Carries both halves of the instructions (entry→exit) and responses
/// (exit→entry) mpsc channel pairs.
pub struct DataChannels {
    /// Send instructions toward the exit peer.
    pub instructions_tx: mpsc::Sender<EntryNodeInstruction>,
    /// Receive instructions (consumed by the task that writes them to the transport).
    pub instructions_rx: mpsc::Receiver<EntryNodeInstruction>,
    /// Send responses toward the entry peer.
    pub responses_tx: mpsc::Sender<ExitNodeResponse>,
    /// Receive responses (consumed by the entity that processes exit responses).
    pub responses_rx: mpsc::Receiver<ExitNodeResponse>,
}

impl DataChannels {
    #[must_use]
    pub fn new() -> Self {
        let (instructions_tx, instructions_rx) = mpsc::channel(DATA_CHANNEL_CAPACITY);
        let (responses_tx, responses_rx) = mpsc::channel(DATA_CHANNEL_CAPACITY);
        Self {
            instructions_tx,
            instructions_rx,
            responses_tx,
            responses_rx,
        }
    }
}

impl Default for DataChannels {
    fn default() -> Self {
        Self::new()
    }
}

/// Result of accepting a connection on the server.
pub struct AcceptResult<T: Transport> {
    channels: DataChannels,
    peer_addr: String,
    metrics: SharedMetrics,
    /// The already-received peer `Handshake` (extracted from the control stream).
    peer_handshake: Option<Handshake>,
    transport: Arc<T>,
    /// Channel for injecting messages into the control stream.
    control_tx: mpsc::Sender<ControlMessage>,
    /// Receiver for pong-derived latency measurements (milliseconds) from the
    /// control loop. Used by one-shot ping callers.
    latency_rx: Option<mpsc::Receiver<f64>>,
    /// TLS channel binding bytes for PSK proof verification.
    channel_binding: Option<[u8; crate::psk::CHANNEL_BINDING_LEN]>,
}

/// Object-safe version of [`AcceptResult`] for sync type-erasure.
pub struct ErasedAcceptResult {
    pub channels: DataChannels,
    pub peer_addr: String,
    pub metrics: SharedMetrics,
    pub peer_handshake: Option<Handshake>,
    pub transport: Arc<dyn ErasedTransport>,
    pub control_tx: mpsc::Sender<ControlMessage>,
    pub latency_rx: Option<mpsc::Receiver<f64>>,
    pub channel_binding: Option<[u8; crate::psk::CHANNEL_BINDING_LEN]>,
}

impl<T: Transport + 'static> AcceptResult<T>
where
    T::SendStream: 'static,
    T::RecvStream: 'static,
    T::BiStream: Send + 'static,
{
    /// Sync type-erasure: extracts non-generic parts into [`ErasedAcceptResult`].
    #[must_use]
    pub fn erase(mut self) -> ErasedAcceptResult {
        ErasedAcceptResult {
            channels: self.channels,
            peer_addr: self.peer_addr,
            metrics: self.metrics,
            peer_handshake: self.peer_handshake.take(),
            transport: self.transport as Arc<dyn ErasedTransport>,
            control_tx: self.control_tx,
            latency_rx: self.latency_rx.take(),
            channel_binding: self.channel_binding,
        }
    }
}

impl<T: Transport> AcceptResult<T> {
    /// Creates a new accept result with an already-received peer `Handshake`
    /// and a latency receiver for pong-derived RTT measurements.
    #[must_use]
    #[allow(clippy::too_many_arguments)] // accept result construction; will be simplified when builder pattern is adopted
    pub fn with_handshake(
        transport: Arc<T>,
        channels: DataChannels,
        peer_addr: String,
        metrics: SharedMetrics,
        peer_handshake: Option<Handshake>,
        control_tx: mpsc::Sender<ControlMessage>,
        latency_rx: mpsc::Receiver<f64>,
        channel_binding: Option<[u8; crate::psk::CHANNEL_BINDING_LEN]>,
    ) -> Self {
        Self {
            channels,
            peer_addr,
            metrics,
            peer_handshake,
            transport,
            control_tx,
            latency_rx: Some(latency_rx),
            channel_binding,
        }
    }

    /// Consumes the result and returns the data channels and the control sender.
    ///
    /// The caller **must** hold onto the returned `mpsc::Sender` for as long
    /// as the connection should stay alive — dropping it closes the control
    /// stream.
    #[must_use]
    pub fn into_channels(self) -> (DataChannels, mpsc::Sender<ControlMessage>) {
        (self.channels, self.control_tx)
    }

    /// Returns a reference to the peer identifier.
    #[must_use]
    pub fn peer_addr(&self) -> &str {
        &self.peer_addr
    }

    /// Returns a clone of the shared metrics.
    #[must_use]
    pub fn metrics(&self) -> SharedMetrics {
        self.metrics.clone()
    }

    /// Returns the transport for this connection.
    #[must_use]
    pub fn transport(&self) -> std::sync::Arc<T> {
        std::sync::Arc::clone(&self.transport)
    }

    /// Returns a reference to the peer `Handshake`, if available.
    #[must_use]
    pub fn peer_handshake(&self) -> Option<&Handshake> {
        self.peer_handshake.as_ref()
    }

    /// Takes the peer `Handshake`, if available.
    pub fn take_peer_handshake(&mut self) -> Option<Handshake> {
        self.peer_handshake.take()
    }

    /// Returns a clone of the control message sender.
    #[must_use]
    pub fn control_tx(&self) -> &mpsc::Sender<ControlMessage> {
        &self.control_tx
    }

    /// Takes the latency receiver for pong-derived RTT measurements.
    pub fn take_latency_rx(&mut self) -> Option<mpsc::Receiver<f64>> {
        self.latency_rx.take()
    }

    /// Returns the TLS channel binding bytes for this connection.
    #[must_use]
    pub fn channel_binding(&self) -> Option<&[u8; crate::psk::CHANNEL_BINDING_LEN]> {
        self.channel_binding.as_ref()
    }
}

/// Configuration for server with control support.
#[derive(Debug, Clone)]
pub struct ServerOptions {
    /// Handler configuration for control requests.
    pub handler_config: HandlerConfig,
    /// Shared metrics for statistics.
    pub metrics: Option<SharedMetrics>,
    /// Shared peer registry.
    pub peers: Option<SharedRegistry>,
    /// Shared route table.
    pub routes: Option<SharedRouteTable>,
    /// The server's own handshake, sent to peers during the exchange.
    pub local_handshake: Option<Handshake>,
}

pub trait Server {
    type Error: std::error::Error + std::fmt::Debug + Send + Sync + 'static;
    type Transport: Transport;

    /// Creates a new server with the given configuration and options.
    fn try_new(config: config::ServerConfig, options: ServerOptions) -> Result<Self, Self::Error>
    where
        Self: Sized;

    /// Stops the server.
    fn stop(&self) -> Result<(), Self::Error>;

    /// Accepts a new connection.
    fn accept(
        &mut self,
        role: NodeRole,
    ) -> impl std::future::Future<
        Output = Result<Option<AcceptResult<Self::Transport>>, Self::Error>,
    > + Send;

    /// Returns the human-readable protocol name (e.g. "QUIC", "WebSocket").
    fn protocol_name(&self) -> &'static str;

    /// Returns the certificate fingerprint for this server.
    fn fingerprint(&self) -> &str;

    /// Returns the configured PSK, if any.
    fn psk(&self) -> Option<&str>;

    /// Returns the local address the server is actually bound to.
    fn local_addr(&self) -> std::io::Result<std::net::SocketAddr>;
}
