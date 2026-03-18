use std::sync::Arc;

use quinn::{IdleTimeout, VarInt, crypto::rustls::QuicClientConfig};
use tokio::time::Instant;
use wallhack_transport::Transport;

use crate::{
    ClientConfig, NodeRole,
    client::tls_config,
    psk::HandshakeExt,
    server::server::DataChannels,
    transport::{protocol, quic::QuicTransport},
};
use wallhack_wire::{
    control::{ControlMessage, control_message},
    data::Handshake,
};

use super::client::{Client, ConnectResult, ConnectionTasks};

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("failed to connect to server: {0}")]
    Connection(quinn::ConnectionError),

    #[error("failed to connect to server: {0}")]
    Connect(quinn::ConnectError),

    #[error("failed to read from stream: {0}")]
    StreamRead(#[from] quinn::ReadError),

    #[error("timeout waiting for stream: {0}")]
    StreamReadTimeout(tokio::time::error::Elapsed),

    #[error("failed to read from stream: {0}")]
    StreamReadToEnd(quinn::ReadToEndError),

    #[error("failed to write to stream: {0}")]
    StreamWrite(quinn::WriteError),

    #[error(transparent)]
    CryptoError(#[from] quinn::crypto::rustls::NoInitialCipherSuite),

    #[error(transparent)]
    ConnectError(#[from] quinn::ConnectError),

    #[error(transparent)]
    ConnectionError(#[from] quinn::ConnectionError),

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    TlsConfig(#[from] tls_config::Error),

    #[error("transport error: {0}")]
    Transport(#[from] wallhack_transport::TransportError),
}

pub struct QuicClient {
    addr: std::net::SocketAddr,
    hostname: String,
    endpoint: quinn::Endpoint,
    name: Option<String>,
    psk: Option<zeroize::Zeroizing<String>>,
    local_handshake: Option<Handshake>,
    /// Peer registry for direct latency updates in the control loop.
    /// Set by the daemon mode before calling `connect()`.
    pub peer_registry: Option<std::sync::Arc<crate::control::peers::Registry>>,
}

impl Client for QuicClient {
    type Error = Error;
    type Transport = QuicTransport;

    fn try_new(config: ClientConfig) -> Result<Self, Error> {
        let tls_config = tls_config::client_config(config.mtls, config.accept_fingerprint)?;

        let mut transport_config = quinn::TransportConfig::default();
        transport_config.max_idle_timeout(Some(IdleTimeout::from(VarInt::MAX)));
        transport_config.keep_alive_interval(Some(std::time::Duration::from_secs(5)));
        // Increase stream limits for high-throughput UDP (each packet uses a bi-stream)
        transport_config.max_concurrent_bidi_streams(10_000u32.into());
        transport_config.max_concurrent_uni_streams(1_000u32.into());

        let mut client_config: quinn::ClientConfig =
            quinn::ClientConfig::new(Arc::new(QuicClientConfig::try_from(tls_config)?));

        client_config.transport_config(Arc::new(transport_config));

        let mut endpoint = quinn::Endpoint::client(config.bind)?;
        endpoint.set_default_client_config(client_config);

        let hostname = if let Some(host) = config.hostname {
            host
        } else {
            env!("CARGO_PKG_NAME").to_string()
        };

        Ok(Self {
            addr: config.addr,
            hostname,
            endpoint,
            name: config.name,
            psk: config.psk,
            local_handshake: config.local_handshake,
            peer_registry: None,
        })
    }

    #[allow(clippy::too_many_lines)] // refactor candidate
    async fn connect(
        &mut self,
        role: NodeRole,
    ) -> Result<ConnectResult<Self::Transport>, Self::Error> {
        tracing::debug!(
            "Connecting to {} (role={role:?}, server_name={:?})",
            self.addr,
            self.hostname,
        );

        let start = Instant::now();
        let conn = self
            .endpoint
            .connect(self.addr, self.hostname.as_str())?
            .await?;

        tracing::debug!("connected after {:#?}", start.elapsed());

        let remote_addr = conn.remote_address().to_string();

        // Wrap connection in transport abstraction
        let transport = Arc::new(QuicTransport::new(conn));

        // Create control channel
        let (control_tx, control_rx) = tokio::sync::mpsc::channel::<ControlMessage>(64);

        // Send Handshake via the control stream
        {
            let mut handshake = self.local_handshake.clone().unwrap_or_else(|| Handshake {
                capabilities: Some(wallhack_wire::data::Capabilities {
                    tun_capable: false,
                    listening: false,
                    connecting: true,
                    interactive: false,
                }),
                name: self.name.clone().unwrap_or_default(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                psk_proof: Vec::new(),
                routes: Vec::new(),
                hint: None,
            });
            // Name from config takes precedence (the handshake name is set before
            // the PSK proof so that `name` is consistent with what the server sees).
            if let Some(ref n) = self.name {
                handshake.name = n.clone();
            }

            if let Some(ref psk) = self.psk {
                if let Some(binding) = crate::psk::channel_binding_quic(transport.connection()) {
                    handshake.psk_proof = handshake.compute_psk_proof(psk.as_bytes(), &binding);
                } else {
                    tracing::warn!("PSK configured but channel binding extraction failed");
                }
            }
            tracing::debug!("Queuing Handshake with name: {}", handshake.name);
            let msg = ControlMessage {
                message: Some(control_message::Message::Handshake(handshake)),
            };
            control_tx.send(msg).await.map_err(|_| {
                std::io::Error::other("control channel closed before Handshake could be sent")
            })?;
        }

        // Create oneshot for receiving server's Handshake via the control loop.
        let (handshake_tx, handshake_rx) = tokio::sync::oneshot::channel::<Handshake>();
        // Spawn control stream task
        let control_handle = {
            let transport = Arc::clone(&transport);
            let peer_registry = self.peer_registry.clone();
            tokio::spawn(async move {
                let mut channels = protocol::ControlChannels {
                    outgoing_rx: control_rx,
                    handshake_tx: Some(handshake_tx),
                    control_response_tx: None,
                    peer_registry,
                    peer_name: None,
                };
                match protocol::run_control_stream_initiator(
                    &*transport,
                    &mut channels,
                    None, // client doesn't handle ControlRequests
                    std::time::Duration::from_secs(30),
                )
                .await
                {
                    Ok(exit) => tracing::debug!("Control stream finished: {exit:?}"),
                    Err(e) => tracing::debug!("Control stream error: {e}"),
                }
            })
        };

        let channels = DataChannels::new();

        // Incoming data task: accept uni stream from peer, dispatch messages.
        let incoming_handle = {
            let transport = Arc::clone(&transport);
            let instructions_tx = channels.instructions_tx.clone();
            let responses_tx = channels.responses_tx.clone();
            tokio::spawn(async move {
                match transport.accept_uni().await {
                    Ok(Some(mut recv)) => {
                        if let Err(e) =
                            protocol::run_data_in(&mut recv, &instructions_tx, &responses_tx).await
                        {
                            tracing::debug!("Data-in handler finished: {e}");
                        }
                    }
                    Ok(None) => tracing::debug!("Transport closed before data-in stream accepted"),
                    Err(e) => tracing::debug!("Failed to accept data-in stream: {e}"),
                }
            })
        };

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
            remote_addr,
            tasks,
            control_tx,
            Some(handshake_rx),
        ))
    }

    fn stop(&self) -> Result<(), Self::Error> {
        self.endpoint.close(0u32.into(), b"client stopping");
        Ok(())
    }
}
