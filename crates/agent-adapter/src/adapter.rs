use protobuf::{
	SocketSet,
	v2::{
		self, RuntimeErrorResponse, TcpConnectionClosedResponse, TcpConnectionRefusedResponse,
		TcpListenerClosedResponse, TcpListenerListeningResponse, TcpResponse, TcpSendOkResponse,
		agent_response, tcp_response,
	},
};

use crate::{
	session_key::SessionKey,
	sessions::{icmp::IcmpSession, tcp::TcpSession, udp::UdpSession},
};

/// These must only be serious runtime errors, other "errors" like connection
/// refused should be handled gracefully with a Response enum
#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
	#[error(transparent)]
	Io(#[from] std::io::Error),

	#[error("unexpected session protocol for {0:?}. This is probably a bug.")]
	SessionInvalid(SessionKey),

	#[error("conversion error")]
	ConversionError,

	#[error("internal timeout error")]
	InternalTimeout(#[from] tokio::time::error::Elapsed),

	#[error("conversion error")]
	ProtobufConversion(#[from] protobuf::ConversionError),
	// #[error("smoltcp wire error")]
	// SmoltcpWire(#[from] smoltcp::wire::Error),
}

#[derive(Debug)]
pub enum TcpConnectResponse {
	// None { set: SocketSet },
	Connected { set: SocketSet },
	Refused { set: SocketSet },
	Reset { set: SocketSet },
}

impl From<TcpConnectResponse> for agent_response::Response {
	fn from(response: TcpConnectResponse) -> Self {
		match response {
			TcpConnectResponse::Connected { .. } => {
				agent_response::Response::TcpResponse(v2::TcpResponse {
					response: Some(tcp_response::Response::Connected(
						v2::TcpConnectedResponse {},
					)),
				})
			}
			TcpConnectResponse::Refused { .. } => {
				agent_response::Response::TcpResponse(TcpResponse {
					response: Some(tcp_response::Response::ConnectionRefused(
						TcpConnectionRefusedResponse {},
					)),
				})
			}
			TcpConnectResponse::Reset { .. } => {
				agent_response::Response::TcpResponse(v2::TcpResponse {
					response: Some(tcp_response::Response::ConnectionClosed(
						TcpConnectionClosedResponse {},
					)),
				})
			}
		}
	}
}

impl From<TcpConnectResponse> for v2::AgentResponse {
	fn from(response: TcpConnectResponse) -> Self {
		match response {
			TcpConnectResponse::Connected { set } => v2::AgentResponse {
				response: Some(agent_response::Response::TcpResponse(v2::TcpResponse {
					response: Some(tcp_response::Response::Connected(
						v2::TcpConnectedResponse {},
					)),
				})),
				pair: Some(set.into()),
			},
			TcpConnectResponse::Refused { set } => v2::AgentResponse {
				response: Some(agent_response::Response::TcpResponse(v2::TcpResponse {
					response: Some(tcp_response::Response::ConnectionRefused(
						TcpConnectionRefusedResponse {},
					)),
				})),
				pair: Some(set.into()),
			},
			TcpConnectResponse::Reset { set } => v2::AgentResponse {
				response: Some(v2::agent_response::Response::TcpResponse(v2::TcpResponse {
					response: Some(tcp_response::Response::ConnectionClosed(
						TcpConnectionClosedResponse {},
					)),
				})),
				pair: Some(set.into()),
			},
		}
	}
}

#[derive(Debug)]
pub enum SendResponse {
	Ok {
		set: SocketSet,
		size: usize,
		is_new: Option<bool>,
	},
	Reset {
		set: SocketSet,
		reason: String,
	},
	RuntimeError {
		set: SocketSet,
		e: String,
	},
}

impl From<SendResponse> for agent_response::Response {
	fn from(value: SendResponse) -> Self {
		match value {
			SendResponse::Ok { .. } => agent_response::Response::TcpResponse(TcpResponse {
				response: Some(tcp_response::Response::SendOk(TcpSendOkResponse {})),
			}),
			SendResponse::Reset { .. } => agent_response::Response::TcpResponse(TcpResponse {
				response: Some(tcp_response::Response::ConnectionClosed(
					TcpConnectionClosedResponse {},
				)),
			}),
			SendResponse::RuntimeError { e, .. } => {
				agent_response::Response::RuntimeError(RuntimeErrorResponse { reason: e })
			}
		}
	}
}

impl From<SendResponse> for v2::AgentResponse {
	fn from(response: SendResponse) -> Self {
		match response {
			SendResponse::Ok { set, .. } => v2::AgentResponse {
				response: Some(agent_response::Response::TcpResponse(TcpResponse {
					response: Some(tcp_response::Response::SendOk(TcpSendOkResponse {})),
				})),
				pair: Some(set.into()),
			},
			SendResponse::Reset { set, .. } => v2::AgentResponse {
				response: Some(agent_response::Response::TcpResponse(TcpResponse {
					response: Some(tcp_response::Response::ConnectionClosed(
						TcpConnectionClosedResponse {},
					)),
				})),
				pair: Some(set.into()),
			},
			SendResponse::RuntimeError { set: pair, e } => v2::AgentResponse {
				response: Some(agent_response::Response::RuntimeError(
					RuntimeErrorResponse { reason: e },
				)),
				pair: Some(pair.into()),
			},
		}
	}
}

//TcpCloseResponse
#[derive(Debug)]
pub enum TcpCloseResponse {
	Ok { pair: SocketSet },
	Reset { reason: String, pair: SocketSet },
}

impl From<TcpCloseResponse> for agent_response::Response {
	fn from(response: TcpCloseResponse) -> Self {
		match response {
			TcpCloseResponse::Ok { .. } => agent_response::Response::TcpResponse(TcpResponse {
				response: Some(tcp_response::Response::ConnectionClosed(
					TcpConnectionClosedResponse {},
				)),
			}),
			TcpCloseResponse::Reset { .. } => {
				// pair and reason are not used in the protobuf message variant
				agent_response::Response::TcpResponse(TcpResponse {
					response: Some(tcp_response::Response::ConnectionClosed(
						TcpConnectionClosedResponse {},
					)),
				})
			}
		}
	}
}

#[derive(Debug)]
pub enum TcpListenResponse {
	Ok,
	Reset { reason: String, pair: SocketSet },
}

impl From<TcpListenResponse> for agent_response::Response {
	fn from(response: TcpListenResponse) -> Self {
		match response {
			TcpListenResponse::Ok => agent_response::Response::TcpResponse(TcpResponse {
				response: Some(tcp_response::Response::Listening(
					TcpListenerListeningResponse {},
				)),
			}),
			TcpListenResponse::Reset { .. } => {
				// reason is not used in the protobuf message variant
				agent_response::Response::TcpResponse(TcpResponse {
					response: Some(tcp_response::Response::ListenerClosed(
						TcpListenerClosedResponse {},
					)),
				})
			}
		}
	}
}

impl From<TcpListenResponse> for v2::AgentResponse {
	fn from(response: TcpListenResponse) -> Self {
		match response {
			TcpListenResponse::Ok => v2::AgentResponse {
				response: Some(agent_response::Response::TcpResponse(TcpResponse {
					response: Some(tcp_response::Response::Listening(
						TcpListenerListeningResponse {},
					)),
				})),
				pair: None,
			},
			TcpListenResponse::Reset {
				pair: set,
				reason: _,
			} => v2::AgentResponse {
				response: Some(agent_response::Response::TcpResponse(TcpResponse {
					response: Some(tcp_response::Response::ListenerClosed(
						TcpListenerClosedResponse {},
					)),
				})),
				pair: Some(set.into()), // Populate the outer pair
			},
		}
	}
}

#[derive(Debug)]
pub enum TcpListenCloseResponse {
	Ok { set: SocketSet },
	Reset { set: SocketSet, reason: String },
}

impl From<TcpListenCloseResponse> for agent_response::Response {
	fn from(response: TcpListenCloseResponse) -> Self {
		match response {
			TcpListenCloseResponse::Ok { .. } => {
				agent_response::Response::TcpResponse(TcpResponse {
					response: Some(tcp_response::Response::ListenerClosed(
						TcpListenerClosedResponse {},
					)),
				})
			}
			TcpListenCloseResponse::Reset { .. } => {
				agent_response::Response::TcpResponse(TcpResponse {
					response: Some(tcp_response::Response::ListenerClosed(
						TcpListenerClosedResponse {},
					)),
				})
			}
		}
	}
}

impl From<TcpListenCloseResponse> for v2::AgentResponse {
	fn from(response: TcpListenCloseResponse) -> Self {
		match response {
			TcpListenCloseResponse::Ok { set } => v2::AgentResponse {
				response: Some(agent_response::Response::TcpResponse(TcpResponse {
					response: Some(tcp_response::Response::ListenerClosed(
						TcpListenerClosedResponse {},
					)),
				})),
				pair: Some(set.into()),
			},
			TcpListenCloseResponse::Reset { set, reason: _ } => v2::AgentResponse {
				response: Some(agent_response::Response::TcpResponse(TcpResponse {
					response: Some(tcp_response::Response::ListenerClosed(
						TcpListenerClosedResponse {},
					)),
				})),
				pair: Some(set.into()), // Populate the outer pair
			},
		}
	}
}

pub trait AgentAdapter: Send + Sync + 'static {
	/// # Errors
	fn tcp_close(&self, pair: SocketSet) -> Result<TcpCloseResponse, RuntimeError>;

	fn udp_send(
		&self,
		set: SocketSet,
		data: &mut [u8],
	) -> impl std::future::Future<Output = Result<SendResponse, RuntimeError>> + Send;

	fn udp_recv_session(&self, set: SocketSet) -> Result<Option<UdpSession>, RuntimeError>;

	// fn tcp_connect_req(&self, set: SocketSet) -> Result<TcpConnectResponse, RuntimeError>;

	fn tcp_connect(
		&self,
		set: SocketSet,
	) -> impl std::future::Future<Output = Result<TcpConnectResponse, RuntimeError>> + Send;

	fn tcp_send(
		&self,
		set: SocketSet,
		data: Vec<u8>,
	) -> impl std::future::Future<Output = Result<SendResponse, RuntimeError>> + Send;

	fn tcp_recv_session(&self, set: SocketSet) -> Result<Option<TcpSession>, RuntimeError>;

	fn tcp_listen(
		&self,
		set: SocketSet,
	) -> impl std::future::Future<Output = Result<TcpListenResponse, RuntimeError>> + Send;

	fn tcp_listen_close(
		&self,
		set: SocketSet,
	) -> impl std::future::Future<Output = Result<TcpListenCloseResponse, RuntimeError>> + Send;

	fn icmp_session(
		&self,
		set: SocketSet,
		ident: u16,
	) -> impl std::future::Future<Output = Result<Option<IcmpSession>, RuntimeError>> + Send;
}
