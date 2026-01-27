use protobuf::v2::{AgentHello, AgentResponse, HostInstruction};
use tokio::sync::oneshot;

use crate::control::{handler::HandlerConfig, metrics::SharedMetrics};

use super::config;

#[derive(Clone, Copy, Debug)]
pub enum ServerRole {
	Agent,
	Host,
}

pub type Channels = (
	tokio::sync::broadcast::Sender<HostInstruction>,
	tokio::sync::broadcast::Sender<AgentResponse>,
);

/// Result of accepting a connection on the server.
pub struct AcceptResult {
	channels: Channels,
	peer_ident: String,
	metrics: SharedMetrics,
	agent_hello_rx: Option<oneshot::Receiver<AgentHello>>,
}

impl AcceptResult {
	/// Creates a new accept result.
	#[must_use]
	pub fn new(channels: Channels, peer_ident: String, metrics: SharedMetrics) -> Self {
		Self {
			channels,
			peer_ident,
			metrics,
			agent_hello_rx: None,
		}
	}

	/// Creates a new accept result with an AgentHello receiver.
	#[must_use]
	pub fn with_agent_hello(
		channels: Channels,
		peer_ident: String,
		metrics: SharedMetrics,
		agent_hello_rx: oneshot::Receiver<AgentHello>,
	) -> Self {
		Self {
			channels,
			peer_ident,
			metrics,
			agent_hello_rx: Some(agent_hello_rx),
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

	/// Takes the AgentHello receiver, if available.
	///
	/// This can be used to wait for agent identity before creating TUN interfaces.
	pub fn take_agent_hello_rx(&mut self) -> Option<oneshot::Receiver<AgentHello>> {
		self.agent_hello_rx.take()
	}
}

/// Configuration for server with control support.
#[derive(Debug, Clone, Default)]
pub struct ServerOptions {
	/// Handler configuration for control requests.
	pub handler_config: HandlerConfig,
	/// Shared metrics for statistics.
	pub metrics: Option<SharedMetrics>,
}

pub trait Server {
	type Error: std::error::Error + std::fmt::Debug + Send + Sync + 'static;

	/// Creates a new server with the given configuration and options.
	fn try_new(config: config::ServerConfig, options: ServerOptions) -> Result<Self, Self::Error>
	where
		Self: Sized;

	/// Stops the server.
	fn stop(&self) -> Result<(), Self::Error>;

	/// Accepts a new connection.
	fn accept(
		&mut self,
		role: ServerRole,
	) -> impl std::future::Future<Output = Result<Option<AcceptResult>, Self::Error>> + Send;
}
