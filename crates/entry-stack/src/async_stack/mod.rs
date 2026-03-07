pub mod tcp_listener;
pub mod tcp_listener_any;
pub mod tcp_stream;
pub mod udp_socket;

#[cfg(test)]
pub(crate) mod test_helpers;
#[cfg(test)]
mod tests;

use std::{
    collections::{HashMap, HashSet},
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use parking_lot::Mutex;

use smoltcp::{
    phy::Device,
    wire::{IpProtocol, IpVersion, Ipv4Packet, Ipv6Packet, TcpPacket},
};
use tokio::{
    sync::{Notify, watch},
    task::JoinHandle,
};

use crate::inner::{InnerStack, peek_device::PeekDevice};

/// Factory for readiness futures. Called each poll iteration to get a future
/// that resolves when the underlying device has data available.
pub type ReadinessFn = Arc<dyn Fn() -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

// ============================================================================
// SYN proxy types
// ============================================================================

/// Per-port probe result cached by the SYN proxy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheEntry {
    /// Probe in progress — SYN is held, waiting for exit confirmation.
    Probing,
    /// Exit confirmed port is reachable.
    Open,
    /// Exit confirmed port is unreachable.
    Closed,
}

/// A SYN packet held while the exit node is probed for reachability.
pub struct HeldSyn {
    /// The raw IP packet (SYN).
    pub packet: Vec<u8>,
    /// Destination port extracted from the SYN.
    pub dst_port: u16,
}

/// Shared state for the SYN proxy, accessible from poll loop and manager.
pub struct SynProxyState {
    /// When true, skip probing and JIT-bind immediately (optimistic/fast mode).
    fast_mode: AtomicBool,
    /// Per-port cache of probe results.
    cache: Mutex<HashMap<u16, CacheEntry>>,
}

impl SynProxyState {
    /// Create a new SYN proxy state.
    #[must_use]
    pub fn new(fast_mode: bool) -> Self {
        Self {
            fast_mode: AtomicBool::new(fast_mode),
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// Whether fast (optimistic JIT) mode is enabled.
    #[must_use]
    pub fn is_fast_mode(&self) -> bool {
        self.fast_mode.load(Ordering::Relaxed)
    }

    /// Toggle fast mode on or off, clearing the cache on change.
    pub fn set_fast_mode(&self, enabled: bool) {
        self.fast_mode.store(enabled, Ordering::Relaxed);
        self.clear_cache();
    }

    /// Look up the cached status for a port.
    #[must_use]
    pub fn get(&self, port: u16) -> Option<CacheEntry> {
        self.cache.lock().get(&port).copied()
    }

    /// Mark a port as currently being probed.
    pub fn mark_probing(&self, port: u16) {
        self.cache.lock().insert(port, CacheEntry::Probing);
    }

    /// Mark a port as open (exit confirmed reachable).
    pub fn mark_open(&self, port: u16) {
        self.cache.lock().insert(port, CacheEntry::Open);
    }

    /// Mark a port as closed (exit confirmed unreachable).
    pub fn mark_closed(&self, port: u16) {
        self.cache.lock().insert(port, CacheEntry::Closed);
    }

    /// Check if a port is cached as closed.
    #[must_use]
    pub fn is_closed(&self, port: u16) -> bool {
        self.cache.lock().get(&port) == Some(&CacheEntry::Closed)
    }

    /// Clear the entire cache.
    pub fn clear_cache(&self) {
        self.cache.lock().clear();
    }
}

/// Shared state between the poll loop and async socket handles.
///
/// Uses [`parking_lot::Mutex`] for better performance and no poisoning.
pub(crate) struct Shared<D: Device> {
    pub(crate) inner: Mutex<InnerStack<D>>,
    pub(crate) notify: Notify,
}

/// Asynchronous wrapper around [`InnerStack`].
///
/// `Netstack` spawns a background poll loop that drives the smoltcp state
/// machine. It provides [`TcpListener`](tcp_listener::TcpListener) and
/// [`TcpStream`](tcp_stream::TcpStream) types with standard async I/O traits.
///
/// # Examples
///
/// ```no_run
/// use wallhack_entry_stack::async_stack::Netstack;
/// use wallhack_entry_stack::inner::device::VecDevice;
/// use wallhack_entry_stack::config::StackConfig;
/// use smoltcp::wire::{IpCidr, Ipv4Address};
///
/// # async fn example() {
/// let config = StackConfig {
///     ip_addrs: vec![IpCidr::new(Ipv4Address::new(10, 0, 0, 1).into(), 24)],
///     ..StackConfig::default()
/// };
/// let device = VecDevice::new(1500);
/// let stack = Netstack::new(device, config);
/// # }
/// ```
pub struct Netstack<D: Device + Send + 'static> {
    shared: Arc<Shared<D>>,
    poll_handle: JoinHandle<()>,
    jit_tcp: bool,
    jit_udp: bool,
    tcp_ports: Arc<Mutex<HashSet<u16>>>,
    /// Watch sender for the JIT TCP port set; receivers held by `TcpListenerAny`.
    /// Published on every port insert and pruned so that `TcpListenerAny` can do
    /// a cheap `Arc::clone` rather than cloning the full `HashSet` on every wakeup.
    tcp_ports_watch: Arc<watch::Sender<Arc<HashSet<u16>>>>,
    udp_ports: Arc<Mutex<HashSet<u16>>>,
    jit_notify: Arc<Notify>,
    readable_fn: Option<ReadinessFn>,
    /// SYN proxy state (None = no proxy, fast mode by default).
    syn_proxy: Option<Arc<SynProxyState>>,
    /// Channel for sending held SYNs to the connection manager.
    syn_tx: Option<tokio::sync::mpsc::UnboundedSender<HeldSyn>>,
    /// Channel for receiving re-injected packets from the manager.
    inject_rx: Option<Arc<Mutex<tokio::sync::mpsc::UnboundedReceiver<Vec<u8>>>>>,
    /// Notify for waking poll loop when a probe completes.
    wake_notify: Option<Arc<Notify>>,
}

impl<D: Device + Send + 'static> Netstack<D> {
    /// Create a new async network stack and start the background poll loop.
    ///
    /// # Panics
    ///
    /// Panics if called outside a tokio runtime.
    pub fn new(device: D, config: crate::config::StackConfig) -> Self {
        let inner = InnerStack::new(device, config);
        let shared = Arc::new(Shared {
            inner: Mutex::new(inner),
            notify: Notify::new(),
        });

        let poll_handle = {
            let shared = Arc::clone(&shared);
            tokio::spawn(poll_loop_basic(shared))
        };

        let (tcp_ports_watch_tx, _) = watch::channel(Arc::new(HashSet::new()));

        Self {
            shared,
            poll_handle,
            jit_tcp: false,
            jit_udp: false,
            tcp_ports: Arc::new(Mutex::new(HashSet::new())),
            tcp_ports_watch: Arc::new(tcp_ports_watch_tx),
            udp_ports: Arc::new(Mutex::new(HashSet::new())),
            jit_notify: Arc::new(Notify::new()),
            readable_fn: None,
            syn_proxy: None,
            syn_tx: None,
            inject_rx: None,
            wake_notify: None,
        }
    }

    /// Enable JIT TCP listeners for any destination port.
    pub fn enable_tcp_listen_any(&mut self)
    where
        D: PeekDevice,
    {
        self.jit_tcp = true;
        self.shared
            .inner
            .lock()
            .set_jit_tcp_ports(Arc::clone(&self.tcp_ports));
        self.restart_poll_loop();
    }

    /// Enable JIT UDP listeners for any destination port.
    pub fn enable_udp_bind_any(&mut self)
    where
        D: PeekDevice,
    {
        self.jit_udp = true;
        self.restart_poll_loop();
    }

    /// Set a readiness callback for the underlying device.
    ///
    /// When set, the poll loop will await this callback instead of sleeping
    /// 1ms between iterations. This eliminates CPU-wasting busy polling when
    /// the device has no data available.
    pub fn set_readable_fn(&mut self, f: ReadinessFn)
    where
        D: PeekDevice,
    {
        self.readable_fn = Some(f);
        self.restart_poll_loop();
    }

    /// Configure SYN proxy channels and state.
    ///
    /// Returns the receiver for held SYNs and sender for re-injected packets,
    /// plus a wake notify for the poll loop.
    ///
    /// The caller (`ConnectionManager`) uses `syn_rx` to receive held SYNs,
    /// probes the exit, then sends verified packets via `inject_tx` and
    /// wakes the poll loop via `wake_notify`.
    pub fn set_syn_proxy(
        &mut self,
        state: Arc<SynProxyState>,
    ) -> (
        tokio::sync::mpsc::UnboundedReceiver<HeldSyn>,
        tokio::sync::mpsc::UnboundedSender<Vec<u8>>,
        Arc<Notify>,
    )
    where
        D: PeekDevice,
    {
        let (syn_tx, syn_rx) = tokio::sync::mpsc::unbounded_channel();
        let (inject_tx, inject_rx) = tokio::sync::mpsc::unbounded_channel();
        let wake_notify = Arc::new(Notify::new());

        self.syn_proxy = Some(state);
        self.syn_tx = Some(syn_tx);
        self.inject_rx = Some(Arc::new(Mutex::new(inject_rx)));
        self.wake_notify = Some(Arc::clone(&wake_notify));
        self.restart_poll_loop();

        (syn_rx, inject_tx, wake_notify)
    }

    fn restart_poll_loop(&mut self)
    where
        D: PeekDevice,
    {
        self.poll_handle.abort();
        let shared = Arc::clone(&self.shared);
        let notify = Arc::clone(&self.jit_notify);
        let config = JitPollConfig {
            jit_tcp: self.jit_tcp,
            jit_udp: self.jit_udp,
            tcp_ports: Arc::clone(&self.tcp_ports),
            tcp_ports_watch: Arc::clone(&self.tcp_ports_watch),
            udp_ports: Arc::clone(&self.udp_ports),
            readable_fn: self.readable_fn.clone(),
            syn_proxy: self.syn_proxy.clone(),
            syn_tx: self.syn_tx.clone(),
            inject_rx: self.inject_rx.clone(),
            wake_notify: self.wake_notify.clone(),
        };
        self.poll_handle = tokio::spawn(poll_loop_jit(shared, notify, config));
        self.wake();
    }

    /// Create a TCP listener on the given port.
    ///
    /// # Errors
    ///
    /// Returns an error if the port is invalid or the listen socket cannot
    /// be created.
    #[must_use]
    pub fn tcp_listen(&self, port: u16, backlog: usize) -> tcp_listener::TcpListener<D> {
        tcp_listener::TcpListener::new(Arc::clone(&self.shared), port, backlog)
    }

    /// Create a TCP listener that accepts on any port via JIT binding.
    ///
    /// # Errors
    ///
    /// Returns an error if the listener cannot be created.
    pub fn tcp_listen_any(
        &mut self,
    ) -> Result<tcp_listener_any::TcpListenerAny<D>, crate::error::Error>
    where
        D: PeekDevice,
    {
        self.enable_tcp_listen_any();
        Ok(tcp_listener_any::TcpListenerAny::new(
            Arc::clone(&self.shared),
            Arc::clone(&self.jit_notify),
            self.tcp_ports_watch.subscribe(),
        ))
    }

    /// Create a UDP socket that accepts on any port via JIT binding.
    ///
    /// # Errors
    ///
    /// Returns an error if the socket cannot be created.
    pub fn udp_bind_any(&mut self) -> Result<udp_socket::UdpSocketAny<D>, crate::error::Error>
    where
        D: PeekDevice,
    {
        self.enable_udp_bind_any();
        Ok(udp_socket::UdpSocketAny::new(
            Arc::clone(&self.shared),
            Arc::clone(&self.jit_notify),
            Arc::clone(&self.udp_ports),
        ))
    }

    /// Wake the poll loop to process pending work immediately.
    pub fn wake(&self) {
        self.shared.notify.notify_one();
    }
}

impl<D: Device + Send + 'static> Drop for Netstack<D> {
    fn drop(&mut self) {
        self.poll_handle.abort();
    }
}

/// Background poll loop that drives the smoltcp state machine.
///
/// Acquires the mutex, calls `InnerStack::poll()`, checks `poll_at()` for
/// the next deadline, then sleeps until either the deadline or a notification.
///
/// # Cancellation safety
///
/// This function is safe to cancel (abort) at any point — the only state
/// is behind the mutex, which is never held across an await.
async fn poll_loop_basic<D: Device + Send + 'static>(shared: Arc<Shared<D>>) {
    let mut prune_counter: u32 = 0;
    loop {
        let delay = {
            let mut inner = shared.inner.lock();
            let now = inner.now();
            inner.poll(now);
            prune_and_notify(&mut inner, &shared.notify, &mut prune_counter);
            inner.poll_at(now).map(|poll_at| {
                let diff = poll_at - now;
                tokio::time::Duration::from_millis(diff.total_millis())
            })
        };

        match delay {
            Some(d) if d.is_zero() => {
                tokio::task::yield_now().await;
            }
            Some(d) => {
                tokio::select! {
                    () = tokio::time::sleep(d) => {}
                    () = shared.notify.notified() => {}
                }
            }
            None => {
                shared.notify.notified().await;
            }
        }
    }
}

/// Configuration for the JIT poll loop, bundled to avoid too many arguments.
struct JitPollConfig {
    jit_tcp: bool,
    jit_udp: bool,
    tcp_ports: Arc<Mutex<HashSet<u16>>>,
    /// Watch sender to keep `TcpListenerAny` receivers updated when the port set changes.
    tcp_ports_watch: Arc<watch::Sender<Arc<HashSet<u16>>>>,
    udp_ports: Arc<Mutex<HashSet<u16>>>,
    readable_fn: Option<ReadinessFn>,
    syn_proxy: Option<Arc<SynProxyState>>,
    syn_tx: Option<tokio::sync::mpsc::UnboundedSender<HeldSyn>>,
    inject_rx: Option<Arc<Mutex<tokio::sync::mpsc::UnboundedReceiver<Vec<u8>>>>>,
    wake_notify: Option<Arc<Notify>>,
}

async fn poll_loop_jit<D: Device + Send + 'static + PeekDevice>(
    shared: Arc<Shared<D>>,
    notify: Arc<Notify>,
    config: JitPollConfig,
) {
    let mut prune_counter: u32 = 0;
    loop {
        let delay = {
            let mut inner = shared.inner.lock();
            let now = inner.now();

            // Inject re-verified SYN packets from the manager.
            if let Some(ref rx) = config.inject_rx
                && let Some(mut rx) = rx.try_lock()
            {
                while let Ok(packet) = rx.try_recv() {
                    inner.device_mut().inject_pending(packet);
                }
            }

            if config.jit_tcp || config.jit_udp {
                jit_poll_ingress(&mut inner, &config, &notify);
            }

            // smoltcp emits at most one TCP segment per socket per poll().
            // After the normal poll (ingress + one egress), drain additional
            // egress segments to emit a burst without re-doing ingress.
            // Scale rounds inversely with socket count to keep lock duration
            // predictable: ~128 rounds for 1 socket, ~4 for 32+ sockets.
            inner.poll(now);
            let drain_rounds = (128 / inner.socket_count().max(1)).max(4);
            inner.drain_egress(now, drain_rounds);

            prune_and_notify(&mut inner, &notify, &mut prune_counter);
            inner.poll_at(now).map(|poll_at| {
                let diff = poll_at - now;
                tokio::time::Duration::from_millis(diff.total_millis())
            })
        };

        // Resync the TCP ports watch after each prune cycle (every 100 iterations).
        // InnerStack's prune path updates the mutex via jit_tcp_ports; we snapshot
        // and publish so TcpListenerAny receivers see the updated port set.
        if config.jit_tcp && prune_counter.is_multiple_of(100) {
            let snapshot = Arc::new(config.tcp_ports.lock().clone());
            config.tcp_ports_watch.send_replace(snapshot);
        }

        match delay {
            Some(d) if d.is_zero() => {
                tokio::task::yield_now().await;
            }
            Some(d) => {
                tokio::select! {
                    () = async {
                        match &config.readable_fn {
                            Some(f) => f().await,
                            None => tokio::time::sleep(d.min(tokio::time::Duration::from_millis(1))).await,
                        }
                    } => {}
                    () = shared.notify.notified() => {}
                    () = async {
                        match &config.wake_notify {
                            Some(n) => n.notified().await,
                            None => std::future::pending().await,
                        }
                    } => {}
                }
            }
            None => {
                tokio::select! {
                    () = async {
                        match &config.readable_fn {
                            Some(f) => f().await,
                            None => tokio::time::sleep(tokio::time::Duration::from_millis(1)).await,
                        }
                    } => {}
                    () = shared.notify.notified() => {}
                    () = async {
                        match &config.wake_notify {
                            Some(n) => n.notified().await,
                            None => std::future::pending().await,
                        }
                    } => {}
                }
            }
        }
    }
}

/// Phase 2+3: JIT ingress processing with optional SYN proxy.
fn jit_poll_ingress<D: Device + Send + 'static + PeekDevice>(
    inner: &mut InnerStack<D>,
    config: &JitPollConfig,
    notify: &Arc<Notify>,
) {
    let syn_proxy_active = config.syn_proxy.as_ref().is_some_and(|s| !s.is_fast_mode());

    if syn_proxy_active {
        handle_syn_proxy_ingress(inner, config, notify);
    } else {
        let port_info: Vec<_> = inner
            .peek_all_ingress()
            .iter()
            .filter_map(|pkt| parse_l4(pkt))
            .collect();
        for (protocol, dst_port, is_syn) in port_info {
            let _ = jit_bind_port(inner, protocol, dst_port, is_syn, config, notify);
        }
    }

    // Drop unmatched TCP, but exempt closed-cached ports.
    if config.jit_tcp {
        drop_unmatched_tcp_with_proxy(inner, config.syn_proxy.as_ref());
    }
}

/// Periodically prune closed sockets and notify listeners.
fn prune_and_notify<D: Device + Send + 'static>(
    inner: &mut InnerStack<D>,
    notify: &Notify,
    prune_counter: &mut u32,
) {
    *prune_counter = prune_counter.wrapping_add(1);
    if prune_counter.is_multiple_of(100) {
        let socket_count = inner.socket_count();
        let pruned = inner.prune_closed_tcp_sockets()
            + inner.prune_stale_syn_received(std::time::Duration::from_mins(1));

        if pruned > 0 || socket_count > 5 {
            let states = inner.tcp_state_summary();
            tracing::debug!(
                socket_count,
                pruned,
                remaining = inner.socket_count(),
                states,
                "Socket state"
            );
        }
    }
    notify.notify_waiters();
}

/// Handle SYN proxy ingress: classify each pending packet, hold unknown SYNs,
/// JIT-bind open ports, let closed ports through (no JIT bind → smoltcp RST).
fn handle_syn_proxy_ingress<D: Device + Send + 'static + PeekDevice>(
    inner: &mut InnerStack<D>,
    config: &JitPollConfig,
    notify: &Arc<Notify>,
) {
    let Some(state) = config.syn_proxy.as_ref() else {
        return;
    };

    // Collect work to be done.
    let mut held_syns: Vec<HeldSyn> = Vec::new();
    let mut probing_ports: HashSet<u16> = HashSet::new();
    let mut bind_tasks: Vec<(IpProtocol, u16, bool)> = Vec::new();

    // Classify packets.
    {
        let packets = inner.peek_all_ingress();
        for pkt in packets {
            let Some((protocol, dst_port, is_syn)) = parse_l4(pkt) else {
                continue;
            };

            if protocol == IpProtocol::Tcp && is_syn && config.jit_tcp {
                match state.get(dst_port) {
                    Some(CacheEntry::Open) => {
                        bind_tasks.push((protocol, dst_port, is_syn));
                    }
                    Some(CacheEntry::Closed) => {
                        // Keep in pending (no JIT bind), smoltcp will RST.
                    }
                    Some(CacheEntry::Probing) => {
                        probing_ports.insert(dst_port);
                    }
                    None => {
                        state.mark_probing(dst_port);
                        probing_ports.insert(dst_port);
                        held_syns.push(HeldSyn {
                            packet: pkt.clone(),
                            dst_port,
                        });
                    }
                }
            } else {
                bind_tasks.push((protocol, dst_port, is_syn));
            }
        }
    }

    // Perform JIT binding.
    for (protocol, dst_port, is_syn) in bind_tasks {
        let _ = jit_bind_port(inner, protocol, dst_port, is_syn, config, notify);
    }

    // Drop held/probing SYN packets from the pending queue.
    if !probing_ports.is_empty() {
        inner.device_mut().retain_pending(|pkt| {
            let Some((protocol, dst_port, is_syn)) = parse_l4(pkt) else {
                return true;
            };
            !(protocol == IpProtocol::Tcp && is_syn && probing_ports.contains(&dst_port))
        });
    }

    // Send held SYNs to the manager for probing.
    if let Some(tx) = config.syn_tx.as_ref() {
        for held in held_syns {
            let _ = tx.send(held);
        }
    }
}

fn jit_bind_port<D: Device + Send + 'static>(
    inner: &mut InnerStack<D>,
    protocol: IpProtocol,
    dst_port: u16,
    is_syn: bool,
    config: &JitPollConfig,
    notify: &Arc<Notify>,
) -> Result<(), crate::error::Error> {
    tracing::trace!(
        ?protocol,
        dst_port,
        is_syn,
        jit_tcp = config.jit_tcp,
        jit_udp = config.jit_udp,
        "JIT: parsed packet"
    );

    match protocol {
        IpProtocol::Tcp if config.jit_tcp && dst_port != 0 && is_syn => {
            // Create a LISTEN socket for EACH SYN packet.
            // smoltcp transitions LISTEN -> SYN_RECEIVED -> ESTABLISHED per socket,
            // so we need one LISTEN socket per incoming connection.
            tracing::debug!(dst_port, "JIT: SYN packet detected");
            inner.tcp_listen(dst_port)?;
            let mut ports = config.tcp_ports.lock();
            ports.insert(dst_port);
            let snapshot = Arc::new(ports.clone());
            drop(ports);
            config.tcp_ports_watch.send_replace(snapshot);
            notify.notify_waiters();
        }
        IpProtocol::Udp if config.jit_udp && dst_port != 0 => {
            tracing::debug!(
                dst_port,
                socket_count = inner.socket_count(),
                "JIT binding UDP listener"
            );
            inner.ensure_udp_listener(dst_port)?;
            config.udp_ports.lock().insert(dst_port);
            notify.notify_waiters();
        }
        _ => {}
    }

    Ok(())
}

fn parse_l4(packet: &[u8]) -> Option<(IpProtocol, u16, bool)> {
    let version = IpVersion::of_packet(packet).ok()?;
    match version {
        IpVersion::Ipv4 => parse_ipv4_l4(packet),
        IpVersion::Ipv6 => parse_ipv6_l4(packet),
    }
}

fn parse_ipv4_l4(packet: &[u8]) -> Option<(IpProtocol, u16, bool)> {
    let ipv4_pkt = Ipv4Packet::new_checked(packet).ok()?;
    let protocol = ipv4_pkt.next_header();
    let payload = ipv4_pkt.payload();

    let (dst_port, is_syn) = match protocol {
        IpProtocol::Tcp => {
            let tcp_pkt = TcpPacket::new_checked(payload).ok()?;
            (tcp_pkt.dst_port(), tcp_pkt.syn() && !tcp_pkt.ack())
        }
        IpProtocol::Udp => {
            if payload.len() < 4 {
                return None;
            }
            (u16::from_be_bytes([payload[2], payload[3]]), false)
        }
        _ => return None,
    };

    Some((protocol, dst_port, is_syn))
}

fn parse_ipv6_l4(packet: &[u8]) -> Option<(IpProtocol, u16, bool)> {
    let ipv6_pkt = Ipv6Packet::new_checked(packet).ok()?;
    let mut next_header = ipv6_pkt.next_header();
    let mut payload = ipv6_pkt.payload();

    // Walk the extension header chain until we reach a known L4 protocol or an
    // unrecognised header. Known extension headers all have the same layout:
    // byte 0 = next header, byte 1 = hdr_ext_len (in units of 8 bytes, not
    // counting the first 8 bytes), so total length = (hdr_ext_len + 1) * 8.
    // The Fragment header is fixed at 8 bytes (hdr_ext_len field unused there,
    // but we treat it the same since byte 1 is 0 in practice → 8 bytes total).
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
            IpProtocol::Tcp => {
                let tcp_pkt = TcpPacket::new_checked(payload).ok()?;
                let dst_port = tcp_pkt.dst_port();
                let is_syn = tcp_pkt.syn() && !tcp_pkt.ack();
                return Some((IpProtocol::Tcp, dst_port, is_syn));
            }
            IpProtocol::Udp => {
                if payload.len() < 4 {
                    return None;
                }
                let dst_port = u16::from_be_bytes([payload[2], payload[3]]);
                return Some((IpProtocol::Udp, dst_port, false));
            }
            _ => return None,
        }
    }
}

/// Drop pending TCP packets that have no matching socket, with SYN proxy
/// exemptions.
///
/// smoltcp replies with RST to any TCP segment that doesn't match a socket.
/// In a tunnel context this leaks the entry node's presence to scanners
/// (e.g. nmap marks the host "up" on receiving a RST to a probe ACK).
/// By silently dropping unmatched segments we behave like a filtered host.
///
/// **SYN proxy exemption**: SYN packets to ports cached as `Closed` are kept
/// so that smoltcp (with no listener on that port) generates a native RST.
/// This is how nmap sees "closed" instead of "filtered".
fn drop_unmatched_tcp_with_proxy<D: Device + Send + 'static + PeekDevice>(
    inner: &mut InnerStack<D>,
    syn_proxy: Option<&Arc<SynProxyState>>,
) {
    use smoltcp::socket::Socket;

    // Collect ports that have an active TCP socket to avoid borrow conflicts.
    let active_ports: HashSet<u16> = inner
        .sockets()
        .iter()
        .filter_map(|(_, socket)| {
            let Socket::Tcp(tcp) = socket else {
                return None;
            };
            if tcp.state() == smoltcp::socket::tcp::State::Closed {
                return None;
            }
            let port = tcp
                .local_endpoint()
                .map_or_else(|| tcp.listen_endpoint().port, |ep| ep.port);
            if port == 0 { None } else { Some(port) }
        })
        .collect();

    inner.device_mut().retain_pending(|pkt| {
        let Some((protocol, dst_port, is_syn)) = parse_l4(pkt) else {
            return true;
        };
        if protocol != IpProtocol::Tcp {
            return true;
        }
        // Keep if a socket exists for this port.
        if active_ports.contains(&dst_port) {
            return true;
        }
        // SYN proxy exemption: let SYN packets to closed ports through
        // so smoltcp generates a native RST (no listener → RST).
        is_syn && syn_proxy.is_some_and(|state| state.is_closed(dst_port))
    });
}
