use std::{sync::Arc, time::Duration};

use prost::Message;
use protobuf::v2::{AgentResponse, HostInstruction, TunnelMessage, tunnel_message};
use quinn::{IdleTimeout, WriteError, crypto::rustls::QuicServerConfig};

use crate::server::{
	server::ServerRole,
	tls::{ALPN_QUIC_HTTP, configure_crypto},
};

use super::{
	config::ServerConfig,
	server::{AcceptResult, Server},
	tls,
};

#[derive(thiserror::Error, Debug)]
pub enum Error {
	#[error("tls config error: {0}")]
	StartTls(#[from] quinn::crypto::rustls::NoInitialCipherSuite),

	#[error("tls config error: {0}")]
	Connection(#[from] quinn::ConnectionError),

	#[error("io error: {0}")]
	Io(#[from] std::io::Error),

	#[error("server tls error: {0}")]
	ServerTls(#[from] tls::Error),

	#[error("tls error: {0}")]
	Tls(#[from] rustls::Error),

	// quinn::VarIntBoundsExceeded
	#[error("quinn bounds error: {0}")]
	Quinn(#[from] quinn::VarIntBoundsExceeded),
}

pub struct QuicServer {
	endpoint: quinn::Endpoint,
}

impl Server for QuicServer {
	type Error = Error;

	fn try_new(config: ServerConfig) -> Result<Self, Error> {
		let (cert_der, priv_key) = configure_crypto(config.tls)?;

		let mut server_crypto = rustls::ServerConfig::builder()
			.with_no_client_auth()
			.with_single_cert(cert_der, priv_key)?;

		server_crypto.alpn_protocols = ALPN_QUIC_HTTP.iter().map(|&x| x.into()).collect();

		let mut server_config =
			quinn::ServerConfig::with_crypto(Arc::new(QuicServerConfig::try_from(server_crypto)?));

		let transport_config = Arc::get_mut(&mut server_config.transport).unwrap();

		let timeout = IdleTimeout::try_from(Duration::from_secs(10))?;
		transport_config.max_idle_timeout(Some(timeout));
		transport_config.keep_alive_interval(Some(Duration::from_secs(5)));
		// transport_config.max_concurrent_uni_streams(1_u8.into());

		tracing::trace!("Server Config {:?}", server_config);
		tracing::debug!("will listen on {}", config.listen);

		let endpoint = quinn::Endpoint::server(server_config, config.listen)?;

		tracing::info!("local_addr {:?}", endpoint.local_addr());

		Ok(Self { endpoint })
	}

	async fn accept(&mut self, role: ServerRole) -> Result<Option<AcceptResult>, Error> {
		tracing::debug!("waiting for next connection...");

		let Some(incoming) = self.endpoint.accept().await else {
			return Err(Error::Io(std::io::Error::other(
				"failed to accept incoming connection",
			)));
		};

		let connection = incoming.await?;

		let (instructions, _) = tokio::sync::broadcast::channel::<HostInstruction>(10);
		let (responses, _) = tokio::sync::broadcast::channel::<AgentResponse>(10);

		// Task 1: Handles incoming streams from the client (Common for both roles)
		// This connection will be used to accept incoming streams and send the
		// messages out onto the right channel
		let connection0 = connection.clone();
		let responses0 = responses.clone();
		let instructions0 = instructions.clone();

		tokio::spawn(async move {
			tracing::trace!("spawning incoming client connection handler");
			let mtu = 2000;

			loop {
				tracing::trace!("Awaiting next stream on this connection...");
				let mut peer_recv = match connection0.accept_uni().await {
					Ok(stream) => stream,
					Err(e) => {
						tracing::error!("While accepting a unidirectional stream: {}", e);
						break; // Exit task on connection error
					}
				};

				match peer_recv.read_to_end(mtu).await {
					Ok(data) => {
						if data.is_empty() {
							tracing::info!("recv_stream closed by peer (0 bytes read)");
							continue; // Or break if one stream closing means end of interaction for this task
						}

						tracing::trace!("Received data from peer: {:02X?}", data);

						let msg = match TunnelMessage::decode(prost::bytes::Bytes::from(data)) {
							Ok(result) => result,
							Err(e) => {
								tracing::error!("Decoder error: {}", e);
								continue;
							}
						};

						match msg.message {
							Some(tunnel_message::Message::AgentResponse(resp)) => {
								tracing::trace!("Received AgentResponse from peer {:?}", resp);
								if responses0.send(resp).is_err() {
									tracing::warn!(
										"Error sending AgentResponse to internal channel (receiver dropped?)"
									);
								}
							}
							Some(tunnel_message::Message::HostInstruction(instr_msg)) => {
								tracing::trace!("Received HostInstruction from peer");
								if instructions0.send(instr_msg).is_err() {
									tracing::warn!(
										"Error sending HostInstruction to internal channel (receiver dropped?)"
									);
								}
							}
							Some(tunnel_message::Message::RawPacket(msg)) => {
								tracing::warn!("Unhandled message type: {:?}", msg);
							}
							None => {
								tracing::warn!("Received TunnelMessage with no message type.");
							}
						}
					}
					Err(e) => {
						tracing::error!("Error reading from stream: {}", e);
						// Decide if this error should break the loop or just log and
						// continue For persistent connections, might want to continue. For
						// stream-specific errors, continue. If it's a connection-level
						// error disguised as a read error, then break.
						// quinn::ReadToEndError can indicate connection loss.
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
			tracing::trace!("Incoming client connection handler task finished.");
		});

		match role {
			ServerRole::Agent => {
				tracing::info!("Spawning task to send responses to peer");
				// Task 2: Handles outgoing AgentResponse messages (Agent Role)
				let connection1 = connection.clone();
				let mut responses1 = responses.subscribe(); // Agent's orchestrator will send to `responses_send`

				tokio::spawn(async move {
					tracing::trace!("spawning outgoing responses handler task (Agent Role)");
					let mtu = 2000; // Max message size
					let mut buf = Vec::with_capacity(mtu); // Reusable buffer

					loop {
						let response = match responses1.recv().await {
							Ok(result) => result,
							Err(tokio::sync::broadcast::error::RecvError::Closed) => {
								tracing::info!(
									"Responses channel closed. Exiting outgoing responses task."
								);
								break;
							}
							Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
								tracing::warn!(
									"Responses channel lagged by {}. Some responses missed.",
									n
								);
								continue;
							}
						};

						let span = tracing::span!(
							tracing::Level::TRACE,
							"quic_server_send_resp",
							// cid = response.correlation_id
						);

						let _enter = span.enter();

						tracing::trace!("sending AgentResponse to peer {}", response);

						let mut peer_send = match connection1.open_uni().await {
							Ok(peer) => peer,
							Err(e) => {
								tracing::error!("Error opening uni stream for response: {}", e);
								break; // If we can't open a stream, likely a connection issue
							}
						};

						let tunnel_message = TunnelMessage::from(response);
						buf.clear(); // Reuse buffer
						match tunnel_message.encode(&mut buf) {
							Ok(()) => {}
							Err(e) => {
								tracing::error!("Error encoding response TunnelMessage: {}", e);
								continue; // Skip this message
							}
						}

						// tracing::trace!("sending tunnel message: {}", tunnel_message);

						match peer_send.write_all(&buf).await {
							Ok(()) => tracing::trace!("Response sent to peer"),
							Err(e @ WriteError::Stopped(_)) => {
								tracing::error!("Sending response was stopped: {}", e);
								// This indicates the stream was stopped by the peer, might not
								// need to break loop for connection
								continue;
							}
							Err(e) => {
								tracing::error!("Error sending response: {}", e);
								break; // Other write errors might indicate connection issues
							}
						}

						// Gracefully close the stream
						if let Err(e) = peer_send.finish() {
							tracing::warn!("Failed to finish uni stream for response: {}", e);
						}
					}
					tracing::trace!("Outgoing responses handler task finished.");
				});
			}
			ServerRole::Host => {
				tracing::info!("Spawning task to send instructions to peer.");

				let connection2 = connection.clone();
				let mut instructions2 = instructions.subscribe();

				tracing::trace!(
					"Subscribed to instructions channel for Host Role: rcvr count {}",
					instructions.receiver_count()
				);

				tokio::spawn(async move {
					tracing::trace!("spawning outgoing instructions handler task (Host Role)");
					let mtu = 2000; // Max message size
					let mut buf = Vec::with_capacity(mtu); // Reusable buffer

					loop {
						tracing::debug!("waiting for instructions to send to agent");
						let host_instruction = match instructions2.recv().await {
							Ok(result) => result,
							Err(tokio::sync::broadcast::error::RecvError::Closed) => {
								tracing::info!(
									"Instructions channel closed. Exiting outgoing instructions task."
								);
								break;
							}
							Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
								tracing::warn!(
									"Instructions channel lagged by {}. Some instructions missed.",
									n
								);
								continue;
							}
						};

						let span = tracing::span!(tracing::Level::TRACE, "quic_server_send_instr",);
						let _enter = span.enter();

						tracing::trace!(
							"Sending HostInstruction of type {:?} to peer",
							host_instruction.instruction
						);
						let mut peer_send = match connection2.open_uni().await {
							Ok(peer) => peer,
							Err(e) => {
								tracing::error!("Error opening uni stream for instruction: {}", e);
								break; // If we can't open a stream, likely a connection issue
							}
						};

						let tunnel_message: TunnelMessage = host_instruction.into();
						buf.clear(); // Reuse buffer
						match tunnel_message.encode(&mut buf) {
							Ok(()) => {}
							Err(e) => {
								tracing::error!("Error encoding instruction: {}", e);
								continue; // Skip this message
							}
						}

						tracing::trace!("sending message: {}", tunnel_message);
						match peer_send.write_all(&buf).await {
							Ok(()) => {
								tracing::trace!("Instruction sent successfully over QUIC");
							}
							Err(e @ WriteError::Stopped(_)) => {
								tracing::error!("Sending instruction was stopped: {}", e);
								// Stream stopped by peer, might not need to break loop for
								// connection
								continue;
							}
							Err(e) => {
								tracing::error!("Error sending instruction to QUIC: {}", e);
								break; // Other write errors might indicate connection issues
							}
						}
						if let Err(e) = peer_send.finish() {
							// Gracefully close the stream
							tracing::warn!("Failed to finish uni stream for instruction: {}", e);
						}
					}
					tracing::trace!("Outgoing instructions handler task (Host Role) finished.");
				});
			}
		}

		Ok(Some(AcceptResult::new(
			(instructions, responses),
			connection.remote_address().to_string(),
		)))
	}

	fn stop(&self) -> Result<(), Self::Error> {
		self.endpoint.close(0_u32.into(), b"server stopping");
		tracing::info!("QUIC server endpoint close initiated.");
		Ok(())
	}
}
