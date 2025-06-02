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

pub struct ConnectResult {
	channels: Channels,
	peer_ident: String,
}

impl ConnectResult {
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
