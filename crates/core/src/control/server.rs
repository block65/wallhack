//! Control channel QUIC server implementation.
//!
//! Provides a QUIC server for control connections from REPL clients.

use std::{net::SocketAddr, sync::Arc, time::Duration};

use prost::Message;
use quinn::{Endpoint, IdleTimeout, RecvStream, SendStream, crypto::rustls::QuicServerConfig};
use wallhack_wire::control::ControlRequest;

use crate::server::tls::{ALPN_QUIC_HTTP, configure_crypto};

use super::{
    handler::{Handler, HandlerConfig},
    metrics::SharedMetrics,
    peers::Registry,
    routes::RouteTable,
};

/// Maximum control message size (1 MB).
const MAX_MESSAGE_SIZE: usize = 1024 * 1024;

/// Errors that can occur in the control server.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Protocol error: {0}")]
    Protocol(#[from] prost::DecodeError),

    #[error("QUIC connection error: {0}")]
    Connection(#[from] quinn::ConnectionError),

    #[error("QUIC read error: {0}")]
    Read(#[from] quinn::ReadToEndError),

    #[error("QUIC write error: {0}")]
    Write(#[from] quinn::WriteError),

    #[error("TLS error: {0}")]
    Tls(#[from] rustls::Error),

    #[error("TLS config error: {0}")]
    TlsConfig(#[from] crate::server::tls::Error),

    #[error("QUIC crypto error: {0}")]
    QuicCrypto(#[from] quinn::crypto::rustls::NoInitialCipherSuite),

    #[error("QUIC bounds error: {0}")]
    QuicBounds(#[from] quinn::VarIntBoundsExceeded),

    #[error("Stream closed: {0}")]
    StreamClosed(#[from] quinn::ClosedStream),

    #[error("Message too large: {0} bytes")]
    MessageTooLarge(usize),
}

/// Control server that listens on a QUIC endpoint.
pub struct ControlServer {
    endpoint: Endpoint,
    handler: Arc<Handler>,
}

impl ControlServer {
    /// Creates a new control server bound to the given address.
    ///
    /// # Errors
    ///
    /// Returns an error if the endpoint cannot be created or bound.
    pub fn bind(
        addr: SocketAddr,
        config: HandlerConfig,
        metrics: SharedMetrics,
    ) -> Result<Self, Error> {
        let (cert_der, priv_key, _fingerprint) = configure_crypto(None)?;

        let mut server_crypto = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(cert_der, priv_key)?;

        server_crypto.alpn_protocols = ALPN_QUIC_HTTP.iter().map(|&x| x.into()).collect();

        let mut server_config =
            quinn::ServerConfig::with_crypto(Arc::new(QuicServerConfig::try_from(server_crypto)?));

        let transport_config = Arc::get_mut(&mut server_config.transport)
            .ok_or_else(|| Error::Io(std::io::Error::other("transport config not unique")))?;
        let timeout = IdleTimeout::try_from(Duration::from_secs(30))?;
        transport_config.max_idle_timeout(Some(timeout));
        transport_config.keep_alive_interval(Some(Duration::from_secs(10)));

        let (route_updates, _) = tokio::sync::broadcast::channel(16);
        let endpoint = Endpoint::server(server_config, addr)?;
        let handler = Arc::new(Handler::new(
            config,
            metrics,
            Arc::new(Registry::new()),
            RouteTable::shared(),
            route_updates,
        ));

        Ok(Self { endpoint, handler })
    }

    /// Returns the local address this server is bound to.
    ///
    /// # Errors
    ///
    /// Returns an error if the local address cannot be retrieved.
    pub fn local_addr(&self) -> Result<SocketAddr, Error> {
        Ok(self.endpoint.local_addr()?)
    }

    /// Runs the control server, accepting and handling connections.
    ///
    /// This method runs indefinitely until cancelled.
    ///
    /// # Cancellation Safety
    ///
    /// This method is cancellation safe. If cancelled, any in-progress
    /// connection handling will be terminated.
    pub async fn run(&self) -> Result<(), Error> {
        tracing::info!(
            "Control server listening on {}",
            self.endpoint.local_addr()?
        );

        while let Some(incoming) = self.endpoint.accept().await {
            let handler = Arc::clone(&self.handler);

            tokio::spawn(async move {
                match incoming.await {
                    Ok(connection) => {
                        let remote = connection.remote_address();
                        tracing::debug!("Control connection from {remote}");

                        if let Err(e) = handle_connection(connection, handler).await {
                            tracing::warn!("Control connection error from {remote}: {e}");
                        }
                    }
                    Err(e) => {
                        tracing::warn!("Failed to accept control connection: {e}");
                    }
                }
            });
        }

        Ok(())
    }

    /// Accepts a single connection and handles it.
    ///
    /// Returns after the connection is closed.
    ///
    /// # Errors
    ///
    /// Returns an error if accepting or handling the connection fails.
    pub async fn accept_one(&self) -> Result<(), Error> {
        let incoming = self
            .endpoint
            .accept()
            .await
            .ok_or_else(|| Error::Io(std::io::Error::other("endpoint closed")))?;

        let connection = incoming.await?;
        handle_connection(connection, Arc::clone(&self.handler)).await
    }

    /// Shuts down the control server.
    pub fn shutdown(&self) {
        self.endpoint.close(0u32.into(), b"shutdown");
    }
}

/// Handles a single control connection.
///
/// Uses bidirectional streams for request-response pairs.
async fn handle_connection(
    connection: quinn::Connection,
    handler: Arc<Handler>,
) -> Result<(), Error> {
    loop {
        // Accept bidirectional stream for each request-response pair
        let (send, recv) = match connection.accept_bi().await {
            Ok(streams) => streams,
            Err(quinn::ConnectionError::ApplicationClosed(_)) => {
                tracing::debug!("Control connection closed by client");
                return Ok(());
            }
            Err(quinn::ConnectionError::LocallyClosed) => {
                return Ok(());
            }
            Err(e) => return Err(e.into()),
        };

        let handler = Arc::clone(&handler);
        tokio::spawn(async move {
            if let Err(e) = handle_request(send, recv, handler).await {
                tracing::warn!("Control request error: {e}");
            }
        });
    }
}

/// Handles a single control request on a bidirectional stream.
async fn handle_request(
    mut send: SendStream,
    mut recv: RecvStream,
    handler: Arc<Handler>,
) -> Result<(), Error> {
    // Read request
    let request_bytes = recv.read_to_end(MAX_MESSAGE_SIZE).await?;

    // Decode request
    let request = ControlRequest::decode(&request_bytes[..])?;

    // Handle request
    let response = handler.handle(request);

    // Encode and send response
    let response_bytes = response.encode_to_vec();
    send.write_all(&response_bytes).await?;
    send.finish()?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{NodeRole, control::metrics::Metrics};
    use quinn::ClientConfig;
    use std::time::Duration;
    use wallhack_wire::control::{ControlResponse, PingRequest, control_request, control_response};

    fn insecure_client_config() -> ClientConfig {
        let mut crypto = rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(SkipServerVerification))
            .with_no_client_auth();

        // Match the server's ALPN protocols
        crypto.alpn_protocols = ALPN_QUIC_HTTP.iter().map(|&x| x.into()).collect();

        ClientConfig::new(Arc::new(
            quinn::crypto::rustls::QuicClientConfig::try_from(crypto).unwrap(),
        ))
    }

    #[derive(Debug)]
    struct SkipServerVerification;

    impl rustls::client::danger::ServerCertVerifier for SkipServerVerification {
        fn verify_server_cert(
            &self,
            _end_entity: &rustls::pki_types::CertificateDer<'_>,
            _intermediates: &[rustls::pki_types::CertificateDer<'_>],
            _server_name: &rustls::pki_types::ServerName<'_>,
            _ocsp_response: &[u8],
            _now: rustls::pki_types::UnixTime,
        ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
            Ok(rustls::client::danger::ServerCertVerified::assertion())
        }

        fn verify_tls12_signature(
            &self,
            _message: &[u8],
            _cert: &rustls::pki_types::CertificateDer<'_>,
            _dss: &rustls::DigitallySignedStruct,
        ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
            Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
        }

        fn verify_tls13_signature(
            &self,
            _message: &[u8],
            _cert: &rustls::pki_types::CertificateDer<'_>,
            _dss: &rustls::DigitallySignedStruct,
        ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
            Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
        }

        fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
            vec![
                rustls::SignatureScheme::RSA_PKCS1_SHA256,
                rustls::SignatureScheme::RSA_PKCS1_SHA384,
                rustls::SignatureScheme::RSA_PKCS1_SHA512,
                rustls::SignatureScheme::ECDSA_NISTP256_SHA256,
                rustls::SignatureScheme::ECDSA_NISTP384_SHA384,
                rustls::SignatureScheme::ECDSA_NISTP521_SHA512,
                rustls::SignatureScheme::RSA_PSS_SHA256,
                rustls::SignatureScheme::RSA_PSS_SHA384,
                rustls::SignatureScheme::RSA_PSS_SHA512,
                rustls::SignatureScheme::ED25519,
            ]
        }
    }

    #[tokio::test]
    async fn test_server_ping() {
        let metrics = Arc::new(Metrics::default());
        let server = ControlServer::bind(
            "127.0.0.1:0".parse().unwrap(),
            HandlerConfig::new(
                NodeRole::Entry,
                "wallhackd".to_string(),
                "0.0.0".to_string(),
            ),
            metrics,
        )
        .unwrap();

        let server_addr = server.local_addr().unwrap();

        // Spawn server to handle one connection
        let server_handle = tokio::spawn(async move { server.accept_one().await });

        // Give server time to start
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Create client endpoint
        let client_endpoint = Endpoint::client("0.0.0.0:0".parse().unwrap()).unwrap();

        // Connect to server
        let connection = client_endpoint
            .connect_with(insecure_client_config(), server_addr, "localhost")
            .unwrap()
            .await
            .unwrap();

        // Open bidirectional stream
        let (mut send, mut recv) = connection.open_bi().await.unwrap();

        // Send ping request
        let request = ControlRequest {
            request: Some(control_request::Request::Ping(PingRequest {})),
        };
        let request_bytes = request.encode_to_vec();
        send.write_all(&request_bytes).await.unwrap();
        send.finish().unwrap();

        // Read response
        let response_bytes = recv.read_to_end(MAX_MESSAGE_SIZE).await.unwrap();
        let response = ControlResponse::decode(&response_bytes[..]).unwrap();

        match response.response {
            Some(control_response::Response::Ping(ping)) => {
                assert!(!ping.version.is_empty());
            }
            _ => panic!("Expected ping response"),
        }

        // Close connection
        connection.close(0u32.into(), b"done");

        // Wait for server
        server_handle.await.unwrap().unwrap();
    }
}
