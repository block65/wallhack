//! QUIC transport implementation.
//!
//! Wraps [`quinn::Connection`] to implement the [`Transport`] trait.

use std::{
    net::SocketAddr,
    pin::Pin,
    task::{Context, Poll},
};

use quinn::{ConnectionError, RecvStream, SendStream};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tracing::debug;

use crate::{BiStream, Transport, TransportError};

/// A bidirectional QUIC stream.
///
/// Combines a [`SendStream`] and [`RecvStream`] into a single bidirectional
/// stream.
pub struct QuicBiStream {
    send: SendStream,
    recv: RecvStream,
}

impl QuicBiStream {
    /// Creates a new bidirectional stream from QUIC send and receive streams.
    #[must_use]
    pub fn new(send: SendStream, recv: RecvStream) -> Self {
        Self { send, recv }
    }
}

impl AsyncRead for QuicBiStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.recv).poll_read(cx, buf)
    }
}

impl AsyncWrite for QuicBiStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.send)
            .poll_write(cx, buf)
            .map_err(|e| std::io::Error::other(e.to_string()))
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.send)
            .poll_flush(cx)
            .map_err(|e| std::io::Error::other(e.to_string()))
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.send)
            .poll_shutdown(cx)
            .map_err(|e| std::io::Error::other(e.to_string()))
    }
}

impl BiStream for QuicBiStream {
    async fn finish(&mut self) -> Result<(), TransportError> {
        self.send
            .finish()
            .map_err(|e| TransportError::stream(e.to_string()))
    }
}

/// QUIC transport wrapping a [`quinn::Connection`].
pub struct QuicTransport {
    connection: quinn::Connection,
}

impl QuicTransport {
    /// Creates a new QUIC transport from an established connection.
    #[must_use]
    pub fn new(connection: quinn::Connection) -> Self {
        debug!(remote_addr = %connection.remote_address(), "creating QUIC transport");
        Self { connection }
    }

    /// Returns a reference to the underlying QUIC connection.
    #[must_use]
    pub fn connection(&self) -> &quinn::Connection {
        &self.connection
    }
}

/// Maps a [`quinn::ConnectionError`] to a [`TransportError`], distinguishing
/// graceful close (returns `None` sentinel) from actual errors.
fn map_quic_connection_error(e: &ConnectionError) -> Result<Option<()>, TransportError> {
    match e {
        ConnectionError::ApplicationClosed(_) | ConnectionError::LocallyClosed => Ok(None),
        ConnectionError::TimedOut => Err(TransportError::Timeout),
        ConnectionError::Reset
        | ConnectionError::TransportError(_)
        | ConnectionError::ConnectionClosed(_)
        | ConnectionError::VersionMismatch
        | ConnectionError::CidsExhausted => Err(TransportError::connection_closed(e.to_string())),
    }
}

impl Transport for QuicTransport {
    type SendStream = SendStream;
    type RecvStream = RecvStream;
    type BiStream = QuicBiStream;

    async fn open_uni(&self) -> Result<Self::SendStream, TransportError> {
        debug!(remote_addr = %self.connection.remote_address(), "opening QUIC unidirectional stream");
        self.connection.open_uni().await.map_err(|e| match e {
            ConnectionError::TimedOut => TransportError::Timeout,
            _ => TransportError::connection_closed(e.to_string()),
        })
    }

    async fn open_bi(&self) -> Result<Self::BiStream, TransportError> {
        debug!(remote_addr = %self.connection.remote_address(), "opening QUIC bidirectional stream");
        let (send, recv) = self.connection.open_bi().await.map_err(|e| match e {
            ConnectionError::TimedOut => TransportError::Timeout,
            _ => TransportError::connection_closed(e.to_string()),
        })?;
        Ok(QuicBiStream::new(send, recv))
    }

    async fn accept_uni(&self) -> Result<Option<Self::RecvStream>, TransportError> {
        match self.connection.accept_uni().await {
            Ok(stream) => Ok(Some(stream)),
            Err(e) => map_quic_connection_error(&e).map(|_| None),
        }
    }

    async fn accept_bi(&self) -> Result<Option<Self::BiStream>, TransportError> {
        match self.connection.accept_bi().await {
            Ok((send, recv)) => Ok(Some(QuicBiStream::new(send, recv))),
            Err(e) => map_quic_connection_error(&e).map(|_| None),
        }
    }

    async fn close(&self) -> Result<(), TransportError> {
        debug!(remote_addr = %self.connection.remote_address(), "closing QUIC transport");
        self.connection.close(0u32.into(), b"closing");
        Ok(())
    }

    fn remote_addr(&self) -> Option<SocketAddr> {
        Some(self.connection.remote_address())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use crate::{BiStream, Transport};

    use super::QuicTransport;

    fn make_server_config() -> (quinn::ServerConfig, CertificateDer<'static>) {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let params = rcgen::CertificateParams::new(vec!["localhost".into()]).expect("valid params");
        let key_pair = rcgen::KeyPair::generate().expect("key generation");
        let cert = params.self_signed(&key_pair).expect("self-signed cert");
        let cert_der = CertificateDer::from(cert.der().to_vec());
        let key_der = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key_pair.serialize_der()));
        let config = quinn::ServerConfig::with_single_cert(vec![cert_der.clone()], key_der)
            .expect("server config");
        (config, cert_der)
    }

    fn make_client_config(server_cert: CertificateDer<'static>) -> quinn::ClientConfig {
        let mut roots = rustls::RootCertStore::empty();
        roots.add(server_cert).expect("add root cert");
        let tls = rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        let quic =
            quinn::crypto::rustls::QuicClientConfig::try_from(tls).expect("quic client config");
        quinn::ClientConfig::new(Arc::new(quic))
    }

    async fn connected_pair() -> (QuicTransport, QuicTransport) {
        let (server_config, cert_der) = make_server_config();
        let server_ep =
            quinn::Endpoint::server(server_config, "127.0.0.1:0".parse().unwrap()).unwrap();
        let server_addr = server_ep.local_addr().unwrap();

        let mut client_ep = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
        client_ep.set_default_client_config(make_client_config(cert_der));

        let (client_conn, server_conn) = tokio::join!(
            async {
                client_ep
                    .connect(server_addr, "localhost")
                    .unwrap()
                    .await
                    .unwrap()
            },
            async { server_ep.accept().await.unwrap().await.unwrap() },
        );
        (
            QuicTransport::new(client_conn),
            QuicTransport::new(server_conn),
        )
    }

    #[tokio::test]
    async fn test_open_accept_uni() {
        let (client, server) = connected_pair().await;

        let mut send = client.open_uni().await.unwrap();
        send.write_all(b"hello quic").await.unwrap();
        send.shutdown().await.unwrap();

        let mut recv = server.accept_uni().await.unwrap().unwrap();
        let buf = recv.read_to_end(1024).await.unwrap();
        assert_eq!(buf, b"hello quic");
    }

    #[tokio::test]
    async fn test_open_accept_bi() {
        let (client, server) = connected_pair().await;

        let mut client_bi = client.open_bi().await.unwrap();
        client_bi.write_all(b"ping").await.unwrap();
        client_bi.flush().await.unwrap();

        let mut server_bi = server.accept_bi().await.unwrap().unwrap();
        let mut buf = [0u8; 4];
        server_bi.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"ping");

        server_bi.write_all(b"pong").await.unwrap();
        server_bi.finish().await.unwrap();

        let mut buf = [0u8; 4];
        client_bi.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"pong");
    }

    #[tokio::test]
    async fn test_close_maps_to_none() {
        // QuicTransport::close() sends ApplicationClosed; the peer's accept_uni
        // should return Ok(None) rather than an error.
        let (client, server) = connected_pair().await;
        client.close().await.unwrap();
        let result = server.accept_uni().await.unwrap();
        assert!(
            result.is_none(),
            "expected None after peer close, got {result:?}"
        );
    }
}
