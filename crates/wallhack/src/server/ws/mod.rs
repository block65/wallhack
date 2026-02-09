//! WebSocket server implementation.
//!
//! Provides a WebSocket server for tunnel connections.

use std::{
	io,
	pin::Pin,
	sync::Arc,
	task::{Context, Poll},
	time::Duration,
};

use protobuf::{
	control_v2::{ControlMessage, control_message},
	v2::{EntryNodeInstruction, ExitNodeHello, ExitNodeResponse},
};
use tokio::{
	io::{AsyncRead, AsyncWrite, ReadBuf},
	net::{TcpListener, TcpStream},
};
use tokio_rustls::TlsAcceptor;
use tokio_tungstenite::WebSocketStream;
use transport::Transport;
use yamux::Mode;

use crate::{
	NodeRole,
	control::{handler::Handler, metrics::Metrics, peers::Registry, routes::RouteTable},
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
	pub fn new(config: Option<super::config::TlsConfig>) -> Result<(Self, String), Error> {
		let (cert_der, priv_key, fingerprint) = super::tls::configure_crypto(config)?;

		let server_config = rustls::ServerConfig::builder()
			.with_no_client_auth()
			.with_single_cert(cert_der, priv_key)?;

		let acceptor = TlsAcceptor::from(Arc::new(server_config));
		Ok((Self { acceptor }, fingerprint))
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
	tls: WsTlsConfig,
	options: ServerOptions,
	fingerprint: String,
	psk: Option<String>,
}

impl Server for WsServer {
	type Error = Error;
	type Transport = WsTransport;

	fn try_new(config: ServerConfig, options: ServerOptions) -> Result<Self, Error> {
		let std_listener = std::net::TcpListener::bind(config.listen)?;
		std_listener.set_nonblocking(true)?;
		let listener = TcpListener::from_std(std_listener)?;

		let (tls, fingerprint) = WsTlsConfig::new(config.tls).inspect(|_| {
			tracing::debug!("WebSocket TLS enabled (self-signed if no cert provided)")
		})?;

		tracing::info!("WebSocket server listening on {:?}", listener.local_addr());

		Ok(Self {
			listener,
			tls,
			options,
			fingerprint,
			psk: config.psk,
		})
	}

	async fn accept(
		&mut self,
		_role: NodeRole,
	) -> Result<Option<AcceptResult<Self::Transport>>, Error> {
		tracing::debug!("waiting for next WebSocket connection...");

		let (tcp_stream, raw_addr) = self.listener.accept().await?;
		let peer_addr = crate::normalize_socket_addr(raw_addr);
		tracing::debug!("TCP connection from {peer_addr}");

		// Wrap in TLS and perform WebSocket upgrade
		let tls_stream = self.tls.acceptor.accept(tcp_stream).await?;
		let ws_stream = accept_websocket(MaybeTlsStream::Tls(Box::new(tls_stream))).await?;

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

		// Accept first bidi stream — this is the control stream.
		let Some(mut control_stream) = transport.accept_bi().await.map_err(|e| {
			std::io::Error::other(format!("failed to accept control bidi stream: {e}"))
		})?
		else {
			return Err(Error::Io(std::io::Error::other(
				"transport closed before control stream accepted",
			)));
		};

		// Read the first message — must be a ControlMessage::Hello (with timeout).
		let hello_result = tokio::time::timeout(
			Duration::from_secs(10),
			bridge::read_length_delimited::<ControlMessage, _>(
				&mut control_stream,
				bridge::CONTROL_MTU,
			),
		)
		.await;

		let exit_hello: Option<ExitNodeHello> = match hello_result {
			Ok(Ok(msg)) => match msg.message {
				Some(control_message::Message::Hello(hello)) => {
					tracing::info!(
						"Received Hello: id={}, version={}",
						hello.exit_id,
						hello.version,
					);
					Some(hello)
				}
				other => {
					tracing::warn!("Expected Hello as first control message, got: {other:?}");
					None
				}
			},
			Ok(Err(e)) => {
				tracing::warn!("Failed to read Hello from control stream: {e}");
				None
			}
			Err(_elapsed) => {
				tracing::warn!("Timed out waiting for Hello on control stream");
				None
			}
		};

		// Get or create shared metrics
		let metrics = self
			.options
			.metrics
			.clone()
			.unwrap_or_else(|| Arc::new(Metrics::default()));

		let (instructions, _) = tokio::sync::broadcast::channel::<EntryNodeInstruction>(65536);
		let (responses, _) = tokio::sync::broadcast::channel::<ExitNodeResponse>(65536);

		// Create control channel for injecting outgoing control messages
		let (control_tx, mut control_rx) = tokio::sync::mpsc::channel::<ControlMessage>(64);

		// Spawn control stream task with handler
		let handler_config = self.options.handler_config.clone();
		let metrics_ctrl = Arc::clone(&metrics);
		let peers_ctrl = self
			.options
			.peers
			.clone()
			.unwrap_or_else(|| Arc::new(Registry::new()));
		let routes_ctrl = self
			.options
			.routes
			.clone()
			.unwrap_or_else(RouteTable::shared);

		tokio::spawn(async move {
			let handler = Handler::new(handler_config, metrics_ctrl, peers_ctrl, routes_ctrl);
			let exit = bridge::run_control_loop(
				&mut control_stream,
				&mut control_rx,
				Some(&handler),
				None, // Hello already read above
				None, // pong handled inline
				None, // server doesn't issue ControlRequests
				Duration::from_secs(30),
			)
			.await;
			tracing::debug!("Control stream finished: {exit:?}");
		});

		// Data tasks are NOT spawned here — the caller does that after PSK validation.
		Ok(Some(AcceptResult::with_exit_hello(
			Arc::clone(&transport),
			(instructions, responses),
			peer_addr.to_string(),
			metrics,
			exit_hello,
			control_tx,
		)))
	}

	fn stop(&self) -> Result<(), Self::Error> {
		tracing::info!("WebSocket server stop initiated.");
		Ok(())
	}

	fn fingerprint(&self) -> &str {
		&self.fingerprint
	}

	fn psk(&self) -> Option<&str> {
		self.psk.as_deref()
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
