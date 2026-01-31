//! WebSocket transport with yamux multiplexing.
//!
//! This module implements the [`Transport`] trait over a WebSocket connection
//! using yamux for stream multiplexing.

use std::{
	future::Future,
	io,
	net::SocketAddr,
	pin::Pin,
	task::{Context, Poll},
	time::Duration,
};

use futures::{AsyncReadExt as FuturesAsyncReadExt, AsyncWriteExt as FuturesAsyncWriteExt};
use tokio::{
	io::{AsyncRead, AsyncWrite, ReadBuf},
	sync::{mpsc, oneshot},
};
use tokio_util::compat::{FuturesAsyncReadCompatExt, TokioAsyncReadCompatExt};
use yamux::{Config, Connection, ConnectionError, Mode, Stream as YamuxStream};

use crate::{BiStream, Transport, TransportError};

/// Stream type prefix for data/unidirectional streams.
const STREAM_TYPE_DATA: u8 = 0x00;

/// Stream type prefix for control/bidirectional streams.
const STREAM_TYPE_CONTROL: u8 = 0x01;

/// Timeout for reading the stream type prefix.
const PREFIX_READ_TIMEOUT: Duration = Duration::from_secs(5);

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

use std::collections::VecDeque;

/// Commands sent to the driver task.
enum Command {
	OpenUni(oneshot::Sender<Result<YamuxStream, TransportError>>),
	OpenBi(oneshot::Sender<Result<YamuxStream, TransportError>>),
	Close,
}

/// The driver task that manages the yamux connection and streams.
pub struct Driver {
	connection: Connection<tokio_util::compat::Compat<Box<dyn TokioAsyncReadWrite>>>,
	cmd_rx: mpsc::Receiver<Command>,
	incoming_uni_tx: mpsc::Sender<YamuxStream>,
	incoming_bi_tx: mpsc::Sender<YamuxStream>,
	pending_open_uni: VecDeque<oneshot::Sender<Result<YamuxStream, TransportError>>>,
	pending_open_bi: VecDeque<oneshot::Sender<Result<YamuxStream, TransportError>>>,
	shutdown: bool,
}

impl Future for Driver {
	type Output = Result<(), ConnectionError>;

	fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
		let this = self.get_mut();

		loop {
			let mut progress = false;

			// 1. Process commands if we aren't shutting down
			if !this.shutdown {
				while let Poll::Ready(Some(cmd)) = this.cmd_rx.poll_recv(cx) {
					progress = true;
					match cmd {
						Command::OpenUni(tx) => {
							this.pending_open_uni.push_back(tx);
						}
						Command::OpenBi(tx) => {
							this.pending_open_bi.push_back(tx);
						}
						Command::Close => {
							this.shutdown = true;
							break;
						}
					}
				}
			}

			// 2. Drive pending stream opens
			// We only try to open one at a time to avoid complex state management
			// and respect yamux's flow control/max streams.
			if let Some(tx) = this.pending_open_uni.pop_front() {
				match this.connection.poll_new_outbound(cx) {
					Poll::Ready(Ok(stream)) => {
						progress = true;
						let _ = tx.send(Ok(stream));
					}
					Poll::Ready(Err(e)) => {
						progress = true;
						let _ = tx.send(Err(TransportError::connection_closed(e.to_string())));
					}
					Poll::Pending => {
						this.pending_open_uni.push_front(tx);
					}
				}
			}

			if let Some(tx) = this.pending_open_bi.pop_front() {
				match this.connection.poll_new_outbound(cx) {
					Poll::Ready(Ok(stream)) => {
						progress = true;
						let _ = tx.send(Ok(stream));
					}
					Poll::Ready(Err(e)) => {
						progress = true;
						let _ = tx.send(Err(TransportError::connection_closed(e.to_string())));
					}
					Poll::Pending => {
						this.pending_open_bi.push_front(tx);
					}
				}
			}

			// 3. Drive the connection (inbound streams and I/O)
			if this.shutdown {
				match this.connection.poll_close(cx) {
					Poll::Ready(Ok(())) => return Poll::Ready(Ok(())),
					Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
					Poll::Pending => {}
				}
			} else {
				match this.connection.poll_next_inbound(cx) {
					Poll::Ready(Some(Ok(mut stream))) => {
						let uni_tx = this.incoming_uni_tx.clone();
						let bi_tx = this.incoming_bi_tx.clone();

						tokio::spawn(async move {
							let mut prefix = [0u8; 1];
							let read_result = tokio::time::timeout(
								PREFIX_READ_TIMEOUT,
								stream.read_exact(&mut prefix),
							)
							.await;

							match read_result {
								Ok(Ok(_)) => match prefix[0] {
									STREAM_TYPE_DATA => {
										let _ = uni_tx.send(stream).await;
									}
									STREAM_TYPE_CONTROL => {
										let _ = bi_tx.send(stream).await;
									}
									_ => {}
								},
								_ => {}
							}
						});
						progress = true;
					}
					Poll::Ready(Some(Err(e))) => return Poll::Ready(Err(e)),
					Poll::Ready(None) => return Poll::Ready(Ok(())),
					Poll::Pending => {}
				}
			}

			if !progress {
				return Poll::Pending;
			}
		}
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
	cmd_tx: mpsc::Sender<Command>,
	remote_addr: Option<SocketAddr>,
	incoming_uni_rx: tokio::sync::Mutex<mpsc::Receiver<YamuxStream>>,
	incoming_bi_rx: tokio::sync::Mutex<mpsc::Receiver<YamuxStream>>,
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
	pub fn new<S>(stream: S, mode: Mode, remote_addr: Option<SocketAddr>) -> (Self, Driver)
	where
		S: AsyncRead + AsyncWrite + Send + Unpin + 'static,
	{
		let config = Config::default();

		// Box the stream and convert to futures-compatible I/O
		let boxed_stream: Box<dyn TokioAsyncReadWrite> = Box::new(stream);
		let compat_stream = boxed_stream.compat();

		let connection = Connection::new(compat_stream, config, mode);

		// Channels for incoming streams (sorted by type)
		let (incoming_uni_tx, incoming_uni_rx) = mpsc::channel(32);
		let (incoming_bi_tx, incoming_bi_rx) = mpsc::channel(32);
		let (cmd_tx, cmd_rx) = mpsc::channel(32);

		let driver = Driver {
			connection,
			cmd_rx,
			incoming_uni_tx,
			incoming_bi_tx,
			pending_open_uni: VecDeque::new(),
			pending_open_bi: VecDeque::new(),
			shutdown: false,
		};

		let transport = Self {
			cmd_tx,
			remote_addr,
			incoming_uni_rx: tokio::sync::Mutex::new(incoming_uni_rx),
			incoming_bi_rx: tokio::sync::Mutex::new(incoming_bi_rx),
		};

		(transport, driver)
	}
}

impl Transport for WsTransport {
	type SendStream = WsSendStream;
	type RecvStream = WsRecvStream;
	type BiStream = WsBiStream;

	async fn open_uni(&self) -> Result<Self::SendStream, TransportError> {
		let (tx, rx) = oneshot::channel();
		self.cmd_tx
			.send(Command::OpenUni(tx))
			.await
			.map_err(|_| TransportError::connection_closed("transport driver dropped"))?;

		let mut stream = rx
			.await
			.map_err(|_| TransportError::connection_closed("transport driver dropped"))??;

		// Write the stream type prefix using futures AsyncWriteExt
		stream
			.write_all(&[STREAM_TYPE_DATA])
			.await
			.map_err(|e| TransportError::stream(e.to_string()))?;

		Ok(WsSendStream::new(stream))
	}

	async fn open_bi(&self) -> Result<Self::BiStream, TransportError> {
		let (tx, rx) = oneshot::channel();
		self.cmd_tx
			.send(Command::OpenBi(tx))
			.await
			.map_err(|_| TransportError::connection_closed("transport driver dropped"))?;

		let mut stream = rx
			.await
			.map_err(|_| TransportError::connection_closed("transport driver dropped"))??;

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
		self.cmd_tx
			.send(Command::Close)
			.await
			.map_err(|_| TransportError::connection_closed("transport driver dropped"))
	}

	fn remote_addr(&self) -> Option<SocketAddr> {
		self.remote_addr
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use tokio::io::{AsyncReadExt, AsyncWriteExt};

	#[tokio::test]
	async fn test_ws_transport_basic() {
		let (s1, s2) = tokio::io::duplex(1024);

		let (client_transport, client_driver) = WsTransport::new(s1, Mode::Client, None);
		let (server_transport, server_driver) = WsTransport::new(s2, Mode::Server, None);

		let client_handle = tokio::spawn(async move {
			client_driver.await.expect("client driver failed");
		});

		let server_handle = tokio::spawn(async move {
			server_driver.await.expect("server driver failed");
		});

		// 1. Test Unidirectional Stream
		let mut client_send = client_transport.open_uni().await.expect("open_uni failed");
		client_send
			.write_all(b"hello uni")
			.await
			.expect("write failed");
		client_send.shutdown().await.expect("shutdown failed");

		let mut server_recv = server_transport
			.accept_uni()
			.await
			.expect("accept_uni failed")
			.expect("stream was None");
		let mut buf = Vec::new();
		server_recv
			.read_to_end(&mut buf)
			.await
			.expect("read failed");
		assert_eq!(buf, b"hello uni");

		// 2. Test Bidirectional Stream
		let mut client_bi = client_transport.open_bi().await.expect("open_bi failed");
		client_bi.write_all(b"ping").await.expect("write failed");
		client_bi.flush().await.expect("flush failed");

		let mut server_bi = server_transport
			.accept_bi()
			.await
			.expect("accept_bi failed")
			.expect("stream was None");
		let mut buf = [0u8; 4];
		server_bi.read_exact(&mut buf).await.expect("read failed");
		assert_eq!(&buf, b"ping");

		server_bi.write_all(b"pong").await.expect("write failed");
		server_bi.finish().await.expect("finish failed");

		let mut buf = [0u8; 4];
		client_bi.read_exact(&mut buf).await.expect("read failed");
		assert_eq!(&buf, b"pong");

		// 3. Test Concurrent Opening
		let mut open_futs = Vec::new();
		for _ in 0..10 {
			open_futs.push(client_transport.open_uni());
		}
		let streams = futures::future::try_join_all(open_futs)
			.await
			.expect("concurrent open_uni failed");
		assert_eq!(streams.len(), 10);

		// 4. Test Closing
		client_transport.close().await.expect("close failed");

		// Wait for handles to finish
		let _ = tokio::join!(client_handle, server_handle);
	}
}
