pub mod tcp_listener;
pub mod tcp_listener_any;
pub mod tcp_stream;
pub mod udp_socket;

use std::{
	collections::HashSet,
	sync::{Arc, Mutex},
};

use smoltcp::{
	phy::Device,
	time::Instant as SmolInstant,
	wire::{IpProtocol, IpVersion},
};
use tokio::{sync::Notify, task::JoinHandle};

use crate::inner::{InnerStack, peek_device::PeekDevice};

/// Shared state between the poll loop and async socket handles.
///
/// Uses [`std::sync::Mutex`] because the lock is never held across `.await`.
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
/// use netstack::async_stack::Netstack;
/// use netstack::inner::device::VecDevice;
/// use netstack::config::StackConfig;
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
	udp_ports: Arc<Mutex<HashSet<u16>>>,
	jit_notify: Arc<Notify>,
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

		Self {
			shared,
			poll_handle,
			jit_tcp: false,
			jit_udp: false,
			tcp_ports: Arc::new(Mutex::new(HashSet::new())),
			udp_ports: Arc::new(Mutex::new(HashSet::new())),
			jit_notify: Arc::new(Notify::new()),
		}
	}

	/// Enable JIT TCP listeners for any destination port.
	pub fn enable_tcp_listen_any(&mut self)
	where
		D: PeekDevice,
	{
		self.jit_tcp = true;
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

	fn restart_poll_loop(&mut self)
	where
		D: PeekDevice,
	{
		self.poll_handle.abort();
		let shared = Arc::clone(&self.shared);
		let jit_tcp = self.jit_tcp;
		let jit_udp = self.jit_udp;
		let tcp_ports = Arc::clone(&self.tcp_ports);
		let udp_ports = Arc::clone(&self.udp_ports);
		let notify = Arc::clone(&self.jit_notify);
		self.poll_handle = tokio::spawn(poll_loop_jit(
			shared, jit_tcp, jit_udp, tcp_ports, udp_ports, notify,
		));
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
		backlog: usize,
	) -> Result<tcp_listener_any::TcpListenerAny<D>, crate::error::Error>
	where
		D: PeekDevice,
	{
		self.enable_tcp_listen_any();
		Ok(tcp_listener_any::TcpListenerAny::new(
			Arc::clone(&self.shared),
			Arc::clone(&self.jit_notify),
			Arc::clone(&self.tcp_ports),
			backlog,
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
	loop {
		let delay = {
			let mut inner = shared.inner.lock().expect("poll loop mutex poisoned");
			let now = SmolInstant::from_millis(
				i64::try_from(
					std::time::SystemTime::now()
						.duration_since(std::time::UNIX_EPOCH)
						.expect("system clock before epoch")
						.as_millis(),
				)
				.expect("timestamp overflow"),
			);
			inner.poll(now);
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

async fn poll_loop_jit<D: Device + Send + 'static + PeekDevice>(
	shared: Arc<Shared<D>>,
	jit_tcp: bool,
	jit_udp: bool,
	tcp_ports: Arc<Mutex<HashSet<u16>>>,
	udp_ports: Arc<Mutex<HashSet<u16>>>,
	notify: Arc<Notify>,
) {
	let mut prune_counter: u32 = 0;
	loop {
		let delay = {
			let mut inner = shared.inner.lock().expect("poll loop mutex poisoned");
			let now = SmolInstant::from_millis(
				i64::try_from(
					std::time::SystemTime::now()
						.duration_since(std::time::UNIX_EPOCH)
						.expect("system clock before epoch")
						.as_millis(),
				)
				.expect("timestamp overflow"),
			);
			if jit_tcp || jit_udp {
				// Get ALL pending packets and create listeners for each SYN.
				// This handles burst arrivals where multiple SYNs arrive before
				// poll() can process them.
				let packets = inner.peek_all_ingress();
				for packet in &packets {
					let _ = jit_bind_ports(
						&mut inner, packet, jit_tcp, jit_udp, &tcp_ports, &udp_ports, &notify,
					);
				}
			}
			inner.poll(now);
			// Periodically prune closed sockets and log state
			prune_counter = prune_counter.wrapping_add(1);
			if prune_counter.is_multiple_of(100) {
				let socket_count = inner.socket_count();
				let pruned = inner.prune_closed_tcp_sockets();
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
			// Notify listeners after poll - sockets may have transitioned to established
			notify.notify_waiters();
			inner.poll_at(now).map(|poll_at| {
				let diff = poll_at - now;
				tokio::time::Duration::from_millis(diff.total_millis())
			})
		};

		match delay {
			Some(d) if d.is_zero() => {
				// Need to poll again immediately, but yield to let other tasks run
				tokio::task::yield_now().await;
			}
			Some(d) => {
				// We need to poll even if the stack says it can wait, because
				// new packets might arrive on the TUN interface which we need
				// to JIT bind.
				//
				// TODO: This is inefficient (busy loop with 1ms sleep).
				// Proper fix requires AsyncDevice trait so we can await on read.
				tokio::select! {
					() = tokio::time::sleep(d.min(tokio::time::Duration::from_millis(10))) => {}
					() = shared.notify.notified() => {}
				}
			}
			None => {
				// Same here - wake up periodically to check for new packets
				tokio::select! {
					() = tokio::time::sleep(tokio::time::Duration::from_millis(10)) => {}
					() = shared.notify.notified() => {}
				}
			}
		}
	}
}

fn jit_bind_ports<D: Device + Send + 'static>(
	inner: &mut InnerStack<D>,
	packet: &[u8],
	jit_tcp: bool,
	jit_udp: bool,
	tcp_ports: &Arc<Mutex<HashSet<u16>>>,
	udp_ports: &Arc<Mutex<HashSet<u16>>>,
	notify: &Arc<Notify>,
) -> Result<(), crate::error::Error> {
	let Some((protocol, dst_port, is_syn)) = parse_l4(packet) else {
		tracing::trace!(packet_len = packet.len(), "JIT: failed to parse L4");
		return Ok(());
	};

	tracing::trace!(
		?protocol,
		dst_port,
		is_syn,
		jit_tcp,
		jit_udp,
		"JIT: parsed packet"
	);

	match protocol {
		IpProtocol::Tcp if jit_tcp && dst_port != 0 && is_syn => {
			// Create a LISTEN socket for EACH SYN packet.
			// smoltcp transitions LISTEN -> SYN_RECEIVED -> ESTABLISHED per socket,
			// so we need one LISTEN socket per incoming connection.
			tracing::debug!(dst_port, "JIT: SYN packet detected");
			inner.tcp_listen(dst_port)?;
			tcp_ports.lock().expect("tcp port lock").insert(dst_port);
			notify.notify_waiters();
		}
		IpProtocol::Udp if jit_udp && dst_port != 0 => {
			tracing::debug!(
				dst_port,
				socket_count = inner.socket_count(),
				"JIT binding UDP listener"
			);
			inner.ensure_udp_listener(dst_port)?;
			udp_ports.lock().expect("udp port lock").insert(dst_port);
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
	if packet.len() < 20 {
		return None;
	}
	let ihl = (packet[0] & 0x0f) as usize * 4;
	if packet.len() < ihl + 4 {
		return None;
	}
	let protocol = IpProtocol::from(packet[9]);
	let dst_port = u16::from_be_bytes([packet[ihl + 2], packet[ihl + 3]]);
	// Check TCP SYN flag (offset 13 in TCP header, bit 1)
	let is_syn = if protocol == IpProtocol::Tcp && packet.len() >= ihl + 14 {
		(packet[ihl + 13] & 0x02) != 0 && (packet[ihl + 13] & 0x10) == 0 // SYN but not ACK
	} else {
		false
	};
	Some((protocol, dst_port, is_syn))
}

fn parse_ipv6_l4(packet: &[u8]) -> Option<(IpProtocol, u16, bool)> {
	if packet.len() < 44 {
		return None;
	}
	let next_header = IpProtocol::from(packet[6]);
	let dst_port = u16::from_be_bytes([packet[42], packet[43]]);
	// Check TCP SYN flag (offset 13 in TCP header after IPv6 header)
	let is_syn = if next_header == IpProtocol::Tcp && packet.len() >= 54 {
		(packet[40 + 13] & 0x02) != 0 && (packet[40 + 13] & 0x10) == 0 // SYN but not ACK
	} else {
		false
	};
	Some((next_header, dst_port, is_syn))
}
