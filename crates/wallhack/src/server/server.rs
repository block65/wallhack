use protobuf::{
	control_v2::ControlMessage,
	v2::{EntryNodeInstruction, ExitNodeHello, ExitNodeResponse},
};
use tokio::sync::mpsc;
use transport::Transport;

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
	/// The already-received `ExitNodeHello` (extracted from the control stream).
	exit_hello: Option<ExitNodeHello>,
	transport: std::sync::Arc<T>,
	/// Channel for injecting messages into the control stream.
	control_tx: mpsc::Sender<ControlMessage>,
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
			exit_hello: None,
			transport,
			control_tx,
		}
	}

	/// Creates a new accept result with an already-received `ExitNodeHello`.
	#[must_use]
	pub fn with_exit_hello(
		transport: std::sync::Arc<T>,
		channels: Channels,
		peer_addr: String,
		metrics: SharedMetrics,
		exit_hello: Option<ExitNodeHello>,
		control_tx: mpsc::Sender<ControlMessage>,
	) -> Self {
		Self {
			channels,
			peer_addr,
			metrics,
			exit_hello,
			transport,
			control_tx,
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

	/// Returns a reference to the `ExitNodeHello` data, if available.
	#[must_use]
	pub fn exit_hello(&self) -> Option<&ExitNodeHello> {
		self.exit_hello.as_ref()
	}

	/// Takes the `ExitNodeHello` data, if available.
	pub fn take_exit_hello(&mut self) -> Option<ExitNodeHello> {
		self.exit_hello.take()
	}

	/// Returns a clone of the control message sender.
	#[must_use]
	pub fn control_tx(&self) -> &mpsc::Sender<ControlMessage> {
		&self.control_tx
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
	) -> impl std::future::Future<Output = Result<Option<AcceptResult<Self::Transport>>, Self::Error>>
	+ Send;

	/// Returns the certificate fingerprint for this server.
	fn fingerprint(&self) -> &str;

	/// Returns the configured PSK, if any.
	fn psk(&self) -> Option<&str>;

	/// Returns the local address the server is actually bound to.
	fn local_addr(&self) -> std::io::Result<std::net::SocketAddr>;
}
