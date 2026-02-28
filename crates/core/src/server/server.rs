use tokio::sync::mpsc;
use wallhack_transport::Transport;
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

pub type Channels = (
    tokio::sync::broadcast::Sender<EntryNodeInstruction>,
    tokio::sync::broadcast::Sender<ExitNodeResponse>,
);

/// Result of accepting a connection on the server.
pub struct AcceptResult<T: Transport> {
    channels: Channels,
    peer_addr: String,
    metrics: SharedMetrics,
    /// The already-received peer `Handshake` (extracted from the control stream).
    peer_handshake: Option<Handshake>,
    transport: std::sync::Arc<T>,
    /// Channel for injecting messages into the control stream.
    control_tx: mpsc::Sender<ControlMessage>,
    /// Receiver for pong-derived latency measurements (milliseconds) from the
    /// control loop. Used by one-shot ping callers.
    latency_rx: Option<mpsc::Receiver<f64>>,
    /// TLS channel binding bytes for PSK proof verification.
    channel_binding: Option<[u8; crate::psk::CHANNEL_BINDING_LEN]>,
}

impl<T: Transport> AcceptResult<T> {
    /// Creates a new accept result.
    #[must_use]
    pub fn new(
        transport: std::sync::Arc<T>,
        channels: Channels,
        peer_addr: String,
        metrics: SharedMetrics,
        control_tx: mpsc::Sender<ControlMessage>,
    ) -> Self {
        Self {
            channels,
            peer_addr,
            metrics,
            peer_handshake: None,
            transport,
            control_tx,
            latency_rx: None,
            channel_binding: None,
        }
    }

    /// Creates a new accept result with an already-received peer `Handshake`
    /// and a latency receiver for pong-derived RTT measurements.
    #[must_use]
    #[allow(clippy::too_many_arguments)] // accept result construction; will be simplified when builder pattern is adopted
    pub fn with_handshake(
        transport: std::sync::Arc<T>,
        channels: Channels,
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
    pub fn channels(self) -> (Channels, mpsc::Sender<ControlMessage>) {
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
