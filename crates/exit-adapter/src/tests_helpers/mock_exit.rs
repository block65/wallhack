use std::sync::Arc;

use protobuf::{
	SocketAddrPair,
	v2::{
		ExitNodeResponse, EntryNodeInstruction, IcmpSendInstruction, TcpCloseInstruction,
		TcpConnectInstruction, TcpListenCloseInstruction, TcpListenInstruction, TcpSendInstruction,
		UdpSendInstruction, entry_node_instruction::Instruction,
	},
};
use tokio::sync::broadcast;

use crate::{
	adapter::{
		ExitAdapter, RuntimeError, SendResponse, TcpCloseResponse, TcpStreamResponse,
		TcpListenResponse, TcpListenCloseResponse,
	},
	sessions,
};
#[cfg(unix)]
use crate::sessions::icmp::IcmpSession;

#[derive(Default)]
pub struct NullExitAdapter {}

impl ExitAdapter for NullExitAdapter {
	fn tcp_close(&self, pair: crate::SocketSet) -> Result<TcpCloseResponse, RuntimeError> {
		tracing::trace!("Closing connection for pair: {}", pair);
		Ok(TcpCloseResponse::Ok { pair })
	}

	async fn tcp_send(
		&self,
		set: crate::SocketSet,
		data: &[u8],
		fin: bool,
	) -> Result<SendResponse, RuntimeError> {
		Ok(SendResponse::Ok {
			set,
			size: data.len(),
			is_new: None,
		})
	}

	async fn tcp_connect(
		&self,
		set: crate::SocketSet,
	) -> Result<TcpStreamResponse, RuntimeError> {
		Ok(TcpStreamResponse::Connected { set })
	}

	async fn tcp_listen(
		&self,
		_pair: crate::SocketSet,
	) -> Result<TcpListenResponse, RuntimeError> {
		todo!()
	}
	
	async fn tcp_listen_close(
		&self,
		_set: crate::SocketSet,
	) -> Result<TcpListenCloseResponse, RuntimeError> {
		todo!()
	}

	async fn udp_send(
		&self,
		_pair: crate::SocketSet,
		_data: &[u8],
	) -> Result<SendResponse, RuntimeError> {
		todo!()
	}

	#[cfg(unix)]
	async fn icmp_session(
		&self,
		_pair: crate::SocketSet,
		_ident: u16,
	) -> Result<Option<IcmpSession>, RuntimeError> {
		todo!()
	}

	fn udp_recv_session(
		&self,
		_pair: crate::SocketSet,
	) -> Result<Option<sessions::udp::UdpSession>, RuntimeError> {
		todo!()
	}

	fn tcp_recv_session(
		&self,
		_pair: crate::SocketSet,
	) -> std::result::Result<Option<sessions::tcp::TcpSession>, RuntimeError> {
		todo!()
	}
}

pub async fn run(
	tx: broadcast::Sender<ExitNodeResponse>,
	mut rx: broadcast::Receiver<EntryNodeInstruction>,
) -> anyhow::Result<()> {
	let adapter = Arc::new(NullExitAdapter::default());

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

				let pair = SocketSet::try_from(pair)?;

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

			Some(Instruction::TcpSend(TcpSendInstruction { pair, data, fin })) => {
				let Some(pair) = pair else {
					tracing::error!("Received TcpConnectInstruction with None pair");
					continue;
				};
				tracing::trace!("Received send data request: {:?}", pair);
				let fut = async move {
					match adapter_clone.tcp_send(pair.try_into()?, &data, fin).await {
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
