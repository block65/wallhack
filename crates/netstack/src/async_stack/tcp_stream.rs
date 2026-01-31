use std::{
	io,
	pin::Pin,
	sync::Arc,
	task::{Context, Poll},
};

use smoltcp::{iface::SocketHandle, phy::Device, socket::tcp};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

use super::Shared;
use crate::inner::InnerStack;

/// An async TCP stream backed by the smoltcp stack.
///
/// Implements [`AsyncRead`] and [`AsyncWrite`]. When the stream is dropped the
/// underlying socket is aborted and the poll loop is notified.
pub struct TcpStream<D: Device + Send + 'static> {
	shared: Arc<Shared<D>>,
	handle: SocketHandle,
}

/// Check if a socket handle exists in the socket set.
fn socket_exists<D: Device>(inner: &InnerStack<D>, handle: SocketHandle) -> bool {
	inner.sockets().iter().any(|(h, _)| h == handle)
}

impl<D: Device + Send + 'static> TcpStream<D> {
	pub(crate) fn new(shared: Arc<Shared<D>>, handle: SocketHandle) -> Self {
		Self { shared, handle }
	}

	/// Returns the socket handle.
	#[must_use]
	pub fn handle(&self) -> SocketHandle {
		self.handle
	}

	/// Returns the current TCP state, or Closed if socket was pruned.
	///
	/// # Panics
	///
	/// Panics if the mutex is poisoned.
	#[must_use]
	pub fn state(&self) -> tcp::State {
		let inner = self.shared.inner.lock();
		if !socket_exists(&inner, self.handle) {
			return tcp::State::Closed;
		}
		inner.tcp_socket(self.handle).state()
	}

	/// Returns the remote endpoint for this stream, if connected.
	///
	/// # Panics
	///
	/// Panics if the mutex is poisoned.
	#[must_use]
	pub fn remote_endpoint(&self) -> Option<smoltcp::wire::IpEndpoint> {
		let inner = self.shared.inner.lock();
		if !socket_exists(&inner, self.handle) {
			return None;
		}
		inner.tcp_socket(self.handle).remote_endpoint()
	}

	/// Returns the local endpoint for this stream, if connected.
	///
	/// # Panics
	///
	/// Panics if the mutex is poisoned.
	#[must_use]
	pub fn local_endpoint(&self) -> Option<smoltcp::wire::IpEndpoint> {
		let inner = self.shared.inner.lock();
		if !socket_exists(&inner, self.handle) {
			return None;
		}
		inner.tcp_socket(self.handle).local_endpoint()
	}
}

impl<D: Device + Send + 'static> AsyncRead for TcpStream<D> {
	fn poll_read(
		self: Pin<&mut Self>,
		cx: &mut Context<'_>,
		buf: &mut ReadBuf<'_>,
	) -> Poll<io::Result<()>> {
		let mut inner = self.shared.inner.lock();

		// Check if socket still exists (may have been pruned)
		if !socket_exists(&inner, self.handle) {
			return Poll::Ready(Err(io::Error::new(
				io::ErrorKind::NotConnected,
				"socket was closed",
			)));
		}

		let socket: &mut tcp::Socket<'_> = inner.sockets_mut().get_mut(self.handle);

		socket.register_recv_waker(cx.waker());

		if !socket.may_recv() {
			// Connection closed for reading
			return Poll::Ready(Ok(()));
		}

		match socket.recv_slice(buf.initialize_unfilled()) {
			Ok(0) => Poll::Pending,
			Ok(n) => {
				tracing::trace!(bytes = n, "TcpStream recv");
				buf.advance(n);
				drop(inner);
				self.shared.notify.notify_one();
				Poll::Ready(Ok(()))
			}
			Err(tcp::RecvError::Finished) => Poll::Ready(Ok(())),
			Err(tcp::RecvError::InvalidState) => Poll::Ready(Err(io::Error::new(
				io::ErrorKind::NotConnected,
				"recv: invalid state",
			))),
		}
	}
}

impl<D: Device + Send + 'static> AsyncWrite for TcpStream<D> {
	fn poll_write(
		self: Pin<&mut Self>,
		cx: &mut Context<'_>,
		buf: &[u8],
	) -> Poll<io::Result<usize>> {
		let mut inner = self.shared.inner.lock();

		// Check if socket still exists (may have been pruned)
		if !socket_exists(&inner, self.handle) {
			return Poll::Ready(Err(io::Error::new(
				io::ErrorKind::NotConnected,
				"socket was closed",
			)));
		}

		let socket: &mut tcp::Socket<'_> = inner.sockets_mut().get_mut(self.handle);

		socket.register_send_waker(cx.waker());

		if !socket.may_send() {
			return Poll::Ready(Err(io::Error::new(
				io::ErrorKind::BrokenPipe,
				"send: socket not writable",
			)));
		}

		match socket.send_slice(buf) {
			Ok(0) => Poll::Pending,
			Ok(n) => {
				tracing::trace!(bytes = n, "TcpStream send");
				drop(inner);
				self.shared.notify.notify_one();
				Poll::Ready(Ok(n))
			}
			Err(tcp::SendError::InvalidState) => Poll::Ready(Err(io::Error::new(
				io::ErrorKind::NotConnected,
				"send: invalid state",
			))),
		}
	}

	fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
		// smoltcp flushes on poll(), which is handled by the poll loop
		self.shared.notify.notify_one();
		Poll::Ready(Ok(()))
	}

	fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
		let mut inner = self.shared.inner.lock();

		// Check if socket still exists (may have been pruned)
		if !socket_exists(&inner, self.handle) {
			return Poll::Ready(Ok(())); // Already gone, consider it shutdown
		}

		let socket: &mut tcp::Socket<'_> = inner.sockets_mut().get_mut(self.handle);
		socket.close();
		drop(inner);
		self.shared.notify.notify_one();
		Poll::Ready(Ok(()))
	}
}

impl<D: Device + Send + 'static> Drop for TcpStream<D> {
	fn drop(&mut self) {
		let mut inner = self.shared.inner.lock();
		// Check if socket still exists before trying to access it
		let exists = inner.sockets().iter().any(|(h, _)| h == self.handle);
		if !exists {
			tracing::trace!(handle = ?self.handle, "TcpStream dropped but socket already gone");
			return;
		}

		// Abort the socket (sends RST, transitions to Closed)
		// Do NOT remove - let prune_closed_tcp_sockets clean it up after
		// the RST has been sent by the next poll() cycle
		let socket: &mut tcp::Socket<'_> = inner.sockets_mut().get_mut(self.handle);
		socket.abort();
		tracing::debug!(handle = ?self.handle, "TcpStream dropped, socket aborted");
		drop(inner);
		self.shared.notify.notify_one();
	}
}
