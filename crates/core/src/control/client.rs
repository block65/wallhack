//! Control channel QUIC client implementation.
//!
//! Provides a client for connecting to control servers.

use std::{
    net::{Ipv4Addr, SocketAddr, SocketAddrV4},
    sync::Arc,
};

use prost::Message;
use quinn::{ClientConfig, Connection, Endpoint};
use wallhack_wire::control::{ControlRequest, ControlResponse};

use crate::server::tls::ALPN_QUIC_HTTP;

/// Maximum control message size (1 MB).
const MAX_MESSAGE_SIZE: usize = 1024 * 1024;

/// Errors that can occur in the control client.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Protocol error: {0}")]
    Protocol(#[from] prost::DecodeError),

    #[error("QUIC connection error: {0}")]
    Connection(#[from] quinn::ConnectionError),

    #[error("QUIC connect error: {0}")]
    Connect(#[from] quinn::ConnectError),

    #[error("QUIC read error: {0}")]
    Read(#[from] quinn::ReadToEndError),

    #[error("QUIC write error: {0}")]
    Write(#[from] quinn::WriteError),

    #[error("TLS error: {0}")]
    Tls(#[from] rustls::Error),

    #[error("Stream closed: {0}")]
    StreamClosed(#[from] quinn::ClosedStream),

    #[error("QUIC crypto error: {0}")]
    QuicCrypto(#[from] quinn::crypto::rustls::NoInitialCipherSuite),

    #[error("Empty response")]
    EmptyResponse,
}

/// Control client for connecting to a control server.
pub struct ControlClient {
    endpoint: Endpoint,
    connection: Connection,
}

impl ControlClient {
    /// Connects to a control server at the given address.
    ///
    /// Uses an insecure TLS configuration that skips certificate verification.
    /// This is appropriate for local/trusted network control connections.
    ///
    /// # Errors
    ///
    /// Returns an error if the connection cannot be established.
    pub async fn connect(addr: SocketAddr, server_name: &str) -> Result<Self, Error> {
        let client_config = insecure_client_config()?;
        let bind_addr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0));
        let endpoint = Endpoint::client(bind_addr)?;

        let connection = endpoint
            .connect_with(client_config, addr, server_name)?
            .await?;

        Ok(Self {
            endpoint,
            connection,
        })
    }

    /// Sends a control request and returns the response.
    ///
    /// # Errors
    ///
    /// Returns an error if the request cannot be sent or the response cannot be read.
    pub async fn request(&self, request: ControlRequest) -> Result<ControlResponse, Error> {
        // Open bidirectional stream for this request
        let (mut send, mut recv) = self.connection.open_bi().await?;

        // Encode and send request
        let request_bytes = request.encode_to_vec();
        send.write_all(&request_bytes).await?;
        send.finish()?;

        // Read response
        let response_bytes = recv.read_to_end(MAX_MESSAGE_SIZE).await?;
        let response = ControlResponse::decode(&response_bytes[..])?;

        Ok(response)
    }

    /// Returns the remote address of the connected server.
    #[must_use]
    pub fn remote_addr(&self) -> SocketAddr {
        self.connection.remote_address()
    }

    /// Closes the connection.
    pub fn close(&self) {
        self.connection.close(0u32.into(), b"client closing");
    }
}

impl Drop for ControlClient {
    fn drop(&mut self) {
        self.endpoint.close(0u32.into(), b"client dropped");
    }
}

/// Creates an insecure client configuration that skips certificate verification.
fn insecure_client_config() -> Result<ClientConfig, Error> {
    let _ = rustls::crypto::ring::default_provider().install_default();

    let mut crypto = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(SkipServerVerification))
        .with_no_client_auth();

    crypto.alpn_protocols = ALPN_QUIC_HTTP.iter().map(|&x| x.into()).collect();

    Ok(ClientConfig::new(Arc::new(
        quinn::crypto::rustls::QuicClientConfig::try_from(crypto)?,
    )))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        NodeRole,
        control::{handler::HandlerConfig, metrics::Metrics, server::ControlServer},
    };
    use std::time::Duration;
    use wallhack_wire::control::{PingRequest, StatsRequest, control_request, control_response};

    #[tokio::test]
    async fn test_client_ping() {
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

        // Spawn server
        tokio::spawn(async move { server.run().await });

        // Give server time to start
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Connect client
        let client = ControlClient::connect(server_addr, "localhost")
            .await
            .unwrap();

        // Send ping
        let request = ControlRequest {
            request: Some(control_request::Request::Ping(PingRequest {})),
        };
        let response = client.request(request).await.unwrap();

        match response.response {
            Some(control_response::Response::Ping(ping)) => {
                assert!(!ping.version.is_empty());
            }
            _ => panic!("Expected ping response"),
        }

        // Send stats request
        let request = ControlRequest {
            request: Some(control_request::Request::Stats(StatsRequest {})),
        };
        let response = client.request(request).await.unwrap();

        match response.response {
            Some(control_response::Response::Stats(stats)) => {
                assert_eq!(stats.bytes_in, 0);
            }
            _ => panic!("Expected stats response"),
        }

        client.close();
    }
}
