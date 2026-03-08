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

use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    net::{TcpListener, TcpStream},
};
use tokio_rustls::TlsAcceptor;
use tokio_tungstenite::{WebSocketStream, tungstenite::protocol::WebSocketConfig};
use wallhack_transport::Transport;
use wallhack_wire::{
    control::{ControlMessage, control_message},
    data::Handshake,
};
use yamux::Mode;

use crate::{
    NodeRole, SocketAddrExt as _,
    control::{handler::Handler, metrics::Metrics, peers::Registry, routes::RouteTable},
    psk::HandshakeExt,
    transport::{
        protocol,
        protocol::{AsyncProtoRead as _, AsyncProtoWrite as _},
        websocket::{
            self as ws_upgrade, WebSocketByteStream, WebSocketTransport, WebSocketTransportConfig,
        },
    },
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
    pub fn new(mut config: Option<super::config::TlsConfig>) -> Result<(Self, String), Error> {
        let ca_roots_path = config.as_mut().and_then(|t| t.ca_roots.take());
        let (cert_der, priv_key, fingerprint) = super::tls::configure_crypto(config)?;

        let server_config = if let Some(ca_path) = ca_roots_path {
            let roots = super::tls::load_ca_roots(&ca_path)?;
            let verifier = rustls::server::WebPkiClientVerifier::builder(Arc::new(roots))
                .build()
                .map_err(|e| rustls::Error::General(e.to_string()))?;
            rustls::ServerConfig::builder()
                .with_client_cert_verifier(verifier)
                .with_single_cert(cert_der, priv_key)?
        } else {
            rustls::ServerConfig::builder()
                .with_no_client_auth()
                .with_single_cert(cert_der, priv_key)?
        };

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
pub struct WebSocketServer {
    listener: TcpListener,
    tls: WsTlsConfig,
    options: ServerOptions,
    fingerprint: String,
    psk: Option<zeroize::Zeroizing<String>>,
}

impl Server for WebSocketServer {
    type Error = Error;
    type Transport = WebSocketTransport;

    fn try_new(config: ServerConfig, options: ServerOptions) -> Result<Self, Error> {
        let std_listener = std::net::TcpListener::bind(config.listen)?;
        std_listener.set_nonblocking(true)?;
        let listener = TcpListener::from_std(std_listener)?;

        let (tls, fingerprint) = WsTlsConfig::new(config.tls).inspect(|_| {
            tracing::debug!("WebSocket TLS enabled (self-signed if no cert provided)");
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

    #[allow(clippy::too_many_lines)] // refactor candidate
    async fn accept(
        &mut self,
        _role: NodeRole,
    ) -> Result<Option<AcceptResult<Self::Transport>>, Error> {
        tracing::debug!("waiting for next WebSocket connection...");

        let (tcp_stream, raw_addr) = self.listener.accept().await?;
        let peer_addr = raw_addr.normalize();
        tracing::debug!("TCP connection from {peer_addr}");

        // Wrap in TLS and perform WebSocket upgrade.
        // Extract channel binding BEFORE the WebSocket upgrade consumes the stream.
        let tls_stream = self.tls.acceptor.accept(tcp_stream).await?;
        let (_, server_conn) = tls_stream.get_ref();
        let channel_binding = crate::psk::channel_binding_rustls_server(server_conn);
        let ws_stream = accept_websocket(MaybeTlsStream::Tls(Box::new(tls_stream))).await?;

        // Convert to byte stream and wrap in yamux transport
        let byte_stream = WebSocketByteStream::new(ws_stream);
        let (transport, driver) = WebSocketTransport::new(
            byte_stream,
            Mode::Server,
            Some(peer_addr),
            WebSocketTransportConfig::default(),
        );
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

        // Read the first message — must be a ControlMessage::Handshake (with timeout).
        let handshake_result = tokio::time::timeout(
            Duration::from_secs(10),
            control_stream.read_proto::<ControlMessage>(protocol::CONTROL_MTU),
        )
        .await;

        let peer_handshake: Option<Handshake> = match handshake_result {
            Ok(Ok(msg)) => match msg.message {
                Some(control_message::Message::Handshake(handshake)) => {
                    tracing::debug!("Handshake from {} ({})", handshake.name, handshake.version,);
                    Some(handshake)
                }
                other => {
                    tracing::warn!("Expected Handshake as first control message, got: {other:?}");
                    None
                }
            },
            Ok(Err(e)) => {
                tracing::warn!("Failed to read Handshake from control stream: {e}");
                None
            }
            Err(_elapsed) => {
                tracing::warn!("Timed out waiting for Handshake on control stream");
                None
            }
        };

        // Send our Handshake back to the client.
        if let Some(ref local) = self.options.local_handshake {
            let mut handshake = local.clone();
            if let Some(ref psk) = self.psk
                && let Some(ref binding) = channel_binding
            {
                handshake.psk_proof = handshake.compute_psk_proof(psk.as_bytes(), binding);
            }
            let msg = ControlMessage {
                message: Some(control_message::Message::Handshake(handshake.clone())),
            };
            if let Err(e) = control_stream.write_proto(&msg).await {
                tracing::warn!("Failed to send Handshake: {e}");
            } else {
                tracing::debug!(
                    "Sent Handshake: name={}, version={}",
                    handshake.name,
                    handshake.version,
                );
            }
        }

        // Get or create shared metrics
        let metrics = self
            .options
            .metrics
            .clone()
            .unwrap_or_else(|| Arc::new(Metrics::default()));

        let channels = super::server::DataChannels::new();

        // Create control channel for injecting outgoing control messages
        let (control_tx, control_rx) = tokio::sync::mpsc::channel::<ControlMessage>(64);

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

        // Create latency channel so pong-derived RTT measurements are available
        // to the caller (e.g. for registry updates and one-shot ping responses).
        let (latency_tx, latency_rx) = tokio::sync::mpsc::channel::<f64>(4);

        tokio::spawn(async move {
            let handler = Handler::new(handler_config, metrics_ctrl, peers_ctrl, routes_ctrl);
            let mut channels = protocol::ControlChannels {
                outgoing_rx: control_rx,
                handshake_tx: None, // Handshake already read above
                latency_tx: Some(latency_tx),
                control_response_tx: None, // server doesn't issue ControlRequests
                role_transition_tx: None,
            };
            let mut control_stream = wallhack_transport::erased::BoxBiStream::new(control_stream);
            let exit = channels
                .run(&mut control_stream, Some(&handler), Duration::from_secs(30))
                .await;
            tracing::debug!("Control stream finished: {exit:?}");
        });

        // Data tasks are NOT spawned here — the caller does that after PSK validation.
        Ok(Some(AcceptResult::with_handshake(
            Arc::clone(&transport),
            channels,
            peer_addr.to_string(),
            metrics,
            peer_handshake,
            control_tx,
            latency_rx,
            channel_binding,
        )))
    }

    fn stop(&self) -> Result<(), Self::Error> {
        tracing::info!("WebSocket server stop initiated.");
        Ok(())
    }

    fn protocol_name(&self) -> &'static str {
        "WebSocket"
    }

    fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    fn psk(&self) -> Option<&str> {
        self.psk.as_ref().map(|s| s.as_str())
    }

    fn local_addr(&self) -> std::io::Result<std::net::SocketAddr> {
        self.listener.local_addr()
    }
}

/// Accepts a WebSocket connection.
async fn accept_websocket(
    mut stream: MaybeTlsStream,
) -> Result<WebSocketStream<MaybeTlsStream>, Error> {
    // Perform WebSocket upgrade
    let _upgrade_result = ws_upgrade::upgrade(&mut stream).await?;

    // Maximum message and frame size: one tunnel MTU plus framing overhead.
    // Anything larger is a protocol violation — reject early to prevent unbounded
    // allocations in WsByteStream's read buffer.
    let mut ws_config = WebSocketConfig::default();
    ws_config.max_message_size = Some(65_535 + 512);
    ws_config.max_frame_size = Some(65_535 + 512);

    // Convert to WebSocket stream
    let ws_stream = WebSocketStream::from_raw_socket(
        stream,
        tokio_tungstenite::tungstenite::protocol::Role::Server,
        Some(ws_config),
    )
    .await;

    Ok(ws_stream)
}
