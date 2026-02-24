//! WebSocket yamux stream wrappers.

use std::{
	io,
	pin::Pin,
	task::{Context, Poll},
};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio_util::compat::FuturesAsyncReadCompatExt;
use yamux::Stream as YamuxStream;

use crate::{BiStream, TransportError};

/// A bidirectional yamux stream wrapped for tokio compatibility.
pub struct WebSocketBiStream {
	pub(super) inner: tokio_util::compat::Compat<YamuxStream>,
}

impl WebSocketBiStream {
	pub(super) fn new(inner: YamuxStream) -> Self {
		Self {
			inner: inner.compat(),
		}
	}
}

impl AsyncRead for WebSocketBiStream {
	fn poll_read(
		mut self: Pin<&mut Self>,
		cx: &mut Context<'_>,
		buf: &mut ReadBuf<'_>,
	) -> Poll<io::Result<()>> {
		Pin::new(&mut self.inner).poll_read(cx, buf)
	}
}

impl AsyncWrite for WebSocketBiStream {
	fn poll_write(
		mut self: Pin<&mut Self>,
		cx: &mut Context<'_>,
		buf: &[u8],
	) -> Poll<io::Result<usize>> {
		Pin::new(&mut self.inner).poll_write(cx, buf)
	}

	fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
		Pin::new(&mut self.inner).poll_flush(cx)
	}

	fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
		Pin::new(&mut self.inner).poll_shutdown(cx)
	}
}

impl BiStream for WebSocketBiStream {
	async fn finish(&mut self) -> Result<(), TransportError> {
		tokio::io::AsyncWriteExt::shutdown(&mut self.inner)
			.await
			.map_err(|e| TransportError::stream(e.to_string()))
	}
}

/// A unidirectional send stream (for `open_uni`).
pub struct WebSocketSendStream {
	pub(super) inner: tokio_util::compat::Compat<YamuxStream>,
}

impl WebSocketSendStream {
	pub(super) fn new(inner: YamuxStream) -> Self {
		Self {
			inner: inner.compat(),
		}
	}
}

impl AsyncWrite for WebSocketSendStream {
	fn poll_write(
		mut self: Pin<&mut Self>,
		cx: &mut Context<'_>,
		buf: &[u8],
	) -> Poll<io::Result<usize>> {
		Pin::new(&mut self.inner).poll_write(cx, buf)
	}

	fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
		Pin::new(&mut self.inner).poll_flush(cx)
	}

	fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
		Pin::new(&mut self.inner).poll_shutdown(cx)
	}
}

/// A unidirectional receive stream (for `accept_uni`).
pub struct WebSocketRecvStream {
	pub(super) inner: tokio_util::compat::Compat<YamuxStream>,
}

impl WebSocketRecvStream {
	pub(super) fn new(inner: YamuxStream) -> Self {
		Self {
			inner: inner.compat(),
		}
	}
}

impl AsyncRead for WebSocketRecvStream {
	fn poll_read(
		mut self: Pin<&mut Self>,
		cx: &mut Context<'_>,
		buf: &mut ReadBuf<'_>,
	) -> Poll<io::Result<()>> {
		Pin::new(&mut self.inner).poll_read(cx, buf)
	}
}
