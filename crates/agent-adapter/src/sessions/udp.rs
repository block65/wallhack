use std::{net::SocketAddr, sync::Arc};

use crate::adapter::RuntimeError;

use super::common::{RxSession, SessionStatus};

#[derive(Debug, Clone)]
pub struct UdpSession {
	socket: Arc<tokio::net::UdpSocket>,
	// set: SocketSet,
}

impl UdpSession {
	pub fn new(socket: tokio::net::UdpSocket /* set: SocketSet */) -> Self {
		Self {
			socket: Arc::new(socket),
			// set,
		}
	}
}

impl RxSession for UdpSession {
	async fn send(&self, dest: SocketAddr, buf: &mut [u8]) -> Result<SessionStatus, RuntimeError> {
		loop {
			// Wait for the socket to be writable
			if let Err(io_err) = self.socket.writable().await {
				return match io_err.kind() {
					std::io::ErrorKind::BrokenPipe
					| std::io::ErrorKind::ConnectionReset
					| std::io::ErrorKind::ConnectionAborted => Ok(SessionStatus::PeerClosed),
					_ => Err(RuntimeError::from(io_err)),
				};
			}

			// Try to send the data
			match self.socket.send_to(buf, dest).await {
				Ok(n) => return Ok(SessionStatus::DataIo { size: n }),
				Err(io_err) if io_err.kind() == std::io::ErrorKind::WouldBlock => {
					tracing::warn!("Operation would block, retrying.");
					// continue;
				}
				Err(io_err) => {
					tracing::warn!("io_err {:?}", io_err);
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
		loop {
			if let Err(io_err) = self.socket.readable().await {
				return match io_err.kind() {
					std::io::ErrorKind::BrokenPipe
					| std::io::ErrorKind::ConnectionReset
					| std::io::ErrorKind::ConnectionAborted => Ok(SessionStatus::PeerClosed),
					_ => Err(RuntimeError::from(io_err)),
				};
			}

			match self.socket.recv(buf).await {
				Ok(n) => return Ok(SessionStatus::DataIo { size: n }),
				Err(io_err) if io_err.kind() == std::io::ErrorKind::WouldBlock => {
					tracing::warn!("Operation would block, retrying. {:?}", io_err);
					// continue;
				}
				Err(io_err) => {
					tracing::warn!("io_err {:?}", io_err);

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
// WARNING: This file contains AI-generated edits
