//! WebSocket client implementation.
//!
//! Provides a WebSocket client for tunnel connections.

use std::{
    io,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    net::TcpStream,
};
use tokio_rustls::TlsConnector;
use tokio_tungstenite::{
    WebSocketStream, client_async_with_config, tungstenite::protocol::WebSocketConfig,
};
use wallhack_wire::{
    control::{ControlMessage, control_message},
    data::Handshake,
};
use yamux::Mode;

use crate::{
    NodeRole,
    client::config::ClientConfig,
    psk::HandshakeExt,
    server::server::DataChannels,
    transport::{
        Transport, protocol,
        websocket::{WebSocketByteStream, WebSocketTransport, WebSocketTransportConfig},
    },
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

/// Parse a proxy URL string into (`is_socks5`, host, port).
/// Handles: `socks5://`, `socks5h://`, `http://`, `https://`, bare host:port.
/// Strips user:pass@ credentials. Returns None if unparseable.
fn parse_proxy_url(url: &str) -> Option<(bool, String, u16)> {
    let (is_socks5, rest) = if let Some(r) = url
        .strip_prefix("socks5://")
        .or_else(|| url.strip_prefix("socks5h://"))
    {
        (true, r)
    } else {
        let r = url
            .strip_prefix("http://")
            .or_else(|| url.strip_prefix("https://"))
            .unwrap_or(url);
        (false, r)
    };

    // Strip any trailing path component
    let rest = rest.split('/').next()?;
    // Strip user:pass@ credentials
    let rest = rest.rsplit_once('@').map_or(rest, |(_, after)| after);

    let (host, port_str) = rest.rsplit_once(':')?;
    let port = port_str.parse().ok()?;
    Some((is_socks5, host.to_string(), port))
}

/// Detect proxy for a given target from standard env vars.
/// Follows curl conventions: `HTTPS_PROXY` > `ALL_PROXY` for TLS, `HTTP_PROXY` > `ALL_PROXY` for plain.
/// Respects `NO_PROXY` comma-separated list (exact match or domain suffix).
/// Returns None when no proxy is configured or target is in `NO_PROXY`.
fn detect_proxy(use_tls: bool, target_host: &str) -> Option<(bool, String, u16)> {
    // Check NO_PROXY / no_proxy first
    let no_proxy = std::env::var("NO_PROXY")
        .or_else(|_| std::env::var("no_proxy"))
        .unwrap_or_default();
    for entry in no_proxy.split(',') {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        if entry == "*"
            || target_host.eq_ignore_ascii_case(entry)
            || target_host
                .strip_suffix(entry)
                .is_some_and(|s| s.ends_with('.'))
        {
            return None;
        }
    }

    let raw = if use_tls {
        std::env::var("HTTPS_PROXY").or_else(|_| std::env::var("https_proxy"))
    } else {
        std::env::var("HTTP_PROXY").or_else(|_| std::env::var("http_proxy"))
    }
    .or_else(|_| std::env::var("ALL_PROXY"))
    .or_else(|_| std::env::var("all_proxy"))
    .ok()?;

    parse_proxy_url(&raw)
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
    #[allow(clippy::result_large_err)] // Error carries TLS/IO context; not worth boxing for this call path
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
    #[allow(clippy::result_large_err)] // Error carries TLS/IO context; not worth boxing for this call path
    #[allow(clippy::too_many_lines)] // refactor candidate
    pub async fn connect(
        &mut self,
        role: NodeRole,
    ) -> Result<ConnectResult<WebSocketTransport>, Error> {
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

        // Maximum WebSocket message and frame size: one tunnel MTU plus framing overhead.
        // Anything larger is a protocol violation — reject at the tungstenite layer
        // rather than buffering into an unbounded read_buf in WsByteStream.
        let mut ws_config = WebSocketConfig::default();
        ws_config.max_message_size = Some(65_535 + 512);
        ws_config.max_frame_size = Some(65_535 + 512);

        // Connect TCP — route through proxy if HTTPS_PROXY / ALL_PROXY / SOCKS5 env vars are set
        let tcp_stream = {
            let proxy = detect_proxy(self.tls_connector.is_some(), &hostname);
            if let Some((is_socks5, proxy_host, proxy_port)) = proxy {
                if is_socks5 {
                    tracing::debug!("Connecting via SOCKS5 proxy {proxy_host}:{proxy_port}");
                    tokio_socks::tcp::Socks5Stream::connect(
                        (proxy_host.as_str(), proxy_port),
                        (hostname.as_str(), addr.port()),
                    )
                    .await
                    .map_err(|e| Error::Io(std::io::Error::other(e)))?
                    .into_inner()
                } else {
                    tracing::debug!("Connecting via HTTP CONNECT proxy {proxy_host}:{proxy_port}");
                    let mut stream = TcpStream::connect((proxy_host.as_str(), proxy_port)).await?;
                    async_http_proxy::http_connect_tokio(&mut stream, &hostname, addr.port())
                        .await
                        .map_err(|e| Error::Io(std::io::Error::other(e)))?;
                    stream
                }
            } else {
                TcpStream::connect(addr).await?
            }
        };
        let peer_addr = tcp_stream.peer_addr().ok();
        let remote_addr_str = peer_addr.map_or_else(|| addr.to_string(), |a| a.to_string());

        // Wrap in TLS if configured and perform WebSocket handshake.
        // Extract TLS channel binding BEFORE the WebSocket upgrade consumes the
        // TLS stream — after upgrade the rustls session is no longer accessible.
        let mut channel_binding = None;
        let ws_stream: WebSocketStream<MaybeTlsStream> = if let Some(connector) =
            &self.tls_connector
        {
            let server_name = rustls::pki_types::ServerName::try_from(hostname.clone())
                .map_err(|_| Error::InvalidDnsName(hostname.clone()))?;
            let tls_stream = connector.connect(server_name, tcp_stream).await?;

            // Extract channel binding from the rustls ClientConnection.
            let (_, client_conn) = tls_stream.get_ref();
            channel_binding = crate::psk::channel_binding_rustls_client(client_conn);

            let (ws, _response) = client_async_with_config(
                &url,
                MaybeTlsStream::Tls(Box::new(tls_stream)),
                Some(ws_config),
            )
            .await?;
            ws
        } else {
            let (ws, _response) =
                client_async_with_config(&url, MaybeTlsStream::Plain(tcp_stream), Some(ws_config))
                    .await?;
            ws
        };

        tracing::debug!("WebSocket connected to {addr}");

        // Convert to byte stream and wrap in yamux transport
        let byte_stream = WebSocketByteStream::new(ws_stream);
        let (transport, driver) = WebSocketTransport::new(
            byte_stream,
            Mode::Client,
            peer_addr,
            WebSocketTransportConfig::default(),
        );
        let transport = Arc::new(transport);

        // Spawn the yamux driver
        tokio::spawn(async move {
            if let Err(e) = driver.await {
                tracing::debug!("Yamux driver finished: {e}");
            }
        });

        // Create control channel
        let (control_tx, control_rx) = tokio::sync::mpsc::channel::<ControlMessage>(64);

        // Send Handshake via the control stream
        {
            let capabilities = self
                .config
                .base
                .local_handshake
                .as_ref()
                .and_then(|h| h.capabilities)
                .unwrap_or(wallhack_wire::data::Capabilities {
                    tun_capable: false,
                    listening: false,
                    connecting: true,
                });
            let mut handshake = Handshake {
                capabilities: Some(capabilities),
                name: self.config.base.name.clone().unwrap_or_default(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                psk_proof: Vec::new(),
                routes: Vec::new(),
                hint: None,
            };

            if let Some(ref psk) = self.config.base.psk {
                if let Some(ref binding) = channel_binding {
                    handshake.psk_proof = handshake.compute_psk_proof(psk.as_bytes(), binding);
                } else {
                    tracing::warn!("PSK configured but channel binding extraction failed");
                }
            }
            tracing::debug!("Queuing Handshake with name: {}", handshake.name);
            let msg = ControlMessage {
                message: Some(control_message::Message::Handshake(handshake)),
            };
            let _ = control_tx.send(msg).await;
        }

        // Create oneshot for receiving server's Handshake via the control loop.
        let (handshake_tx, handshake_rx) = tokio::sync::oneshot::channel::<Handshake>();

        // Spawn control stream task
        let transport_ctrl = Arc::clone(&transport);
        let control_handle = tokio::spawn(async move {
            let mut channels = protocol::ControlChannels {
                outgoing_rx: control_rx,
                handshake_tx: Some(handshake_tx), // receive server's Handshake
                latency_tx: None,
                control_response_tx: None,
                role_transition_tx: None,
            };
            match protocol::run_control_stream_initiator(
                &*transport_ctrl,
                &mut channels,
                None, // client doesn't handle ControlRequests
                std::time::Duration::from_secs(30),
            )
            .await
            {
                Ok(exit) => tracing::debug!("Control stream finished: {exit:?}"),
                Err(e) => tracing::debug!("Control stream error: {e}"),
            }
        });

        let channels = DataChannels::new();

        // Incoming data task: accept uni stream from peer, dispatch messages.
        let transport_data = Arc::clone(&transport);
        let instructions_in = channels.instructions_tx.clone();
        let responses_in = channels.responses_tx.clone();

        let incoming_handle = tokio::spawn(async move {
            match transport_data.accept_uni().await {
                Ok(Some(mut recv)) => {
                    if let Err(e) =
                        protocol::run_data_in(&mut recv, &instructions_in, &responses_in).await
                    {
                        tracing::debug!("Data-in handler finished: {e}");
                    }
                }
                Ok(None) => tracing::debug!("Transport closed before data-in stream accepted"),
                Err(e) => tracing::debug!("Failed to accept data-in stream: {e}"),
            }
        });

        // Outgoing data task is NOT spawned here; the caller opens the uni stream
        // and drives run_send_instructions / run_send_responses as appropriate for
        // its role, consuming the receiver from DataChannels.

        let tasks = ConnectionTasks {
            incoming: incoming_handle,
            control: control_handle,
        };

        Ok(ConnectResult::new(
            Arc::clone(&transport),
            channels,
            remote_addr_str,
            tasks,
            control_tx,
            Some(handshake_rx),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // parse_proxy_url is a pure function — test exhaustively without touching env vars.

    #[test]
    fn parse_socks5_scheme() {
        assert_eq!(
            parse_proxy_url("socks5://proxy.corp:1080"),
            Some((true, "proxy.corp".to_string(), 1080))
        );
    }

    #[test]
    fn parse_socks5h_scheme() {
        // socks5h:// (remote DNS) treated the same as socks5://
        assert_eq!(
            parse_proxy_url("socks5h://proxy.corp:1080"),
            Some((true, "proxy.corp".to_string(), 1080))
        );
    }

    #[test]
    fn parse_http_scheme() {
        assert_eq!(
            parse_proxy_url("http://squid:3128"),
            Some((false, "squid".to_string(), 3128))
        );
    }

    #[test]
    fn parse_https_scheme() {
        // https:// proxy URL is still an HTTP CONNECT proxy, not SOCKS5
        assert_eq!(
            parse_proxy_url("https://squid:3128"),
            Some((false, "squid".to_string(), 3128))
        );
    }

    #[test]
    fn parse_bare_host_port() {
        // No scheme — treated as HTTP CONNECT proxy
        assert_eq!(
            parse_proxy_url("squid.corp:3128"),
            Some((false, "squid.corp".to_string(), 3128))
        );
    }

    #[test]
    fn parse_strips_credentials() {
        assert_eq!(
            parse_proxy_url("http://user:pass@squid.corp:3128"),
            Some((false, "squid.corp".to_string(), 3128))
        );
    }

    #[test]
    fn parse_strips_credentials_socks5() {
        assert_eq!(
            parse_proxy_url("socks5://alice:secret@proxy:1080"),
            Some((true, "proxy".to_string(), 1080))
        );
    }

    #[test]
    fn parse_strips_trailing_path() {
        assert_eq!(
            parse_proxy_url("http://squid:3128/"),
            Some((false, "squid".to_string(), 3128))
        );
    }

    #[test]
    fn parse_ipv4_address() {
        assert_eq!(
            parse_proxy_url("socks5://127.0.0.1:1080"),
            Some((true, "127.0.0.1".to_string(), 1080))
        );
    }

    #[test]
    fn parse_empty_string_is_none() {
        assert_eq!(parse_proxy_url(""), None);
    }

    #[test]
    fn parse_missing_port_is_none() {
        assert_eq!(parse_proxy_url("http://squid"), None);
    }

    #[test]
    fn parse_non_numeric_port_is_none() {
        assert_eq!(parse_proxy_url("http://squid:abc"), None);
    }

    #[test]
    fn parse_port_out_of_range_is_none() {
        assert_eq!(parse_proxy_url("http://squid:99999"), None);
    }
}
