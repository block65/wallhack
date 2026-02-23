use wallhack_wire::{
	SocketSet,
	v2::{
		self, RuntimeErrorResponse, TcpConnectionClosedResponse, TcpConnectionRefusedResponse,
		TcpListenerClosedResponse, TcpListenerListeningResponse, TcpOkResponse, TcpResponse,
		exit_node_response, tcp_response,
	},
};

#[cfg(unix)]
use crate::sessions::icmp::IcmpSession;
use crate::{
	session_key::SessionKey,
	sessions::{tcp::TcpSession, udp::UdpSession},
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
	ProtobufConversion(#[from] wallhack_wire::ConversionError),
	// #[error("smoltcp wire error")]
	// SmoltcpWire(#[from] smoltcp::wire::Error),
}

#[derive(Debug)]
pub enum TcpStreamResponse {
	// None { set: SocketSet },
	Connected { set: SocketSet },
	Refused { set: SocketSet },
	Reset { set: SocketSet },
}

impl From<TcpStreamResponse> for exit_node_response::Response {
	fn from(response: TcpStreamResponse) -> Self {
		match response {
			TcpStreamResponse::Connected { .. } => {
				exit_node_response::Response::TcpResponse(v2::TcpResponse {
					response: Some(tcp_response::Response::Connected(
						v2::TcpConnectedResponse {},
					)),
				})
			}
			TcpStreamResponse::Refused { .. } => {
				exit_node_response::Response::TcpResponse(TcpResponse {
					response: Some(tcp_response::Response::ConnectionRefused(
						TcpConnectionRefusedResponse {},
					)),
				})
			}
			TcpStreamResponse::Reset { .. } => {
				exit_node_response::Response::TcpResponse(v2::TcpResponse {
					response: Some(tcp_response::Response::ConnectionClosed(
						TcpConnectionClosedResponse {},
					)),
				})
			}
		}
	}
}

impl From<TcpStreamResponse> for v2::ExitNodeResponse {
	fn from(response: TcpStreamResponse) -> Self {
		match response {
			TcpStreamResponse::Connected { set } => v2::ExitNodeResponse {
				response: Some(exit_node_response::Response::TcpResponse(v2::TcpResponse {
					response: Some(tcp_response::Response::Connected(
						v2::TcpConnectedResponse {},
					)),
				})),
				pair: Some(set.into()),
			},
			TcpStreamResponse::Refused { set } => v2::ExitNodeResponse {
				response: Some(exit_node_response::Response::TcpResponse(v2::TcpResponse {
					response: Some(tcp_response::Response::ConnectionRefused(
						TcpConnectionRefusedResponse {},
					)),
				})),
				pair: Some(set.into()),
			},
			TcpStreamResponse::Reset { set } => v2::ExitNodeResponse {
				response: Some(v2::exit_node_response::Response::TcpResponse(
					v2::TcpResponse {
						response: Some(tcp_response::Response::ConnectionClosed(
							TcpConnectionClosedResponse {},
						)),
					},
				)),
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

impl From<SendResponse> for exit_node_response::Response {
	fn from(value: SendResponse) -> Self {
		match value {
			SendResponse::Ok { .. } => exit_node_response::Response::TcpResponse(TcpResponse {
				response: Some(tcp_response::Response::Ok(TcpOkResponse {})),
			}),
			SendResponse::Reset { .. } => exit_node_response::Response::TcpResponse(TcpResponse {
				response: Some(tcp_response::Response::ConnectionClosed(
					TcpConnectionClosedResponse {},
				)),
			}),
			SendResponse::RuntimeError { e, .. } => {
				exit_node_response::Response::RuntimeError(RuntimeErrorResponse { reason: e })
			}
		}
	}
}

impl From<SendResponse> for v2::ExitNodeResponse {
	fn from(response: SendResponse) -> Self {
		match response {
			SendResponse::Ok { set, .. } => v2::ExitNodeResponse {
				response: Some(exit_node_response::Response::TcpResponse(TcpResponse {
					response: Some(tcp_response::Response::Ok(TcpOkResponse {})),
				})),
				pair: Some(set.into()),
			},
			SendResponse::Reset { set, .. } => v2::ExitNodeResponse {
				response: Some(exit_node_response::Response::TcpResponse(TcpResponse {
					response: Some(tcp_response::Response::ConnectionClosed(
						TcpConnectionClosedResponse {},
					)),
				})),
				pair: Some(set.into()),
			},
			SendResponse::RuntimeError { set: pair, e } => v2::ExitNodeResponse {
				response: Some(exit_node_response::Response::RuntimeError(
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

impl From<TcpCloseResponse> for exit_node_response::Response {
	fn from(response: TcpCloseResponse) -> Self {
		match response {
			TcpCloseResponse::Ok { .. } => exit_node_response::Response::TcpResponse(TcpResponse {
				response: Some(tcp_response::Response::ConnectionClosed(
					TcpConnectionClosedResponse {},
				)),
			}),
			TcpCloseResponse::Reset { .. } => {
				// pair and reason are not used in the protobuf message variant
				exit_node_response::Response::TcpResponse(TcpResponse {
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

impl From<TcpListenResponse> for exit_node_response::Response {
	fn from(response: TcpListenResponse) -> Self {
		match response {
			TcpListenResponse::Ok => exit_node_response::Response::TcpResponse(TcpResponse {
				response: Some(tcp_response::Response::Listening(
					TcpListenerListeningResponse {},
				)),
			}),
			TcpListenResponse::Reset { .. } => {
				// reason is not used in the protobuf message variant
				exit_node_response::Response::TcpResponse(TcpResponse {
					response: Some(tcp_response::Response::ListenerClosed(
						TcpListenerClosedResponse {},
					)),
				})
			}
		}
	}
}

impl From<TcpListenResponse> for v2::ExitNodeResponse {
	fn from(response: TcpListenResponse) -> Self {
		match response {
			TcpListenResponse::Ok => v2::ExitNodeResponse {
				response: Some(exit_node_response::Response::TcpResponse(TcpResponse {
					response: Some(tcp_response::Response::Listening(
						TcpListenerListeningResponse {},
					)),
				})),
				pair: None,
			},
			TcpListenResponse::Reset {
				pair: set,
				reason: _,
			} => v2::ExitNodeResponse {
				response: Some(exit_node_response::Response::TcpResponse(TcpResponse {
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

impl From<TcpListenCloseResponse> for exit_node_response::Response {
	fn from(response: TcpListenCloseResponse) -> Self {
		match response {
			TcpListenCloseResponse::Ok { .. } => {
				exit_node_response::Response::TcpResponse(TcpResponse {
					response: Some(tcp_response::Response::ListenerClosed(
						TcpListenerClosedResponse {},
					)),
				})
			}
			TcpListenCloseResponse::Reset { .. } => {
				exit_node_response::Response::TcpResponse(TcpResponse {
					response: Some(tcp_response::Response::ListenerClosed(
						TcpListenerClosedResponse {},
					)),
				})
			}
		}
	}
}

impl From<TcpListenCloseResponse> for v2::ExitNodeResponse {
	fn from(response: TcpListenCloseResponse) -> Self {
		match response {
			TcpListenCloseResponse::Ok { set } => v2::ExitNodeResponse {
				response: Some(exit_node_response::Response::TcpResponse(TcpResponse {
					response: Some(tcp_response::Response::ListenerClosed(
						TcpListenerClosedResponse {},
					)),
				})),
				pair: Some(set.into()),
			},
			TcpListenCloseResponse::Reset { set, reason: _ } => v2::ExitNodeResponse {
				response: Some(exit_node_response::Response::TcpResponse(TcpResponse {
					response: Some(tcp_response::Response::ListenerClosed(
						TcpListenerClosedResponse {},
					)),
				})),
				pair: Some(set.into()), // Populate the outer pair
			},
		}
	}
}

pub trait ExitAdapter: Send + Sync + 'static {
	/// # Errors
	fn tcp_close(&self, pair: SocketSet) -> Result<TcpCloseResponse, RuntimeError>;

	fn udp_send(
		&self,
		set: SocketSet,
		data: &[u8],
	) -> impl std::future::Future<Output = Result<SendResponse, RuntimeError>> + Send;

	fn udp_recv_session(&self, set: SocketSet) -> Result<Option<UdpSession>, RuntimeError>;

	// fn tcp_connect_req(&self, set: SocketSet) -> Result<TcpConnectResponse, RuntimeError>;

	fn tcp_connect(
		&self,
		set: SocketSet,
	) -> impl std::future::Future<Output = Result<TcpStreamResponse, RuntimeError>> + Send;

	fn tcp_send(
		&self,
		set: SocketSet,
		data: &[u8],
		fin: bool,
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

	#[cfg(unix)]
	fn icmp_session(
		&self,
		set: SocketSet,
		ident: u16,
	) -> impl std::future::Future<Output = Result<Option<IcmpSession>, RuntimeError>> + Send;
}
