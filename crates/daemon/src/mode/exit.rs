//! Exit node implementation.
//!
//! The exit node processes incoming instructions by making syscalls to the
//! local network. It can either connect to a source peer (default) or
//! listen for incoming connections. The daemon is headless — no REPL, no TTY.

use std::{sync::Arc, time::Duration};

use tokio::io::AsyncWriteExt;

use wallhack_core::{
    NodeRole,
    control::{
        handler::SharedNodeState,
        metrics::Metrics,
        peers::{ConnectionSide, Registry},
    },
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

    // Build handshake advertising local CIDRs so the entry peer can
    // auto-install OS routes on connect.
    let local_handshake = wallhack_wire::data::Handshake {
        capabilities: Some(wallhack_wire::data::Capabilities {
            tun_capable: false,
            listening: false,
            connecting: true,
            interactive: std::io::IsTerminal::is_terminal(&std::io::stdin()),
        }),
        name: name.to_string(),
        version: global.version.clone(),
        psk_proof: Vec::new(),
        routes: crate::netlink::enumerate_local_cidrs(),
        hint: Some(wallhack_wire::data::RoleHint {
            level: wallhack_wire::data::HintLevel::Fixed as i32,
            target: wallhack_wire::data::NodeRole::RoleExit as i32,
        }),
    };

    match spec.protocol {
        Protocol::Udp => {
            #[cfg(feature = "quic")]
            {
                let client_config = crate::config::build_quic_client_config(
                    global,
                    endpoint,
                    Some(name.to_string()),
                    security,
                    Some(local_handshake.clone()),
                );
                let ctx = Arc::clone(ctx);
                let peer_addr = peer_addr.clone();
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
                        let erased = connect_result.erase();
                        let ctx = Arc::clone(&ctx);
                        let peer_addr = peer_addr.clone();
                        async move {
                            run_exit_loop_inner(
                                erased.transport,
                                erased.channels.instructions_rx,
                                erased.channels.responses_tx,
                                erased.channels.responses_rx,
                                erased.control_tx,
                                erased.tasks,
                                erased.peer_handshake_rx,
                                erased.latency_rx,
                                &peer_addr,
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
                    Some(local_handshake),
                );
                let ctx = Arc::clone(ctx);
                let peer_addr = peer_addr.clone();
                crate::transport::connect_loop(
                    || {
                        let cfg = client_config.clone();
                        async move {
                            let mut client = wallhack_core::client::ws::WsClient::new(cfg)?;
                            client.connect(NodeRole::Exit).await
                        }
                    },
                    |connect_result| {
                        let erased = connect_result.erase();
                        let ctx = Arc::clone(&ctx);
                        let peer_addr = peer_addr.clone();
                        async move {
                            run_exit_loop_inner(
                                erased.transport,
                                erased.channels.instructions_rx,
                                erased.channels.responses_tx,
                                erased.channels.responses_rx,
                                erased.control_tx,
                                erased.tasks,
                                erased.peer_handshake_rx,
                                erased.latency_rx,
                                &peer_addr,
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
            "wallhack".to_string(),
            global.version.clone(),
        ),
        metrics: Some(Arc::clone(&ctx.metrics)),
        peers: Some(Arc::clone(&ctx.peers)),
        routes: None,
        route_updates: None,
        local_handshake: Some(wallhack_wire::data::Handshake {
            capabilities: Some(wallhack_wire::data::Capabilities {
                tun_capable: false,
                listening: true,
                connecting: false,
                interactive: std::io::IsTerminal::is_terminal(&std::io::stdin()),
            }),
            name: node_name.to_string(),
            version: global.version.clone(),
            psk_proof: Vec::new(),
            routes: crate::netlink::enumerate_local_cidrs(),
            hint: Some(wallhack_wire::data::RoleHint {
                level: wallhack_wire::data::HintLevel::Fixed as i32,
                target: wallhack_wire::data::NodeRole::RoleExit as i32,
            }),
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
// REASON: spawns transport, orchestrator, and stream listener tasks per connection; inherently broad
#[allow(clippy::too_many_lines)]
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
            Ok(Some(mut accept_result)) => {
                let peer_addr = accept_result.peer_addr().to_string();

                // Register the connecting peer using handshake name and capabilities.
                let peer_hs = accept_result.peer_handshake();
                let peer_name = peer_hs
                    .filter(|h| !h.name.is_empty())
                    .map_or_else(|| peer_addr.clone(), |h| h.name.clone());
                let peer_role = peer_hs
                    .and_then(|h| h.capabilities)
                    .map_or(NodeRole::Exit, super::peer_role_from_capabilities);
                tracing::info!("Peer connected: name={peer_name} addr={peer_addr}");
                ctx.peers.register(
                    peer_name.clone(),
                    peer_addr,
                    peer_role,
                    ConnectionSide::Accept,
                );

                let transport: Arc<dyn ErasedTransport> = accept_result.transport();
                let latency_rx = accept_result
                    .take_latency_rx()
                    .unwrap_or_else(|| tokio::sync::mpsc::channel(1).1);
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
                {
                    let transport = Arc::clone(&transport);
                    let instr_in = instructions_tx.clone();
                    let resp_in = responses_tx.clone();
                    tokio::spawn(async move {
                        match transport.accept_uni_erased().await {
                            Ok(Some(mut recv)) => {
                                if let Err(e) = run_data_in(&mut recv, &instr_in, &resp_in).await {
                                    tracing::debug!("Data-in handler finished: {e}");
                                }
                            }
                            Ok(None) => tracing::debug!("Transport closed before data-in stream"),
                            Err(e) => tracing::debug!("Failed to accept data-in stream: {e}"),
                        }
                    });
                }

                // Outgoing: open uni stream to entry peer, send responses.
                {
                    let transport = Arc::clone(&transport);
                    tokio::spawn(async move {
                        match transport.open_uni_erased().await {
                            Ok(mut send) => {
                                if let Err(e) = run_send_responses(&mut send, responses_rx).await {
                                    tracing::debug!("Send-responses handler finished: {e}");
                                }
                            }
                            Err(e) => tracing::debug!("Failed to open send stream: {e}"),
                        }
                    });
                }

                let stream_fut = run_stream_listener(transport);
                let ctx = Arc::clone(ctx);
                tokio::spawn(async move {
                    let _heartbeat = super::spawn_heartbeat(
                        control_tx,
                        Some(latency_rx),
                        peer_name.clone(),
                        Arc::clone(&ctx.peers),
                    );

                    let drive_fut = orchestrator.drive(responses_tx, instructions_rx);
                    tokio::pin!(drive_fut);
                    tokio::pin!(stream_fut);

                    tokio::select! {
                        result = &mut drive_fut => {
                            if let Err(e) = result {
                                tracing::debug!("Orchestrator finished: {e}");
                            }
                        }
                        result = &mut stream_fut => {
                            if let Err(e) = result {
                                tracing::debug!("Stream handler finished: {e}");
                            }
                        }
                    }

                    ctx.peers.unregister(&peer_name);
                    tracing::info!("Peer disconnected: {peer_name}");
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
// REASON: threading transport, instructions, responses, control, tasks, handshake, latency, peer_addr, ctx
#[allow(clippy::too_many_arguments)]
async fn run_exit_loop_inner(
    transport: Arc<dyn ErasedTransport>,
    instructions_rx: tokio::sync::mpsc::Receiver<wallhack_wire::data::EntryNodeInstruction>,
    responses_tx: tokio::sync::mpsc::Sender<wallhack_wire::data::ExitNodeResponse>,
    responses_rx: tokio::sync::mpsc::Receiver<wallhack_wire::data::ExitNodeResponse>,
    control_tx: tokio::sync::mpsc::Sender<wallhack_wire::control::ControlMessage>,
    mut tasks: wallhack_core::client::client::ConnectionTasks,
    peer_handshake_rx: Option<tokio::sync::oneshot::Receiver<wallhack_wire::data::Handshake>>,
    latency_rx: Option<tokio::sync::mpsc::Receiver<f64>>,
    peer_addr: &str,
    ctx: &ExitContext,
) -> Result<(), NodeError> {
    use wallhack_core::transport::protocol::run_send_responses;

    tracing::info!("Connected to {peer_addr}");

    // Resolve the peer's handshake (delivered asynchronously via the
    // control loop). Fall back to the address if unavailable.
    let peer_handshake = resolve_peer_handshake(peer_handshake_rx).await;
    let peer_name = peer_handshake
        .as_ref()
        .filter(|h| !h.name.is_empty())
        .map_or_else(|| peer_addr.to_string(), |h| h.name.clone());

    // Derive peer role from capabilities, then register.
    let peer_capabilities = peer_handshake
        .as_ref()
        .and_then(|h| h.capabilities)
        .unwrap_or_default();
    let peer_role = super::peer_role_from_capabilities(peer_capabilities);
    ctx.peers.register(
        peer_name.clone(),
        peer_addr.to_string(),
        peer_role,
        ConnectionSide::Connect,
    );
    ctx.peers
        .update_capabilities(&peer_name, &peer_capabilities);

    // Outgoing: open uni stream to entry peer, send responses.
    {
        let transport = Arc::clone(&transport);
        tokio::spawn(async move {
            match transport.open_uni_erased().await {
                Ok(mut send) => {
                    if let Err(e) = run_send_responses(&mut send, responses_rx).await {
                        tracing::debug!("Send-responses handler finished: {e}");
                    }
                }
                Err(e) => tracing::debug!("Failed to open send stream: {e}"),
            }
        });
    }

    let adapter = SyscallExitAdapter::new();
    let _reaper = adapter.start_reaper(
        std::time::Duration::from_mins(1),
        std::time::Duration::from_mins(5),
    );
    let orchestrator = Orchestrator::new(Arc::new(adapter), Arc::clone(&ctx.metrics));

    let _heartbeat = super::spawn_heartbeat(
        control_tx,
        latency_rx,
        peer_name.clone(),
        Arc::clone(&ctx.peers),
    );

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
                Err(e) => tracing::debug!("Orchestrator finished: {e}"),
            }
        }
        result = &mut stream_fut => {
            if let Err(e) = result { tracing::debug!("Stream handler finished: {e}"); }
        }
        () = &mut disconnect_fut => {
            tracing::debug!("Transport disconnected");
        }
    }

    ctx.peers.unregister(&peer_name);
    tracing::info!("Peer disconnected: {peer_name}");

    Ok(())
}

/// Await the peer's handshake from a oneshot receiver (with timeout).
///
/// Returns the received `Handshake` if available within the timeout, or
/// `None` if the receiver is absent, the sender dropped, or the timeout
/// expires.
async fn resolve_peer_handshake(
    rx: Option<tokio::sync::oneshot::Receiver<wallhack_wire::data::Handshake>>,
) -> Option<wallhack_wire::data::Handshake> {
    let rx = rx?;
    match tokio::time::timeout(std::time::Duration::from_secs(10), rx).await {
        Ok(Ok(hs)) => Some(hs),
        _ => None,
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

/// Connect to a target with retry for transient errors (EHOSTUNREACH).
///
/// Under concurrent load (e.g. nmap scans), ARP resolution can fail transiently
/// when many connections target different IPs simultaneously. A short retry
/// gives the neighbor table time to populate.
async fn tcp_connect_with_retry(
    target: std::net::SocketAddr,
) -> Result<tokio::net::TcpStream, std::io::Error> {
    const MAX_RETRIES: u32 = 2;
    const RETRY_DELAY: Duration = Duration::from_millis(100);

    let mut last_err = None;
    for attempt in 0..=MAX_RETRIES {
        match tokio::net::TcpStream::connect(target).await {
            Ok(stream) => return Ok(stream),
            Err(e) => {
                // EHOSTUNREACH=113, ENETUNREACH=101 on Linux.
                let retryable = matches!(e.raw_os_error(), Some(113 | 101));
                if retryable && attempt < MAX_RETRIES {
                    tracing::trace!(target = %target, attempt, "Transient connect error, retrying");
                    tokio::time::sleep(RETRY_DELAY).await;
                    last_err = Some(e);
                } else {
                    return Err(e);
                }
            }
        }
    }
    Err(last_err.unwrap_or_else(|| std::io::Error::other("retry exhausted")))
}

// REASON: symmetric TCP and UDP session protocol arms each with connect, status, and data relay logic
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
            match tcp_connect_with_retry(target).await {
                Ok(mut socket) => {
                    let status = wallhack_wire::data::TcpStreamStatus {
                        status: wallhack_wire::data::ResponseStatus::Success.into(),
                        reason: String::new(),
                    };
                    stream
                        .write_proto(&status)
                        .await
                        .map_err(|e| NodeError::Stream(Box::new(e)))?;
                    tracing::debug!(target = %target, "TCP relay connected");
                    let (bytes_in, bytes_out) = tokio::io::copy_bidirectional_with_sizes(
                        &mut *stream,
                        &mut socket,
                        64 * 1024,
                        64 * 1024,
                    )
                    .await?;
                    tracing::debug!(
                        target = %target, bytes_in, bytes_out, "TCP relay closed"
                    );
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
