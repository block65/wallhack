use std::sync::Arc;

use protobuf::{
	SocketAddrPair,
	v2::{
		AgentResponse, HostInstruction, IcmpSendInstruction, TcpCloseInstruction,
		TcpConnectInstruction, TcpListenCloseInstruction, TcpListenInstruction, TcpSendInstruction,
		UdpSendInstruction, host_instruction::Instruction,
	},
};
use tokio::sync::broadcast;

use crate::{
	adapter::{
		AgentAdapter, RuntimeError, SendResponse, TcpCloseResponse, TcpConnectResponse,
		TcpListenResponse,
	},
	sessions::{self, icmp::IcmpSession},
};

#[derive(Default)]
pub struct NullAgentAdapter {}

impl AgentAdapter for NullAgentAdapter {
	fn tcp_close(&self, pair: crate::SocketAddrPair) -> Result<TcpCloseResponse, RuntimeError> {
		tracing::trace!("Closing connection for pair: {}", pair);
		Ok(TcpCloseResponse::Ok { pair })
	}

	async fn tcp_send(
		&self,
		pair: crate::SocketAddrPair,
		data: Vec<u8>,
	) -> Result<SendResponse, RuntimeError> {
		Ok(SendResponse::Ok {
			pair,
			size: data.len(),
			is_new: None,
		})
	}

	async fn tcp_connect(
		&self,
		pair: crate::SocketAddrPair,
	) -> Result<TcpConnectResponse, RuntimeError> {
		Ok(TcpConnectResponse::Ok { pair })
	}

	async fn tcp_listen(
		&self,
		_pair: crate::SocketAddrPair,
	) -> Result<TcpListenResponse, RuntimeError> {
		todo!()
	}

	async fn udp_send(
		&self,
		_pair: crate::SocketAddrPair,
		_data: &mut [u8],
	) -> Result<SendResponse, RuntimeError> {
		todo!()
	}

	async fn icmp_session(
		&self,
		_pair: crate::SocketAddrPair,
	) -> Result<IcmpSession, RuntimeError> {
		todo!()
	}

	fn udp_recv_session(
		&self,
		_pair: crate::SocketAddrPair,
	) -> Result<sessions::udp::UdpSession, RuntimeError> {
		todo!()
	}

	fn tcp_recv_session(
		&self,
		_pair: crate::SocketAddrPair,
	) -> std::result::Result<sessions::tcp::TcpSession, RuntimeError> {
		todo!()
	}
}

pub async fn run(
	tx: broadcast::Sender<AgentResponse>,
	mut rx: broadcast::Receiver<HostInstruction>,
) -> anyhow::Result<()> {
	let adapter = Arc::new(NullAgentAdapter::default());

	loop {
		let instruct = rx.recv().await?;
		let adapter_clone = adapter.clone();

		tracing::trace!("Received instruction: {:?}", instruct);

		let tx_clone = tx.clone();

		match instruct.instruction {
			Some(Instruction::TcpConnect(TcpConnectInstruction { pair })) => {
				let Some(pair) = pair else {
					tracing::error!("Received TcpConnectInstruction with None pair");
					continue;
				};

				let pair = SocketAddrPair::try_from(pair)?;

				tracing::trace!("Received connect request: {}", pair);
				let fut = async move {
					match adapter_clone.tcp_connect(pair).await {
						Ok(response) => {
							tracing::trace!("Connect got response {:?}", response);
							tx_clone.send(response.into()).unwrap_or_else(|e| {
								tracing::error!("Error sending response on channel: {}", e);
								0
							});
							Ok(())
						}
						Err(e) => Err(e),
					}
				};
				tokio::spawn(fut);
			}

			Some(Instruction::TcpSend(TcpSendInstruction { pair, data })) => {
				let Some(pair) = pair else {
					tracing::error!("Received TcpConnectInstruction with None pair");
					continue;
				};
				tracing::trace!("Received send data request: {:?}", pair);
				let fut = async move {
					match adapter_clone.tcp_send(pair.try_into()?, data).await {
						Ok(response) => {
							tracing::trace!("SendData got response {:?}", response);
							tx_clone.send(response.into()).unwrap_or_else(|e| {
								tracing::error!("Error sending response on channel: {}", e);
								0
							});
							Ok(())
						}
						Err(e) => Err(e),
					}
				};

				tokio::spawn(fut);
			}

			Some(Instruction::TcpClose(TcpCloseInstruction { pair })) => {
				let Some(pair) = pair else {
					tracing::error!("Received TcpConnectInstruction with None pair");
					continue;
				};
				match adapter_clone.tcp_close(pair.try_into()?) {
					Ok(response) => tracing::trace!("Close got response {:?}", response),
					Err(e) => tracing::error!("Error closing: {}", e),
				}
			}

			Some(Instruction::UdpSend(UdpSendInstruction { .. })) => {
				todo!()
			}

			Some(Instruction::IcmpSend(IcmpSendInstruction { .. })) => {
				todo!()
			}
			Some(Instruction::TcpListen(TcpListenInstruction { .. })) => todo!(),
			Some(Instruction::TcpListenClose(TcpListenCloseInstruction { .. })) => todo!(),
			None => todo!(),
		}
	}

	// Ok(())
}
