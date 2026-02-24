use std::sync::Arc;

use bytes::Bytes;

use tokio::{sync::broadcast, task::JoinSet};
use wallhack_wire::data::{
    self, EntryNodeInstruction, ExitNodeResponse, RuntimeErrorResponse, TcpCloseInstruction,
    TcpConnectInstruction, TcpListenCloseInstruction, TcpListenInstruction, TcpSendInstruction,
    UdpSendInstruction, entry_node_instruction::Instruction, exit_node_response,
};
#[cfg(unix)]
use wallhack_wire::data::{
    IcmpEchoRequest, IcmpSendInstruction, icmp_response, icmp_send_instruction::IcmpMessage,
};

use wallhack_exit_adapter::{
    SocketSet,
    adapter::{ExitAdapter, RuntimeError, SendResponse, TcpStreamResponse},
    sessions::{self, common::RxSession},
};

use crate::control::metrics::SharedMetrics;

#[derive(Clone)]
struct MetricsSender {
    sender: broadcast::Sender<ExitNodeResponse>,
    metrics: SharedMetrics,
}

impl MetricsSender {
    fn send(&self, msg: ExitNodeResponse) -> Result<usize, Error> {
        use prost::Message;
        self.metrics.inc_packets_out(1);
        self.metrics.inc_bytes_out(msg.encoded_len() as u64);
        self.sender
            .send(msg)
            .map_err(|e| Error::ChannelSend(Box::new(e)))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    ChannelRecv(#[from] tokio::sync::broadcast::error::RecvError),

    #[error(transparent)]
    ChannelSend(Box<tokio::sync::broadcast::error::SendError<ExitNodeResponse>>),

    #[error(transparent)]
    Runtime(#[from] RuntimeError),

    #[error("InvalidInstruction")]
    InvalidInstruction,
}

pub struct Orchestrator<A: ExitAdapter> {
    adapter: Arc<A>,
    metrics: SharedMetrics,
}

fn extract_socket_set(pair: Option<data::SocketAddressPair>) -> Result<SocketSet, Error> {
    let pair = pair.ok_or_else(|| {
        tracing::error!("Invalid instruction: missing pair");
        Error::InvalidInstruction
    })?;
    pair.try_into().map_err(|e| {
        tracing::error!("Invalid instruction: {e}");
        Error::InvalidInstruction
    })
}

impl<A: ExitAdapter> Orchestrator<A> {
    pub fn new(adapter: Arc<A>, metrics: SharedMetrics) -> Self {
        Self { adapter, metrics }
    }

    pub async fn drive(
        self,
        responses: broadcast::Sender<ExitNodeResponse>,
        mut instructions: broadcast::Receiver<EntryNodeInstruction>,
    ) -> Result<(), Error> {
        let responses = MetricsSender {
            sender: responses,
            metrics: self.metrics.clone(),
        };

        let mut tasks = JoinSet::new();

        loop {
            tokio::select! {
                result = instructions.recv() => {
                    let instr = result?;
                    {
                        use prost::Message;
                        self.metrics.inc_packets_in(1);
                        self.metrics.inc_bytes_in(instr.encoded_len() as u64);
                    }

                    tracing::trace!("Received instruction: {}", instr);

                    match instr.instruction {
                        Some(Instruction::TcpConnect(i)) => handle_tcp_connect(i, &self.adapter, &responses, &mut tasks)?,
                        Some(Instruction::TcpSend(i)) => handle_tcp_send(i, &self.adapter, &responses, &mut tasks)?,
                        Some(Instruction::TcpClose(i)) => handle_tcp_close(i, &self.adapter, &responses)?,
                        Some(Instruction::UdpSend(i)) => handle_udp_send(i, &self.adapter, &responses, &mut tasks)?,
                        #[cfg(unix)]
                        Some(Instruction::IcmpSend(i)) => handle_icmp_send(i, &self.adapter, &responses, &mut tasks)?,
                        #[cfg(not(unix))]
                        Some(Instruction::IcmpSend(_)) => tracing::warn!("ICMP not supported on this platform"),
                        Some(Instruction::TcpListen(i)) => handle_tcp_listen(i, &self.adapter, &responses, &mut tasks)?,
                        Some(Instruction::TcpListenClose(i)) => handle_tcp_listen_close(i, &self.adapter, &responses, &mut tasks)?,
                        None => tracing::warn!("Received instruction with no variant set, ignoring"),
                    }
                }
                Some(result) = tasks.join_next() => {
                    if let Err(e) = result {
                        tracing::warn!("Spawned task panicked: {e}");
                    }
                }
            }
        }
    }
}

fn handle_tcp_connect<A: ExitAdapter>(
    instr: TcpConnectInstruction,
    adapter: &Arc<A>,
    responses: &MetricsSender,
    tasks: &mut JoinSet<()>,
) -> Result<(), Error> {
    let set = extract_socket_set(instr.pair)?;
    tracing::debug!("Received TcpConnect instruction for {set}, attempting remote connection");

    let adapter = Arc::clone(adapter);
    let responses = responses.clone();

    tasks.spawn(async move {
        match adapter.tcp_connect(set).await {
            Ok(TcpStreamResponse::Connected { set }) => {
                tracing::debug!("Connected to remote for {set}, sending Connected response");
                if let Err(e) = responses.send(ExitNodeResponse {
                    pair: Some(set.into()),
                    response: Some(exit_node_response::Response::TcpResponse(
                        data::TcpResponse {
                            response: Some(data::tcp_response::Response::Connected(
                                data::TcpConnectedResponse {},
                            )),
                        },
                    )),
                }) {
                    tracing::error!("TcpConnect: Error sending Connected response for {set}: {e}");
                    return;
                }

                tracing::debug!("Spawning recv task for {set}");
                if let Err(e) = run_tcp_recv(adapter, set, responses).await {
                    tracing::error!("TCP recv loop for {set} ended with error: {e}");
                }
            }
            Ok(TcpStreamResponse::Reset { set }) => {
                tracing::warn!("Connection to remote for {set} reset");
                if let Err(e) = responses.send(ExitNodeResponse {
                    pair: Some(set.into()),
                    response: Some(exit_node_response::Response::TcpResponse(
                        data::TcpResponse {
                            response: Some(data::tcp_response::Response::ConnectionClosed(
                                data::TcpConnectionClosedResponse {},
                            )),
                        },
                    )),
                }) {
                    tracing::error!(
                        "TcpConnect: Error sending ConnectionRefused response for {set}: {e}"
                    );
                }
            }
            Ok(TcpStreamResponse::Refused { set }) => {
                tracing::warn!("Connection to remote for {set} refused");
                if let Err(e) = responses.send(ExitNodeResponse {
                    pair: Some(set.into()),
                    response: Some(exit_node_response::Response::TcpResponse(
                        data::TcpResponse {
                            response: Some(data::tcp_response::Response::ConnectionRefused(
                                data::TcpConnectionRefusedResponse {},
                            )),
                        },
                    )),
                }) {
                    tracing::error!(
                        "TcpConnect: Error sending ConnectionRefused response for {set}: {e}"
                    );
                }
            }
            Err(e) => {
                tracing::error!("Connection to remote for {set} failed with runtime error: {e}");
                if let Err(send_e) = responses.send(ExitNodeResponse {
                    pair: Some(set.into()),
                    response: Some(exit_node_response::Response::RuntimeError(
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
    });

    Ok(())
}

async fn run_tcp_recv<A: ExitAdapter>(
    adapter: Arc<A>,
    set: SocketSet,
    responses: MetricsSender,
) -> Result<(), Error> {
    let mtu = 1500;
    let mut recv_buf = vec![0; mtu];

    let session = match adapter.tcp_recv_session(set) {
        Ok(Some(session)) => session,
        Ok(None) => {
            tracing::error!("No TCP recv session available for {set}");
            return Ok(());
        }
        Err(e) => {
            tracing::error!("Error getting TCP recv session for {set}: {e}");
            return Ok(());
        }
    };

    loop {
        match session.recv(&mut recv_buf).await {
            Ok(sessions::common::SessionStatus::DataIo { size }) => {
                tracing::trace!("Received {size} bytes {set}. DataRecv");
                responses.send(ExitNodeResponse {
                    pair: Some(set.into()),
                    response: Some(exit_node_response::Response::TcpResponse(
                        data::TcpResponse {
                            response: Some(data::tcp_response::Response::DataRecv(
                                data::TcpDataRecvResponse {
                                    data: Bytes::copy_from_slice(&recv_buf[..size]),
                                    fin: false,
                                },
                            )),
                        },
                    )),
                })?;
            }
            Ok(sessions::common::SessionStatus::PeerClosed) => {
                tracing::trace!("Peer closed {set}. Sending DataRecv with fin");
                responses.send(ExitNodeResponse {
                    pair: Some(set.into()),
                    response: Some(exit_node_response::Response::TcpResponse(
                        data::TcpResponse {
                            response: Some(data::tcp_response::Response::DataRecv(
                                data::TcpDataRecvResponse {
                                    data: Bytes::new(),
                                    fin: true,
                                },
                            )),
                        },
                    )),
                })?;
                break;
            }
            Err(e) => {
                tracing::error!("Error receiving TCP data for {set}: {e}");
                responses.send(ExitNodeResponse {
                    pair: Some(set.into()),
                    response: Some(exit_node_response::Response::RuntimeError(
                        data::RuntimeErrorResponse {
                            reason: e.to_string(),
                        },
                    )),
                })?;
                break;
            }
        }
    }

    tracing::trace!("Recv task for {set} finished.");
    Ok(())
}

fn handle_tcp_send<A: ExitAdapter>(
    instr: TcpSendInstruction,
    adapter: &Arc<A>,
    responses: &MetricsSender,
    tasks: &mut JoinSet<()>,
) -> Result<(), Error> {
    let set = extract_socket_set(instr.pair)?;
    tracing::trace!("Received TcpSendData request: {set} fin={}", instr.fin);

    let adapter = Arc::clone(adapter);
    let responses = responses.clone();

    tasks.spawn(async move {
        let result: Result<(), Error> = async {
            let response = adapter.tcp_send(set, &instr.data, instr.fin).await?;
            responses.send(response.into())?;
            Ok(())
        }
        .await;
        if let Err(e) = result {
            tracing::error!("TcpSend error for {set}: {e}");
        }
    });

    Ok(())
}

fn handle_tcp_close<A: ExitAdapter>(
    instr: TcpCloseInstruction,
    adapter: &Arc<A>,
    responses: &MetricsSender,
) -> Result<(), Error> {
    let set = extract_socket_set(instr.pair)?;
    adapter.tcp_close(set)?;

    responses.send(ExitNodeResponse {
        pair: Some(set.into()),
        response: Some(exit_node_response::Response::TcpResponse(
            data::TcpResponse {
                response: Some(data::tcp_response::Response::ConnectionClosed(
                    data::TcpConnectionClosedResponse {},
                )),
            },
        )),
    })?;

    Ok(())
}

fn handle_udp_send<A: ExitAdapter>(
    instr: UdpSendInstruction,
    adapter: &Arc<A>,
    responses: &MetricsSender,
    tasks: &mut JoinSet<()>,
) -> Result<(), Error> {
    let set = extract_socket_set(instr.pair)?;

    let adapter = Arc::clone(adapter);
    let responses = responses.clone();
    let data = instr.data;

    tasks.spawn(async move {
        let result: Result<(), Error> = async {
            let response = adapter.udp_send(set, &data).await?;

            let is_new = matches!(
                response,
                SendResponse::Ok {
                    is_new: Some(true),
                    ..
                }
            );

            if is_new {
                tracing::debug!("Spawning udp_recv task");
                if let Err(e) = run_udp_recv(adapter, set, responses).await {
                    tracing::error!("UDP recv for {set} ended with error: {e}");
                }
            }

            Ok(())
        }
        .await;
        if let Err(e) = result {
            tracing::error!("UdpSend error for {set}: {e}");
        }
    });

    Ok(())
}

async fn run_udp_recv<A: ExitAdapter>(
    adapter: Arc<A>,
    set: SocketSet,
    responses: MetricsSender,
) -> Result<(), Error> {
    let session = match adapter.udp_recv_session(set) {
        Ok(Some(session)) => session,
        Ok(None) => {
            tracing::error!("No UDP recv session available for {set}");
            return Ok(());
        }
        Err(e) => {
            tracing::error!("Error getting UDP recv session for {set}: {e}");
            return Ok(());
        }
    };

    let mtu = 1500;
    let mut recv_buf = vec![0; mtu];

    loop {
        match session.recv(&mut recv_buf).await {
            Ok(sessions::common::SessionStatus::DataIo { size }) => {
                tracing::debug!("Received {size} bytes from UDP session {set}");
                responses.send(ExitNodeResponse {
                    pair: Some(set.into()),
                    response: Some(exit_node_response::Response::UdpResponse(
                        data::UdpResponse {
                            response: Some(data::udp_response::Response::DataRecv(
                                data::UdpDataRecvResponse {
                                    data: Bytes::copy_from_slice(&recv_buf[..size]),
                                },
                            )),
                        },
                    )),
                })?;
            }
            Ok(sessions::common::SessionStatus::PeerClosed) => {
                tracing::debug!("UDP session {set} closed");
                break;
            }
            Err(e) => {
                tracing::error!("UDP recv error for {set}: {e}");
                responses.send(ExitNodeResponse {
                    pair: Some(set.into()),
                    response: Some(exit_node_response::Response::RuntimeError(
                        RuntimeErrorResponse {
                            reason: e.to_string(),
                        },
                    )),
                })?;
                break;
            }
        }
    }

    tracing::trace!("UDP recv task for {set} finished.");
    Ok(())
}

#[cfg(unix)]
fn handle_icmp_send<A: ExitAdapter>(
    instr: IcmpSendInstruction,
    adapter: &Arc<A>,
    responses: &MetricsSender,
    tasks: &mut JoinSet<()>,
) -> Result<(), Error> {
    let set = extract_socket_set(instr.pair)?;

    let Some(IcmpMessage::IcmpEchoRequest(IcmpEchoRequest {
        seq_no,
        ident,
        data,
    })) = instr.icmp_message
    else {
        tracing::error!("Invalid IcmpSendInstruction {:?}", instr.icmp_message);
        return Err(Error::InvalidInstruction);
    };

    tracing::trace!("Received IcmpSendEchoRequest: {}", set);

    let adapter = Arc::clone(adapter);
    let responses = responses.clone();

    tasks.spawn(async move {
        let result: Result<(), Error> = async {
            #[allow(clippy::cast_possible_truncation)]
            let session = match adapter.icmp_session(set, ident as u16).await {
                Ok(Some(session)) => session,
                Ok(None) => {
                    tracing::error!("No ICMP session available for {set}");
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
                    tracing::trace!("Received {} byte ICMP response: {:?}", size, recv_buf);
                }
                sessions::common::SessionStatus::PeerClosed => {
                    tracing::warn!("ICMP session closed by peer");
                    recv_buf.clear();
                }
            }

            responses.send(ExitNodeResponse {
                pair: Some(set.into()),
                response: Some(exit_node_response::Response::IcmpResponse(
                    data::IcmpResponse {
                        response: Some(icmp_response::Response::DataRecv(
                            data::IcmpDataRecvResponse {
                                data: recv_buf.into(),
                                echo_ident: ident,
                            },
                        )),
                    },
                )),
            })?;

            Ok(())
        }
        .await;
        if let Err(e) = result {
            tracing::error!("ICMP send error for {set}: {e}");
        }
    });

    Ok(())
}

fn handle_tcp_listen<A: ExitAdapter>(
    instr: TcpListenInstruction,
    adapter: &Arc<A>,
    responses: &MetricsSender,
    tasks: &mut JoinSet<()>,
) -> Result<(), Error> {
    let set = extract_socket_set(instr.pair)?;

    let adapter = Arc::clone(adapter);
    let responses = responses.clone();

    tracing::debug!("Spawning tcp_listen task for pair: {:?}", set);
    tasks.spawn(async move {
        match adapter.tcp_listen(set).await {
            Ok(tcp_listen_response) => {
                tracing::debug!("tcp_listen successful for pair: {:?}", set);
                if let Err(e) = responses.send(tcp_listen_response.into()) {
                    tracing::error!("TcpListen Error sending response on channel: {e}");
                }
            }
            Err(e) => {
                tracing::error!("Error in tcp_listen for pair {:?}: {e}", set);
                let response = ExitNodeResponse {
                    pair: Some(set.into()),
                    response: Some(exit_node_response::Response::RuntimeError(
                        RuntimeErrorResponse {
                            reason: e.to_string(),
                        },
                    )),
                };
                if let Err(e) = responses.send(response) {
                    tracing::error!("TcpListen Error sending error response on channel: {e}");
                }
            }
        }
    });

    Ok(())
}

fn handle_tcp_listen_close<A: ExitAdapter>(
    instr: TcpListenCloseInstruction,
    adapter: &Arc<A>,
    responses: &MetricsSender,
    tasks: &mut JoinSet<()>,
) -> Result<(), Error> {
    let set = extract_socket_set(instr.pair)?;

    let adapter = Arc::clone(adapter);
    let responses = responses.clone();

    tracing::debug!("Spawning tcp_listen_close task for pair: {:?}", set);
    tasks.spawn(async move {
        match adapter.tcp_listen_close(set).await {
            Ok(tcp_listen_close_response) => {
                tracing::debug!("tcp_listen_close successful for pair: {:?}", set);
                if let Err(e) = responses.send(tcp_listen_close_response.into()) {
                    tracing::error!("TcpListenClose Error sending response on channel: {e}");
                }
            }
            Err(e) => {
                tracing::error!("Error in tcp_listen_close for pair {:?}: {e}", set);
                let response = ExitNodeResponse {
                    pair: Some(set.into()),
                    response: Some(exit_node_response::Response::RuntimeError(
                        RuntimeErrorResponse {
                            reason: e.to_string(),
                        },
                    )),
                };
                if let Err(e) = responses.send(response) {
                    tracing::error!("TcpListenClose Error sending error response on channel: {e}");
                }
            }
        }
    });

    Ok(())
}
