use protobuf::v2::{EntryNodeInstruction, ExitNodeHello, ExitNodeResponse};
use tokio::sync::oneshot;
use transport::Transport;

use crate::{
	NodeRole,
	control::{handler::HandlerConfig, metrics::SharedMetrics},
};

use super::config;

pub type Channels = (
	tokio::sync::broadcast::Sender<EntryNodeInstruction>,
	tokio::sync::broadcast::Sender<ExitNodeResponse>,
);

/// Result of accepting a connection on the server.
pub struct AcceptResult<T: Transport> {
	channels: Channels,
	peer_ident: String,
	metrics: SharedMetrics,
	hello_rx: Option<oneshot::Receiver<ExitNodeHello>>,
	transport: std::sync::Arc<T>,
}

impl<T: Transport> AcceptResult<T> {
	/// Creates a new accept result.
	#[must_use]
	pub fn new(
		transport: std::sync::Arc<T>,
		channels: Channels,
		peer_ident: String,
		metrics: SharedMetrics,
	) -> Self {
		Self {
			channels,
			peer_ident,
			metrics,
			hello_rx: None,
			transport,
		}
	}

	/// Creates a new accept result with an `ExitNodeHello` receiver.
	#[must_use]
	pub fn with_exit_hello(
		transport: std::sync::Arc<T>,
		channels: Channels,
		peer_ident: String,
		metrics: SharedMetrics,
		hello_rx: oneshot::Receiver<ExitNodeHello>,
	) -> Self {
		Self {
			channels,
			peer_ident,
			metrics,
			hello_rx: Some(hello_rx),
			transport,
		}
	}

	/// Consumes the result and returns the data channels.
	#[must_use]
	pub fn channels(self) -> Channels {
		self.channels
	}

	/// Returns a reference to the peer identifier.
	#[must_use]
	pub fn client_ident(&self) -> &str {
		&self.peer_ident
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

	/// Takes the `ExitNodeHello` receiver, if available.
	///
	/// This can be used to wait for exit node identity before creating TUN
	/// interfaces.
	pub fn take_hello_rx(&mut self) -> Option<oneshot::Receiver<ExitNodeHello>> {
		self.hello_rx.take()
	}
}

/// Configuration for server with control support.
#[derive(Debug, Clone)]
pub struct ServerOptions {
	/// Handler configuration for control requests.
	pub handler_config: HandlerConfig,
	/// Shared metrics for statistics.
	pub metrics: Option<SharedMetrics>,
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
}
