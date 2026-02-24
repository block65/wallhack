//! [`WebSocketTransport`] and its configuration.

use std::{collections::VecDeque, net::SocketAddr, time::Duration};

use futures::stream::FuturesUnordered;

use futures::AsyncWriteExt as FuturesAsyncWriteExt;
use tokio::sync::{mpsc, oneshot};
use tracing::debug;
use yamux::Mode;

use crate::{Transport, TransportError};

use super::{
	driver::{Command, Driver, STREAM_TYPE_CONTROL, STREAM_TYPE_DATA, make_connection},
	streams::{WebSocketBiStream, WebSocketRecvStream, WebSocketSendStream},
};

/// Configuration for a [`WebSocketTransport`].
#[derive(Debug, Clone)]
pub struct WebSocketTransportConfig {
	/// Timeout for reading the stream type prefix on inbound streams.
	pub prefix_read_timeout: Duration,
	/// Underlying yamux connection config.
	pub yamux: yamux::Config,
}

impl Default for WebSocketTransportConfig {
	fn default() -> Self {
		Self {
			prefix_read_timeout: Duration::from_secs(5),
			yamux: yamux::Config::default(),
		}
	}
}

/// WebSocket transport using yamux multiplexing.
///
/// # Driver Pattern
///
/// The yamux connection requires a background task to drive I/O. When creating
/// a `WebSocketTransport`, you must spawn the returned [`Driver`] future:
///
/// ```ignore
/// let (transport, driver) = WebSocketTransport::new(ws_stream, Mode::Client, remote_addr, WebSocketTransportConfig::default());
/// tokio::spawn(driver);
/// ```
pub struct WebSocketTransport {
	cmd_tx: mpsc::Sender<Command>,
	remote_addr: Option<SocketAddr>,
	incoming_uni_rx: tokio::sync::Mutex<mpsc::Receiver<yamux::Stream>>,
	incoming_bi_rx: tokio::sync::Mutex<mpsc::Receiver<yamux::Stream>>,
}

impl WebSocketTransport {
	/// Creates a new WebSocket transport with yamux multiplexing.
	///
	/// Returns the transport and a driver future that must be spawned to drive I/O.
	pub fn new<S>(
		stream: S,
		mode: Mode,
		remote_addr: Option<SocketAddr>,
		config: WebSocketTransportConfig,
	) -> (Self, Driver)
	where
		S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Unpin + 'static,
	{
		debug!(?remote_addr, ?mode, "creating WebSocket transport");

		let connection = make_connection(stream, mode, config.yamux);

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
			pending_prefix_reads: FuturesUnordered::new(),
			prefix_read_timeout: config.prefix_read_timeout,
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

impl Transport for WebSocketTransport {
	type SendStream = WebSocketSendStream;
	type RecvStream = WebSocketRecvStream;
	type BiStream = WebSocketBiStream;

	async fn open_uni(&self) -> Result<Self::SendStream, TransportError> {
		let (tx, rx) = oneshot::channel();
		self.cmd_tx
			.send(Command::OpenUni(tx))
			.await
			.map_err(|_| TransportError::connection_closed("transport driver dropped"))?;

		let mut stream = rx
			.await
			.map_err(|_| TransportError::connection_closed("transport driver dropped"))??;

		stream
			.write_all(&[STREAM_TYPE_DATA])
			.await
			.map_err(|e| TransportError::stream(e.to_string()))?;

		Ok(WebSocketSendStream::new(stream))
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

		stream
			.write_all(&[STREAM_TYPE_CONTROL])
			.await
			.map_err(|e| TransportError::stream(e.to_string()))?;

		Ok(WebSocketBiStream::new(stream))
	}

	async fn accept_uni(&self) -> Result<Option<Self::RecvStream>, TransportError> {
		let mut rx = self.incoming_uni_rx.lock().await;
		Ok(rx.recv().await.map(WebSocketRecvStream::new))
	}

	async fn accept_bi(&self) -> Result<Option<Self::BiStream>, TransportError> {
		let mut rx = self.incoming_bi_rx.lock().await;
		Ok(rx.recv().await.map(WebSocketBiStream::new))
	}

	async fn close(&self) -> Result<(), TransportError> {
		debug!(remote_addr = ?self.remote_addr, "closing WebSocket transport");
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
	use crate::BiStream as _;
	use tokio::io::{AsyncReadExt, AsyncWriteExt};

	#[tokio::test]
	async fn test_websocket_transport_basic() {
		let (s1, s2) = tokio::io::duplex(1024);

		let (client_transport, client_driver) =
			WebSocketTransport::new(s1, Mode::Client, None, WebSocketTransportConfig::default());
		let (server_transport, server_driver) =
			WebSocketTransport::new(s2, Mode::Server, None, WebSocketTransportConfig::default());

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
