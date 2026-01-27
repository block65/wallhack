//! WebSocket transport with yamux multiplexing.
//!
//! This module implements the [`Transport`] trait over a WebSocket connection
//! using yamux for stream multiplexing.

use std::{
	io,
	net::SocketAddr,
	pin::Pin,
	sync::Arc,
	task::{Context, Poll},
};

use futures::{AsyncReadExt as FuturesAsyncReadExt, AsyncWriteExt as FuturesAsyncWriteExt};
use tokio::{
	io::{AsyncRead, AsyncWrite, ReadBuf},
	sync::{Mutex, mpsc},
};
use tokio_util::compat::{FuturesAsyncReadCompatExt, TokioAsyncReadCompatExt};
use yamux::{Config, Connection, ConnectionError, Mode, Stream as YamuxStream};

use super::{BiStream, Transport, TransportError};

/// Stream type prefix for data/unidirectional streams.
const STREAM_TYPE_DATA: u8 = 0x00;

/// Stream type prefix for control/bidirectional streams.
const STREAM_TYPE_CONTROL: u8 = 0x01;

/// A bidirectional yamux stream wrapped for tokio compatibility.
pub struct WsBiStream {
	inner: tokio_util::compat::Compat<YamuxStream>,
}

impl WsBiStream {
	fn new(inner: YamuxStream) -> Self {
		Self {
			inner: inner.compat(),
		}
	}
}

impl AsyncRead for WsBiStream {
	fn poll_read(
		mut self: Pin<&mut Self>,
		cx: &mut Context<'_>,
		buf: &mut ReadBuf<'_>,
	) -> Poll<io::Result<()>> {
		Pin::new(&mut self.inner).poll_read(cx, buf)
	}
}

impl AsyncWrite for WsBiStream {
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

impl BiStream for WsBiStream {
	async fn finish(&mut self) -> Result<(), TransportError> {
		tokio::io::AsyncWriteExt::shutdown(&mut self.inner)
			.await
			.map_err(|e| TransportError::stream(e.to_string()))
	}
}

/// A unidirectional send stream (for open_uni).
pub struct WsSendStream {
	inner: tokio_util::compat::Compat<YamuxStream>,
}

impl WsSendStream {
	fn new(inner: YamuxStream) -> Self {
		Self {
			inner: inner.compat(),
		}
	}
}

impl AsyncWrite for WsSendStream {
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

/// A unidirectional receive stream (for accept_uni).
pub struct WsRecvStream {
	inner: tokio_util::compat::Compat<YamuxStream>,
}

impl WsRecvStream {
	fn new(inner: YamuxStream) -> Self {
		Self {
			inner: inner.compat(),
		}
	}
}

impl AsyncRead for WsRecvStream {
	fn poll_read(
		mut self: Pin<&mut Self>,
		cx: &mut Context<'_>,
		buf: &mut ReadBuf<'_>,
	) -> Poll<io::Result<()>> {
		Pin::new(&mut self.inner).poll_read(cx, buf)
	}
}

/// WebSocket transport using yamux multiplexing.
///
/// # Driver Pattern
///
/// The yamux connection requires a background task to drive I/O. When creating
/// a `WsTransport`, you must spawn the returned driver future:
///
/// ```ignore
/// let (transport, driver) = WsTransport::new(ws_stream, Mode::Client, remote_addr);
/// tokio::spawn(driver);
/// ```
pub struct WsTransport {
	connection: Arc<Mutex<Connection<tokio_util::compat::Compat<Box<dyn TokioAsyncReadWrite>>>>>,
	remote_addr: Option<SocketAddr>,
	incoming_uni_rx: Mutex<mpsc::Receiver<YamuxStream>>,
	incoming_bi_rx: Mutex<mpsc::Receiver<YamuxStream>>,
}

/// Trait alias for types that implement both tokio AsyncRead and AsyncWrite.
pub trait TokioAsyncReadWrite: AsyncRead + AsyncWrite + Send + Unpin {}
impl<T: AsyncRead + AsyncWrite + Send + Unpin> TokioAsyncReadWrite for T {}

impl WsTransport {
	/// Creates a new WebSocket transport with yamux multiplexing.
	///
	/// Returns the transport and a driver future that must be spawned to drive I/O.
	///
	/// # Arguments
	///
	/// * `stream` - The underlying byte stream (typically a `WsByteStream`)
	/// * `mode` - Whether this is a client or server connection
	/// * `remote_addr` - The remote peer's address, if known
	pub fn new<S>(
		stream: S,
		mode: Mode,
		remote_addr: Option<SocketAddr>,
	) -> (
		Self,
		impl Future<Output = Result<(), ConnectionError>> + Send,
	)
	where
		S: AsyncRead + AsyncWrite + Send + Unpin + 'static,
	{
		let config = Config::default();

		// Box the stream and convert to futures-compatible I/O
		let boxed_stream: Box<dyn TokioAsyncReadWrite> = Box::new(stream);
		let compat_stream = boxed_stream.compat();

		let connection = Connection::new(compat_stream, config, mode);
		let connection = Arc::new(Mutex::new(connection));

		// Channels for incoming streams (sorted by type)
		let (incoming_uni_tx, incoming_uni_rx) = mpsc::channel(32);
		let (incoming_bi_tx, incoming_bi_rx) = mpsc::channel(32);

		let driver_connection = Arc::clone(&connection);
		let driver = async move {
			loop {
				let mut conn = driver_connection.lock().await;

				// Poll for the next incoming stream
				let poll_result = futures::future::poll_fn(|cx| conn.poll_next_inbound(cx)).await;

				match poll_result {
					Some(Ok(mut stream)) => {
						// Spawn a task to read the stream type prefix and route
						let uni_tx = incoming_uni_tx.clone();
						let bi_tx = incoming_bi_tx.clone();

						tokio::spawn(async move {
							// Read the 1-byte stream type prefix using futures AsyncReadExt
							let mut prefix = [0u8; 1];
							if stream.read_exact(&mut prefix).await.is_err() {
								tracing::warn!("Failed to read stream type prefix");
								return;
							}

							match prefix[0] {
								STREAM_TYPE_DATA => {
									if uni_tx.send(stream).await.is_err() {
										tracing::debug!("Incoming uni channel closed");
									}
								}
								STREAM_TYPE_CONTROL => {
									if bi_tx.send(stream).await.is_err() {
										tracing::debug!("Incoming bi channel closed");
									}
								}
								unknown => {
									tracing::warn!("Unknown stream type prefix: {unknown:#x}");
								}
							}
						});
					}
					Some(Err(e)) => {
						tracing::debug!("Yamux connection error: {e}");
						return Err(e);
					}
					None => {
						tracing::debug!("Yamux connection closed");
						return Ok(());
					}
				}
			}
		};

		let transport = Self {
			connection,
			remote_addr,
			incoming_uni_rx: Mutex::new(incoming_uni_rx),
			incoming_bi_rx: Mutex::new(incoming_bi_rx),
		};

		(transport, driver)
	}
}

impl Transport for WsTransport {
	type SendStream = WsSendStream;
	type RecvStream = WsRecvStream;
	type BiStream = WsBiStream;

	async fn open_uni(&self) -> Result<Self::SendStream, TransportError> {
		let mut conn = self.connection.lock().await;

		let mut stream = futures::future::poll_fn(|cx| conn.poll_new_outbound(cx))
			.await
			.map_err(|e| TransportError::connection_closed(e.to_string()))?;

		// Write the stream type prefix using futures AsyncWriteExt
		stream
			.write_all(&[STREAM_TYPE_DATA])
			.await
			.map_err(|e| TransportError::stream(e.to_string()))?;

		Ok(WsSendStream::new(stream))
	}

	async fn open_bi(&self) -> Result<Self::BiStream, TransportError> {
		let mut conn = self.connection.lock().await;

		let mut stream = futures::future::poll_fn(|cx| conn.poll_new_outbound(cx))
			.await
			.map_err(|e| TransportError::connection_closed(e.to_string()))?;

		// Write the stream type prefix using futures AsyncWriteExt
		stream
			.write_all(&[STREAM_TYPE_CONTROL])
			.await
			.map_err(|e| TransportError::stream(e.to_string()))?;

		Ok(WsBiStream::new(stream))
	}

	async fn accept_uni(&self) -> Result<Option<Self::RecvStream>, TransportError> {
		let mut rx = self.incoming_uni_rx.lock().await;
		Ok(rx.recv().await.map(WsRecvStream::new))
	}

	async fn accept_bi(&self) -> Result<Option<Self::BiStream>, TransportError> {
		let mut rx = self.incoming_bi_rx.lock().await;
		Ok(rx.recv().await.map(WsBiStream::new))
	}

	async fn close(&self) -> Result<(), TransportError> {
		let mut conn = self.connection.lock().await;
		futures::future::poll_fn(|cx| conn.poll_close(cx))
			.await
			.map_err(|e| TransportError::connection_closed(e.to_string()))
	}

	fn remote_addr(&self) -> Option<SocketAddr> {
		self.remote_addr
	}
}
