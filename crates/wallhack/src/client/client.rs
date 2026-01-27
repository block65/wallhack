use tokio::task::JoinHandle;

use protobuf::v2::{AgentResponse, HostInstruction};

use super::config;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClientRole {
	Host,
	Agent,
}

pub type Channels = (
	tokio::sync::broadcast::Sender<HostInstruction>,
	tokio::sync::broadcast::Sender<AgentResponse>,
);

/// Handles to the background tasks that service the connection.
/// When these tasks complete, the connection is dead and reconnection should be attempted.
pub struct ConnectionTasks {
	/// Task handling incoming data from the transport.
	pub incoming: JoinHandle<()>,
	/// Task handling outgoing data to the transport.
	pub outgoing: JoinHandle<()>,
}

impl ConnectionTasks {
	/// Wait for any connection task to complete, indicating the connection is dead.
	pub async fn wait_for_disconnect(&mut self) {
		tokio::select! {
			_ = &mut self.incoming => {
				tracing::debug!("Incoming task completed - connection dead");
			}
			_ = &mut self.outgoing => {
				tracing::debug!("Outgoing task completed - connection dead");
			}
		}
	}
}

pub struct ConnectResult {
	channels: Channels,
	peer_ident: String,
	tasks: ConnectionTasks,
}

impl ConnectResult {
	#[must_use]
	pub fn new(channels: Channels, peer_ident: String, tasks: ConnectionTasks) -> Self {
		Self {
			channels,
			peer_ident,
			tasks,
		}
	}

	#[must_use]
	pub fn channels(&self) -> &Channels {
		&self.channels
	}

	#[must_use]
	pub fn into_parts(self) -> (Channels, ConnectionTasks) {
		(self.channels, self.tasks)
	}

	#[must_use]
	pub fn client_ident(&self) -> &str {
		&self.peer_ident
	}
}

pub trait Client {
	type Error: std::error::Error + std::fmt::Debug + Send + Sync + 'static;

	fn try_new(config: config::ClientConfig) -> Result<Self, Self::Error>
	where
		Self: Sized;

	fn stop(&self) -> Result<(), Self::Error>;

	fn connect(
		&mut self,
		role: ClientRole,
	) -> impl std::future::Future<Output = Result<ConnectResult, Self::Error>> + Send;
}
