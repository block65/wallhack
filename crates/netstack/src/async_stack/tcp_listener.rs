use std::sync::Arc;

use smoltcp::{iface::SocketHandle, phy::Device, socket::tcp};

use super::{Shared, tcp_stream::TcpStream};
use crate::error::Error;

/// A TCP listener for a specific port backed by the smoltcp stack.
///
/// Uses a simple "seen list" approach - iterates all sockets in the stack,
/// returns ESTABLISHED ones that haven't been returned before.
pub struct TcpListener<D: Device + Send + 'static> {
	shared: Arc<Shared<D>>,
	port: u16,
	/// Handles we've already returned - don't return again
	seen: std::collections::HashSet<SocketHandle>,
}

impl<D: Device + Send + 'static> TcpListener<D> {
	/// Creates a new listener for the given port.
	pub(crate) fn new(shared: Arc<Shared<D>>, port: u16, _backlog_size: usize) -> Self {
		Self {
			shared,
			port,
			seen: std::collections::HashSet::new(),
		}
	}

	/// Accept the next incoming TCP connection.
	///
	/// Waits until a connection is available.
	///
	/// # Errors
	///
	/// Returns an error if the listener cannot poll for connections.
	pub async fn accept(&mut self) -> Result<TcpStream<D>, Error> {
		loop {
			if let Some(stream) = self.poll_accept()? {
				return Ok(stream);
			}
			self.shared.notify.notified().await;
		}
	}

	/// Poll for a new incoming TCP connection without waiting.
	///
	/// Returns `Ok(None)` if no connection is ready yet.
	///
	/// # Errors
	///
	/// Currently always returns `Ok`. Reserved for future error conditions.
	///
	/// # Panics
	///
	/// Panics if the internal mutex is poisoned (another thread panicked while holding the lock).
	pub fn poll_accept(&mut self) -> Result<Option<TcpStream<D>>, Error> {
		let inner = self.shared.inner.lock();

		// Clean up seen set - remove handles that no longer exist or are closed
		self.seen.retain(|&h| {
			inner.sockets().iter().any(|(handle, socket)| {
				if handle != h {
					return false;
				}
				let smoltcp::socket::Socket::Tcp(tcp) = socket else {
					return false;
				};
				!matches!(tcp.state(), tcp::State::Closed | tcp::State::TimeWait)
			})
		});

		// Find an ESTABLISHED socket for our port that we haven't seen
		for (handle, socket) in inner.sockets().iter() {
			let smoltcp::socket::Socket::Tcp(tcp_socket) = socket else {
				continue;
			};

			if tcp_socket.listen_endpoint().port != self.port {
				continue;
			}

			if self.seen.contains(&handle) {
				continue;
			}

			// Accept if established (or about to be)
			if tcp_socket.is_active() && tcp_socket.may_send() {
				tracing::debug!(port = self.port, ?handle, state = ?tcp_socket.state(), "Accepting connection");
				self.seen.insert(handle);
				return Ok(Some(TcpStream::new(Arc::clone(&self.shared), handle)));
			}
		}

		Ok(None)
	}

	/// Returns the port this listener is bound to.
	#[must_use]
	pub fn port(&self) -> u16 {
		self.port
	}
}

impl<D: Device + Send + 'static> Drop for TcpListener<D> {
	fn drop(&mut self) {
		// Nothing to clean up - sockets are managed by JIT
	}
}
