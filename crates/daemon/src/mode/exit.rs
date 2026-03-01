//! Exit node implementation.
//!
//! The exit node processes incoming instructions by making syscalls to the
//! local network. It can either connect to a source peer (default) or
//! listen for incoming connections. The daemon is headless — no REPL, no TTY.

use std::{sync::Arc, time::Duration};

use tokio::io::AsyncWriteExt;

use wallhack_core::{
    NodeRole,
    client::client::ConnectResult,
    control::{metrics::Metrics, peers::Registry},
    exit::{net::SyscallExitAdapter, orchestrator::Orchestrator},
    server::server::Server,
    transport::{
        BiStream, Transport,
        protocol::{AsyncProtoRead as _, AsyncProtoWrite as _, SESSION_INIT_MTU},
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
) -> Result<(), NodeError> {
    let security = SecurityParams {
        psk: global.psk.clone(),
        accept_fingerprint: cfg.accept_fingerprint.clone(),
    };

    let ctx = Arc::new(ExitContext { metrics, peers });

    match &cfg.connectivity {
        ConnectivitySpec::Both { connect, listen } => {
            run_exit_both(global, &cfg.name, connect, listen, &ctx).await
        }
        ConnectivitySpec::Connect(spec) => {
            run_exit_connector(global, &cfg.name, spec, &security, &ctx).await
        }
        ConnectivitySpec::Listen(spec) => run_exit_listener(global, &cfg.name, spec, &ctx).await,
    }
}

/// Run in connect-only mode (standard exit).
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
                        let ctx = Arc::clone(&ctx);
                        let pa = pa.clone();
                        async move { run_exit_loop(connect_result, &pa, &ctx).await }
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
                        let ctx = Arc::clone(&ctx);
                        let pa = pa.clone();
                        async move { run_exit_loop(connect_result, &pa, &ctx).await }
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

/// Run with both connect and listen (`ConnectivitySpec::Both`).
// TODO(13c): ConnectivitySpec::Both should resolve to the relay role via
// auto-negotiation, not run as exit. This entire code path goes away once
// Phase 13c lands.
async fn run_exit_both(
    global: &GlobalConfig,
    name: &str,
    connect_spec: &AddressSpec,
    listen_spec: &AddressSpec,
    ctx: &Arc<ExitContext>,
) -> Result<(), NodeError> {
    tracing::info!("Connecting to {}...", connect_spec.addr);
    let peer_addr =
        crate::transport::resolve_endpoint(&connect_spec.addr, global.dns_server.as_deref())
            .await?;
    let listen_addr: std::net::SocketAddr =
        listen_spec.addr.parse::<crate::net::ListenAddr>()?.into();

    let security = SecurityParams {
        psk: global.psk.clone(),
        accept_fingerprint: None,
    };

    match connect_spec.protocol {
        Protocol::Udp => {
            #[cfg(feature = "quic")]
            {
                run_quic_exit_both(global, peer_addr, listen_addr, name, ctx, &security).await
            }
            #[cfg(not(feature = "quic"))]
            Err(NodeError::TransportUnavailable("quic"))
        }
        Protocol::Tcp => {
            #[cfg(feature = "websocket")]
            {
                run_ws_exit_both(global, peer_addr, listen_addr, name, ctx, &security).await
            }
            #[cfg(not(feature = "websocket"))]
            Err(NodeError::TransportUnavailable("websocket"))
        }
    }
}

#[cfg(feature = "quic")]
async fn run_quic_exit_both(
    global: &GlobalConfig,
    peer_addr: std::net::SocketAddr,
    listen_addr: std::net::SocketAddr,
    name: &str,
    ctx: &Arc<ExitContext>,
    security: &SecurityParams,
) -> Result<(), NodeError> {
    use wallhack_core::{control::handler::HandlerConfig, server::server::ServerOptions};

    let client_config = crate::config::build_quic_client_config(
        global,
        peer_addr,
        Some(name.to_string()),
        security,
        None,
    );
    let connect_result = crate::transport::connect_with_retry(|| {
        let cfg = client_config.clone();
        async move {
            use wallhack_core::client::client::Client;
            let mut client = wallhack_core::client::quic::QuicClient::try_new(cfg)?;
            client.connect(NodeRole::Exit).await
        }
    })
    .await?;

    tracing::info!("Connected to peer {peer_addr}");
    let (source_instr, source_resp) = connect_result.channels().clone();

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
                connecting: true,
            }),
            name: name.to_string(),
            version: crate::built_info::PKG_VERSION.to_string(),
            psk_proof: Vec::new(),
            routes: Vec::new(),
            hint: None,
        }),
    };
    let server_config =
        crate::config::build_server_config(&global.tls, listen_addr, global.psk.clone(), None);
    let mut server =
        wallhack_core::server::quic::QuicServer::try_new(server_config, server_options)
            .map_err(|e| NodeError::Transport(Box::new(e)))?;
    let bound = server.local_addr()?;
    tracing::info!(
        "Exit (both): connected to {peer_addr}, listening on {bound} ({})",
        server.protocol_name()
    );
    run_accept_bridge_loop(&mut server, &source_instr, &source_resp).await
}

#[cfg(feature = "websocket")]
async fn run_ws_exit_both(
    global: &GlobalConfig,
    peer_addr: std::net::SocketAddr,
    listen_addr: std::net::SocketAddr,
    name: &str,
    ctx: &Arc<ExitContext>,
    security: &SecurityParams,
) -> Result<(), NodeError> {
    use wallhack_core::{control::handler::HandlerConfig, server::server::ServerOptions};

    let client_config = crate::config::build_ws_client_config(
        global,
        peer_addr,
        Some(name.to_string()),
        security,
        None,
    );
    let connect_result = crate::transport::connect_with_retry(|| {
        let cfg = client_config.clone();
        async move {
            let mut client = wallhack_core::client::ws::WsClient::new(cfg)?;
            client.connect(NodeRole::Exit).await
        }
    })
    .await?;

    tracing::info!("Connected to peer {peer_addr}");
    let (source_instr, source_resp) = connect_result.channels().clone();

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
                connecting: true,
            }),
            name: name.to_string(),
            version: crate::built_info::PKG_VERSION.to_string(),
            psk_proof: Vec::new(),
            routes: Vec::new(),
            hint: None,
        }),
    };
    let server_config =
        crate::config::build_server_config(&global.tls, listen_addr, global.psk.clone(), None);
    let mut server =
        wallhack_core::server::ws::WebSocketServer::try_new(server_config, server_options)?;
    let bound = server.local_addr()?;
    tracing::info!(
        "Exit (both): connected to {peer_addr}, listening on {bound} ({})",
        server.protocol_name()
    );
    run_accept_bridge_loop(&mut server, &source_instr, &source_resp).await
}

/// Accept loop that bridges each peer connection to source channels.
async fn run_accept_bridge_loop<S: Server>(
    server: &mut S,
    source_instr: &tokio::sync::broadcast::Sender<wallhack_wire::data::EntryNodeInstruction>,
    source_resp: &tokio::sync::broadcast::Sender<wallhack_wire::data::ExitNodeResponse>,
) -> Result<(), NodeError>
where
    S::Error: std::error::Error + Send + Sync + 'static,
    S::Transport: Send + Sync + 'static,
{
    loop {
        match server.accept(NodeRole::Exit).await {
            Ok(Some(accept_result)) => {
                tracing::info!("Peer connected: {}", accept_result.peer_addr());
                crate::transport::bridge_channels(accept_result, source_instr, source_resp);
            }
            Ok(None) => {
                tracing::info!("Server closed");
                break;
            }
            Err(e) => {
                tracing::warn!("Accept error: {e}");
            }
        }
    }
    Ok(())
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
{
    loop {
        match server.accept(NodeRole::Exit).await {
            Ok(Some(accept_result)) => {
                let peer_addr = accept_result.peer_addr().to_string();
                tracing::info!("Peer connected: {peer_addr}");

                // Register the connecting peer.
                let peer_name = accept_result
                    .peer_handshake()
                    .map_or_else(|| peer_addr.clone(), |h| h.name.clone());
                ctx.peers
                    .register(peer_name.clone(), peer_addr, NodeRole::Entry);

                let transport = accept_result.transport();
                let adapter = SyscallExitAdapter::new();
                let _reaper = adapter.start_reaper(
                    std::time::Duration::from_mins(1),
                    std::time::Duration::from_mins(5),
                );
                let orchestrator = Orchestrator::new(Arc::new(adapter), Arc::clone(&ctx.metrics));
                let stream_fut = run_stream_listener(transport);
                let ((instr, resp), control_tx) = accept_result.channels();
                let ctx = Arc::clone(ctx);
                tokio::spawn(async move {
                    let _keep_alive = control_tx;
                    tokio::select! {
                        result = orchestrator.drive(resp.clone(), instr.subscribe()) => {
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
                    // Unregister peer when connection ends.
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

/// Drive the exit node orchestrator with a connected peer.
///
/// Returns when the connection drops (caller should reconnect).
async fn run_exit_loop<T: wallhack_core::transport::Transport + 'static>(
    connect_result: ConnectResult<T>,
    peer_addr: &str,
    ctx: &ExitContext,
) -> Result<(), NodeError> {
    tracing::info!("Connected to {peer_addr}");

    // Register the entry node as a peer. ConnectResult carries no peer
    // identity (no hello exchange in this direction), so use addr as id.
    ctx.peers.register(
        peer_addr.to_string(),
        peer_addr.to_string(),
        NodeRole::Entry,
    );

    // Create syscall adapter for local network access
    let adapter = SyscallExitAdapter::new();
    let _reaper = adapter.start_reaper(
        std::time::Duration::from_mins(1),
        std::time::Duration::from_mins(5),
    );
    let orchestrator = Orchestrator::new(Arc::new(adapter), Arc::clone(&ctx.metrics));

    let transport = connect_result.transport();
    let ((instr, resp), mut tasks, _control_tx) = connect_result.into_parts();
    let stream_fut = run_stream_listener(transport);
    let disconnect_fut = tasks.wait_for_disconnect();

    // Pin the long-running futures so we can select over them
    tokio::pin!(stream_fut);
    tokio::pin!(disconnect_fut);
    let drive_fut = orchestrator.drive(resp, instr.subscribe());
    tokio::pin!(drive_fut);

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

    // Unregister the peer when connection drops.
    ctx.peers.unregister(peer_addr);

    Ok(())
}

pub(crate) async fn run_stream_listener<T: Transport>(
    transport: std::sync::Arc<T>,
) -> Result<(), NodeError>
where
    T::BiStream: 'static,
{
    tracing::trace!("Stream listener started");
    loop {
        let Some(mut stream) = transport
            .accept_bi()
            .await
            .map_err(|e| NodeError::Stream(Box::new(e)))?
        else {
            return Ok(());
        };
        tracing::trace!("Accepted bi-stream from entry");
        tokio::spawn(async move {
            if let Err(e) = handle_stream(&mut stream).await {
                tracing::warn!("stream handler failed: {e}");
            }
        });
    }
}

async fn handle_stream<S: BiStream>(stream: &mut S) -> Result<(), NodeError> {
    let init: wallhack_wire::data::SessionInit = stream
        .read_proto(SESSION_INIT_MTU)
        .await
        .map_err(|e| NodeError::Stream(Box::new(e)))?;
    tracing::trace!(target = %init.target_addr, source = %init.source_addr, protocol = init.protocol, "SessionInit received");
    let target: std::net::SocketAddr = init.target_addr.parse()?;
    let source: Option<std::net::SocketAddr> = if init.source_addr.is_empty() {
        None
    } else {
        Some(init.source_addr.parse()?)
    };
    match init.protocol {
        val if val == wallhack_wire::data::SessionProtocol::Tcp as i32 => {
            match tokio::net::TcpStream::connect(target).await {
                Ok(mut socket) => {
                    let status = wallhack_wire::data::SessionStatus {
                        status: wallhack_wire::data::ResponseStatus::Success.into(),
                        reason: String::new(),
                    };
                    stream
                        .write_proto(&status)
                        .await
                        .map_err(|e| NodeError::Stream(Box::new(e)))?;
                    let _ = tokio::io::copy_bidirectional(&mut *stream, &mut socket).await?;
                }
                Err(e) => {
                    let status_code = match e.kind() {
                        std::io::ErrorKind::ConnectionRefused => {
                            wallhack_wire::data::ResponseStatus::ConnectionRefused
                        }
                        _ => wallhack_wire::data::ResponseStatus::HostUnreachable,
                    };
                    let status = wallhack_wire::data::SessionStatus {
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
            tracing::warn!("unsupported session protocol {}", init.protocol);
        }
    }
    Ok(())
}
