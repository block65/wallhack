use std::sync::Arc;

use quinn::{IdleTimeout, VarInt, crypto::rustls::QuicClientConfig};
use tokio::time::Instant;
use wallhack_transport::Transport;

use crate::{
    ClientConfig, NodeRole,
    client::tls_config,
    transport::{bridge, quic::QuicTransport},
};
use wallhack_wire::{
    control::{ControlMessage, control_message},
    data::{EntryNodeInstruction, ExitNodeHello, ExitNodeResponse},
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
    psk: Option<String>,
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
        })
    }

    #[allow(clippy::too_many_lines)] // refactor candidate
    async fn connect(
        &mut self,
        role: NodeRole,
    ) -> Result<ConnectResult<Self::Transport>, Self::Error> {
        tracing::debug!(
            "{:?} connecting to {} with server name {:?}",
            role,
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
        let (control_tx, mut control_rx) = tokio::sync::mpsc::channel::<ControlMessage>(64);

        // If exit node, send Hello via the control stream
        if let Some(ref name) = self.name {
            tracing::debug!("Queuing ExitNodeHello with name: {}", name);
            let hello = ControlMessage {
                message: Some(control_message::Message::Hello(ExitNodeHello {
                    name: name.clone(),
                    version: env!("CARGO_PKG_VERSION").to_string(),
                    auth_token: self.psk.clone().unwrap_or_default(),
                })),
            };
            control_tx.send(hello).await.map_err(|_| {
                std::io::Error::other("control channel closed before Hello could be sent")
            })?;
        }

        // Spawn control stream task
        let transport_ctrl = Arc::clone(&transport);
        let control_handle = tokio::spawn(async move {
            match bridge::run_control_stream_initiator(
                &*transport_ctrl,
                &mut control_rx,
                None, // client doesn't handle ControlRequests
                None, // pong handled inline
                None, // no ControlResponse channel needed now
                std::time::Duration::from_secs(30),
            )
            .await
            {
                Ok(exit) => tracing::debug!("Control stream finished: {exit:?}"),
                Err(e) => tracing::debug!("Control stream error: {e}"),
            }
        });

        let (instructions, _) = tokio::sync::broadcast::channel::<EntryNodeInstruction>(65536);
        let (responses, _) = tokio::sync::broadcast::channel::<ExitNodeResponse>(65536);

        // Data task 1: Incoming data (accept uni stream, read data messages)
        let transport_data = Arc::clone(&transport);
        let instructions_tx = instructions.clone();
        let responses_tx = responses.clone();

        let incoming_handle = tokio::spawn(async move {
            // Accept uni stream from peer for incoming data
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
                tracing::debug!("Opening stream to send instructions to peer");
                let transport_out = Arc::clone(&transport);
                // Subscribe before spawning so messages sent while open_uni() is
                // in-flight are not dropped.
                let instructions_rx = instructions.subscribe();

                tokio::spawn(async move {
                    match transport_out.open_uni().await {
                        Ok(mut send) => {
                            if let Err(e) =
                                bridge::run_send_instructions(&mut send, instructions_rx).await
                            {
                                tracing::debug!("Send-instructions handler finished: {e}");
                            }
                        }
                        Err(e) => tracing::debug!("Failed to open send stream: {e}"),
                    }
                })
            }
            NodeRole::Exit => {
                tracing::debug!("Opening stream to send responses to peer");
                let transport_out = Arc::clone(&transport);
                // Subscribe before spawning so messages sent while open_uni() is
                // in-flight are not dropped.
                let responses_rx = responses.subscribe();

                tokio::spawn(async move {
                    match transport_out.open_uni().await {
                        Ok(mut send) => {
                            if let Err(e) =
                                bridge::run_send_responses(&mut send, responses_rx).await
                            {
                                tracing::debug!("Send-responses handler finished: {e}");
                            }
                        }
                        Err(e) => tracing::debug!("Failed to open send stream: {e}"),
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
            remote_addr,
            tasks,
            control_tx,
        ))
    }

    fn stop(&self) -> Result<(), Self::Error> {
        self.endpoint.close(0u32.into(), b"client stopping");
        Ok(())
    }
}
