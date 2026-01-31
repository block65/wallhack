use std::net::{Shutdown, SocketAddr};

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

	/// Perform a half-close (shutdown the write side) of the TCP stream.
	///
	/// # Errors
	/// Returns `RuntimeError::Io` if the underlying shutdown syscall fails.
	pub fn shutdown_write(&self) -> Result<(), RuntimeError> {
		let sock = socket2::SockRef::from(self.stream.as_ref());
		sock.shutdown(Shutdown::Write)?;
		Ok(())
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
			tracing::trace!("Attempting to send data over TCP stream...");
			self.stream.writable().await?;

			match self.stream.try_write(buf) {
				Ok(0) => {
					tracing::trace!("Wrote 0 bytes, peer likely closed connection.");
					return Ok(SessionStatus::PeerClosed);
				}
				Ok(n) => {
					tracing::trace!("Successfully wrote {n} bytes.");
					return Ok(SessionStatus::DataIo { size: n });
				}
				Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
					tracing::trace!("Operation would block, retrying.");
					tokio::task::yield_now().await;
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
			tracing::trace!("Attempting to read from TCP stream...");
			self.stream.readable().await?;

			match self.stream.try_read(buf) {
				Ok(0) => {
					tracing::trace!("Read 0 bytes, peer likely closed connection.");
					return Ok(SessionStatus::PeerClosed);
				}
				Ok(n) => {
					tracing::trace!("Successfully read {n} bytes.");
					return Ok(SessionStatus::DataIo { size: n });
				}
				Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
					tracing::trace!("Operation would block, retrying.");
					tokio::task::yield_now().await;
					continue;
				}
				Err(e) => {
					return Err(e.into());
				}
			}
		}
	}
}
