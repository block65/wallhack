//! WebSocket byte stream adapter.
//!
//! Adapts a message-framed [`WebSocketStream`] into a byte-oriented
//! [`AsyncRead`] + [`AsyncWrite`] stream suitable for yamux multiplexing.

use std::{
	io,
	pin::Pin,
	task::{Context, Poll},
};

use futures::{sink::Sink, stream::Stream};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio_tungstenite::tungstenite::Message;

/// A byte stream adapter over a WebSocket connection.
///
/// This adapter converts the message-based WebSocket protocol into a continuous
/// byte stream suitable for use with stream multiplexers like yamux.
///
/// # Implementation Notes
///
/// - Reads buffer partial message data for consumption across multiple read
///   calls
/// - Writes send binary messages for each write operation
/// - Only binary WebSocket messages are used for data; text/ping/pong/close are
///   handled separately
pub struct WsByteStream<S> {
	inner: S,
	read_buf: Vec<u8>,
	read_pos: usize,
}

impl<S> WsByteStream<S> {
	/// Creates a new byte stream adapter over the given WebSocket stream.
	#[must_use]
	pub fn new(inner: S) -> Self {
		Self {
			inner,
			read_buf: Vec::new(),
			read_pos: 0,
		}
	}

	/// Returns a reference to the underlying WebSocket stream.
	#[must_use]
	pub fn inner(&self) -> &S {
		&self.inner
	}

	/// Returns a mutable reference to the underlying WebSocket stream.
	pub fn inner_mut(&mut self) -> &mut S {
		&mut self.inner
	}

	/// Consumes this adapter and returns the underlying WebSocket stream.
	#[must_use]
	pub fn into_inner(self) -> S {
		self.inner
	}
}

impl<S> AsyncRead for WsByteStream<S>
where
	S: Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
	fn poll_read(
		mut self: Pin<&mut Self>,
		cx: &mut Context<'_>,
		buf: &mut ReadBuf<'_>,
	) -> Poll<io::Result<()>> {
		// First, drain any buffered data from a previous message
		if self.read_pos < self.read_buf.len() {
			let remaining = &self.read_buf[self.read_pos..];
			let to_copy = remaining.len().min(buf.remaining());
			buf.put_slice(&remaining[..to_copy]);
			self.read_pos += to_copy;

			// If we've consumed the entire buffer, clear it
			if self.read_pos >= self.read_buf.len() {
				self.read_buf.clear();
				self.read_pos = 0;
			}

			return Poll::Ready(Ok(()));
		}

		// Buffer is empty, read the next WebSocket message
		loop {
			match Pin::new(&mut self.inner).poll_next(cx) {
				Poll::Ready(Some(Ok(msg))) => {
					match msg {
						Message::Binary(data) => {
							if data.is_empty() {
								continue;
							}

							let to_copy = data.len().min(buf.remaining());
							buf.put_slice(&data[..to_copy]);

							// Buffer any remaining data
							if to_copy < data.len() {
								self.read_buf = data.into();
								self.read_pos = to_copy;
							}

							return Poll::Ready(Ok(()));
						}
						Message::Close(_) => {
							// Connection closed
							return Poll::Ready(Ok(()));
						}
						Message::Ping(_)
						| Message::Pong(_)
						| Message::Text(_)
						| Message::Frame(_) => {}
					}
				}
				Poll::Ready(Some(Err(e))) => {
					return Poll::Ready(Err(io::Error::other(e.to_string())));
				}
				Poll::Ready(None) => {
					// Stream ended
					return Poll::Ready(Ok(()));
				}
				Poll::Pending => {
					return Poll::Pending;
				}
			}
		}
	}
}

impl<S> AsyncWrite for WsByteStream<S>
where
	S: Sink<Message, Error = tokio_tungstenite::tungstenite::Error> + Unpin,
{
	fn poll_write(
		mut self: Pin<&mut Self>,
		cx: &mut Context<'_>,
		buf: &[u8],
	) -> Poll<io::Result<usize>> {
		// First ensure the sink is ready to receive
		match Pin::new(&mut self.inner).poll_ready(cx) {
			Poll::Ready(Ok(())) => {}
			Poll::Ready(Err(e)) => {
				return Poll::Ready(Err(io::Error::other(e.to_string())));
			}
			Poll::Pending => {
				return Poll::Pending;
			}
		}

		// Send the data as a binary message
		let msg = Message::Binary(buf.to_vec().into());
		match Pin::new(&mut self.inner).start_send(msg) {
			Ok(()) => Poll::Ready(Ok(buf.len())),
			Err(e) => Poll::Ready(Err(io::Error::other(e.to_string()))),
		}
	}

	fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
		Pin::new(&mut self.inner)
			.poll_flush(cx)
			.map_err(|e| io::Error::other(e.to_string()))
	}

	fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
		// Send a close message
		match Pin::new(&mut self.inner).poll_ready(cx) {
			Poll::Ready(Ok(())) => {}
			Poll::Ready(Err(e)) => {
				return Poll::Ready(Err(io::Error::other(e.to_string())));
			}
			Poll::Pending => {
				return Poll::Pending;
			}
		}

		if let Err(e) = Pin::new(&mut self.inner).start_send(Message::Close(None)) {
			return Poll::Ready(Err(io::Error::other(e.to_string())));
		}

		Pin::new(&mut self.inner)
			.poll_close(cx)
			.map_err(|e| io::Error::other(e.to_string()))
	}
}
