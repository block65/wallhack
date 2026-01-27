use std::sync::Arc;

use protobuf::{
	SocketSet,
	v2::{AgentResponse, HostInstruction, tunnel_message},
};

use crate::control::metrics::SharedMetrics;

use super::adapter::HostAdapter;

#[derive(Debug, thiserror::Error)]
pub enum Error {
	#[error("No receivers for instructions. This is a bug.")]
	NoReceivers,

	#[error("Network adapter error: {0}")]
	NetworkAdapterError(#[from] Box<dyn std::error::Error + Send + Sync>),

	#[error("Failed to send instruction: {0}")]
	SendError(#[from] tokio::sync::broadcast::error::SendError<HostInstruction>),

	#[error("Failed to receive response: {0}")]
	RecvError(#[from] tokio::sync::broadcast::error::RecvError),
}

pub struct HostOrchestrator<TNetworkAdapter>
where
	TNetworkAdapter: HostAdapter + Send + 'static,
{
	network_adapter: Arc<TNetworkAdapter>,
	metrics: SharedMetrics,
}

impl<TNetworkAdapter> HostOrchestrator<TNetworkAdapter>
where
	TNetworkAdapter: HostAdapter + Send + Sync + 'static,
	TNetworkAdapter::Error: std::error::Error + Send + Sync + 'static,
{
	pub fn new(network_adapter: TNetworkAdapter, metrics: SharedMetrics) -> Self {
		Self {
			network_adapter: Arc::new(network_adapter),
			metrics,
		}
	}

	pub async fn drive(
		&self,
		channels: (
			tokio::sync::broadcast::Sender<HostInstruction>,
			tokio::sync::broadcast::Receiver<AgentResponse>,
		),
	) -> Result<(), Error> {
		// tracing::info!("starting up...");

		let (instructions, mut responses) = channels;

		if instructions.receiver_count() == 0 {
			tracing::error!("No receivers for instructions. This is a bug.");
			return Err(Error::NoReceivers);
		}

		let net0 = Arc::clone(&self.network_adapter);
		let metrics0 = self.metrics.clone();
		let host_to_agent_fut = async move {
			let mtu = 2000;
			let mut buf = vec![0u8; mtu];

			tracing::debug!("starting host_to_agent_fut...");

			let loop_result: Result<(), Error> = loop {
				// WARN:noisy
				tracing::trace!("waiting for next instruction...");
				match net0.next_message(&mut buf).await {
					Ok(messages) => {
						for message in messages {
							tracing::trace!("Received: {message}");

							let span = tracing::span!(
								tracing::Level::TRACE,
								"host_to_agent_fut",
								message = %message
							);

							let _enter = span.enter();

							match message {
								tunnel_message::Message::HostInstruction(msg) => {
									use prost::Message;
									metrics0.inc_packets_out(1);
									metrics0.inc_bytes_out(msg.encoded_len() as u64);
									instructions.send(msg)?;
								}
								tunnel_message::Message::AgentResponse(response) => {
									let Some((pair, response)) =
										response.pair.zip(response.response)
									else {
										tracing::warn!(
											"Received response without pair or response data."
										);
										continue;
									};

									let set = match SocketSet::try_from(pair) {
										Ok(lol) => lol,
										Err(e) => {
											tracing::error!("Failed to convert pair: {}", e);
											continue;
										}
									};

									match net0.handle_response(set, response).await {
										Ok(()) => tracing::trace!("Sent data to TUN interface."),
										Err(e) => tracing::error!("tun error: {}", e),
									}
								}
								tunnel_message::Message::RawPacket(_) => todo!(),
								tunnel_message::Message::AgentHello(hello) => {
									// TODO: Route agent_id to session manager for TUN naming
									tracing::info!(
										"Received AgentHello: id={}, version={}",
										hello.agent_id,
										hello.version
									);
								}
							}
						}
					}
					Err(e) => {
						tracing::error!("error: {}", e);
						break Err(Error::NetworkAdapterError(e.into()));
					}
				}

				// TODO: Check if we need to yield here. It seems like the tun adapter
				// blocks
				// tokio::task::yield_now().await;
			};

			loop_result
		};

		let net1 = Arc::clone(&self.network_adapter);
		let metrics1 = self.metrics.clone();
		let agent_to_host_fut = async move {
			tracing::debug!("starting agent_to_host_fut (waiting for responses from agent)...");

			loop {
				match responses.recv().await {
					Ok(msg) => {
						use prost::Message;
						metrics1.inc_packets_in(1);
						metrics1.inc_bytes_in(msg.encoded_len() as u64);
						tracing::trace!("Channel received {msg}");

						let span = tracing::span!(tracing::Level::TRACE, "responses", msg = %msg);
						let _enter = span.enter();

						let Some((pair, response)) = msg.pair.zip(msg.response) else {
							tracing::warn!("Response without pair and/or data");
							continue;
						};

						let set = match SocketSet::try_from(pair) {
							Ok(s) => s,
							Err(e) => {
								tracing::error!("Failed to convert pair: {}", e);
								continue; // skip loop
							}
						};

						match net1.handle_response(set, response).await {
							Ok(()) => tracing::trace!("Sent data to TUN interface."),
							Err(e) => tracing::error!("handle_response error: {}", e),
						}
					}
					Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
						tracing::warn!(
							"Agent response channel lagged by {n}, skipping missed messages"
						);
						continue;
					}
					Err(e) => {
						tracing::error!("Agent channel closed: {e}");
						break Err::<(), _>(Error::RecvError(e));
					}
				}
			}

			// tracing::warn!("ended agent_to_host_fut...");

			// result
		};

		// if one side is completed, the other is useless
		tokio::select! {
			res = host_to_agent_fut => {
				tracing::info!("host_to_agent_fut task completed. {:?}", res);
				res?;
			}
			res = agent_to_host_fut => {
				tracing::info!("agent_to_host_fut task completed {:?}.", res);
				// res?;
			}
			else => {
				tracing::warn!("Both tasks ended unexpectedly.");
			}
		}

		tracing::info!("Stopped.");
		Ok(())
	}
}
