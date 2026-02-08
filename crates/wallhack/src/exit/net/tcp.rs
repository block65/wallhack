use std::io;

use super::adapter::{SyscallExitAdapter, TimestampedSession};

use exit_adapter::{
	SocketSet,
	adapter::{
		RuntimeError, SendResponse, TcpCloseResponse, TcpListenCloseResponse, TcpListenResponse,
		TcpStreamResponse,
	},
	session::Session,
	session_key::SessionKey,
	sessions::{self, common::RxSession},
};

impl SyscallExitAdapter {
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
	) -> Result<TcpStreamResponse, RuntimeError> {
		tracing::debug!("Received connect {}", set);

		let (_, dst_addr) = set.into();

		match tokio::net::TcpStream::connect(dst_addr).await {
			Ok(stream) => {
				tracing::debug!("Connected to {}", dst_addr);
				let key = SessionKey::Tcp(set);
				self.sessions.insert(
					key,
					TimestampedSession::new(Session::Tcp(sessions::tcp::TcpSession::new(stream))),
				);
				Ok(TcpStreamResponse::Connected { set })
			}
			Err(e) => match e.kind() {
				io::ErrorKind::ConnectionRefused => Ok(TcpStreamResponse::Refused { set }),
				io::ErrorKind::ConnectionReset
				| io::ErrorKind::ConnectionAborted
				| io::ErrorKind::BrokenPipe => Ok(TcpStreamResponse::Reset { set }),
				_ => Err(e.into()),
			},
		}
	}

	pub async fn tcp_send_impl(
		&self,
		set: SocketSet,
		buf: &[u8],
		fin: bool,
	) -> Result<SendResponse, RuntimeError> {
		tracing::trace!("Received send data request: {:?}", set,);

		let key = SessionKey::Tcp(set);

		let (_, dest) = set.into();

		let response = {
			let maybe_session = self.sessions.get(&key);

			match maybe_session {
				Some(session) => {
					if let Session::Tcp(session) = &session.value().session {
						// Write data if present
						if !buf.is_empty() {
							tracing::trace!("Sending data");
							match session.send(dest, buf).await {
								Ok(sessions::common::SessionStatus::DataIo { size, .. }) => {
									tracing::trace!("Sent {size} bytes to socket");
									if fin {
										tracing::trace!("Shutting down write side for {set}");
										session.shutdown_write()?;
									}
									SendResponse::Ok {
										size,
										set,
										is_new: None,
									}
								}
								Ok(sessions::common::SessionStatus::PeerClosed) => {
									SendResponse::Reset {
										set,
										reason: "peer closed".to_string(),
									}
								}
								Err(e) => return Err(e),
							}
						} else if fin {
							// No data, just shutdown
							tracing::trace!("FIN-only: shutting down write side for {set}");
							session.shutdown_write()?;
							SendResponse::Ok {
								size: 0,
								set,
								is_new: None,
							}
						} else {
							// No data and no fin — nothing to do
							SendResponse::Ok {
								size: 0,
								set,
								is_new: None,
							}
						}
					} else {
						return Err(RuntimeError::SessionInvalid(key));
					}
				}
				None => SendResponse::Reset {
					set,
					reason: "session disappeared".to_string(),
				},
			}
		};

		// Touch session to update last_activity
		if let Some(mut session) = self.sessions.get_mut(&key) {
			session.touch();
		}

		Ok(response)
	}

	pub fn tcp_recv_session_impl(
		&self,
		set: SocketSet,
	) -> Result<Option<sessions::tcp::TcpSession>, RuntimeError> {
		let key = SessionKey::Tcp(set);
		let maybe_session = self.sessions.get(&key);
		match maybe_session {
			Some(session) => {
				if let Session::Tcp(session) = &session.value().session {
					Ok(Some(session.clone()))
				} else {
					Err(RuntimeError::SessionInvalid(key))
				}
			}
			None => Ok(None),
		}
	}

	pub fn tcp_listen_impl(&self, _set: SocketSet) -> Result<TcpListenResponse, RuntimeError> {
		Err(io::Error::new(io::ErrorKind::Unsupported, "tcp_listen not yet implemented").into())
	}

	pub fn tcp_listen_close_impl(
		&self,
		_set: SocketSet,
	) -> Result<TcpListenCloseResponse, RuntimeError> {
		Err(io::Error::new(
			io::ErrorKind::Unsupported,
			"tcp_listen_close not yet implemented",
		)
		.into())
	}
}
