use std::{
	io,
	pin::Pin,
	sync::Arc,
	task::{Context, Poll},
};

use smoltcp::{iface::SocketHandle, phy::Device, socket::tcp, time::Instant as SmolInstant};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

use super::Shared;

/// An async TCP stream backed by the smoltcp stack.
///
/// Implements [`AsyncRead`] and [`AsyncWrite`]. When the stream is dropped
/// the underlying socket is aborted and the poll loop is notified.
pub struct TcpStream<D: Device + Send + 'static> {
	shared: Arc<Shared<D>>,
	handle: SocketHandle,
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

	/// Returns the current TCP state.
	///
	/// # Panics
	///
	/// Panics if the mutex is poisoned.
	#[must_use]
	pub fn state(&self) -> tcp::State {
		let inner = self.shared.inner.lock().expect("mutex poisoned");
		inner.tcp_socket(self.handle).state()
	}
}

impl<D: Device + Send + 'static> AsyncRead for TcpStream<D> {
	fn poll_read(
		self: Pin<&mut Self>,
		cx: &mut Context<'_>,
		buf: &mut ReadBuf<'_>,
	) -> Poll<io::Result<()>> {
		let mut inner = self.shared.inner.lock().expect("mutex poisoned");
		let socket: &mut tcp::Socket<'_> = inner.sockets_mut().get_mut(self.handle);

		socket.register_recv_waker(cx.waker());

		if !socket.may_recv() {
			// Connection closed for reading
			return Poll::Ready(Ok(()));
		}

		match socket.recv_slice(buf.initialize_unfilled()) {
			Ok(0) => Poll::Pending,
			Ok(n) => {
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
		let mut inner = self.shared.inner.lock().expect("mutex poisoned");
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
		let mut inner = self.shared.inner.lock().expect("mutex poisoned");
		let socket: &mut tcp::Socket<'_> = inner.sockets_mut().get_mut(self.handle);
		socket.close();
		drop(inner);
		self.shared.notify.notify_one();
		Poll::Ready(Ok(()))
	}
}

impl<D: Device + Send + 'static> Drop for TcpStream<D> {
	fn drop(&mut self) {
		if let Ok(mut inner) = self.shared.inner.lock() {
			let now = SmolInstant::from_millis(
				i64::try_from(
					std::time::SystemTime::now()
						.duration_since(std::time::UNIX_EPOCH)
						.unwrap_or_default()
						.as_millis(),
				)
				.unwrap_or(0),
			);
			let socket: &mut tcp::Socket<'_> = inner.sockets_mut().get_mut(self.handle);
			socket.abort();
			inner.poll(now);
		}
		self.shared.notify.notify_one();
	}
}
