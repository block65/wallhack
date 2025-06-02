use std::{sync::Arc, vec};

use protobuf::v2::{
	self, AgentResponse, HostInstruction, IcmpEchoRequest, IcmpSendInstruction,
	RuntimeErrorResponse, TcpCloseInstruction, TcpConnectInstruction, TcpListenCloseInstruction,
	TcpListenInstruction, TcpSendInstruction, UdpSendInstruction, agent_response,
	host_instruction::Instruction, icmp_response, icmp_send_instruction::IcmpMessage,
};
use tokio::sync::broadcast;

use agent_adapter::{
	SocketSet,
	adapter::{AgentAdapter, RuntimeError, SendResponse, TcpStreamResponse},
	sessions::{self, common::RxSession},
};

#[derive(Debug, thiserror::Error)]
pub enum Error {
	#[error(transparent)]
	ChannelRecv(#[from] tokio::sync::broadcast::error::RecvError),

	#[error(transparent)]
	ChannelSend(#[from] tokio::sync::broadcast::error::SendError<AgentResponse>),

	#[error(transparent)]
	Runtime(#[from] RuntimeError),

	#[error("InvalidInstruction")]
	InvalidInstruction,
}

pub struct Orchestrator<A: AgentAdapter> {
	adapter: Arc<A>,
}

macro_rules! extract_socket_pair {
	($pair:expr, $instruction:expr) => {{
		let Some(pair) = $pair else {
			tracing::error!("Invalid instruction: missing pair");
			return Err(Error::InvalidInstruction);
		};

		match pair.try_into() {
			Ok(set) => set,
			Err(e) => {
				tracing::error!("Invalid instruction: {e}");
				return Err(Error::InvalidInstruction);
			}
		}
	}};
}

impl<A: AgentAdapter> Orchestrator<A> {
	pub fn new(adapter: Arc<A>) -> Self {
		Self { adapter }
	}

	pub async fn drive(
		self,
		responses: broadcast::Sender<AgentResponse>,
		mut instructions: broadcast::Receiver<HostInstruction>,
	) -> Result<(), Error> {
		loop {
			tracing::trace!("Waiting for next instruction from host...");

			// clones for loop
			let adapter0 = self.adapter.clone();
			let responses0 = responses.clone();

			let host_instr = instructions.recv().await?;
			tracing::debug!("Received instruction: {}", host_instr);

			match host_instr.instruction {
				// TcpConnectInstruction - Agent establishes the actual connection to the remote host
				Some(Instruction::TcpConnect(TcpConnectInstruction { pair })) => {
					let set: SocketSet = extract_socket_pair!(pair, host_instr.instruction);
					tracing::trace!(
						"Received TcpConnect instruction for {set}, attempting remote connection"
					);

					let fut = async move {
						match adapter0.tcp_connect(set).await {
							Ok(TcpStreamResponse::Connected { set }) => {
								tracing::trace!(
									"Agent connected to remote for {set}, sending Connected response"
								);
								// Send Connected response to TUN
								if let Err(e) = responses0.send(AgentResponse {
									pair: Some(set.into()),
									response: Some(agent_response::Response::TcpResponse(
										v2::TcpResponse {
											response: Some(v2::tcp_response::Response::Connected(
												v2::TcpConnectedResponse {},
											)),
										},
									)),
								}) {
									tracing::error!(
										"TcpConnect: Error sending Connected response for {set}: {e}"
									);
									return; // Don't proceed to spawn recv task if we can't send Connected
								}

								let fut = async move {
									let mtu = 1500; // Or get from config/adapter
									let mut recv_buf = vec![0; mtu];

									let session = match adapter0.tcp_recv_session(set) {
										Ok(Some(session)) => session,
										Ok(None) => {
											tracing::error!(
												"No TCP recv session available for {set}"
											);
											return Ok(());
										}
										Err(e) => {
											tracing::error!(
												"Error getting TCP recv session for {set}: {e}"
											);
											// Optionally send a runtime error back to TUN if appropriate
											// This might be tricky as the primary connection response is already sent.
											return Ok(());
										}
									};

									// Loop to continuously receive data
									let responses2 = responses0.clone();

									loop {
										match session.recv(&mut recv_buf).await {
											Ok(sessions::common::SessionStatus::DataIo {
												size,
											}) => {
												tracing::trace!(
													"Received {size} bytes {set}. DataRecv",
												);
												responses2.send(AgentResponse {
													pair: Some(set.into()),
													response: Some(
														agent_response::Response::TcpResponse(
															v2::TcpResponse {
																response: Some(
																	v2::tcp_response::Response::DataRecv(
																		v2::TcpDataRecvResponse {
																			data: recv_buf[..size].to_vec()
																		},
																	)
																)
															},
														)),
													})?;
											}
											Ok(sessions::common::SessionStatus::PeerClosed) => {
												tracing::trace!(
													"Peer closed {set}. ConnectionClosed"
												);

												responses2.send(AgentResponse {
													pair: Some(set.into()),
													response: Some(
														agent_response::Response::TcpResponse(
															v2::TcpResponse {
																response: Some(
																	v2::tcp_response::Response::ConnectionClosed(
																		v2::TcpConnectionClosedResponse {},
																	)
																)
															},
														)),
													})?;
												break; // Exit loop as peer closed
											}
											Err(e) => {
												tracing::error!(
													"Error receiving TCP data for {set}: {e}"
												);
												responses2.send(AgentResponse {
													pair: Some(set.into()),
													response: Some(
														agent_response::Response::RuntimeError(
															v2::RuntimeErrorResponse {
																reason: e.to_string(),
															},
														),
													),
												})?;
												break; // Exit loop on error
											}
										}
									}
									tracing::debug!("Recv task for {set} finished.");

									Ok::<(), Error>(())
								};

								tracing::debug!("Spawning recv task for {set}");

								tokio::spawn(
									fut, // .instrument(
									    // 	tracing::info_span!("agent_tcp_recv_loop", set = %set),
									    // ),
								);
							}

							// ConnectionReset
							Ok(TcpStreamResponse::Reset { set }) => {
								tracing::warn!("Agent connection to remote for {set} reset");
								if let Err(e) = responses0.send(AgentResponse {
									pair: Some(set.into()),
									response: Some(agent_response::Response::TcpResponse(
										v2::TcpResponse {
											response: Some(
												v2::tcp_response::Response::ConnectionClosed(
													v2::TcpConnectionClosedResponse {},
												),
											),
										},
									)),
								}) {
									tracing::error!(
										"TcpConnect: Error sending ConnectionRefused response for {set}: {e}"
									);
								}
							}

							// ConnectionRefused
							Ok(TcpStreamResponse::Refused { set }) => {
								tracing::warn!("Agent connection to remote for {set} refused");
								if let Err(e) = responses0.send(AgentResponse {
									pair: Some(set.into()),
									response: Some(agent_response::Response::TcpResponse(
										v2::TcpResponse {
											response: Some(
												v2::tcp_response::Response::ConnectionRefused(
													v2::TcpConnectionRefusedResponse {}, // Assuming default constructible
												),
											),
										},
									)),
								}) {
									tracing::error!(
										"TcpConnect: Error sending ConnectionRefused response for {set}: {e}"
									);
								}
							}

							// Unhandled variant
							// Ok(TcpConnectResponse::None { set }) => {
							// 	tracing::warn!(
							// 		"Agent connection attempt for {set} resulted in unhandled variant",
							// 	);
							// }
							Err(e) => {
								// RuntimeError from adapter_clone.tcp_connect
								tracing::error!(
									"Agent connection to remote for {set} failed with runtime error: {e}"
								);
								if let Err(send_e) = responses0.send(AgentResponse {
									pair: Some(set.into()),
									response: Some(agent_response::Response::RuntimeError(
										RuntimeErrorResponse {
											reason: e.to_string(),
										},
									)),
								}) {
									tracing::error!(
										"TcpConnect: Error sending RuntimeError response for {set}: {send_e}"
									);
								}
							}
						}
					};

					tokio::spawn(
						fut, // .instrument(tracing::info_span!("agent_tcp_connect_actual", set = %set)),
					);
				}

				// TcpSendInstruction
				Some(Instruction::TcpSend(TcpSendInstruction { pair, data })) => {
					let pair: SocketSet = extract_socket_pair!(pair, host_instr.instruction);
					tracing::trace!("Received TcpSendData request: {pair}");
					let fut = async move {
						let response = adapter0.tcp_send(pair, data).await?;
						responses0.send(response.into())?;
						Ok::<(), Error>(())
					};
					tokio::spawn(fut);
				}

				// TcpCloseInstruction
				Some(Instruction::TcpClose(TcpCloseInstruction { pair })) => {
					let set: SocketSet = extract_socket_pair!(pair, host_instr.instruction);
					adapter0.tcp_close(set)?;

					responses0.send(AgentResponse {
						pair: Some(set.into()),
						response: Some(agent_response::Response::TcpResponse(v2::TcpResponse {
							response: Some(v2::tcp_response::Response::ConnectionClosed(
								v2::TcpConnectionClosedResponse {},
							)),
						})),
					})?;

					responses0.send(AgentResponse {
						pair: Some(set.into()),
						response: Some(agent_response::Response::TcpResponse(v2::TcpResponse {
							response: Some(v2::tcp_response::Response::ConnectionClosed(
								v2::TcpConnectionClosedResponse {},
							)),
						})),
					})?;
				}

				// UdpSendInstruction
				Some(Instruction::UdpSend(UdpSendInstruction { pair, mut data })) => {
					let set = extract_socket_pair!(pair, host_instr.instruction);

					let send_fut = async move {
						let response = adapter0.udp_send(set, &mut data).await?;

						let is_new = if let SendResponse::Ok {
							is_new: maybe_is_new,
							..
						} = response
						{
							maybe_is_new == Some(true)
						} else {
							false
						};

						// need to spawn a receive task if this is new
						if is_new {
							let recv_fut = async move {
								// let session = adapter_clone.udp_recv_session(set)?;

								let session = match adapter0.udp_recv_session(set) {
									Ok(Some(session)) => session,
									Ok(None) => {
										tracing::error!("No UDP recv session available for {set}");
										return Ok::<(), Error>(());
									}
									Err(e) => {
										tracing::error!(
											"Error getting UDP recv session for {set}: {e}"
										);
										return Ok::<(), Error>(());
									}
								};

								let mtu = 1500;
								let mut recv_buf = vec![0; mtu];

								let response = match session.recv(&mut recv_buf).await {
									Ok(sessions::common::SessionStatus::DataIo { size }) => {
										// RecvResponse::Ok {
										// 	set,
										// 	data: recv_buf[..size].to_vec(),
										// 	size,
										// }
										tracing::debug!(
											"Received {} bytes from UDP session: {:?}",
											size,
											set
										);
										AgentResponse {
											pair: Some(set.into()),
											response: Some(agent_response::Response::UdpResponse(
												v2::UdpResponse {
													response: Some(
														v2::udp_response::Response::DataRecv(
															v2::UdpDataRecvResponse {
																data: recv_buf[..size].to_vec(),
															},
														),
													),
												},
											)),
										}
									}
									Ok(sessions::common::SessionStatus::PeerClosed) => {
										tracing::warn!("Unexpected Peer closed for UDP");
										AgentResponse {
											pair: Some(set.into()),
											response: Some(agent_response::Response::UdpResponse(
												v2::UdpResponse { response: None },
											)),
										}
									}
									Err(e) => {
										tracing::error!("Error receiving data: {e}");
										AgentResponse {
											pair: Some(set.into()),
											response: Some(agent_response::Response::RuntimeError(
												RuntimeErrorResponse {
													reason: e.to_string(),
												},
											)),
										}
									}
								};

								responses0.send(response)?;

								Ok::<(), Error>(())
							};

							tracing::debug!("Spawning udp_recv task");
							let h = tokio::spawn(
								recv_fut, //.instrument(tracing::info_span!("udp_recv", pair = %pair)),
							);
							tracing::debug!("Spawned udp_recv task {}", h.id());
						}

						Ok::<(), Error>(())
					};

					tokio::spawn(send_fut); //.instrument(tracing::info_span!("udp_send", pair = ?set)));
				}

				// IcmpSendInstruction
				Some(Instruction::IcmpSend(IcmpSendInstruction { icmp_message, pair })) => {
					let pair: SocketSet = extract_socket_pair!(pair, host_instr.instruction);

					let Some(IcmpMessage::IcmpEchoRequest(IcmpEchoRequest {
						seq_no,
						ident,
						data,
					})) = icmp_message
					else {
						tracing::error!("Invalid IcmpSendInstruction {icmp_message:?}");
						return Err(Error::InvalidInstruction);
					};

					tracing::trace!("Received IcmpSendEchoRequest: {}", pair);
					let send_fut = async move {
						#[allow(clippy::cast_possible_truncation)]
						// let session = adapter_clone.icmp_session(pair, ident as u16).await?;
						let session = match adapter0.icmp_session(pair, ident as u16).await {
							Ok(Some(session)) => session,
							Ok(None) => {
								tracing::error!("No ICMP session available for {pair}");
								return Err(Error::InvalidInstruction);
							}
							Err(e) => {
								tracing::error!("Error creating ICMP session: {e}");
								return Err(e.into());
							}
						};

						let mut recv_buf = vec![0; 1500];
						#[allow(clippy::cast_possible_truncation)]
						let session_status = session
							.echo_request(&data, seq_no as u16, &mut recv_buf)
							.await?;

						match session_status {
							sessions::common::SessionStatus::DataIo { size } => {
								recv_buf.truncate(size);
								tracing::debug!(
									"Received {} byte ICMP response: {:?}",
									size,
									recv_buf
								);
							}
							sessions::common::SessionStatus::PeerClosed => {
								tracing::warn!("ICMP session closed by peer");
								recv_buf.clear();
							}
						}

						responses0.send(AgentResponse {
							pair: Some(pair.into()),
							response: Some(agent_response::Response::IcmpResponse(
								v2::IcmpResponse {
									response: Some(icmp_response::Response::DataRecv(
										v2::IcmpDataRecvResponse {
											data: recv_buf,
											echo_ident: ident,
										},
									)),
								},
							)),
						})?;

						Ok::<(), Error>(())
					};

					tracing::trace!("Spawning icmp_ping task");
					let handle = tokio::spawn(
						send_fut, // .instrument(tracing::info_span!("icmp_ping", pair = ?pair.clone())),
					);
					tracing::debug!("Spawned icmp_ping task id {}", handle.id());
				}
				Some(Instruction::TcpListen(TcpListenInstruction { pair })) => {
					let pair: SocketSet = extract_socket_pair!(pair, host_instr.instruction);
					let adapter_clone = Arc::clone(&self.adapter);
					let res_tx_for_loop = responses.clone();

					tracing::debug!("Spawning tcp_listen task for pair: {:?}", pair);
					tokio::spawn(async move {
						match adapter_clone.tcp_listen(pair).await {
							Ok(tcp_listen_response) => {
								tracing::debug!("tcp_listen successful for pair: {:?}", pair);
								if let Err(e) = res_tx_for_loop.send(tcp_listen_response.into()) {
									tracing::error!(
										"TcpListen Error sending response on channel: {e}"
									);
								}
							}
							Err(e) => {
								tracing::error!("Error in tcp_listen for pair {:?}: {e}", pair);
								let response = AgentResponse {
									pair: Some(pair.into()),
									response: Some(agent_response::Response::RuntimeError(
										RuntimeErrorResponse {
											reason: e.to_string(),
										},
									)),
								};
								if let Err(e) = res_tx_for_loop.send(response) {
									tracing::error!(
										"TcpListen Error sending error response on channel: {e}"
									);
								}
							}
						}
					});
				}
				Some(Instruction::TcpListenClose(TcpListenCloseInstruction { pair })) => {
					let pair: SocketSet = extract_socket_pair!(pair, host_instr.instruction);
					let adapter_clone = Arc::clone(&self.adapter);
					let res_tx_for_loop = responses.clone();

					tracing::debug!("Spawning tcp_listen_close task for pair: {:?}", pair);
					tokio::spawn(async move {
						match adapter_clone.tcp_listen_close(pair).await {
							Ok(tcp_listen_close_response) => {
								tracing::debug!("tcp_listen_close successful for pair: {:?}", pair);
								if let Err(e) =
									res_tx_for_loop.send(tcp_listen_close_response.into())
								{
									tracing::error!(
										"TcpListenClose Error sending response on channel: {e}"
									);
								}
							}
							Err(e) => {
								tracing::error!(
									"Error in tcp_listen_close for pair {:?}: {e}",
									pair
								);
								let response = AgentResponse {
									pair: Some(pair.into()),
									response: Some(agent_response::Response::RuntimeError(
										RuntimeErrorResponse {
											reason: e.to_string(),
										},
									)),
								};
								if let Err(e) = res_tx_for_loop.send(response) {
									tracing::error!(
										"TcpListenClose Error sending error response on channel: {e}"
									);
								}
							}
						}
					});
				}

				None => todo!(),
			}
		}
	}
}
// WARNING: This file contains AI-generated edits
