use std::io;

use super::adapter::SyscallAgentAdapter;

use agent_adapter::{
	SocketSet,
	adapter::{
		RuntimeError, SendResponse, TcpCloseResponse, TcpConnectResponse, TcpListenCloseResponse,
		TcpListenResponse,
	},
	session::Session,
	session_key::SessionKey,
	sessions::{self, common::RxSession},
};

impl SyscallAgentAdapter {
	pub fn tcp_close_impl(&self, pair: SocketSet) -> Result<TcpCloseResponse, RuntimeError> {
		tracing::debug!("Received close request {}", pair);

		let key = SessionKey::Tcp(pair);
		let maybe_session = self.sessions.remove(&key);

		let response = if maybe_session.is_some() {
			tracing::debug!("closed session for pair {}", pair);
			TcpCloseResponse::Ok { pair }
		} else {
			tracing::debug!("session not found: {}", pair);
			TcpCloseResponse::Reset {
				pair,
				reason: "session not found".to_string(),
			}
		};

		Ok(response)
	}

	pub async fn tcp_connect_impl(
		&self,
		set: SocketSet,
	) -> Result<TcpConnectResponse, RuntimeError> {
		tracing::debug!("Received connect {}", set);

		let (_, dst_addr) = set.into();

		match tokio::net::TcpStream::connect(dst_addr).await {
			Ok(stream) => {
				tracing::debug!("Connected to {}", dst_addr);
				let key = SessionKey::Tcp(set);
				self.sessions
					.insert(key, Session::Tcp(sessions::tcp::TcpSession::new(stream)));
				Ok(TcpConnectResponse::Connected { set })
			}
			Err(e) => match e.kind() {
				io::ErrorKind::ConnectionRefused => Ok(TcpConnectResponse::Refused { set }),
				io::ErrorKind::ConnectionReset
				| io::ErrorKind::ConnectionAborted
				| io::ErrorKind::BrokenPipe => Ok(TcpConnectResponse::Reset { set }),
				_ => Err(e.into()),
			},
		}
	}

	pub async fn tcp_send_impl(
		&self,
		set: SocketSet,
		mut buf: Vec<u8>,
	) -> Result<SendResponse, RuntimeError> {
		tracing::trace!("Received send data request: {:?}", set,);

		let key = SessionKey::Tcp(set);
		let maybe_session = self.sessions.get(&key);
		tracing::trace!("Flow: {:?}", maybe_session);

		let (_, dest) = set.into();

		let response = match maybe_session {
			Some(session) => {
				if let Session::Tcp(session) = session.value() {
					tracing::trace!("Sending data");
					match session.send(dest, &mut buf).await {
						Ok(sessions::common::SessionStatus::DataIo { size, .. }) => {
							tracing::trace!("Sent {} bytes to socket", size);
							SendResponse::Ok {
								size,
								set,
								is_new: None,
							}
						}
						Ok(sessions::common::SessionStatus::PeerClosed) => SendResponse::Reset {
							set,
							reason: "peer closed".to_string(),
						},
						Err(e) => return Err(e),
					}
				} else {
					return Err(RuntimeError::SessionInvalid(key));
				}
			}
			None => SendResponse::Reset {
				set,
				reason: "session disappeared".to_string(),
			},
		};

		Ok(response)
	}

	pub fn tcp_recv_session_impl(
		&self,
		set: SocketSet,
	) -> Result<Option<sessions::tcp::TcpSession>, RuntimeError> {
		let key = SessionKey::Tcp(set);
		let maybe_session = self.sessions.get(&key);
		tracing::trace!("maybe_session: {:?}", maybe_session);
		match maybe_session {
			Some(session) => {
				if let Session::Tcp(session) = session.value() {
					Ok(Some(session.clone()))
				} else {
					// non-tcp session - should not happen
					Err(RuntimeError::SessionInvalid(key))
				}
			}
			None => Ok(None),
		}
	}

	pub fn tcp_listen_impl(&self, _set: SocketSet) -> Result<TcpListenResponse, RuntimeError> {
		todo!("Implement tcp_listen_impl");
	}

	pub fn tcp_listen_close_impl(
		&self,
		_set: SocketSet,
	) -> Result<TcpListenCloseResponse, RuntimeError> {
		todo!("Implement tcp_listen_close_impl");
	}
}
// WARNING: This file contains AI-generated edits
