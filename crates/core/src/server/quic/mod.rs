use std::{sync::Arc, time::Duration};

use quinn::{IdleTimeout, crypto::rustls::QuicServerConfig};
use wallhack_transport::Transport;
use wallhack_wire::{
    control::{ControlMessage, control_message},
    data::Handshake,
};

use crate::{
    NodeRole, SocketAddrExt as _,
    psk::HandshakeExt,
    server::tls::{ALPN_QUIC_HTTP, configure_crypto},
    transport::{
        protocol,
        protocol::{AsyncProtoRead as _, AsyncProtoWrite as _},
        quic::QuicTransport,
    },
};

use super::{
    config::ServerConfig,
    server::{AcceptResult, Server, ServerOptions},
    tls,
};

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("tls config error: {0}")]
    StartTls(#[from] quinn::crypto::rustls::NoInitialCipherSuite),

    #[error("tls config error: {0}")]
    Connection(#[from] quinn::ConnectionError),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("server tls error: {0}")]
    ServerTls(#[from] tls::Error),

    #[error("tls error: {0}")]
    Tls(#[from] rustls::Error),

    #[error("quinn bounds error: {0}")]
    Quinn(#[from] quinn::VarIntBoundsExceeded),
}

pub struct QuicServer {
    endpoint: quinn::Endpoint,
    options: ServerOptions,
    fingerprint: String,
    psk: Option<zeroize::Zeroizing<String>>,
}

impl Server for QuicServer {
    type Error = Error;
    type Transport = QuicTransport;

    fn try_new(mut config: ServerConfig, options: ServerOptions) -> Result<Self, Error> {
        let ca_roots_path = config.tls.as_mut().and_then(|t| t.ca_roots.take());
        let (cert_der, priv_key, fingerprint) = configure_crypto(config.tls)?;

        let mut server_crypto = if let Some(ca_path) = ca_roots_path {
            let roots = tls::load_ca_roots(&ca_path)?;
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

        server_crypto.alpn_protocols = ALPN_QUIC_HTTP.iter().map(|&x| x.into()).collect();

        let mut server_config =
            quinn::ServerConfig::with_crypto(Arc::new(QuicServerConfig::try_from(server_crypto)?));

        let transport_config =
            Arc::get_mut(&mut server_config.transport).expect("transport config has no other refs");

        let timeout = IdleTimeout::try_from(Duration::from_mins(1))?;
        transport_config.max_idle_timeout(Some(timeout));
        transport_config.keep_alive_interval(Some(Duration::from_secs(10)));

        tracing::trace!("Server Config {:?}", server_config);
        tracing::debug!("will listen on {}", config.listen);

        let endpoint = quinn::Endpoint::server(server_config, config.listen)?;

        tracing::debug!("local_addr {:?}", endpoint.local_addr());

        Ok(Self {
            endpoint,
            options,
            fingerprint,
            psk: config.psk,
        })
    }

    #[allow(clippy::too_many_lines)] // sequential accept pipeline; splitting would obscure the flow
    async fn accept(
        &mut self,
        _role: NodeRole,
    ) -> Result<Option<AcceptResult<Self::Transport>>, Error> {
        tracing::debug!("waiting for next connection...");

        let Some(incoming) = self.endpoint.accept().await else {
            return Err(Error::Io(std::io::Error::other(
                "failed to accept incoming connection",
            )));
        };

        let connection = incoming.await?;
        let remote_addr = connection.remote_address().normalize().to_string();

        // Wrap connection in transport abstraction
        let transport = Arc::new(QuicTransport::new(connection));

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

        // Extract channel binding for PSK proof (used for both sending and
        // verifying). Must happen before we send our handshake.
        let channel_binding = crate::psk::channel_binding_quic(transport.connection());

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

        let control_stream = wallhack_transport::erased::BoxBiStream::new(control_stream);
        Ok(Some(super::server::spawn_server_tasks(
            transport,
            control_stream,
            &self.options,
            peer_handshake,
            remote_addr,
            channel_binding,
        )))
    }

    fn stop(&self) -> Result<(), Self::Error> {
        self.endpoint.close(0_u32.into(), b"server stopping");
        tracing::info!("QUIC server endpoint close initiated.");
        Ok(())
    }

    fn protocol_name(&self) -> &'static str {
        "QUIC"
    }

    fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    fn psk(&self) -> Option<&str> {
        self.psk.as_ref().map(|s| s.as_str())
    }

    fn local_addr(&self) -> std::io::Result<std::net::SocketAddr> {
        self.endpoint.local_addr()
    }
}
