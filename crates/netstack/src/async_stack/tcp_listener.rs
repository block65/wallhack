use std::sync::Arc;

use smoltcp::{iface::SocketHandle, phy::Device, socket::tcp};

use super::{Shared, tcp_stream::TcpStream};
use crate::error::Error;

/// An async TCP listener that accepts incoming connections.
///
/// Internally maintains a backlog pool of listen sockets. When a socket
/// transitions from LISTEN to ESTABLISHED, it is yielded as a [`TcpStream`] and
/// a fresh listen socket replaces it in the pool.
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
		let mut found_handles = std::collections::HashSet::new();

		{
			let mut inner = shared.inner.lock().expect("mutex poisoned");

			// First, find any existing sockets for this port (JIT-created)
			let handle = inner.tcp_find_or_listen(port)?;
			backlog.push(handle);
			found_handles.insert(handle);

			// Create additional backlog sockets up to backlog_size
			// But avoid duplicates
			while backlog.len() < backlog_size {
				let handle = inner.tcp_listen(port)?;
				if found_handles.insert(handle) {
					backlog.push(handle);
				} else {
					// tcp_listen returned a duplicate, something is wrong
					break;
				}
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
	/// This method is cancel-safe. If dropped before completion, no connection is
	/// lost — the socket remains in the backlog and will be found on the next
	/// `accept()` call.
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
	/// Returns an error if the connection cannot be polled.
	///
	/// # Panics
	///
	/// Panics if the internal mutex is poisoned.
	pub fn poll_accept(&mut self) -> Result<Option<TcpStream<D>>, Error> {
		let inner = self.shared.inner.lock().expect("mutex poisoned");
		for (idx, &handle) in self.backlog.iter().enumerate() {
			let socket: &tcp::Socket<'_> = inner.tcp_socket(handle);
			if socket.is_active() && socket.may_send() {
				let established_handle = self.backlog.remove(idx);
				drop(inner);

				let mut inner = self.shared.inner.lock().expect("mutex poisoned");
				match inner.tcp_listen(self.port) {
					Ok(new_handle) => self.backlog.push(new_handle),
					Err(e) => {
						if self.backlog.is_empty() {
							return Err(e);
						}
					}
				}

				return Ok(Some(TcpStream::new(
					Arc::clone(&self.shared),
					established_handle,
				)));
			}
		}

		Ok(None)
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
