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
    net::{IpAddr, SocketAddr},
    pin::Pin,
    sync::Arc,
    time::{Duration, Instant},
};

use parking_lot::Mutex;

use smoltcp::{
    phy::{ChecksumCapabilities, Device},
    wire::{
        IpProtocol, IpVersion, Ipv4Packet, Ipv4Repr, Ipv6Packet, TcpControl, TcpPacket, TcpRepr,
        TcpSeqNumber,
    },
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
// SYN probe cache types
// ============================================================================

/// Cache entries expire after this duration. Prevents stale results from
/// persisting when scanning multiple hosts (per-port cache may differ across
/// targets) and ensures transient network issues don't cause permanent denial.
const CACHE_TTL: Duration = Duration::from_secs(5);

/// Per-(host, port) probe result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheEntry {
    /// Probe in progress — SYN is held, waiting for exit confirmation.
    Probing,
    /// Exit confirmed port is reachable.
    Open,
    /// Exit confirmed port is unreachable (ECONNREFUSED) — smoltcp will RST.
    Closed,
    /// Host unreachable (EHOSTUNREACH) — respond with ICMP Destination Unreachable.
    Unreachable,
}

/// A SYN packet held while the exit node is probed for reachability.
pub struct HeldSyn {
    /// The raw IP packet (SYN).
    pub packet: Vec<u8>,
    /// Destination address (IP + port) — cache lookup key.
    pub dst_addr: SocketAddr,
}

/// Probe result cache for the SYN intercept path, shared between poll loop and connection manager.
pub struct SynProbeCache {
    /// Per-(host, port) cache of probe results with insertion time for TTL expiry.
    cache: Mutex<HashMap<SocketAddr, (CacheEntry, Instant)>>,
}

impl SynProbeCache {
    /// Create a new probe cache.
    #[must_use]
    pub fn new() -> Self {
        Self {
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// Look up the cached status for a (host, port). Returns `None` if expired or absent.
    #[must_use]
    pub fn get(&self, addr: SocketAddr) -> Option<CacheEntry> {
        let cache = self.cache.lock();
        let &(entry, created) = cache.get(&addr)?;
        if created.elapsed() > CACHE_TTL {
            return None;
        }
        Some(entry)
    }

    /// Mark a (host, port) as currently being probed.
    pub fn mark_probing(&self, addr: SocketAddr) {
        self.cache
            .lock()
            .insert(addr, (CacheEntry::Probing, Instant::now()));
    }

    /// Mark a (host, port) as open (exit confirmed reachable).
    pub fn mark_open(&self, addr: SocketAddr) {
        self.cache
            .lock()
            .insert(addr, (CacheEntry::Open, Instant::now()));
    }

    /// Mark a (host, port) as closed (ECONNREFUSED) — smoltcp will RST.
    pub fn mark_closed(&self, addr: SocketAddr) {
        self.cache
            .lock()
            .insert(addr, (CacheEntry::Closed, Instant::now()));
    }

    /// Mark a (host, port) as unreachable (EHOSTUNREACH) — respond with ICMP.
    pub fn mark_unreachable(&self, addr: SocketAddr) {
        self.cache
            .lock()
            .insert(addr, (CacheEntry::Unreachable, Instant::now()));
    }

    /// Check if a (host, port) is cached as closed (and not expired).
    #[must_use]
    pub fn is_closed(&self, addr: SocketAddr) -> bool {
        let cache = self.cache.lock();
        matches!(cache.get(&addr), Some(&(CacheEntry::Closed, created)) if created.elapsed() <= CACHE_TTL)
    }

    /// Clear the entire cache.
    pub fn clear_cache(&self) {
        self.cache.lock().clear();
    }
}

impl Default for SynProbeCache {
    fn default() -> Self {
        Self::new()
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
    /// Port probe cache (None = disabled).
    probe_cache: Option<Arc<SynProbeCache>>,
    /// Channel for sending held SYNs to the connection manager.
    syn_tx: Option<tokio::sync::mpsc::UnboundedSender<HeldSyn>>,
    /// Channel for receiving re-injected packets from the manager.
    inject_rx: Option<Arc<Mutex<tokio::sync::mpsc::UnboundedReceiver<Vec<u8>>>>>,
    /// Channel for sending raw packets (e.g. ICMP) to the TUN device (bypassing smoltcp).
    egress_tx: Option<tokio::sync::mpsc::UnboundedSender<Vec<u8>>>,
    /// Channel for forwarding intercepted ICMP Echo Requests to the manager.
    icmp_tx: Option<tokio::sync::mpsc::UnboundedSender<Vec<u8>>>,
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
            probe_cache: None,
            syn_tx: None,
            inject_rx: None,
            egress_tx: None,
            icmp_tx: None,
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

    /// Configure SYN probe cache channels and state.
    ///
    /// Returns:
    /// - `syn_rx`: receiver for held SYNs (manager probes exit then acts on result)
    /// - `inject_tx`: sender for re-injected packets back to smoltcp ingress
    /// - `wake_notify`: wake the poll loop when a probe completes
    /// - `egress_rx`: receiver for raw packets (ICMP) to write directly to the TUN
    /// - `icmp_rx`: receiver for intercepted ICMP Echo Requests to forward via tunnel
    #[allow(clippy::type_complexity)]
    pub fn set_probe_cache(
        &mut self,
        state: Arc<SynProbeCache>,
    ) -> (
        tokio::sync::mpsc::UnboundedReceiver<HeldSyn>,
        tokio::sync::mpsc::UnboundedSender<Vec<u8>>,
        Arc<Notify>,
        tokio::sync::mpsc::UnboundedReceiver<Vec<u8>>,
        tokio::sync::mpsc::UnboundedReceiver<Vec<u8>>,
    )
    where
        D: PeekDevice,
    {
        let (syn_tx, syn_rx) = tokio::sync::mpsc::unbounded_channel();
        let (inject_tx, inject_rx) = tokio::sync::mpsc::unbounded_channel();
        let (egress_tx, egress_rx) = tokio::sync::mpsc::unbounded_channel();
        let (icmp_tx, icmp_rx) = tokio::sync::mpsc::unbounded_channel();
        let wake_notify = Arc::new(Notify::new());

        self.probe_cache = Some(state);
        self.syn_tx = Some(syn_tx);
        self.inject_rx = Some(Arc::new(Mutex::new(inject_rx)));
        self.egress_tx = Some(egress_tx);
        self.icmp_tx = Some(icmp_tx);
        self.wake_notify = Some(Arc::clone(&wake_notify));
        self.restart_poll_loop();

        (syn_rx, inject_tx, wake_notify, egress_rx, icmp_rx)
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
            probe_cache: self.probe_cache.clone(),
            syn_tx: self.syn_tx.clone(),
            inject_rx: self.inject_rx.clone(),
            egress_tx: self.egress_tx.clone(),
            icmp_tx: self.icmp_tx.clone(),
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
    probe_cache: Option<Arc<SynProbeCache>>,
    syn_tx: Option<tokio::sync::mpsc::UnboundedSender<HeldSyn>>,
    inject_rx: Option<Arc<Mutex<tokio::sync::mpsc::UnboundedReceiver<Vec<u8>>>>>,
    /// Channel for sending raw packets (ICMP) directly to the TUN (bypasses smoltcp).
    egress_tx: Option<tokio::sync::mpsc::UnboundedSender<Vec<u8>>>,
    /// Channel for forwarding intercepted ICMP Echo Requests to the manager.
    icmp_tx: Option<tokio::sync::mpsc::UnboundedSender<Vec<u8>>>,
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

/// Phase 2+3: JIT ingress processing with optional SYN probe intercept.
fn jit_poll_ingress<D: Device + Send + 'static + PeekDevice>(
    inner: &mut InnerStack<D>,
    config: &JitPollConfig,
    notify: &Arc<Notify>,
) {
    // Intercept ICMP Echo Requests before smoltcp auto-replies (any_ip mode).
    // Extracted packets are forwarded to the manager for tunnel delivery.
    if let Some(ref icmp_tx) = config.icmp_tx {
        extract_icmp_echo_requests(inner, icmp_tx);
    }

    if config.probe_cache.is_some() {
        handle_probed_syn_ingress(inner, config, notify);
    } else {
        let port_info: Vec<_> = inner
            .peek_all_ingress()
            .iter()
            .filter_map(|pkt| parse_l4(pkt))
            .collect();
        for (protocol, dst_addr, is_syn) in port_info {
            let _ = jit_bind_port(inner, protocol, dst_addr, is_syn, config, notify);
        }
    }

    // Drop unmatched TCP, but exempt closed-cached ports.
    if config.jit_tcp {
        drop_unmatched_tcp(inner, config.probe_cache.as_ref());
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
            + inner.prune_stale_syn_received(std::time::Duration::from_mins(1))
            + inner.prune_stale_listeners(std::time::Duration::from_secs(30));

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

/// Handle probed SYN ingress: classify each pending packet, hold unknown SYNs,
/// JIT-bind open ports, let closed ports through (no JIT bind → smoltcp RST),
/// silently drop filtered (unreachable) ports.
fn handle_probed_syn_ingress<D: Device + Send + 'static + PeekDevice>(
    inner: &mut InnerStack<D>,
    config: &JitPollConfig,
    notify: &Arc<Notify>,
) {
    let Some(state) = config.probe_cache.as_ref() else {
        return;
    };

    // Collect work to be done.
    let mut held_syns: Vec<HeldSyn> = Vec::new();
    let mut drop_addrs: HashSet<SocketAddr> = HashSet::new();
    let mut egress_packets: Vec<Vec<u8>> = Vec::new();
    let mut bind_tasks: Vec<(IpProtocol, SocketAddr, bool)> = Vec::new();

    // Classify packets.
    {
        let packets = inner.peek_all_ingress();
        if !packets.is_empty() {
            tracing::debug!(
                "handle_probed_syn_ingress: peeking {} packets",
                packets.len()
            );
        }
        for pkt in packets {
            let Some((protocol, dst_addr, is_syn)) = parse_l4(pkt) else {
                continue;
            };

            tracing::trace!(?protocol, ?dst_addr, is_syn, "analyzing ingress packet");

            if protocol == IpProtocol::Tcp && is_syn && config.jit_tcp {
                match state.get(dst_addr) {
                    Some(CacheEntry::Open) => {
                        tracing::debug!(?dst_addr, "Cache hit: OPEN -> Bind");
                        bind_tasks.push((protocol, dst_addr, is_syn));
                    }
                    Some(CacheEntry::Closed) => {
                        tracing::debug!(?dst_addr, "Cache hit: CLOSED -> RST");
                        if let Some(rst) = build_ipv4_rst(pkt) {
                            egress_packets.push(rst);
                        }
                        drop_addrs.insert(dst_addr);
                    }
                    Some(CacheEntry::Probing) => {
                        tracing::debug!(?dst_addr, "Cache hit: PROBING -> Drop");
                        // Another SYN arrived while probe is in flight — drop.
                        drop_addrs.insert(dst_addr);
                    }
                    Some(CacheEntry::Unreachable) => {
                        tracing::debug!(?dst_addr, "Cache hit: UNREACHABLE -> ICMP");
                        if let Some(icmp) = build_icmp_host_unreachable(pkt) {
                            egress_packets.push(icmp);
                        }
                        drop_addrs.insert(dst_addr);
                    }
                    None => {
                        tracing::info!(?dst_addr, "Cache miss -> Start Probe");
                        state.mark_probing(dst_addr);
                        drop_addrs.insert(dst_addr);
                        held_syns.push(HeldSyn {
                            packet: pkt.clone(),
                            dst_addr,
                        });
                    }
                }
            } else {
                bind_tasks.push((protocol, dst_addr, is_syn));
            }
        }
    }

    // Perform JIT binding.
    for (protocol, dst_addr, is_syn) in bind_tasks {
        let _ = jit_bind_port(inner, protocol, dst_addr, is_syn, config, notify);
    }

    // Drop held/probing/filtered SYN packets from the pending queue.
    if !drop_addrs.is_empty() {
        inner.device_mut().retain_pending(|pkt| {
            let Some((protocol, dst_addr, is_syn)) = parse_l4(pkt) else {
                return true;
            };
            !(protocol == IpProtocol::Tcp && is_syn && drop_addrs.contains(&dst_addr))
        });
    }

    // Send held SYNs to the manager for probing.
    if let Some(tx) = config.syn_tx.as_ref() {
        for held in held_syns {
            tracing::debug!(dst=?held.dst_addr, "Sending SYN to manager for probing");
            let _ = tx.send(held);
        }
    }

    // Send raw packets (ICMP, RST) to the TUN (bypassing smoltcp).
    if let Some(tx) = config.egress_tx.as_ref() {
        for pkt in egress_packets {
            let _ = tx.send(pkt);
        }
    }
}

/// Extract ICMP Echo Request packets from the pending queue before smoltcp
/// processes them (smoltcp with `any_ip=true` would auto-reply locally).
/// Matched packets are removed from pending and sent via `icmp_tx` for
/// forwarding through the tunnel to the actual target.
fn extract_icmp_echo_requests<D: Device + Send + 'static + PeekDevice>(
    inner: &mut InnerStack<D>,
    icmp_tx: &tokio::sync::mpsc::UnboundedSender<Vec<u8>>,
) {
    let packets = inner.peek_all_ingress();
    let has_icmp = packets.iter().any(|pkt| is_icmp_echo_request(pkt));
    if !has_icmp {
        return;
    }

    // Collect packets to forward before mutating the queue.
    let to_forward: Vec<Vec<u8>> = packets
        .iter()
        .filter(|pkt| is_icmp_echo_request(pkt))
        .cloned()
        .collect();

    inner
        .device_mut()
        .retain_pending(|pkt| !is_icmp_echo_request(pkt));

    for pkt in to_forward {
        tracing::trace!(len = pkt.len(), "forwarding ICMP echo request to manager");
        let _ = icmp_tx.send(pkt);
    }
}

/// Test whether a raw IP packet is an ICMP Echo Request (ping).
///
/// Matches `ICMPv4` type 8 and `ICMPv6` type 128.
fn is_icmp_echo_request(packet: &[u8]) -> bool {
    let Ok(version) = IpVersion::of_packet(packet) else {
        return false;
    };
    match version {
        IpVersion::Ipv4 => {
            let Ok(ipv4) = Ipv4Packet::new_checked(packet) else {
                return false;
            };
            if ipv4.next_header() != IpProtocol::Icmp {
                return false;
            }
            let payload = ipv4.payload();
            // ICMP type 8 = Echo Request, minimum 8 bytes (type, code, cksum, id, seq)
            payload.len() >= 8 && payload[0] == 8
        }
        IpVersion::Ipv6 => is_icmpv6_echo_request(packet),
    }
}

/// Check for `ICMPv6` Echo Request (type 128), walking extension headers.
fn is_icmpv6_echo_request(packet: &[u8]) -> bool {
    let Ok(ipv6) = Ipv6Packet::new_checked(packet) else {
        return false;
    };
    let mut next_header = ipv6.next_header();
    let mut payload = ipv6.payload();

    loop {
        match next_header {
            IpProtocol::HopByHop
            | IpProtocol::Ipv6Route
            | IpProtocol::Ipv6Frag
            | IpProtocol::Ipv6Opts => {
                if payload.len() < 2 {
                    return false;
                }
                next_header = IpProtocol::from(payload[0]);
                let ext_len = (usize::from(payload[1]) + 1) * 8;
                if payload.len() < ext_len {
                    return false;
                }
                payload = &payload[ext_len..];
            }
            IpProtocol::Icmpv6 => {
                // ICMPv6 type 128 = Echo Request
                return payload.len() >= 8 && payload[0] == 128;
            }
            _ => return false,
        }
    }
}

fn jit_bind_port<D: Device + Send + 'static>(
    inner: &mut InnerStack<D>,
    protocol: IpProtocol,
    dst_addr: SocketAddr,
    is_syn: bool,
    config: &JitPollConfig,
    notify: &Arc<Notify>,
) -> Result<(), crate::error::Error> {
    let dst_port = dst_addr.port();
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
            // Create a wildcard LISTEN socket for EACH SYN. smoltcp transitions
            // LISTEN -> SYN_RECEIVED -> ESTABLISHED per socket, so we need one
            // per incoming connection.
            //
            // Cross-host isolation (A:22 vs B:22) is handled by the SocketAddr-
            // keyed SynProbeCache: only SYNs whose (IP, port) is cached as Open
            // ever reach this point. We cannot use host-specific listeners here
            // because smoltcp's AnyIP mode skips specific-IP sockets whose addr
            // is not in the interface ip_addrs list.
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

fn parse_l4(packet: &[u8]) -> Option<(IpProtocol, SocketAddr, bool)> {
    let version = IpVersion::of_packet(packet).ok()?;
    match version {
        IpVersion::Ipv4 => parse_ipv4_l4(packet),
        IpVersion::Ipv6 => parse_ipv6_l4(packet),
    }
}

fn parse_ipv4_l4(packet: &[u8]) -> Option<(IpProtocol, SocketAddr, bool)> {
    let ipv4_pkt = Ipv4Packet::new_checked(packet).ok()?;
    let protocol = ipv4_pkt.next_header();
    let dst_ip = IpAddr::V4(ipv4_pkt.dst_addr());
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

    Some((protocol, SocketAddr::new(dst_ip, dst_port), is_syn))
}

fn parse_ipv6_l4(packet: &[u8]) -> Option<(IpProtocol, SocketAddr, bool)> {
    let ipv6_pkt = Ipv6Packet::new_checked(packet).ok()?;
    let dst_ip = IpAddr::V6(ipv6_pkt.dst_addr());
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
                return Some((IpProtocol::Tcp, SocketAddr::new(dst_ip, dst_port), is_syn));
            }
            IpProtocol::Udp => {
                if payload.len() < 4 {
                    return None;
                }
                let dst_port = u16::from_be_bytes([payload[2], payload[3]]);
                return Some((IpProtocol::Udp, SocketAddr::new(dst_ip, dst_port), false));
            }
            _ => return None,
        }
    }
}

/// Build an ICMP Destination Unreachable (Host Unreachable) packet from an
/// original IPv4 packet.
///
/// The ICMP packet is addressed FROM the target IP (as if the last-hop router
/// reported unreachable) TO the original sender. Per RFC 792, the ICMP payload
/// contains the original IP header + first 8 bytes of the original L4 data.
///
/// Returns `None` if the packet is too short or not IPv4.
#[must_use]
pub fn build_icmp_host_unreachable(original: &[u8]) -> Option<Vec<u8>> {
    // Only handle IPv4 for now.
    if original.len() < 28 || (original[0] >> 4) != 4 {
        return None;
    }
    let ihl = (original[0] & 0x0f) as usize * 4;
    if original.len() < ihl + 8 {
        return None;
    }

    // Original IPs: [12..16] = src, [16..20] = dst.
    let original_src = &original[12..16];
    let original_dst = &original[16..20];

    // ICMP payload: original IP header + first 8 bytes of L4.
    let payload_len = ihl + 8;
    let _ = &original[..payload_len];

    // Total: 20 (IP) + 8 (ICMP header) + payload_len.
    let icmp_msg_len = 8 + payload_len;
    let total_len = 20 + icmp_msg_len;
    let mut pkt = vec![0u8; total_len];

    // -- IPv4 header (20 bytes) --
    // We are spoofing the "router" (Exit Node) sending the ICMP error.
    // The source IP of the ICMP packet should be the destination IP of the original SYN
    // (the host that is unreachable), or the gateway.
    // Nmap expects the source to be the target host or an intermediate router.
    // Let's use the original destination as the source (spoofing the target itself saying "I am unreachable").
    // Or we could use a specific gateway IP if we had one.
    pkt[0] = 0x45; // version=4, IHL=5
    // Total Length
    #[allow(clippy::cast_possible_truncation)]
    pkt[2..4].copy_from_slice(&(total_len as u16).to_be_bytes());
    // ID (random or 0)
    pkt[4..6].copy_from_slice(&0u16.to_be_bytes());
    // Flags + Fragment Offset
    pkt[6..8].copy_from_slice(&0u16.to_be_bytes());
    pkt[8] = 64; // TTL
    pkt[9] = 1; // protocol: ICMP
    pkt[10..12].copy_from_slice(&0u16.to_be_bytes()); // Checksum placeholder
    pkt[12..16].copy_from_slice(original_dst); // Source IP = Target IP
    pkt[16..20].copy_from_slice(original_src); // Dest IP = Scanner IP

    // IPv4 Header Checksum
    let ip_cksum = internet_checksum(&pkt[..20]);
    pkt[10..12].copy_from_slice(&ip_cksum.to_be_bytes());

    // -- ICMP Header (8 bytes) + Payload --
    let icmp_start = 20;
    // Type 3: Destination Unreachable
    pkt[icmp_start] = 3;
    // Code 1: Host Unreachable
    pkt[icmp_start + 1] = 1;
    // Checksum placeholder
    pkt[icmp_start + 2..icmp_start + 4].copy_from_slice(&0u16.to_be_bytes());
    // Unused (4 bytes) - strictly zero for Host Unreachable
    pkt[icmp_start + 4..icmp_start + 8].copy_from_slice(&0u32.to_be_bytes());

    // Copy original IP header + first 8 bytes of original payload
    // Ensure we don't read past end of original
    let copy_len = std::cmp::min(original.len(), ihl + 8);
    pkt[icmp_start + 8..icmp_start + 8 + copy_len].copy_from_slice(&original[..copy_len]);

    // ICMP Checksum (over ICMP header and data)
    let icmp_cksum = internet_checksum(&pkt[icmp_start..]);
    pkt[icmp_start + 2..icmp_start + 4].copy_from_slice(&icmp_cksum.to_be_bytes());

    Some(pkt)
}

/// RFC 1071 Internet checksum (ones' complement sum of 16-bit words).
fn internet_checksum(data: &[u8]) -> u16 {
    let mut sum: u32 = 0;
    let mut i = 0;
    while i + 1 < data.len() {
        let word = u16::from_be_bytes([data[i], data[i + 1]]);
        sum = sum.wrapping_add(u32::from(word));
        i += 2;
    }
    if i < data.len() {
        sum = sum.wrapping_add(u32::from(data[i]) << 8);
    }
    while (sum >> 16) != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    #[allow(clippy::cast_possible_truncation)]
    !(sum as u16)
}

/// Drop pending TCP packets that have no matching socket.
///
/// smoltcp replies with RST to any TCP segment that doesn't match a socket.
/// In a tunnel context this leaks the entry node's presence to scanners
/// (e.g. nmap marks the host "up" on receiving a RST to a probe ACK).
/// By silently dropping unmatched segments we behave like a filtered host.
///
/// **Strict Flow Matching**: To support `AnyIP` (wildcard) listeners without
/// leaking RSTs for unmatched ACKs (e.g. nmap ping scan), we enforce:
/// - **SYN**: Allowed if a listener exists OR if it's a new probe.
/// - **!SYN**: Allowed ONLY if it matches an existing 4-tuple (Established).
fn drop_unmatched_tcp<D: Device + Send + 'static + PeekDevice>(
    inner: &mut InnerStack<D>,
    _probe_cache: Option<&Arc<SynProbeCache>>,
) {
    use smoltcp::{socket::Socket, wire::IpAddress};

    // Collect Active Flows (for !SYN matching).
    // We need (LocalPort, RemoteIP, RemotePort) to match ingress packets.
    // (Ingress DstPort == LocalPort, Ingress SrcIP == RemoteIP, Ingress SrcPort == RemotePort)
    // Performance Note: Using O(N) scan here.
    // TODO: For high-throughput (Fix 2), replace this with a persistent Conntrack Map (O(1)).
    // For now, we perform the scan to satisfy correctness, but acknowledge the bottleneck.
    let mut active_flows: HashSet<(u16, IpAddress, u16)> = HashSet::new();

    for socket in inner.sockets().iter() {
        let Socket::Tcp(tcp) = socket.1 else { continue };
        match tcp.state() {
            smoltcp::socket::tcp::State::Closed | smoltcp::socket::tcp::State::Listen => {}
            _ => {
                // Established, SynReceived, FinWait, etc.
                if let Some(remote) = tcp.remote_endpoint() {
                    let local_port = tcp.local_endpoint().map_or(0, |e| e.port);
                    if local_port != 0 {
                        active_flows.insert((local_port, remote.addr, remote.port));
                    }
                }
            }
        }
    }

    inner.device_mut().retain_pending(|pkt| {
        let Some((protocol, dst_addr, is_syn)) = parse_l4(pkt) else {
            return true;
        };
        // Only filter TCP
        if protocol != IpProtocol::Tcp {
            return true;
        }

        // Allow SYN (handled by handle_probed_syn_ingress for probing/JIT).
        // We never drop SYNs here; the probe logic decides their fate.
        if is_syn {
            return true;
        }

        // Strict Flow Matching for !SYN (ACK, FIN, RST, PSH, etc.)
        // Packet: DstPort=LocalPort, SrcIP=RemoteIP, SrcPort=RemotePort

        let Some(src_info) = parse_l4_src(pkt) else {
            return false; // malformed/unknown src, drop safely
        };

        let (src_ip, src_port) = src_info;
        let key = (dst_addr.port(), src_ip, src_port);

        if active_flows.contains(&key) {
            return true;
        }

        // Drop unmatched non-SYN (ACK scan, stray packets)
        false
    });
}

/// Extract Source IP and Port from a packet (for flow matching).
fn parse_l4_src(packet: &[u8]) -> Option<(smoltcp::wire::IpAddress, u16)> {
    let version = IpVersion::of_packet(packet).ok()?;
    match version {
        IpVersion::Ipv4 => {
            let ipv4 = Ipv4Packet::new_checked(packet).ok()?;
            let src_ip = smoltcp::wire::IpAddress::Ipv4(ipv4.src_addr());
            let payload = ipv4.payload();
            let tcp = TcpPacket::new_checked(payload).ok()?;
            Some((src_ip, tcp.src_port()))
        }
        IpVersion::Ipv6 => {
            let ipv6 = Ipv6Packet::new_checked(packet).ok()?;
            let src_ip = smoltcp::wire::IpAddress::Ipv6(ipv6.src_addr());
            parse_ipv6_src_port(packet).map(|p| (src_ip, p))
        }
    }
}

/// Extract Source Port from IPv6 TCP packet (handling extension headers).
fn parse_ipv6_src_port(packet: &[u8]) -> Option<u16> {
    let ipv6_pkt = Ipv6Packet::new_checked(packet).ok()?;
    let mut next_header = ipv6_pkt.next_header();
    let mut payload = ipv6_pkt.payload();

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
                return Some(tcp_pkt.src_port());
            }
            _ => return None,
        }
    }
}

#[allow(dead_code)]
fn build_rst_reply(original: &[u8]) -> Option<Vec<u8>> {
    let version = IpVersion::of_packet(original).ok()?;
    match version {
        IpVersion::Ipv4 => build_ipv4_rst(original),
        IpVersion::Ipv6 => None, // TODO: IPv6 support
    }
}

fn build_ipv4_rst(original: &[u8]) -> Option<Vec<u8>> {
    let ipv4_in = Ipv4Packet::new_checked(original).ok()?;
    let tcp_in = TcpPacket::new_checked(ipv4_in.payload()).ok()?;

    // Logic for RST sequence numbers (RFC 793):
    // If input has ACK, RST seq = ACK
    // If input has no ACK, RST seq = 0, ACK = SEQ + LEN
    let payload_len = tcp_in.payload().len();
    let (seq, ack) = if tcp_in.ack() {
        (tcp_in.ack_number(), None)
    } else {
        let len = payload_len + usize::from(tcp_in.syn()) + usize::from(tcp_in.fin());
        (TcpSeqNumber(0), Some(tcp_in.seq_number() + len))
    };

    let src_addr = ipv4_in.dst_addr();
    let dst_addr = ipv4_in.src_addr();

    let tcp_repr = TcpRepr {
        src_port: tcp_in.dst_port(),
        dst_port: tcp_in.src_port(),
        control: TcpControl::Rst,
        seq_number: seq,
        ack_number: ack,
        window_len: 0,
        window_scale: None,
        max_seg_size: None,
        sack_permitted: false,
        sack_ranges: [None; 3],
        payload: &[],
        timestamp: None,
    };

    let ip_repr = Ipv4Repr {
        src_addr,
        dst_addr,
        next_header: IpProtocol::Tcp,
        payload_len: tcp_repr.header_len() + tcp_repr.payload.len(),
        hop_limit: 64,
    };

    let total_len = ip_repr.buffer_len() + tcp_repr.header_len() + tcp_repr.payload.len();
    let mut buf = vec![0u8; total_len];

    let mut ipv4_out = Ipv4Packet::new_unchecked(&mut buf);
    ip_repr.emit(&mut ipv4_out, &ChecksumCapabilities::default());

    let mut tcp_out = TcpPacket::new_unchecked(ipv4_out.payload_mut());
    tcp_repr.emit(
        &mut tcp_out,
        &src_addr.into(),
        &dst_addr.into(),
        &ChecksumCapabilities::default(),
    );

    Some(buf)
}
