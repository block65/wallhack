//! WebSocket server implementation.
//!
//! Provides a WebSocket server for tunnel connections.

use std::{
	io,
	pin::Pin,
	sync::Arc,
	task::{Context, Poll},
};

use protobuf::v2::{EntryNodeInstruction, ExitNodeResponse};
use tokio::{
	io::{AsyncRead, AsyncWrite, ReadBuf},
	net::{TcpListener, TcpStream},
};
use tokio_rustls::TlsAcceptor;
use tokio_tungstenite::WebSocketStream;
use yamux::Mode;

use crate::{
	NodeRole,
	control::{handler::Handler, metrics::Metrics},
	transport::{bridge, ws::WsTransport, ws_adapter::WsByteStream, ws_upgrade},
};

use super::{
	config::ServerConfig,
	server::{AcceptResult, Server, ServerOptions},
};

/// Errors that can occur in the WebSocket server.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
	#[error("io error: {0}")]
	Io(#[from] std::io::Error),

	#[error("tls error: {0}")]
	Tls(#[from] rustls::Error),

	#[error("websocket upgrade error: {0}")]
	Upgrade(#[from] ws_upgrade::UpgradeError),

	#[error("tls config error: {0}")]
	TlsConfig(#[from] super::tls::Error),
}

/// TLS configuration for the WebSocket server.
pub struct WsTlsConfig {
	acceptor: TlsAcceptor,
}

impl WsTlsConfig {
	/// Creates a new TLS configuration from certificate and key.
	///
	/// # Errors
	///
	/// Returns an error if the TLS configuration cannot be created.
	pub fn new(config: Option<super::config::TlsConfig>) -> Result<Option<Self>, Error> {
		let Some(tls_config) = config else {
			return Ok(None);
		};

		let (cert_der, priv_key) = super::tls::configure_crypto(Some(tls_config))?;

		let server_config = rustls::ServerConfig::builder()
			.with_no_client_auth()
			.with_single_cert(cert_der, priv_key)?;

		let acceptor = TlsAcceptor::from(Arc::new(server_config));
		Ok(Some(Self { acceptor }))
	}
}

/// A stream that may or may not be TLS-encrypted.
pub enum MaybeTlsStream {
	Plain(TcpStream),
	Tls(Box<tokio_rustls::server::TlsStream<TcpStream>>),
}

impl AsyncRead for MaybeTlsStream {
	fn poll_read(
		self: Pin<&mut Self>,
		cx: &mut Context<'_>,
		buf: &mut ReadBuf<'_>,
	) -> Poll<io::Result<()>> {
		match self.get_mut() {
			Self::Plain(s) => Pin::new(s).poll_read(cx, buf),
			Self::Tls(s) => Pin::new(s).poll_read(cx, buf),
		}
	}
}

impl AsyncWrite for MaybeTlsStream {
	fn poll_write(
		self: Pin<&mut Self>,
		cx: &mut Context<'_>,
		buf: &[u8],
	) -> Poll<io::Result<usize>> {
		match self.get_mut() {
			Self::Plain(s) => Pin::new(s).poll_write(cx, buf),
			Self::Tls(s) => Pin::new(s).poll_write(cx, buf),
		}
	}

	fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
		match self.get_mut() {
			Self::Plain(s) => Pin::new(s).poll_flush(cx),
			Self::Tls(s) => Pin::new(s).poll_flush(cx),
		}
	}

	fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
		match self.get_mut() {
			Self::Plain(s) => Pin::new(s).poll_shutdown(cx),
			Self::Tls(s) => Pin::new(s).poll_shutdown(cx),
		}
	}
}

/// WebSocket server for tunnel connections.
pub struct WsServer {
	listener: TcpListener,
	tls: Option<WsTlsConfig>,
	options: ServerOptions,
}

impl Server for WsServer {
	type Error = Error;
	type Transport = WsTransport;

	fn try_new(config: ServerConfig, options: ServerOptions) -> Result<Self, Error> {
		let std_listener = std::net::TcpListener::bind(config.listen)?;
		std_listener.set_nonblocking(true)?;
		let listener = TcpListener::from_std(std_listener)?;

		let tls = WsTlsConfig::new(config.tls)?;

		tracing::info!("WebSocket server listening on {:?}", listener.local_addr());

		Ok(Self {
			listener,
			tls,
			options,
		})
	}

	async fn accept(
		&mut self,
		role: NodeRole,
	) -> Result<Option<AcceptResult<Self::Transport>>, Error> {
		tracing::debug!("waiting for next WebSocket connection...");

		let (tcp_stream, peer_addr) = self.listener.accept().await?;
		tracing::debug!("TCP connection from {peer_addr}");

		// Optionally wrap in TLS and perform WebSocket upgrade
		let ws_stream = if let Some(tls) = &self.tls {
			let tls_stream = tls.acceptor.accept(tcp_stream).await?;
			accept_websocket(MaybeTlsStream::Tls(Box::new(tls_stream))).await?
		} else {
			accept_websocket(MaybeTlsStream::Plain(tcp_stream)).await?
		};

		// Convert to byte stream and wrap in yamux transport
		let byte_stream = WsByteStream::new(ws_stream);
		let (transport, driver) = WsTransport::new(byte_stream, Mode::Server, Some(peer_addr));
		let transport = Arc::new(transport);

		// Spawn the yamux driver
		tokio::spawn(async move {
			if let Err(e) = driver.await {
				tracing::debug!("Yamux driver finished: {e}");
			}
		});

		// Get or create shared metrics
		let metrics = self
			.options
			.metrics
			.clone()
			.unwrap_or_else(|| Arc::new(Metrics::default()));

		let (instructions, _) = tokio::sync::broadcast::channel::<EntryNodeInstruction>(65536);
		let (responses, _) = tokio::sync::broadcast::channel::<ExitNodeResponse>(65536);

		// Create oneshot channel for ExitNodeHello
		let (exit_hello_tx, exit_hello_rx) = tokio::sync::oneshot::channel();

		// Task 0: Control stream handler
		let transport_ctrl = Arc::clone(&transport);
		let handler_config = self.options.handler_config.clone();
		let metrics_ctrl = Arc::clone(&metrics);

		tokio::spawn(async move {
			let handler = Handler::new(handler_config, metrics_ctrl);
			if let Err(e) = bridge::run_control_handler(&*transport_ctrl, &handler).await {
				tracing::debug!("Control handler finished: {e}");
			}
		});

		// Task 1: Incoming data handler (only for entry-side legacy control)
		if matches!(role, NodeRole::Entry) {
			let transport_data = Arc::clone(&transport);
			let responses_tx = responses.clone();
			let instructions_tx = instructions.clone();

			tokio::spawn(async move {
				if let Err(e) = bridge::run_incoming_data(
					&*transport_data,
					&instructions_tx,
					&responses_tx,
					Some(exit_hello_tx),
				)
				.await
				{
					tracing::debug!("Incoming data handler finished: {e}");
				}
			});
		}

		Ok(Some(AcceptResult::with_exit_hello(
			Arc::clone(&transport),
			(instructions, responses),
			peer_addr.to_string(),
			metrics,
			exit_hello_rx,
		)))
	}

	fn stop(&self) -> Result<(), Self::Error> {
		tracing::info!("WebSocket server stop initiated.");
		Ok(())
	}
}

/// Accepts a WebSocket connection.
async fn accept_websocket(
	mut stream: MaybeTlsStream,
) -> Result<WebSocketStream<MaybeTlsStream>, Error> {
	// Perform WebSocket upgrade
	let _upgrade_result = ws_upgrade::upgrade(&mut stream).await?;

	// Convert to WebSocket stream
	let ws_stream = WebSocketStream::from_raw_socket(
		stream,
		tokio_tungstenite::tungstenite::protocol::Role::Server,
		None,
	)
	.await;

	Ok(ws_stream)
}
