use std::net::SocketAddr;

use crate::adapter::RuntimeError;

use super::common::{RxSession, SessionStatus};

#[derive(Debug, Clone)]
pub struct TcpSession {
	stream: std::sync::Arc<tokio::net::TcpStream>,
}

impl TcpSession {
	pub fn new(stream: tokio::net::TcpStream) -> Self {
		Self {
			stream: std::sync::Arc::new(stream),
		}
	}
}

impl RxSession for TcpSession {
	async fn send(
		&self,
		_dst_addr: SocketAddr,
		buf: &mut [u8],
	) -> Result<SessionStatus, RuntimeError> {
		if buf.is_empty() {
			return Ok(SessionStatus::DataIo { size: 0 });
		}
		loop {
			tracing::debug!("Attempting to send data over TCP stream...");
			self.stream.writable().await?;

			match self.stream.try_write(buf) {
				Ok(0) => {
					tracing::debug!("Wrote 0 bytes, peer likely closed connection.");
					return Ok(SessionStatus::PeerClosed);
				}
				Ok(n) => {
					tracing::debug!("Successfully wrote {n} bytes.");
					return Ok(SessionStatus::DataIo { size: n });
				}
				Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
					tracing::warn!("Operation would block, retrying.");
					continue;
				}
				Err(e) => {
					return Err(e.into());
				}
			}
		}
	}

	async fn recv(&self, buf: &mut [u8]) -> Result<SessionStatus, RuntimeError> {
		loop {
			tracing::debug!("Attempting to read from TCP stream...");
			self.stream.readable().await?;

			match self.stream.try_read(buf) {
				Ok(0) => {
					tracing::debug!("Read 0 bytes, peer likely closed connection.");
					return Ok(SessionStatus::PeerClosed);
				}
				Ok(n) => {
					tracing::debug!("Successfully read {n} bytes.");
					return Ok(SessionStatus::DataIo { size: n });
				}
				Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
					tracing::warn!("Operation would block, retrying.");
					continue;
				}
				Err(e) => {
					return Err(e.into());
				}
			}
		}
	}
}

// WARNING: This file contains AI-generated edits
