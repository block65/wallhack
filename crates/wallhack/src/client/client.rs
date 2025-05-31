use protobuf::v2::{AgentResponse, HostInstruction};

use super::config;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClientRole {
	Host,
	Agent,
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
	) -> impl std::future::Future<
		Output = Result<
			(
				tokio::sync::broadcast::Sender<HostInstruction>,
				tokio::sync::broadcast::Sender<AgentResponse>,
			),
			Self::Error,
		>,
	> + Send;
}
