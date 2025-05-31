use std::net::SocketAddr;

use crate::adapter::RuntimeError;

use super::common::{RxSession, SessionStatus};

#[derive(Debug, Clone)]
pub struct TcpSession {
	stream: std::sync::Arc<tokio::net::TcpStream>,
	// set: SocketSet,
}

impl TcpSession {
	pub fn new(stream: tokio::net::TcpStream /* set: SocketSet */) -> Self {
		Self {
			stream: std::sync::Arc::new(stream),
			// set,
		}
	}
}

impl RxSession for TcpSession {
	async fn send(
		&self,
		_dst_addr: SocketAddr,
		buf: &mut [u8],
	) -> Result<SessionStatus, RuntimeError> {
		loop {
			if let Err(io_err) = self.stream.writable().await {
				return match io_err.kind() {
					std::io::ErrorKind::BrokenPipe
					| std::io::ErrorKind::ConnectionReset
					| std::io::ErrorKind::ConnectionAborted => Ok(SessionStatus::PeerClosed),
					_ => Err(RuntimeError::from(io_err)),
				};
			}

			match self.stream.try_write(buf) {
				Ok(0) => return Ok(SessionStatus::PeerClosed),
				Ok(n) => return Ok(SessionStatus::DataIo { size: n }),
				Err(io_err) if io_err.kind() == std::io::ErrorKind::WouldBlock => {
					tracing::warn!("Operation would block, retrying.");
					// try again
				}
				Err(io_err) => {
					return match io_err.kind() {
						std::io::ErrorKind::BrokenPipe
						| std::io::ErrorKind::ConnectionReset
						| std::io::ErrorKind::ConnectionAborted => Ok(SessionStatus::PeerClosed),
						_ => Err(RuntimeError::from(io_err)),
					};
				}
			}
		}
	}

	async fn recv(&self, buf: &mut [u8]) -> Result<SessionStatus, RuntimeError> {
		// loop handles the retry if would block
		loop {
			if let Err(io_err) = self.stream.readable().await {
				return match io_err.kind() {
					std::io::ErrorKind::BrokenPipe
					| std::io::ErrorKind::ConnectionReset
					| std::io::ErrorKind::ConnectionAborted => Ok(SessionStatus::PeerClosed),
					_ => Err(RuntimeError::from(io_err)),
				};
			}

			match self.stream.try_read(buf) {
				Ok(0) => return Ok(SessionStatus::PeerClosed),
				Ok(n) => return Ok(SessionStatus::DataIo { size: n }),
				Err(io_err) if io_err.kind() == std::io::ErrorKind::WouldBlock => {
					tracing::warn!("Operation would block, retrying.");
					// try again
				}
				Err(io_err) => {
					return match io_err.kind() {
						std::io::ErrorKind::BrokenPipe
						| std::io::ErrorKind::ConnectionReset
						| std::io::ErrorKind::ConnectionAborted => Ok(SessionStatus::PeerClosed),
						_ => Err(RuntimeError::from(io_err)),
					};
				}
			}
		}
	}
}
