use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use bytes::Bytes;
use smoltcp::phy::Device;
use tokio::{
    io::unix::AsyncFd,
    sync::{Notify, mpsc},
    time::Instant,
};
use wallhack_entry_stack::async_stack::{
    HeldSyn, Netstack, SynProxyState, udp_socket::UdpSocketAny,
};
use wallhack_transport::ErasedTransport;
use wallhack_wire::{
    data::{
        EntryNodeInstruction, ExitNodeResponse, UdpSendInstruction, entry_node_instruction,
        exit_node_response, udp_response,
    },
    socket_set::SocketSet,
};

use crate::control::metrics::SharedMetrics;

use super::{
    actor::TunActor,
    icmp::{build_icmp_dest_unreachable, icmp_reason_from_str},
    session::run_tcp_session,
    syn_proxy::{parse_syn_target, probe_tcp_target},
};

/// Warn once when connection rate exceeds this threshold (connections/sec)
const HIGH_RATE_THRESHOLD: f64 = 50.0;

/// Window for rate calculation
const RATE_WINDOW: Duration = Duration::from_secs(5);

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("entry-stack error: {0}")]
    Netstack(#[from] wallhack_entry_stack::error::Error),

    #[error("session error: {0}")]
    Session(#[from] super::session::Error),

    #[error("transport error: {0}")]
    Transport(#[from] wallhack_transport::TransportError),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

pub struct ConnectionManager<D: Device + Send + 'static> {
    stack: Netstack<D>,
    transport: Arc<dyn ErasedTransport>,
    metrics: SharedMetrics,
    tun_writer: Arc<AsyncFd<tun::Device>>,
    udp_sessions: HashMap<(smoltcp::wire::IpEndpoint, u16), UdpSession>,
    /// Timestamps of recent TCP connections for rate detection
    recent_connections: Vec<Instant>,
    /// Only warn once about high connection rate
    rate_warned: AtomicBool,
    /// SYN proxy: receive held SYN packets from the poll loop.
    syn_rx: tokio::sync::mpsc::UnboundedReceiver<HeldSyn>,
    /// SYN proxy: send re-injected packets back to the poll loop.
    inject_tx: tokio::sync::mpsc::UnboundedSender<Vec<u8>>,
    /// SYN proxy: wake the poll loop when a probe completes.
    wake_notify: Arc<Notify>,
    /// SYN proxy: shared state for fast mode toggle and port cache.
    syn_proxy_state: Arc<SynProxyState>,
    /// Unified data stream: send UDP instructions to the exit node.
    instructions_tx: mpsc::Sender<EntryNodeInstruction>,
    /// Unified data stream: receive responses from the exit node.
    responses_rx: mpsc::Receiver<ExitNodeResponse>,
}

impl ConnectionManager<super::actor::SmoltcpTunDevice> {
    pub fn new(
        actor: TunActor,
        transport: Arc<dyn ErasedTransport>,
        metrics: SharedMetrics,
        fast_mode: bool,
        instructions_tx: mpsc::Sender<EntryNodeInstruction>,
        responses_rx: mpsc::Receiver<ExitNodeResponse>,
    ) -> (Self, Arc<SynProxyState>) {
        let (mut stack, tun_writer) = actor.into_stack();
        let state = Arc::new(SynProxyState::new(fast_mode));
        let (syn_rx, inject_tx, wake_notify) = stack.set_syn_proxy(Arc::clone(&state));

        let manager = Self {
            stack,
            transport,
            metrics,
            tun_writer,
            udp_sessions: HashMap::new(),
            recent_connections: Vec::new(),
            rate_warned: AtomicBool::new(false),
            syn_rx,
            inject_tx,
            wake_notify,
            syn_proxy_state: Arc::clone(&state),
            instructions_tx,
            responses_rx,
        };
        (manager, state)
    }
}

#[derive(Debug)]
struct UdpSession {
    last_seen: Instant,
}

impl<D: Device + Send + 'static> ConnectionManager<D> {
    #[allow(clippy::too_many_lines)] // refactor candidate
    pub async fn run(mut self) -> Result<(), Error>
    where
        D: wallhack_entry_stack::inner::peek_device::PeekDevice,
    {
        let mut listener = self.stack.tcp_listen_any()?;
        let mut udp = self.stack.udp_bind_any()?;

        let udp_timeout = Duration::from_secs(30);
        let mut udp_buf = vec![0u8; 65535];

        loop {
            tokio::select! {
                stream = listener.accept() => {
                    let stream = stream?;
                    tracing::debug!(
                        local = ?stream.local_endpoint(),
                        remote = ?stream.remote_endpoint(),
                        "TCP stream accepted, spawning session"
                    );

                    // Track connection rate for RTFM warning
                    let now = Instant::now();
                    self.recent_connections.push(now);
                    self.recent_connections.retain(|t| now.duration_since(*t) < RATE_WINDOW);
                    let count = u32::try_from(self.recent_connections.len()).unwrap_or(u32::MAX);
                    let rate = f64::from(count) / RATE_WINDOW.as_secs_f64();
                    if rate > HIGH_RATE_THRESHOLD && !self.rate_warned.swap(true, Ordering::Relaxed) {
                        tracing::warn!("High connection rate detected ({rate:.0}/s)");
                        tracing::warn!("Tip: for scanning (nmap, masscan), consider scan mode for better performance");
                    }

                    self.metrics.inc_active_connections();
                    let transport = Arc::clone(&self.transport);
                    let metrics = self.metrics.clone();
                    tokio::spawn(async move {
                        if let Err(e) = run_tcp_session(stream, transport).await {
                            tracing::debug!("TCP session ended: {e}");
                        }
                        metrics.dec_active_connections();
                    });
                }
                result = udp.recv_from(&mut udp_buf) => {
                    let (size, meta, local_port) = result?;
                    tracing::debug!(
                        size,
                        local_port,
                        remote = %meta.endpoint,
                        local_addr = ?meta.local_address,
                        "UDP packet received from entry stack"
                    );
                    let key = (meta.endpoint, local_port);
                    let now = Instant::now();
                    let entry = self.udp_sessions.entry(key).or_insert_with(|| {
                        self.metrics.inc_active_flows();
                        UdpSession { last_seen: now }
                    });
                    entry.last_seen = now;

                    // Build SocketAddressPair: src = client source, dst = target
                    let src_ip: std::net::IpAddr = meta.endpoint.addr.into();
                    let src_addr = std::net::SocketAddr::new(src_ip, meta.endpoint.port);
                    let Some(local_address) = meta.local_address else {
                        tracing::warn!("UDP: no destination IP (AnyIP not resolved), dropping");
                        continue;
                    };
                    let dst_ip: std::net::IpAddr = local_address.into();
                    let dst_addr = std::net::SocketAddr::new(dst_ip, local_port);

                    let pair = match (src_addr, dst_addr) {
                        (std::net::SocketAddr::V4(src), std::net::SocketAddr::V4(dst)) => {
                            wallhack_wire::data::SocketAddressPair::from(SocketSet::Ipv4((src, dst)))
                        }
                        (std::net::SocketAddr::V6(src), std::net::SocketAddr::V6(dst)) => {
                            wallhack_wire::data::SocketAddressPair::from(SocketSet::Ipv6((src, dst)))
                        }
                        _ => {
                            tracing::warn!("UDP: mixed IPv4/IPv6 endpoint pair, dropping");
                            continue;
                        }
                    };

                    let payload = Bytes::copy_from_slice(&udp_buf[..size]);
                    let instr = EntryNodeInstruction {
                        instruction: Some(entry_node_instruction::Instruction::UdpSend(
                            UdpSendInstruction {
                                pair: Some(pair),
                                data: payload,
                            },
                        )),
                    };
                    if self.instructions_tx.send(instr).await.is_err() {
                        tracing::debug!("UDP: instructions channel closed, stopping");
                        return Ok(());
                    }
                    self.metrics.inc_packets_out(1);
                    self.metrics.inc_bytes_out(size as u64);
                }
                result = self.responses_rx.recv() => {
                    if let Some(response) = result {
                        self.handle_exit_response(&mut udp, response);
                    } else {
                        tracing::debug!("Responses channel closed, connection dead");
                        return Ok(());
                    }
                }
                Some(held) = self.syn_rx.recv() => {
                    let transport = Arc::clone(&self.transport);
                    let state = Arc::clone(&self.syn_proxy_state);
                    let inject_tx = self.inject_tx.clone();
                    let wake = Arc::clone(&self.wake_notify);
                    let port = held.dst_port;
                    tokio::spawn(async move {
                        let Some(target_addr) = parse_syn_target(&held.packet) else {
                            tracing::debug!(port, "SYN probe: failed to parse target");
                            state.mark_closed(port);
                            wake.notify_one();
                            return;
                        };
                        let open = probe_tcp_target(&transport, &target_addr).await;
                        if open {
                            tracing::debug!(port, "SYN probe: open");
                            state.mark_open(port);
                            // Re-inject the original SYN so the poll loop can JIT-bind + process it.
                            let _ = inject_tx.send(held.packet);
                        } else {
                            tracing::debug!(port, "SYN probe: closed");
                            state.mark_closed(port);
                            // Re-inject so smoltcp sees the SYN with no listener → RST.
                            let _ = inject_tx.send(held.packet);
                        }
                        wake.notify_one();
                    });
                }
                () = tokio::time::sleep(Duration::from_secs(5)) => {
                    let now = Instant::now();
                    let metrics = self.metrics.clone();
                    self.udp_sessions.retain(|_, session| {
                        let keep = now.duration_since(session.last_seen) < udp_timeout;
                        if !keep {
                            metrics.dec_active_flows();
                        }
                        keep
                    });
                }
            }
        }
    }

    fn handle_exit_response<D2>(&mut self, udp: &mut UdpSocketAny<D2>, response: ExitNodeResponse)
    where
        D2: Device + Send + 'static,
    {
        let Some(pair) = response.pair else {
            return;
        };

        match response.response {
            Some(exit_node_response::Response::UdpResponse(udp_resp)) => {
                let Ok(socket_set) = SocketSet::try_from(pair) else {
                    tracing::warn!("UDP response: invalid pair, dropping");
                    return;
                };
                let (src_std, dst_std): (std::net::SocketAddr, std::net::SocketAddr) =
                    socket_set.into();

                let client_endpoint = smoltcp::wire::IpEndpoint {
                    addr: src_std.ip().into(),
                    port: src_std.port(),
                };
                let local_port = dst_std.port();
                let local_ip: Option<smoltcp::wire::IpAddress> = Some(dst_std.ip().into());

                match udp_resp.response {
                    Some(udp_response::Response::DataRecv(data_recv))
                        if !data_recv.data.is_empty() =>
                    {
                        // Update session last_seen
                        if let Some(session) =
                            self.udp_sessions.get_mut(&(client_endpoint, local_port))
                        {
                            session.last_seen = Instant::now();
                        }
                        let meta = smoltcp::socket::udp::UdpMetadata {
                            endpoint: client_endpoint,
                            local_address: local_ip,
                            meta: smoltcp::phy::PacketMeta::default(),
                        };
                        if let Err(e) = udp.send_to(local_port, &data_recv.data, meta) {
                            tracing::warn!("Failed to send UDP response to client: {e}");
                        } else {
                            tracing::debug!(
                                local_port,
                                client = %client_endpoint,
                                "UDP response sent to client"
                            );
                            self.metrics.inc_packets_in(1);
                            self.metrics.inc_bytes_in(data_recv.data.len() as u64);
                        }
                    }
                    Some(udp_response::Response::DataRecv(_)) => {
                        tracing::warn!(
                            "Empty UDP response from exit (unexpected; possible broadcast race or echo error)"
                        );
                    }
                    None => {}
                }
            }
            #[cfg(unix)]
            Some(exit_node_response::Response::RuntimeError(err)) => {
                // Attempt to reconstruct the session key and inject an ICMP
                // Destination Port Unreachable. We only have the error reason as
                // a string so we default to Port unreachable.
                let Ok(socket_set) = SocketSet::try_from(pair) else {
                    return;
                };
                let (src_std, dst_std): (std::net::SocketAddr, std::net::SocketAddr) =
                    socket_set.into();
                let client_endpoint = smoltcp::wire::IpEndpoint {
                    addr: src_std.ip().into(),
                    port: src_std.port(),
                };
                let local_port = dst_std.port();
                let target_ip: smoltcp::wire::IpAddress = dst_std.ip().into();

                let reason = icmp_reason_from_str(&err.reason);
                tracing::debug!(
                    reason = %err.reason,
                    icmp_reason = ?reason,
                    client = %client_endpoint,
                    target = %target_ip,
                    "UDP runtime error from exit, injecting ICMP unreachable"
                );

                // The original UDP payload is not available here: RuntimeError
                // arrives from the recv side of a session opened by a prior
                // UdpSend instruction, and that datagram is no longer in scope.
                // RFC 792/4443 flow identification relies on src/dst ports, which
                // are correctly reconstructed from the socket set below.
                if let Some(packet) = build_icmp_dest_unreachable(
                    reason,
                    client_endpoint.addr,
                    target_ip,
                    local_port,
                    client_endpoint.port,
                    &[],
                ) && let Err(e) = self.tun_writer.get_ref().send(&packet)
                {
                    tracing::warn!("Failed to inject ICMP packet: {e}");
                }
            }
            _ => {
                // Other response types (TCP, ICMP) are not relevant to the UDP path
            }
        }
    }
}
