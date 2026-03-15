use std::{collections::HashMap, sync::Arc, time::Duration};

use bytes::Bytes;
use smoltcp::phy::Device;
use tokio::{
    io::unix::AsyncFd,
    sync::{Notify, mpsc},
    time::Instant,
};
use wallhack_entry_stack::async_stack::{
    HeldSyn, Netstack, SynProbeCache, udp_socket::UdpSocketAny,
};
use wallhack_transport::ErasedTransport;
use wallhack_wire::{
    data::{
        EntryNodeInstruction, ExitNodeResponse, UdpSendInstruction, entry_node_instruction,
        exit_node_response, icmp_response, udp_response,
    },
    socket_set::SocketSet,
};

use crate::control::metrics::SharedMetrics;

use super::{
    actor::TunActor,
    icmp::{build_icmp_dest_unreachable, icmp_reason_from_str},
    session::run_tcp_session,
    syn_proxy::{ProbeResult, parse_syn_target, probe_tcp_target},
};

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
    /// Port probe: receive held SYN packets from the poll loop.
    syn_rx: tokio::sync::mpsc::UnboundedReceiver<HeldSyn>,
    /// Port probe: send re-injected packets back to the poll loop.
    inject_tx: tokio::sync::mpsc::UnboundedSender<Vec<u8>>,
    /// Port probe: wake the poll loop when a probe completes.
    wake_notify: Arc<Notify>,
    /// Port probe: cache of per-(host,port) probe results.
    probe_cache: Arc<SynProbeCache>,
    /// Raw packets (ICMP) from the poll loop to write directly to the TUN.
    egress_rx: tokio::sync::mpsc::UnboundedReceiver<Vec<u8>>,
    /// Intercepted ICMP Echo Requests from the poll loop for tunnel forwarding.
    icmp_rx: tokio::sync::mpsc::UnboundedReceiver<Vec<u8>>,
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
        instructions_tx: mpsc::Sender<EntryNodeInstruction>,
        responses_rx: mpsc::Receiver<ExitNodeResponse>,
    ) -> (Self, Arc<SynProbeCache>) {
        let (mut stack, tun_writer) = actor.into_stack();
        let state = Arc::new(SynProbeCache::new());
        let (syn_rx, inject_tx, wake_notify, egress_rx, icmp_rx) =
            stack.set_probe_cache(Arc::clone(&state));

        let manager = Self {
            stack,
            transport,
            metrics,
            tun_writer,
            udp_sessions: HashMap::new(),
            syn_rx,
            inject_tx,
            wake_notify,
            probe_cache: Arc::clone(&state),
            egress_rx,
            icmp_rx,
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
                    let state = Arc::clone(&self.probe_cache);
                    let inject_tx = self.inject_tx.clone();
                    let tun = Arc::clone(&self.tun_writer);
                    let wake = Arc::clone(&self.wake_notify);
                    let dst_addr = held.dst_addr;
                    tokio::spawn(async move {
                        let Some(target_addr) = parse_syn_target(&held.packet) else {
                            tracing::debug!(%dst_addr, "SYN probe: failed to parse target");
                            state.mark_unreachable(dst_addr);
                            wake.notify_one();
                            return;
                        };
                        match probe_tcp_target(&transport, &target_addr).await {
                            ProbeResult::Open => {
                                tracing::debug!(%dst_addr, "SYN probe: open");
                                state.mark_open(dst_addr);
                                let _ = inject_tx.send(held.packet);
                            }
                            ProbeResult::Closed => {
                                tracing::debug!(%dst_addr, "SYN probe: closed");
                                state.mark_closed(dst_addr);
                                let _ = inject_tx.send(held.packet);
                            }
                            ProbeResult::Unreachable => {
                                tracing::debug!(%dst_addr, "SYN probe: unreachable");
                                state.mark_unreachable(dst_addr);
                                // Inject ICMP Host Unreachable so nmap marks host "down".
                                write_icmp_to_tun(&tun, &held.packet);
                            }
                            ProbeResult::TransportError => {
                                // Tunnel issue — don't cache, drop silently.
                                // nmap will see "filtered" (timeout).
                                tracing::debug!(%dst_addr, "SYN probe: transport error");
                            }
                        }
                        wake.notify_one();
                    });
                }
                Some(icmp_packet) = self.egress_rx.recv() => {
                    // ICMP packets from the poll loop (cache-hit retransmits).
                    write_raw_to_tun(&self.tun_writer, &icmp_packet);
                }
                Some(icmp_pkt) = self.icmp_rx.recv() => {
                    // ICMP Echo Request intercepted from the entry stack.
                    // Forward to the exit node for real ping delivery.
                    if let Some(instr) = build_icmp_instruction(&icmp_pkt) {
                        tracing::trace!(len = icmp_pkt.len(), "forwarding ICMP echo request to exit");
                        if self.instructions_tx.send(instr).await.is_err() {
                            tracing::debug!("ICMP: instructions channel closed, stopping");
                            return Ok(());
                        }
                        self.metrics.inc_packets_out(1);
                        self.metrics.inc_bytes_out(icmp_pkt.len() as u64);
                    } else {
                        tracing::warn!(len = icmp_pkt.len(), "failed to build ICMP instruction from packet");
                    }
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

    #[allow(clippy::too_many_lines)]
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
            Some(exit_node_response::Response::IcmpResponse(icmp_resp)) => {
                let Ok(socket_set) = SocketSet::try_from(pair) else {
                    tracing::warn!("ICMP response: invalid pair, dropping");
                    return;
                };
                let (src_std, dst_std): (std::net::SocketAddr, std::net::SocketAddr) =
                    socket_set.into();

                match icmp_resp.response {
                    Some(icmp_response::Response::DataRecv(data_recv))
                        if !data_recv.data.is_empty() =>
                    {
                        #[allow(clippy::cast_possible_truncation)]
                        let original_ident = data_recv.echo_ident as u16;
                        if let Some(packet) =
                            build_icmp_echo_reply(&data_recv.data, original_ident, src_std, dst_std)
                        {
                            tracing::trace!(
                                data_len = data_recv.data.len(),
                                original_ident,
                                "injecting ICMP echo reply into TUN"
                            );
                            write_raw_to_tun(&self.tun_writer, &packet);
                            self.metrics.inc_packets_in(1);
                            self.metrics.inc_bytes_in(data_recv.data.len() as u64);
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
}

/// Build ICMP Host Unreachable from the original SYN packet and write to TUN.
fn write_icmp_to_tun(tun: &Arc<AsyncFd<tun::Device>>, original_syn: &[u8]) {
    if let Some(icmp) = wallhack_entry_stack::async_stack::build_icmp_host_unreachable(original_syn)
    {
        write_raw_to_tun(tun, &icmp);
    }
}

/// Write a raw IP packet directly to the TUN device.
fn write_raw_to_tun(tun: &Arc<AsyncFd<tun::Device>>, packet: &[u8]) {
    if let Err(e) = tun.get_ref().send(packet) {
        tracing::debug!(error = %e, "Failed to write ICMP to TUN");
    }
}

/// Parse a raw IP packet containing an ICMP Echo Request and build an
/// [`EntryNodeInstruction::IcmpSend`] for tunnel delivery.
fn build_icmp_instruction(packet: &[u8]) -> Option<EntryNodeInstruction> {
    use smoltcp::wire::{IpVersion, Ipv4Packet, Ipv6Packet};
    use wallhack_wire::data::{
        IcmpEchoRequest, IcmpSendInstruction, SocketAddressPair, icmp_send_instruction::IcmpMessage,
    };

    let version = IpVersion::of_packet(packet).ok()?;

    match version {
        IpVersion::Ipv4 => {
            let ipv4 = Ipv4Packet::new_checked(packet).ok()?;
            let payload = ipv4.payload();
            if payload.len() < 8 || payload[0] != 8 {
                return None;
            }

            let ident = u16::from_be_bytes([payload[4], payload[5]]);
            let seq_no = u16::from_be_bytes([payload[6], payload[7]]);
            let data = payload[8..].to_vec();

            let src_v4 = std::net::SocketAddrV4::new(ipv4.src_addr(), 0);
            let dst_v4 = std::net::SocketAddrV4::new(ipv4.dst_addr(), 0);
            let pair = SocketAddressPair::from(SocketSet::Ipv4((src_v4, dst_v4)));

            Some(EntryNodeInstruction {
                instruction: Some(entry_node_instruction::Instruction::IcmpSend(
                    IcmpSendInstruction {
                        pair: Some(pair),
                        icmp_message: Some(IcmpMessage::IcmpEchoRequest(IcmpEchoRequest {
                            seq_no: u32::from(seq_no),
                            ident: u32::from(ident),
                            data: data.into(),
                        })),
                    },
                )),
            })
        }
        IpVersion::Ipv6 => {
            let ipv6 = Ipv6Packet::new_checked(packet).ok()?;
            let icmp_payload = find_icmpv6_payload(ipv6.next_header(), ipv6.payload())?;

            if icmp_payload.len() < 8 || icmp_payload[0] != 128 {
                return None;
            }

            let ident = u16::from_be_bytes([icmp_payload[4], icmp_payload[5]]);
            let seq_no = u16::from_be_bytes([icmp_payload[6], icmp_payload[7]]);
            let data = icmp_payload[8..].to_vec();

            let src_addr = std::net::SocketAddrV6::new(ipv6.src_addr(), 0, 0, 0);
            let dst_addr = std::net::SocketAddrV6::new(ipv6.dst_addr(), 0, 0, 0);
            let pair = SocketAddressPair::from(SocketSet::Ipv6((src_addr, dst_addr)));

            Some(EntryNodeInstruction {
                instruction: Some(entry_node_instruction::Instruction::IcmpSend(
                    IcmpSendInstruction {
                        pair: Some(pair),
                        icmp_message: Some(IcmpMessage::IcmpEchoRequest(IcmpEchoRequest {
                            seq_no: u32::from(seq_no),
                            ident: u32::from(ident),
                            data: data.into(),
                        })),
                    },
                )),
            })
        }
    }
}

/// Walk IPv6 extension headers to find the `ICMPv6` payload.
fn find_icmpv6_payload(
    mut next_header: smoltcp::wire::IpProtocol,
    mut payload: &[u8],
) -> Option<&[u8]> {
    use smoltcp::wire::IpProtocol;
    loop {
        match next_header {
            IpProtocol::HopByHop
            | IpProtocol::Ipv6Route
            | IpProtocol::Ipv6Frag
            | IpProtocol::Ipv6Opts => {
                if payload.len() < 2 {
                    return None;
                }
                next_header = IpProtocol::from(payload[0]);
                let ext_len = (usize::from(payload[1]) + 1) * 8;
                if payload.len() < ext_len {
                    return None;
                }
                payload = &payload[ext_len..];
            }
            IpProtocol::Icmpv6 => return Some(payload),
            _ => return None,
        }
    }
}

/// Construct a raw IP + ICMP Echo Reply packet from the exit node's response
/// data. The response `data` is the raw ICMP message from the OS DGRAM socket
/// (ICMP header + payload, no IP header).
///
/// `original_ident` is the Echo identifier from the original request (the
/// kernel on the exit node may have substituted its own).
///
/// `src_std` / `dst_std` are the original pair addresses (src = originator,
/// dst = target). The reply packet reverses these: IP src = target, IP dst =
/// originator.
fn build_icmp_echo_reply(
    raw_icmp: &[u8],
    original_ident: u16,
    src_std: std::net::SocketAddr,
    dst_std: std::net::SocketAddr,
) -> Option<Vec<u8>> {
    use smoltcp::phy::ChecksumCapabilities;

    // Need at least type(1) + code(1) + checksum(2) + ident(2) + seq(2)
    if raw_icmp.len() < 8 {
        return None;
    }

    let caps = ChecksumCapabilities::default();

    match (src_std, dst_std) {
        (std::net::SocketAddr::V4(src_v4), std::net::SocketAddr::V4(dst_v4)) => {
            build_icmpv4_echo_reply(raw_icmp, original_ident, src_v4, dst_v4, &caps)
        }
        (std::net::SocketAddr::V6(src_v6), std::net::SocketAddr::V6(dst_v6)) => {
            build_icmpv6_echo_reply(raw_icmp, original_ident, src_v6, dst_v6, &caps)
        }
        _ => None,
    }
}

fn build_icmpv4_echo_reply(
    raw_icmp: &[u8],
    original_ident: u16,
    src_v4: std::net::SocketAddrV4,
    dst_v4: std::net::SocketAddrV4,
    caps: &smoltcp::phy::ChecksumCapabilities,
) -> Option<Vec<u8>> {
    use smoltcp::wire::{Icmpv4Packet, Icmpv4Repr, IpProtocol, Ipv4Packet, Ipv4Repr};

    let icmp_pkt = Icmpv4Packet::new_checked(raw_icmp).ok()?;
    let repr = Icmpv4Repr::parse(&icmp_pkt, caps).ok()?;

    let Icmpv4Repr::EchoReply { seq_no, data, .. } = repr else {
        return None;
    };

    let reply = Icmpv4Repr::EchoReply {
        ident: original_ident,
        seq_no,
        data,
    };

    // IP: src = target (where the ping went), dst = originator
    let target_ip: smoltcp::wire::Ipv4Address = *dst_v4.ip();
    let client_ip: smoltcp::wire::Ipv4Address = *src_v4.ip();

    let ip_repr = Ipv4Repr {
        src_addr: target_ip,
        dst_addr: client_ip,
        next_header: IpProtocol::Icmp,
        payload_len: reply.buffer_len(),
        hop_limit: 64,
    };

    let total = ip_repr.buffer_len() + reply.buffer_len();
    let mut buf = vec![0u8; total];

    let mut ip_pkt = Ipv4Packet::new_unchecked(&mut buf);
    ip_repr.emit(&mut ip_pkt, caps);

    let mut icmp_out = Icmpv4Packet::new_unchecked(&mut buf[ip_repr.buffer_len()..]);
    reply.emit(&mut icmp_out, caps);

    Some(buf)
}

fn build_icmpv6_echo_reply(
    raw_icmp: &[u8],
    original_ident: u16,
    src_v6: std::net::SocketAddrV6,
    dst_v6: std::net::SocketAddrV6,
    caps: &smoltcp::phy::ChecksumCapabilities,
) -> Option<Vec<u8>> {
    use smoltcp::wire::{Icmpv6Packet, Icmpv6Repr, IpProtocol, Ipv6Packet, Ipv6Repr};

    let target_ip: smoltcp::wire::Ipv6Address = *dst_v6.ip();
    let client_ip: smoltcp::wire::Ipv6Address = *src_v6.ip();

    let icmp_pkt = Icmpv6Packet::new_checked(raw_icmp).ok()?;
    let repr = Icmpv6Repr::parse(&target_ip, &client_ip, &icmp_pkt, caps).ok()?;

    let Icmpv6Repr::EchoReply { seq_no, data, .. } = repr else {
        return None;
    };

    let reply = Icmpv6Repr::EchoReply {
        ident: original_ident,
        seq_no,
        data,
    };

    let ip_repr = Ipv6Repr {
        src_addr: target_ip,
        dst_addr: client_ip,
        next_header: IpProtocol::Icmpv6,
        payload_len: reply.buffer_len(),
        hop_limit: 64,
    };

    let total = ip_repr.buffer_len() + reply.buffer_len();
    let mut buf = vec![0u8; total];

    let mut ip_pkt = Ipv6Packet::new_unchecked(&mut buf);
    ip_repr.emit(&mut ip_pkt);

    let mut icmp_out = Icmpv6Packet::new_unchecked(&mut buf[ip_repr.buffer_len()..]);
    reply.emit(&target_ip, &client_ip, &mut icmp_out, caps);

    Some(buf)
}
