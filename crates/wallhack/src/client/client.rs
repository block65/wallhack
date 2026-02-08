use tokio::{sync::mpsc, task::JoinHandle};

use protobuf::{
	control_v2::ControlMessage,
	v2::{EntryNodeInstruction, ExitNodeResponse},
};

use crate::NodeRole;

use super::config;

pub type Channels = (
	tokio::sync::broadcast::Sender<EntryNodeInstruction>,
	tokio::sync::broadcast::Sender<ExitNodeResponse>,
);

/// Handles to the background tasks that service the connection.
/// When these tasks complete, the connection is dead and reconnection should be attempted.
pub struct ConnectionTasks {
	/// Task handling incoming data from the transport.
	pub incoming: JoinHandle<()>,
	/// Task handling outgoing data to the transport.
	pub outgoing: JoinHandle<()>,
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
			_ = &mut self.outgoing => {
				tracing::debug!("Outgoing task completed - connection dead");
			}
			_ = &mut self.control => {
				tracing::debug!("Control task completed - connection dead");
			}
		}
	}
}

pub struct ConnectResult<T: transport::Transport + ?Sized> {
	channels: Channels,
	peer_ident: String,
	tasks: ConnectionTasks,
	transport: std::sync::Arc<T>,
	/// Channel for injecting messages into the control stream.
	control_tx: mpsc::Sender<ControlMessage>,
}

impl<T: transport::Transport + ?Sized> ConnectResult<T> {
	#[must_use]
	pub fn new(
		transport: std::sync::Arc<T>,
		channels: Channels,
		peer_ident: String,
		tasks: ConnectionTasks,
		control_tx: mpsc::Sender<ControlMessage>,
	) -> Self {
		Self {
			channels,
			peer_ident,
			tasks,
			transport,
			control_tx,
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

	#[must_use]
	pub fn transport(&self) -> std::sync::Arc<T> {
		std::sync::Arc::clone(&self.transport)
	}

	#[must_use]
	pub fn control_tx(&self) -> &mpsc::Sender<ControlMessage> {
		&self.control_tx
	}
}

pub trait Client {
	type Error: std::error::Error + std::fmt::Debug + Send + Sync + 'static;
	type Transport: transport::Transport;

	fn try_new(config: config::ClientConfig) -> Result<Self, Self::Error>
	where
		Self: Sized;

	fn stop(&self) -> Result<(), Self::Error>;

	fn connect(
		&mut self,
		role: NodeRole,
	) -> impl std::future::Future<Output = Result<ConnectResult<Self::Transport>, Self::Error>> + Send;
}
