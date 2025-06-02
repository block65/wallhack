use protobuf::v2::{AgentResponse, HostInstruction};

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

pub struct AcceptResult {
	channels: Channels,
	peer_ident: String,
}

impl AcceptResult {
	#[must_use]
	pub fn new(channels: Channels, peer_ident: String) -> Self {
		Self {
			channels,
			peer_ident,
		}
	}

	#[must_use]
	pub fn channels(self) -> Channels {
		self.channels
	}

	#[must_use]
	pub fn client_ident(&self) -> &str {
		&self.peer_ident
	}
}

pub trait Server {
	type Error: std::error::Error + std::fmt::Debug + Send + Sync + 'static;

	fn try_new(config: config::ServerConfig) -> Result<Self, Self::Error>
	where
		Self: Sized;

	fn stop(&self) -> Result<(), Self::Error>;

	fn accept(
		&mut self,
		role: ServerRole,
	) -> impl std::future::Future<Output = Result<Option<AcceptResult>, Self::Error>> + Send;
}
