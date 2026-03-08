//! Exit node implementation.
//!
//! The exit node processes incoming instructions by making syscalls to the
//! local network. It can either connect to a source peer (default) or
//! listen for incoming connections. The daemon is headless — no REPL, no TTY.

use std::{sync::Arc, time::Duration};

use tokio::io::AsyncWriteExt;

use wallhack_core::{
    NodeRole,
    control::{handler::SharedNodeState, metrics::Metrics, peers::Registry},
    exit::{net::SyscallExitAdapter, orchestrator::Orchestrator},
    server::server::Server,
    transport::{
        BiStream, ErasedTransport, Transport,
        protocol::{AsyncProtoRead as _, AsyncProtoWrite as _, TCP_STREAM_HEADER_MTU},
    },
};

use crate::{
    NodeError,
    address_spec::{AddressSpec, ConnectivitySpec, Protocol},
    config::SecurityParams,
    daemon_config::{ExitConfig, GlobalConfig},
};

/// Delay before reconnecting after an established session drops.
const RECONNECT_DELAY: Duration = Duration::from_millis(500);
/// Timeout for UDP response after forwarding packet.
const UDP_RESPONSE_TIMEOUT: Duration = Duration::from_millis(500);

/// Shared state threaded through the exit node's connection lifecycle.
struct ExitContext {
    metrics: Arc<Metrics>,
    peers: Arc<Registry>,
    node_state: SharedNodeState,
}

/// Run as an exit node (headless daemon).
///
/// State machine dispatches to mode-specific functions based on the current
/// connect/listen configuration.
///
/// # Errors
///
/// Returns error if orchestrator fails (connection errors are retried).
pub async fn run(
    global: &GlobalConfig,
    cfg: &ExitConfig,
    metrics: Arc<Metrics>,
    peers: Arc<Registry>,
    node_state: SharedNodeState,
) -> Result<(), NodeError> {
    let security = SecurityParams {
        psk: global.psk.clone(),
        accept_fingerprint: cfg.accept_fingerprint.clone(),
    };

    let ctx = Arc::new(ExitContext {
        metrics,
        peers,
        node_state,
    });

    match &cfg.connectivity {
        ConnectivitySpec::Both { .. } => Err(NodeError::Config(
            "exit nodes do not support both connect and listen simultaneously; use relay mode"
                .into(),
        )),
        ConnectivitySpec::Connect(spec) => {
            ctx.node_state.set_connected(&spec.addr);
            run_exit_connector(global, &cfg.name, spec, &security, &ctx).await
        }
        ConnectivitySpec::Listen(spec) => run_exit_listener(global, &cfg.name, spec, &ctx).await,
    }
}

/// Run in connect-only mode (standard exit).
#[allow(clippy::too_many_lines)] // verbose due to #[cfg] feature-gate branches per protocol
async fn run_exit_connector(
    global: &GlobalConfig,
    name: &str,
    spec: &AddressSpec,
    security: &SecurityParams,
    ctx: &Arc<ExitContext>,
) -> Result<(), NodeError> {
    tracing::info!("Connecting to {}...", spec.addr);
    let endpoint =
        crate::transport::resolve_endpoint(&spec.addr, global.dns_server.as_deref()).await?;
    let peer_addr = endpoint.to_string();

    match spec.protocol {
        Protocol::Udp => {
            #[cfg(feature = "quic")]
            {
                let client_config = crate::config::build_quic_client_config(
                    global,
                    endpoint,
                    Some(name.to_string()),
                    security,
                    None,
                );
                let ctx = Arc::clone(ctx);
                let pa = peer_addr.clone();
                crate::transport::connect_loop(
                    || {
                        let cfg = client_config.clone();
                        async move {
                            use wallhack_core::client::client::Client;
                            let mut client = wallhack_core::client::quic::QuicClient::try_new(cfg)?;
                            client.connect(NodeRole::Exit).await
                        }
                    },
                    |connect_result| {
                        let e = connect_result.erase();
                        let ctx = Arc::clone(&ctx);
                        let pa = pa.clone();
                        async move {
                            run_exit_loop_inner(
                                e.transport,
                                e.channels.instructions_rx,
                                e.channels.responses_tx,
                                e.channels.responses_rx,
                                e.control_tx,
                                e.tasks,
                                e.peer_handshake_rx,
                                &pa,
                                &ctx,
                            )
                            .await
                        }
                    },
                    RECONNECT_DELAY,
                )
                .await
            }
            #[cfg(not(feature = "quic"))]
            {
                Err(NodeError::TransportUnavailable("quic"))
            }
        }
        Protocol::Tcp => {
            #[cfg(feature = "websocket")]
            {
                let client_config = crate::config::build_ws_client_config(
                    global,
                    endpoint,
                    Some(name.to_string()),
                    security,
                    None,
                );
                let ctx = Arc::clone(ctx);
                let pa = peer_addr.clone();
                crate::transport::connect_loop(
                    || {
                        let cfg = client_config.clone();
                        async move {
                            let mut client = wallhack_core::client::ws::WsClient::new(cfg)?;
                            client.connect(NodeRole::Exit).await
                        }
                    },
                    |connect_result| {
                        let e = connect_result.erase();
                        let ctx = Arc::clone(&ctx);
                        let pa = pa.clone();
                        async move {
                            run_exit_loop_inner(
                                e.transport,
                                e.channels.instructions_rx,
                                e.channels.responses_tx,
                                e.channels.responses_rx,
                                e.control_tx,
                                e.tasks,
                                e.peer_handshake_rx,
                                &pa,
                                &ctx,
                            )
                            .await
                        }
                    },
                    RECONNECT_DELAY,
                )
                .await
            }
            #[cfg(not(feature = "websocket"))]
            {
                Err(NodeError::TransportUnavailable("websocket"))
            }
        }
    }
}

/// Run in listen mode.
async fn run_exit_listener(
    global: &GlobalConfig,
    node_name: &str,
    spec: &AddressSpec,
    ctx: &Arc<ExitContext>,
) -> Result<(), NodeError> {
    use wallhack_core::{control::handler::HandlerConfig, server::server::ServerOptions};

    let addr: std::net::SocketAddr = spec.addr.parse::<crate::net::ListenAddr>()?.into();

    let server_options = ServerOptions {
        handler_config: HandlerConfig::new(
            NodeRole::Exit,
            crate::built_info::PKG_NAME.to_string(),
            crate::built_info::PKG_VERSION.to_string(),
        ),
        metrics: Some(Arc::clone(&ctx.metrics)),
        peers: Some(Arc::clone(&ctx.peers)),
        routes: None,
        local_handshake: Some(wallhack_wire::data::Handshake {
            capabilities: Some(wallhack_wire::data::Capabilities {
                tun_capable: false,
                listening: true,
                connecting: false,
            }),
            name: node_name.to_string(),
            version: crate::built_info::PKG_VERSION.to_string(),
            psk_proof: Vec::new(),
            routes: Vec::new(),
            hint: None,
        }),
    };

    let server_config =
        crate::config::build_server_config(&global.tls, addr, global.psk.clone(), None);

    match spec.protocol {
        Protocol::Udp => {
            #[cfg(feature = "quic")]
            {
                let server =
                    wallhack_core::server::quic::QuicServer::try_new(server_config, server_options)
                        .map_err(|e| NodeError::Transport(Box::new(e)))?;
                let bound = server.local_addr()?;
                ctx.node_state.set_listen_addr(bound);
                tracing::info!("Listening on {bound} ({})", server.protocol_name());
                run_accept_loop(server, ctx).await
            }
            #[cfg(not(feature = "quic"))]
            Err(NodeError::TransportUnavailable("quic"))
        }
        Protocol::Tcp => {
            #[cfg(feature = "websocket")]
            {
                let server = wallhack_core::server::ws::WebSocketServer::try_new(
                    server_config,
                    server_options,
                )?;
                let bound = server.local_addr()?;
                ctx.node_state.set_listen_addr(bound);
                tracing::info!("Listening on {bound} ({})", server.protocol_name());
                run_accept_loop(server, ctx).await
            }
            #[cfg(not(feature = "websocket"))]
            Err(NodeError::TransportUnavailable("websocket"))
        }
    }
}

/// Server accept loop for listen-only mode.
async fn run_accept_loop<S: Server>(mut server: S, ctx: &Arc<ExitContext>) -> Result<(), NodeError>
where
    S::Error: std::error::Error + Send + Sync + 'static,
    S::Transport: Send + Sync + 'static,
    <S::Transport as Transport>::SendStream: 'static,
    <S::Transport as Transport>::RecvStream: 'static,
    <S::Transport as Transport>::BiStream: 'static,
{
    use wallhack_core::{
        server::server::DataChannels,
        transport::{
            ErasedTransport,
            protocol::{run_data_in, run_send_responses},
        },
    };

    loop {
        match server.accept(NodeRole::Exit).await {
            Ok(Some(accept_result)) => {
                let peer_addr = accept_result.peer_addr().to_string();

                // Register the connecting peer using handshake name if available.
                let peer_name = accept_result
                    .peer_handshake()
                    .filter(|h| !h.name.is_empty())
                    .map_or_else(|| peer_addr.clone(), |h| h.name.clone());
                tracing::info!("Peer connected: {peer_name} ({peer_addr})");
                ctx.peers
                    .register(peer_name.clone(), peer_addr, NodeRole::Entry);

                let transport: Arc<dyn ErasedTransport> = accept_result.transport();
                let adapter = SyscallExitAdapter::new();
                let _reaper = adapter.start_reaper(
                    std::time::Duration::from_mins(1),
                    std::time::Duration::from_mins(5),
                );
                let orchestrator = Orchestrator::new(Arc::new(adapter), Arc::clone(&ctx.metrics));
                let (
                    DataChannels {
                        instructions_tx,
                        instructions_rx,
                        responses_tx,
                        responses_rx,
                    },
                    control_tx,
                ) = accept_result.into_channels();

                // Incoming: accept uni stream from entry peer, dispatch instructions.
                let transport_in = Arc::clone(&transport);
                let instr_in = instructions_tx.clone();
                let resp_in = responses_tx.clone();
                tokio::spawn(async move {
                    match transport_in.accept_uni_erased().await {
                        Ok(Some(mut recv)) => {
                            if let Err(e) = run_data_in(&mut recv, &instr_in, &resp_in).await {
                                tracing::debug!("Data-in handler finished: {e}");
                            }
                        }
                        Ok(None) => tracing::debug!("Transport closed before data-in stream"),
                        Err(e) => tracing::debug!("Failed to accept data-in stream: {e}"),
                    }
                });

                // Outgoing: open uni stream to entry peer, send responses.
                let transport_out = Arc::clone(&transport);
                tokio::spawn(async move {
                    match transport_out.open_uni_erased().await {
                        Ok(mut send) => {
                            if let Err(e) = run_send_responses(&mut send, responses_rx).await {
                                tracing::debug!("Send-responses handler finished: {e}");
                            }
                        }
                        Err(e) => tracing::debug!("Failed to open send stream: {e}"),
                    }
                });

                let stream_fut = run_stream_listener(transport);
                let ctx = Arc::clone(ctx);
                tokio::spawn(async move {
                    let _keep_alive = control_tx;
                    tokio::select! {
                        result = orchestrator.drive(responses_tx, instructions_rx) => {
                            if let Err(e) = result {
                                tracing::error!("Orchestrator error: {e}");
                            }
                        }
                        result = stream_fut => {
                            if let Err(e) = result {
                                tracing::error!("Stream handler error: {e}");
                            }
                        }
                    }
                    ctx.peers.unregister(&peer_name);
                });
            }
            Ok(None) => break,
            Err(e) => {
                tracing::warn!("Accept error: {e}");
            }
        }
    }

    Ok(())
}

/// Non-generic exit loop: monomorphized once regardless of transport type.
#[allow(clippy::too_many_arguments)]
async fn run_exit_loop_inner(
    transport: Arc<dyn ErasedTransport>,
    instructions_rx: tokio::sync::mpsc::Receiver<wallhack_wire::data::EntryNodeInstruction>,
    responses_tx: tokio::sync::mpsc::Sender<wallhack_wire::data::ExitNodeResponse>,
    responses_rx: tokio::sync::mpsc::Receiver<wallhack_wire::data::ExitNodeResponse>,
    _control_tx: tokio::sync::mpsc::Sender<wallhack_wire::control::ControlMessage>,
    mut tasks: wallhack_core::client::client::ConnectionTasks,
    peer_handshake_rx: Option<tokio::sync::oneshot::Receiver<wallhack_wire::data::Handshake>>,
    peer_addr: &str,
    ctx: &ExitContext,
) -> Result<(), NodeError> {
    use wallhack_core::transport::protocol::run_send_responses;

    tracing::info!("Connected to {peer_addr}");

    // Resolve the peer's handshake name (delivered asynchronously via the
    // control loop). Fall back to the address if unavailable.
    let peer_name = resolve_peer_name(peer_handshake_rx, peer_addr).await;

    ctx.peers
        .register(peer_name.clone(), peer_addr.to_string(), NodeRole::Entry);

    // Outgoing: open uni stream to entry peer, send responses.
    let transport_out = Arc::clone(&transport);
    tokio::spawn(async move {
        match transport_out.open_uni_erased().await {
            Ok(mut send) => {
                if let Err(e) = run_send_responses(&mut send, responses_rx).await {
                    tracing::debug!("Send-responses handler finished: {e}");
                }
            }
            Err(e) => tracing::debug!("Failed to open send stream: {e}"),
        }
    });

    let adapter = SyscallExitAdapter::new();
    let _reaper = adapter.start_reaper(
        std::time::Duration::from_mins(1),
        std::time::Duration::from_mins(5),
    );
    let orchestrator = Orchestrator::new(Arc::new(adapter), Arc::clone(&ctx.metrics));

    let stream_fut = run_stream_listener(transport);
    tokio::pin!(stream_fut);
    let drive_fut = orchestrator.drive(responses_tx, instructions_rx);
    tokio::pin!(drive_fut);
    let disconnect_fut = tasks.wait_for_disconnect();
    tokio::pin!(disconnect_fut);

    tokio::select! {
        result = &mut drive_fut => {
            match result {
                Ok(()) => tracing::debug!("Connection closed cleanly"),
                Err(e) => tracing::debug!("Orchestrator error: {e}"),
            }
        }
        result = &mut stream_fut => {
            if let Err(e) = result { tracing::warn!("Stream handler error: {e}"); }
        }
        () = &mut disconnect_fut => {
            tracing::debug!("Connection tasks died - transport disconnected");
        }
    }

    ctx.peers.unregister(&peer_name);

    Ok(())
}

/// Await the peer's handshake name from a oneshot receiver (with timeout).
///
/// Returns the handshake name if non-empty, otherwise falls back to the
/// peer address.
async fn resolve_peer_name(
    rx: Option<tokio::sync::oneshot::Receiver<wallhack_wire::data::Handshake>>,
    peer_addr: &str,
) -> String {
    let Some(rx) = rx else {
        return peer_addr.to_string();
    };
    match tokio::time::timeout(std::time::Duration::from_secs(10), rx).await {
        Ok(Ok(hs)) if !hs.name.is_empty() => hs.name,
        _ => peer_addr.to_string(),
    }
}

pub(crate) async fn run_stream_listener(
    transport: std::sync::Arc<dyn ErasedTransport>,
) -> Result<(), NodeError> {
    tracing::trace!("Stream listener started");
    loop {
        let Some(mut stream) = transport
            .accept_bi_erased()
            .await
            .map_err(|e| NodeError::Stream(Box::new(e)))?
        else {
            return Ok(());
        };
        tracing::trace!("Accepted bi-stream from entry");
        tokio::spawn(async move {
            if let Err(e) = handle_stream(&mut stream).await {
                // QUIC STOP_SENDING with code 0 is a graceful close, not an error.
                let msg = e.to_string();
                if msg.contains("error 0") {
                    tracing::debug!("Stream closed by peer");
                } else {
                    tracing::warn!("Stream handler failed: {msg}");
                }
            }
        });
    }
}

#[allow(clippy::too_many_lines)]
async fn handle_stream<S: BiStream>(stream: &mut S) -> Result<(), NodeError> {
    let header: wallhack_wire::data::TcpStreamHeader = stream
        .read_proto(TCP_STREAM_HEADER_MTU)
        .await
        .map_err(|e| NodeError::Stream(Box::new(e)))?;
    tracing::trace!(target = %header.target_addr, source = %header.source_addr, protocol = header.protocol, "TcpStreamHeader received");
    let target: std::net::SocketAddr = header.target_addr.parse()?;
    let source: Option<std::net::SocketAddr> = if header.source_addr.is_empty() {
        None
    } else {
        Some(header.source_addr.parse()?)
    };
    match header.protocol {
        val if val == wallhack_wire::data::SessionProtocol::Tcp as i32 => {
            match tokio::net::TcpStream::connect(target).await {
                Ok(mut socket) => {
                    let status = wallhack_wire::data::TcpStreamStatus {
                        status: wallhack_wire::data::ResponseStatus::Success.into(),
                        reason: String::new(),
                    };
                    stream
                        .write_proto(&status)
                        .await
                        .map_err(|e| NodeError::Stream(Box::new(e)))?;
                    let _ = tokio::io::copy_bidirectional_with_sizes(
                        &mut *stream,
                        &mut socket,
                        64 * 1024,
                        64 * 1024,
                    )
                    .await?;
                }
                Err(e) => {
                    let status_code = match e.kind() {
                        std::io::ErrorKind::ConnectionRefused => {
                            wallhack_wire::data::ResponseStatus::ConnectionRefused
                        }
                        _ => wallhack_wire::data::ResponseStatus::HostUnreachable,
                    };
                    let status = wallhack_wire::data::TcpStreamStatus {
                        status: status_code.into(),
                        reason: e.to_string(),
                    };
                    let _ = stream.write_proto(&status).await;
                    return Err(
                        std::io::Error::new(e.kind(), format!("connect to {target}: {e}")).into(),
                    );
                }
            }
        }
        val if val == wallhack_wire::data::SessionProtocol::Udp as i32 => {
            tracing::trace!(target = %target, source = ?source, "Processing UDP session");
            let socket = tokio::net::UdpSocket::bind(match target {
                std::net::SocketAddr::V4(_) => {
                    std::net::SocketAddr::from((std::net::Ipv4Addr::UNSPECIFIED, 0))
                }
                std::net::SocketAddr::V6(_) => {
                    std::net::SocketAddr::from((std::net::Ipv6Addr::UNSPECIFIED, 0))
                }
            })
            .await?;
            socket.connect(target).await?;
            let mut buf = Vec::new();
            tokio::io::AsyncReadExt::read_to_end(stream, &mut buf).await?;
            tracing::trace!(buf_len = buf.len(), "Read UDP payload from stream");
            if !buf.is_empty() {
                tracing::trace!(target = %target, "Sending UDP to target");
                socket.send(&buf).await?;
                let mut recv_buf = vec![0u8; 65535];
                tracing::trace!("Waiting for UDP response...");
                match tokio::time::timeout(UDP_RESPONSE_TIMEOUT, socket.recv(&mut recv_buf)).await {
                    Ok(Ok(size)) => {
                        tracing::trace!(size, "Received UDP response");
                        stream.write_all(&[0x00]).await?;
                        stream.write_all(&recv_buf[..size]).await?;
                    }
                    Ok(Err(e)) => {
                        let status = match e.kind() {
                            std::io::ErrorKind::ConnectionRefused => Some(0x01u8),
                            std::io::ErrorKind::HostUnreachable => Some(0x02u8),
                            std::io::ErrorKind::NetworkUnreachable => Some(0x03u8),
                            _ => None,
                        };
                        if let Some(code) = status {
                            tracing::trace!("UDP ICMP error: {e}");
                            stream.write_all(&[code]).await?;
                        } else {
                            tracing::trace!("UDP recv error: {e}");
                        }
                    }
                    Err(_) => {
                        tracing::trace!("UDP recv timeout");
                    }
                }
                stream
                    .finish()
                    .await
                    .map_err(|e| NodeError::Stream(Box::new(e)))?;
            }
        }
        _ => {
            tracing::warn!("unsupported session protocol {}", header.protocol);
        }
    }
    Ok(())
}
