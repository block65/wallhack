use std::sync::Arc;

use quinn::{IdleTimeout, VarInt, crypto::rustls::QuicClientConfig};
use tokio::time::Instant;

use crate::{
	ClientConfig,
	client::{client::ClientRole, tls_config},
};
use prost::Message;
use protobuf::v2::{AgentResponse, HostInstruction, TunnelMessage, tunnel_message};
use tokio::sync::broadcast::error::RecvError;

use super::client::Client;

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
}

impl Client for QuicClient {
	type Error = Error;

	fn try_new(args: ClientConfig) -> Result<Self, Error> {
		let tls_config = tls_config::client_config(args.mtls)?;

		let mut transport_config = quinn::TransportConfig::default();
		transport_config.max_idle_timeout(Some(IdleTimeout::from(VarInt::MAX))); // Never timeout
		// transport_config.enable_segmentation_offload(false);

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
		})
	}

	async fn connect(
		&mut self,
		role: ClientRole,
	) -> Result<
		(
			tokio::sync::broadcast::Sender<HostInstruction>,
			tokio::sync::broadcast::Sender<AgentResponse>,
		),
		Self::Error,
	> {
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

		let (instructions, _) = tokio::sync::broadcast::channel::<HostInstruction>(32);
		let (responses, _) = tokio::sync::broadcast::channel::<AgentResponse>(32);

		let connection0 = conn.clone();
		let instructions0 = instructions.clone();
		let responses0 = responses.clone();

		tokio::spawn(async move {
			let mtu = 2000;
			loop {
				tracing::trace!("Waiting for next uni stream from peer");

				let mut stream = match connection0.accept_uni().await {
					Ok(s) => s,
					Err(e) => {
						tracing::error!("Failed to accept uni stream: {e}. Closing task.");
						break; // Connection issue, exit task
					}
				};
				match stream.read_to_end(mtu).await {
					Ok(data) => {
						if data.is_empty() {
							tracing::warn!("stream closed by peer with 0 bytes.");
							continue;
						}
						let msg = match TunnelMessage::decode(prost::bytes::Bytes::from(data)) {
							Ok(m) => m,
							Err(e) => {
								tracing::error!("Failed to decode TunnelMessage: {e}");
								continue;
							}
						};
						match msg.message {
							Some(tunnel_message::Message::AgentResponse(resp)) => {
								tracing::trace!("Received AgentResponse {resp}");
								if responses0.send(resp).is_err() {
									tracing::warn!(
										"no app listener for responses. Channel might be closed. Correct role?"
									);
									break;
								}
								tracing::trace!(
									"Response sent to {} receivers",
									responses0.receiver_count()
								);
							}
							Some(tunnel_message::Message::HostInstruction(instr)) => {
								tracing::trace!("Received HostInstruction {:?}", instr.instruction);
								if instructions0.send(instr).is_err() {
									tracing::warn!(
										"no app listener for instructions. Channel might be closed. Correct role?"
									);
									break;
								}
								tracing::trace!(
									"Instruction sent to {} receivers",
									instructions0.receiver_count()
								);
							}
							_ => {
								tracing::warn!(
									"Received TunnelMessage with unexpected type: {:?}",
									msg.message
								);
							}
						}
					}
					Err(e) => {
						tracing::error!("Failed to read from stream: {e}");
						match e {
							quinn::ReadToEndError::Read(quinn::ReadError::ConnectionLost(_)) => {
								break;
							}
							quinn::ReadToEndError::Read(quinn::ReadError::Reset(_)) => break,
							quinn::ReadToEndError::TooLong => {
								tracing::error!("Stream data too long");
							}
							quinn::ReadToEndError::Read(e) => {
								tracing::error!("Stream read error {e}");
							}
						}
					}
				}
			}
			tracing::debug!("Receiver task finished.");
		});

		let connection1 = conn.clone();
		let instructions1 = instructions.clone();
		let responses1 = responses.clone();

		tokio::spawn(async move {
			let mtu = 2000;
			let mut buf = Vec::with_capacity(mtu);

			if role == ClientRole::Host {
				let mut app_instr_rx = instructions1.subscribe();
				tracing::debug!("Listening for instructions to send to peer.");

				loop {
					match app_instr_rx.recv().await {
						Ok(instruction) => {
							tracing::trace!("received instruction {}", instruction);

							let tunnel_msg = TunnelMessage {
								message: Some(tunnel_message::Message::HostInstruction(
									instruction,
								)),
							};

							tracing::trace!("Opening peer uni stream");
							let mut stream = match connection1.open_uni().await {
								Ok(s) => s,
								Err(e) => {
									tracing::error!(
										"Failed to open uni stream for instruction: {e}"
									);
									break;
								}
							};

							buf.clear();
							if let Err(e) = tunnel_msg.encode(&mut buf) {
								tracing::error!("Failed to encode Instruction: {e}");
								continue;
							}

							tracing::trace!("Sending instruction to peer");
							if let Err(e) = stream.write_all(&buf).await {
								tracing::error!("Failed to send instruction: {e}");
								continue; // Or break depending on error nature
							}
							if let Err(e) = stream.finish() {
								tracing::warn!(
									"Failed to finish uni stream for instruction: {}",
									e
								);
							}
						}
						Err(RecvError::Closed) => {
							tracing::warn!("Application instruction channel closed. Exiting.");
							break;
						}
						Err(RecvError::Lagged(n)) => {
							tracing::warn!("Application instruction channel lagged by {}.", n);
						}
					}
				}
			} else {
				// ClientRole::Agent
				let mut app_resp_rx = responses1.subscribe();
				tracing::debug!("Listening for AgentResponses to send to peer");

				loop {
					tracing::trace!("Waiting for AgentResponse from application");

					match app_resp_rx.recv().await {
						Ok(response) => {
							tracing::trace!("Received AgentResponse {response}");
							let mut stream = match connection1.open_uni().await {
								Ok(s) => s,
								Err(e) => {
									tracing::error!("Failed to open uni stream for response: {e}");
									break;
								}
							};
							let tunnel_msg = TunnelMessage {
								message: Some(tunnel_message::Message::AgentResponse(response)),
							};
							buf.clear();
							if let Err(e) = tunnel_msg.encode(&mut buf) {
								tracing::error!("Failed to encode Response: {e}");
								continue;
							}
							if let Err(e) = stream.write_all(&buf).await {
								tracing::error!("Failed to send response: {e}");
								continue; // Or break depending on error nature
							}
							if let Err(e) = stream.finish() {
								tracing::warn!("Failed to finish uni stream for response: {}", e);
							}
						}
						Err(RecvError::Closed) => {
							tracing::warn!("Application response channel closed. Exiting.");
							break;
						}
						Err(RecvError::Lagged(n)) => {
							tracing::warn!("Application response channel lagged by {}.", n);
						}
					}
				}
			}
			tracing::trace!("Sender task for QUIC client finished.");
		});

		Ok((instructions, responses))
	}

	fn stop(&self) -> Result<(), Self::Error> {
		todo!()
	}
}
