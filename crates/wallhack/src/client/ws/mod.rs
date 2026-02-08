//! WebSocket client implementation.
//!
//! Provides a WebSocket client for tunnel connections.

use std::{
	io,
	pin::Pin,
	sync::Arc,
	task::{Context, Poll},
};

use protobuf::{
	control_v2::{ControlMessage, control_message},
	v2::{EntryNodeInstruction, ExitNodeHello, ExitNodeResponse},
};
use tokio::{
	io::{AsyncRead, AsyncWrite, ReadBuf},
	net::TcpStream,
};
use tokio_rustls::TlsConnector;
use tokio_tungstenite::{WebSocketStream, client_async};
use yamux::Mode;

use crate::{
	NodeRole,
	client::config::ClientConfig,
	transport::{Transport, bridge, ws::WsTransport, ws_adapter::WsByteStream},
};

use super::client::{ConnectResult, ConnectionTasks};

/// Errors that can occur in the WebSocket client.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
	#[error("io error: {0}")]
	Io(#[from] std::io::Error),

	#[error("tls error: {0}")]
	Tls(#[from] rustls::Error),

	#[error("websocket error: {0}")]
	WebSocket(#[from] tokio_tungstenite::tungstenite::Error),

	#[error("tls config error: {0}")]
	TlsConfig(#[from] super::tls_config::Error),

	#[error("invalid DNS name: {0}")]
	InvalidDnsName(String),
}

/// A stream that may or may not be TLS-encrypted (client side).
pub enum MaybeTlsStream {
	Plain(TcpStream),
	Tls(Box<tokio_rustls::client::TlsStream<TcpStream>>),
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

/// WebSocket client configuration.
#[derive(Debug, Clone)]
pub struct WsClientConfig {
	/// The base client config (address, TLS settings).
	pub base: ClientConfig,

	/// WebSocket path (e.g., "/ws").
	pub path: String,

	/// Override host header (for CDN fronting).
	pub host_header: Option<String>,

	/// Use TLS (wss://) instead of plain WS.
	pub use_tls: bool,
}

impl Default for WsClientConfig {
	fn default() -> Self {
		Self {
			base: ClientConfig::default(),
			path: "/ws".to_string(),
			host_header: None,
			use_tls: true,
		}
	}
}

/// WebSocket client for tunnel connections.
pub struct WsClient {
	config: WsClientConfig,
	tls_connector: Option<TlsConnector>,
}

impl WsClient {
	/// Creates a new WebSocket client with the given configuration.
	///
	/// # Errors
	///
	/// Returns an error if TLS configuration fails.
	#[allow(clippy::result_large_err)]
	pub fn new(config: WsClientConfig) -> Result<Self, Error> {
		let tls_connector = if config.use_tls {
			let tls_config = if let Some(ref mtls) = config.base.mtls {
				super::tls_config::client_config(
					Some(mtls.clone()),
					config.base.accept_fingerprint.clone(),
				)?
			} else {
				super::tls_config::client_config(None, config.base.accept_fingerprint.clone())?
			};
			Some(TlsConnector::from(Arc::new(tls_config)))
		} else {
			None
		};

		Ok(Self {
			config,
			tls_connector,
		})
	}

	/// Connects to the WebSocket server.
	///
	/// # Errors
	///
	/// Returns an error if the connection fails.
	#[allow(clippy::result_large_err)]
	pub async fn connect(&mut self, role: NodeRole) -> Result<ConnectResult<WsTransport>, Error> {
		let addr = self.config.base.addr;
		let hostname = self
			.config
			.base
			.hostname
			.clone()
			.unwrap_or_else(|| addr.ip().to_string());

		tracing::debug!("{role:?} connecting to {addr} via WebSocket");

		// Build WebSocket URL
		let scheme = if self.tls_connector.is_some() {
			"wss"
		} else {
			"ws"
		};
		let host_header = self.config.host_header.as_deref().unwrap_or(&hostname);
		let url = format!(
			"{scheme}://{host_header}:{}{}",
			addr.port(),
			self.config.path
		);

		// Connect TCP
		let tcp_stream = TcpStream::connect(addr).await?;
		let peer_addr = tcp_stream.peer_addr().ok();
		let remote_addr_str = peer_addr.map_or_else(|| addr.to_string(), |a| a.to_string());

		// Wrap in TLS if configured and perform WebSocket handshake
		let ws_stream: WebSocketStream<MaybeTlsStream> =
			if let Some(connector) = &self.tls_connector {
				let server_name = rustls::pki_types::ServerName::try_from(hostname.clone())
					.map_err(|_| Error::InvalidDnsName(hostname.clone()))?;
				let tls_stream = connector.connect(server_name, tcp_stream).await?;
				let (ws, _response) =
					client_async(&url, MaybeTlsStream::Tls(Box::new(tls_stream))).await?;
				ws
			} else {
				let (ws, _response) = client_async(&url, MaybeTlsStream::Plain(tcp_stream)).await?;
				ws
			};

		tracing::debug!("WebSocket connected to {addr}");

		// Convert to byte stream and wrap in yamux transport
		let byte_stream = WsByteStream::new(ws_stream);
		let (transport, driver) = WsTransport::new(byte_stream, Mode::Client, peer_addr);
		let transport = Arc::new(transport);

		// Spawn the yamux driver
		tokio::spawn(async move {
			if let Err(e) = driver.await {
				tracing::debug!("Yamux driver finished: {e}");
			}
		});

		// Create control channel
		let (control_tx, mut control_rx) = tokio::sync::mpsc::channel::<ControlMessage>(64);

		// If exit node, send Hello via the control stream
		if let Some(ref exit_id) = self.config.base.exit_id {
			tracing::debug!("Queuing ExitNodeHello with id: {}", exit_id);
			let hello = ControlMessage {
				message: Some(control_message::Message::Hello(ExitNodeHello {
					exit_id: exit_id.clone(),
					version: env!("CARGO_PKG_VERSION").to_string(),
					auth_token: self.config.base.psk.clone().unwrap_or_default(),
				})),
			};
			let _ = control_tx.send(hello).await;
		}

		// Spawn control stream task
		let transport_ctrl = Arc::clone(&transport);
		let control_handle = tokio::spawn(async move {
			match bridge::run_control_stream_initiator(
				&*transport_ctrl,
				&mut control_rx,
				None,
				None,
				None,
				std::time::Duration::from_secs(30),
			)
			.await
			{
				Ok(exit) => tracing::debug!("Control stream finished: {exit:?}"),
				Err(e) => tracing::debug!("Control stream error: {e}"),
			}
		});

		let (instructions, _) = tokio::sync::broadcast::channel::<EntryNodeInstruction>(2048);
		let (responses, _) = tokio::sync::broadcast::channel::<ExitNodeResponse>(2048);

		// Data task 1: Incoming data
		let transport_data = Arc::clone(&transport);
		let instructions_tx = instructions.clone();
		let responses_tx = responses.clone();

		let incoming_handle = tokio::spawn(async move {
			match transport_data.accept_uni().await {
				Ok(Some(mut recv)) => {
					if let Err(e) =
						bridge::run_data_in(&mut recv, &instructions_tx, &responses_tx).await
					{
						tracing::debug!("Data-in handler finished: {e}");
					}
				}
				Ok(None) => tracing::debug!("Transport closed before data-in stream accepted"),
				Err(e) => tracing::debug!("Failed to accept data-in stream: {e}"),
			}
		});

		// Data task 2: Outgoing data based on role
		let outgoing_handle = match role {
			NodeRole::Entry | NodeRole::Relay => {
				tracing::debug!("Opening data-out stream for instructions");
				let transport_out = Arc::clone(&transport);
				let instructions_tx = instructions.clone();

				tokio::spawn(async move {
					match transport_out.open_uni().await {
						Ok(mut send) => {
							if let Err(e) =
								bridge::run_data_out_instructions(&mut send, &instructions_tx).await
							{
								tracing::debug!("Data-out instructions handler finished: {e}");
							}
						}
						Err(e) => tracing::debug!("Failed to open data-out stream: {e}"),
					}
				})
			}
			NodeRole::Exit => {
				tracing::debug!("Opening data-out stream for responses");
				let transport_out = Arc::clone(&transport);
				let responses_tx = responses.clone();

				tokio::spawn(async move {
					match transport_out.open_uni().await {
						Ok(mut send) => {
							if let Err(e) =
								bridge::run_data_out_responses(&mut send, &responses_tx).await
							{
								tracing::debug!("Data-out responses handler finished: {e}");
							}
						}
						Err(e) => tracing::debug!("Failed to open data-out stream: {e}"),
					}
				})
			}
		};

		let tasks = ConnectionTasks {
			incoming: incoming_handle,
			outgoing: outgoing_handle,
			control: control_handle,
		};

		Ok(ConnectResult::new(
			Arc::clone(&transport),
			(instructions, responses),
			remote_addr_str,
			tasks,
			control_tx,
		))
	}
}
