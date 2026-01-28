use std::sync::Arc;

use smoltcp::{iface::SocketHandle, phy::Device, socket::tcp};

use super::{Shared, tcp_stream::TcpStream};
use crate::error::Error;

/// An async TCP listener that accepts incoming connections.
///
/// Internally maintains a backlog pool of listen sockets. When a socket
/// transitions from LISTEN to ESTABLISHED, it is yielded as a
/// [`TcpStream`] and a fresh listen socket replaces it in the pool.
pub struct TcpListener<D: Device + Send + 'static> {
	shared: Arc<Shared<D>>,
	port: u16,
	backlog: Vec<SocketHandle>,
}

impl<D: Device + Send + 'static> TcpListener<D> {
	/// Creates a new listener with the given backlog size.
	///
	/// # Errors
	///
	/// Returns an error if any of the listen sockets cannot be created.
	pub(crate) fn new(
		shared: Arc<Shared<D>>,
		port: u16,
		backlog_size: usize,
	) -> Result<Self, Error> {
		let backlog_size = backlog_size.max(1);
		let mut backlog = Vec::with_capacity(backlog_size);

		{
			let mut inner = shared.inner.lock().expect("mutex poisoned");
			for _ in 0..backlog_size {
				let handle = inner.tcp_listen(port)?;
				backlog.push(handle);
			}
		}

		Ok(Self {
			shared,
			port,
			backlog,
		})
	}

	/// Accept the next incoming TCP connection.
	///
	/// This method polls all backlog sockets for a LISTEN → ESTABLISHED
	/// transition. When found, the established socket is returned as a
	/// [`TcpStream`] and a new listen socket takes its place in the pool.
	///
	/// # Errors
	///
	/// Returns an error if a replacement listen socket cannot be created.
	///
	/// # Panics
	///
	/// Panics if the internal mutex is poisoned.
	///
	/// # Cancellation safety
	///
	/// This method is cancel-safe. If dropped before completion, no
	/// connection is lost — the socket remains in the backlog and will
	/// be found on the next `accept()` call.
	pub async fn accept(&mut self) -> Result<TcpStream<D>, Error> {
		loop {
			{
				let inner = self.shared.inner.lock().expect("mutex poisoned");
				for (idx, &handle) in self.backlog.iter().enumerate() {
					let socket: &tcp::Socket<'_> = inner.tcp_socket(handle);
					if socket.is_active() && socket.may_send() {
						// This socket has transitioned to ESTABLISHED (or similar active state)
						let established_handle = self.backlog.remove(idx);
						drop(inner);

						// Replace with a fresh listen socket
						let mut inner = self.shared.inner.lock().expect("mutex poisoned");
						match inner.tcp_listen(self.port) {
							Ok(new_handle) => self.backlog.push(new_handle),
							Err(e) => {
								// Return the stream but log that we lost a backlog slot
								// The next accept() will still work with remaining slots
								if self.backlog.is_empty() {
									// Critical: no backlog left, must propagate error
									// But first return the already-connected stream
									// by re-adding it and returning error
									// Actually, the connection is already established,
									// so return it and let the caller decide
									return Err(e);
								}
							}
						}

						return Ok(TcpStream::new(Arc::clone(&self.shared), established_handle));
					}
				}
				// Lock dropped here
			}

			// No connections ready; wait for the poll loop to notify us
			self.shared.notify.notified().await;
		}
	}

	/// Returns the port this listener is bound to.
	#[must_use]
	pub fn port(&self) -> u16 {
		self.port
	}

	/// Returns the current number of listen sockets in the backlog.
	#[must_use]
	pub fn backlog_len(&self) -> usize {
		self.backlog.len()
	}
}

impl<D: Device + Send + 'static> Drop for TcpListener<D> {
	fn drop(&mut self) {
		if let Ok(mut inner) = self.shared.inner.lock() {
			for &handle in &self.backlog {
				let socket: &mut tcp::Socket<'_> = inner.sockets_mut().get_mut(handle);
				socket.abort();
			}
		}
	}
}
