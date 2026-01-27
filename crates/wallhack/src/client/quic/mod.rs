use std::sync::Arc;

use quinn::{IdleTimeout, VarInt, crypto::rustls::QuicClientConfig};
use tokio::time::Instant;

use crate::{
	ClientConfig,
	client::{client::ClientRole, tls_config},
	transport::{bridge, quic::QuicTransport},
};
use prost::Message;
use protobuf::v2::{AgentHello, AgentResponse, HostInstruction, TunnelMessage, tunnel_message};

use super::client::{Client, ConnectResult, ConnectionTasks};

#[derive(thiserror::Error, Debug)]
pub enum Error {
	#[error("failed to connect to server: {0}")]
	Connection(quinn::ConnectionError),

	#[error("failed to connect to server: {0}")]
	Connect(quinn::ConnectError),

	#[error("failed to read from stream: {0}")]
	StreamRead(#[from] quinn::ReadError),

	#[error("timeout waiting for stream: {0}")]
	StreamReadTimeout(tokio::time::error::Elapsed),

	#[error("failed to read from stream: {0}")]
	StreamReadToEnd(quinn::ReadToEndError),

	#[error("failed to write to stream: {0}")]
	StreamWrite(quinn::WriteError),

	#[error(transparent)]
	CryptoError(#[from] quinn::crypto::rustls::NoInitialCipherSuite),

	#[error(transparent)]
	ConnectError(#[from] quinn::ConnectError),

	#[error(transparent)]
	ConnectionError(#[from] quinn::ConnectionError),

	#[error(transparent)]
	Io(#[from] std::io::Error),

	#[error(transparent)]
	TlsConfig(#[from] tls_config::Error),
}

pub struct QuicClient {
	addr: std::net::SocketAddr,
	hostname: String,
	endpoint: quinn::Endpoint,
	agent_id: Option<String>,
}

impl Client for QuicClient {
	type Error = Error;

	fn try_new(args: ClientConfig) -> Result<Self, Error> {
		let tls_config = tls_config::client_config(args.mtls)?;

		let mut transport_config = quinn::TransportConfig::default();
		transport_config.max_idle_timeout(Some(IdleTimeout::from(VarInt::MAX)));
		transport_config.keep_alive_interval(Some(std::time::Duration::from_secs(5)));

		let mut client_config: quinn::ClientConfig =
			quinn::ClientConfig::new(Arc::new(QuicClientConfig::try_from(tls_config)?));

		client_config.transport_config(Arc::new(transport_config));

		let mut endpoint = quinn::Endpoint::client(args.bind)?;
		endpoint.set_default_client_config(client_config);

		let hostname = if let Some(host) = args.hostname {
			host
		} else {
			env!("CARGO_PKG_NAME").to_string()
		};

		Ok(Self {
			addr: args.addr,
			hostname,
			endpoint,
			agent_id: args.agent_id,
		})
	}

	async fn connect(&mut self, role: ClientRole) -> Result<ConnectResult, Self::Error> {
		tracing::debug!(
			"{:?} connecting to {} with server name {:?}",
			role,
			self.addr,
			self.hostname,
		);

		let start = Instant::now();
		let conn = self
			.endpoint
			.connect(self.addr, self.hostname.as_str())?
			.await?;

		tracing::debug!("connected after {:#?}", start.elapsed());

		let remote_addr = conn.remote_address().to_string();

		// Wrap connection in transport abstraction
		let transport = Arc::new(QuicTransport::new(conn));

		// Send AgentHello if we have an agent_id (exit nodes)
		if let Some(ref agent_id) = self.agent_id {
			tracing::debug!("Sending AgentHello with id: {}", agent_id);
			let hello = TunnelMessage {
				message: Some(tunnel_message::Message::AgentHello(AgentHello {
					agent_id: agent_id.clone(),
					version: env!("CARGO_PKG_VERSION").to_string(),
				})),
			};
			let mut buf = Vec::new();
			hello.encode(&mut buf).expect("failed to encode AgentHello");

			let mut send = transport.connection().open_uni().await?;
			send.write_all(&buf)
				.await
				.map_err(|e| std::io::Error::other(e.to_string()))?;
			let _ = send.finish(); // Ignore close errors
			tracing::debug!("AgentHello sent successfully");
		}

		let (instructions, _) = tokio::sync::broadcast::channel::<HostInstruction>(256);
		let (responses, _) = tokio::sync::broadcast::channel::<AgentResponse>(256);

		// Task 1: Incoming data handler
		let transport_data = Arc::clone(&transport);
		let instructions_tx = instructions.clone();
		let responses_tx = responses.clone();

		let incoming_handle = tokio::spawn(async move {
			if let Err(e) =
				bridge::run_incoming_data(&*transport_data, &instructions_tx, &responses_tx, None)
					.await
			{
				tracing::debug!("Incoming data handler finished: {e}");
			}
		});

		// Task 2: Outgoing handler based on role
		let outgoing_handle = match role {
			ClientRole::Host => {
				tracing::debug!("Listening for instructions to send to peer.");
				let transport_out = Arc::clone(&transport);
				let instructions_tx = instructions.clone();

				tokio::spawn(async move {
					if let Err(e) =
						bridge::run_outgoing_instructions(&*transport_out, &instructions_tx).await
					{
						tracing::debug!("Outgoing instructions handler finished: {e}");
					}
				})
			}
			ClientRole::Agent => {
				tracing::debug!("Listening for AgentResponses to send to peer");
				let transport_out = Arc::clone(&transport);
				let responses_tx = responses.clone();

				tokio::spawn(async move {
					if let Err(e) =
						bridge::run_outgoing_responses(&*transport_out, &responses_tx).await
					{
						tracing::debug!("Outgoing responses handler finished: {e}");
					}
				})
			}
		};

		let tasks = ConnectionTasks {
			incoming: incoming_handle,
			outgoing: outgoing_handle,
		};

		Ok(ConnectResult::new(
			(instructions, responses),
			remote_addr,
			tasks,
		))
	}

	fn stop(&self) -> Result<(), Self::Error> {
		self.endpoint.close(0u32.into(), b"client stopping");
		Ok(())
	}
}
