//! QUIC transport implementation.
//!
//! Wraps [`quinn::Connection`] to implement the [`Transport`] trait.

use std::{
	net::SocketAddr,
	pin::Pin,
	task::{Context, Poll},
};

use quinn::{RecvStream, SendStream};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

use super::{BiStream, Transport, TransportError};

/// A bidirectional QUIC stream.
///
/// Combines a [`SendStream`] and [`RecvStream`] into a single bidirectional stream.
pub struct QuicBiStream {
	send: SendStream,
	recv: RecvStream,
}

impl QuicBiStream {
	/// Creates a new bidirectional stream from QUIC send and receive streams.
	#[must_use]
	pub fn new(send: SendStream, recv: RecvStream) -> Self {
		Self { send, recv }
	}
}

impl AsyncRead for QuicBiStream {
	fn poll_read(
		mut self: Pin<&mut Self>,
		cx: &mut Context<'_>,
		buf: &mut ReadBuf<'_>,
	) -> Poll<std::io::Result<()>> {
		Pin::new(&mut self.recv).poll_read(cx, buf)
	}
}

impl AsyncWrite for QuicBiStream {
	fn poll_write(
		mut self: Pin<&mut Self>,
		cx: &mut Context<'_>,
		buf: &[u8],
	) -> Poll<std::io::Result<usize>> {
		Pin::new(&mut self.send)
			.poll_write(cx, buf)
			.map_err(|e| std::io::Error::other(e.to_string()))
	}

	fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
		Pin::new(&mut self.send)
			.poll_flush(cx)
			.map_err(|e| std::io::Error::other(e.to_string()))
	}

	fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
		Pin::new(&mut self.send)
			.poll_shutdown(cx)
			.map_err(|e| std::io::Error::other(e.to_string()))
	}
}

impl BiStream for QuicBiStream {
	async fn finish(&mut self) -> Result<(), TransportError> {
		self.send
			.finish()
			.map_err(|e| TransportError::stream(e.to_string()))
	}
}

/// QUIC transport wrapping a [`quinn::Connection`].
pub struct QuicTransport {
	connection: quinn::Connection,
}

impl QuicTransport {
	/// Creates a new QUIC transport from an established connection.
	#[must_use]
	pub fn new(connection: quinn::Connection) -> Self {
		Self { connection }
	}

	/// Returns a reference to the underlying QUIC connection.
	#[must_use]
	pub fn connection(&self) -> &quinn::Connection {
		&self.connection
	}
}

impl Transport for QuicTransport {
	type SendStream = SendStream;
	type RecvStream = RecvStream;
	type BiStream = QuicBiStream;

	async fn open_uni(&self) -> Result<Self::SendStream, TransportError> {
		self.connection
			.open_uni()
			.await
			.map_err(|e| TransportError::connection_closed(e.to_string()))
	}

	async fn open_bi(&self) -> Result<Self::BiStream, TransportError> {
		let (send, recv) = self
			.connection
			.open_bi()
			.await
			.map_err(|e| TransportError::connection_closed(e.to_string()))?;
		Ok(QuicBiStream::new(send, recv))
	}

	async fn accept_uni(&self) -> Result<Option<Self::RecvStream>, TransportError> {
		match self.connection.accept_uni().await {
			Ok(stream) => Ok(Some(stream)),
			Err(
				quinn::ConnectionError::ApplicationClosed(_)
				| quinn::ConnectionError::LocallyClosed,
			) => Ok(None),
			Err(e) => Err(TransportError::connection_closed(e.to_string())),
		}
	}

	async fn accept_bi(&self) -> Result<Option<Self::BiStream>, TransportError> {
		match self.connection.accept_bi().await {
			Ok((send, recv)) => Ok(Some(QuicBiStream::new(send, recv))),
			Err(
				quinn::ConnectionError::ApplicationClosed(_)
				| quinn::ConnectionError::LocallyClosed,
			) => Ok(None),
			Err(e) => Err(TransportError::connection_closed(e.to_string())),
		}
	}

	async fn close(&self) -> Result<(), TransportError> {
		self.connection.close(0u32.into(), b"closing");
		Ok(())
	}

	fn remote_addr(&self) -> Option<SocketAddr> {
		Some(self.connection.remote_address())
	}
}
