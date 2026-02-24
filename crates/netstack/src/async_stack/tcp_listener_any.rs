use std::{collections::HashSet, sync::Arc};

use smoltcp::{iface::SocketHandle, phy::Device, socket::tcp};
use tokio::sync::{Notify, watch};

use crate::error::Error;

/// A TCP listener that accepts connections on any JIT-bound port.
///
/// Instead of maintaining per-port `TcpListener` instances, this does a single
/// O(S) scan of the socket set per `poll_accept` call, returning any
/// ESTABLISHED socket on a tracked port that hasn't been returned before.
pub struct TcpListenerAny<D: Device + Send + 'static> {
	shared: Arc<super::Shared<D>>,
	/// Notify fired when JIT binds a new port.
	jit_notify: Arc<Notify>,
	/// Watch receiver for the JIT port set. Updated by the poll loop on every
	/// port registration and prune cycle; `borrow()` + `Arc::clone()` is a cheap
	/// read that requires no cloning of the underlying `HashSet`.
	ports: watch::Receiver<Arc<HashSet<u16>>>,
	/// Handles already returned by `poll_accept` — don't return again.
	seen: HashSet<SocketHandle>,
}

impl<D: Device + Send + 'static> TcpListenerAny<D> {
	pub(crate) fn new(
		shared: Arc<super::Shared<D>>,
		jit_notify: Arc<Notify>,
		ports: watch::Receiver<Arc<HashSet<u16>>>,
	) -> Self {
		Self {
			shared,
			jit_notify,
			ports,
			seen: HashSet::new(),
		}
	}

	/// Accept the next incoming TCP connection.
	///
	/// # Errors
	///
	/// Returns an error if polling for connections fails.
	pub async fn accept(&mut self) -> Result<super::tcp_stream::TcpStream<D>, Error> {
		loop {
			if let Some(stream) = self.poll_accept()? {
				return Ok(stream);
			}
			// Wait for either:
			// - jit_notify: a new port was JIT-bound
			// - shared.notify: a socket state changed (e.g. became ESTABLISHED)
			tokio::select! {
				() = self.jit_notify.notified() => {}
				() = self.shared.notify.notified() => {}
			}
		}
	}

	/// Poll for a new incoming TCP connection without blocking.
	///
	/// # Errors
	///
	/// Currently always returns `Ok`. Reserved for future error conditions.
	pub fn poll_accept(&mut self) -> Result<Option<super::tcp_stream::TcpStream<D>>, Error> {
		// Cheap Arc::clone — no HashSet allocation. The watch's internal RwLock
		// does not conflict with the poll loop's parking_lot inner lock, so there
		// is no lock-ordering hazard here (unlike the previous Mutex::clone approach).
		let ports = Arc::clone(&*self.ports.borrow());
		let inner = self.shared.inner.lock();

		// Prune seen set: remove handles no longer in the socket set.
		let active: HashSet<SocketHandle> = inner
			.sockets()
			.iter()
			.filter_map(|(h, s)| match s {
				smoltcp::socket::Socket::Tcp(tcp)
					if !matches!(tcp.state(), tcp::State::Closed | tcp::State::TimeWait) =>
				{
					Some(h)
				}
				_ => None,
			})
			.collect();
		self.seen.retain(|h| active.contains(h));

		// Single O(S) scan: find first established socket on a JIT-bound port.
		for (handle, socket) in inner.sockets().iter() {
			let smoltcp::socket::Socket::Tcp(tcp) = socket else {
				continue;
			};
			let port = tcp.listen_endpoint().port;
			if !ports.contains(&port) {
				continue;
			}
			if self.seen.contains(&handle) {
				continue;
			}
			if tcp.is_active() && tcp.may_send() {
				tracing::trace!(port, ?handle, "TcpListenerAny: accepted connection");
				self.seen.insert(handle);
				return Ok(Some(super::tcp_stream::TcpStream::new(
					Arc::clone(&self.shared),
					handle,
				)));
			}
		}
		Ok(None)
	}
}

#[cfg(test)]
mod tests {
	use std::time::Duration;

	use smoltcp::wire::{IpCidr, TcpSeqNumber};
	use tokio::time::Instant;

	use super::{
		super::{Netstack, test_helpers::*},
		TcpListenerAny,
	};
	use crate::{config::StackConfig, inner::device::VecDevice};

	fn make_stack_with_any_listener() -> (Netstack<VecDevice>, TcpListenerAny<VecDevice>) {
		let config = StackConfig {
			ip_addrs: vec![IpCidr::new(STACK_IP.into(), 24)],
			..test_config()
		};
		let device = VecDevice::new(1500);
		let mut stack = Netstack::new(device, config);
		let listener = stack.tcp_listen_any().expect("tcp_listen_any");
		(stack, listener)
	}

	#[tokio::test]
	async fn test_any_accept_single() {
		let (stack, mut listener) = make_stack_with_any_listener();

		complete_handshake(&stack, 8080).await;

		let stream = tokio::time::timeout(Duration::from_secs(2), listener.accept())
			.await
			.expect("timeout")
			.expect("accept");

		assert_eq!(stream.state(), smoltcp::socket::tcp::State::Established);
		let local = stream.local_endpoint().expect("local_endpoint");
		assert_eq!(local.port, 8080);
	}

	#[tokio::test]
	async fn test_any_accept_multiple_ports() {
		let (stack, mut listener) = make_stack_with_any_listener();

		let ports = [80, 443, 8080];
		for (i, &port) in ports.iter().enumerate() {
			let src_port = 20000 + u16::try_from(i).unwrap();
			complete_handshake_from(
				&stack,
				src_port,
				port,
				TcpSeqNumber(5000 + i32::try_from(i).unwrap() * 100),
			)
			.await;
		}

		let mut accepted_ports = Vec::new();
		let start = Instant::now();
		while accepted_ports.len() < 3 {
			if let Ok(Some(s)) = listener.poll_accept() {
				let local = s.local_endpoint().expect("local_endpoint");
				accepted_ports.push(local.port);
			}
			assert!(
				start.elapsed() <= Duration::from_secs(2),
				"Timeout: accepted {}/3 connections",
				accepted_ports.len()
			);
			tokio::task::yield_now().await;
		}

		accepted_ports.sort_unstable();
		assert_eq!(accepted_ports, vec![80, 443, 8080]);
	}

	/// Verify that `accept()` unblocks when a socket transitions to ESTABLISHED
	/// via `shared.notify`, not just when a new port is JIT-bound.
	#[tokio::test]
	async fn test_any_wakeup_on_shared_notify() {
		let (stack, mut listener) = make_stack_with_any_listener();

		// Inject SYN — JIT will create a LISTEN socket and fire jit_notify
		{
			let mut inner = stack.shared.inner.lock();
			inner.device_mut().inject(create_syn_packet(9090));
		}
		stack.wake();

		// Wait for the LISTEN socket to be created
		let start = Instant::now();
		loop {
			{
				let inner = stack.shared.inner.lock();
				if inner.socket_count() > 0 {
					break;
				}
			}
			assert!(
				start.elapsed() <= Duration::from_secs(1),
				"Timeout waiting for JIT socket"
			);
			tokio::task::yield_now().await;
		}

		// poll_accept should return None (socket is SYN_RECEIVED, not ESTABLISHED)
		assert!(
			listener.poll_accept().expect("poll_accept").is_none(),
			"should not accept until ESTABLISHED"
		);

		// Complete the handshake (ACK) — this triggers shared.notify
		let hs = complete_handshake(&stack, 9090).await;
		let _ = hs;

		// Now accept should succeed
		let stream = tokio::time::timeout(Duration::from_secs(2), listener.accept())
			.await
			.expect("timeout")
			.expect("accept");

		assert_eq!(stream.state(), smoltcp::socket::tcp::State::Established);
	}

	#[tokio::test]
	async fn test_any_wakeup_on_jit_notify() {
		let (stack, mut listener) = make_stack_with_any_listener();

		// Spawn accept — should block waiting for jit_notify
		let accept_handle = tokio::spawn(async move { listener.accept().await });

		tokio::time::sleep(Duration::from_millis(50)).await;
		assert!(!accept_handle.is_finished(), "accept should be blocking");

		// Complete a full handshake — JIT bind fires jit_notify
		complete_handshake(&stack, 7777).await;

		let result = tokio::time::timeout(Duration::from_secs(2), accept_handle)
			.await
			.expect("timeout")
			.expect("join")
			.expect("accept");

		assert_eq!(result.state(), smoltcp::socket::tcp::State::Established);
	}

	#[tokio::test]
	async fn test_any_seen_pruning() {
		let (stack, mut listener) = make_stack_with_any_listener();

		complete_handshake(&stack, 5555).await;

		// Accept first
		let start = Instant::now();
		let stream = loop {
			if let Ok(Some(s)) = listener.poll_accept() {
				break s;
			}
			assert!(
				start.elapsed() <= Duration::from_secs(2),
				"Timeout waiting for first accept"
			);
			tokio::task::yield_now().await;
		};

		drop(stream);

		// Prune
		{
			let mut inner = stack.shared.inner.lock();
			let now = inner.now();
			inner.poll(now);
			inner.prune_closed_tcp_sockets();
		}

		// New connection on same port
		complete_handshake_from(&stack, 40000, 5555, TcpSeqNumber(8000)).await;

		let start2 = Instant::now();
		let stream2 = loop {
			if let Ok(Some(s)) = listener.poll_accept() {
				break s;
			}
			assert!(
				start2.elapsed() <= Duration::from_secs(2),
				"Timeout waiting for second accept"
			);
			tokio::task::yield_now().await;
		};

		assert_eq!(stream2.local_endpoint().unwrap().port, 5555);
	}

	#[tokio::test]
	async fn test_any_no_deadlock_under_load() {
		let (stack, mut listener) = make_stack_with_any_listener();

		// Inject 50 SYNs rapidly on different ports
		for port in 1..=50u16 {
			let mut inner = stack.shared.inner.lock();
			inner.device_mut().inject(create_syn_packet(port));
		}
		stack.wake();

		// Do 50 rapid poll_accept calls — should not deadlock
		let start = Instant::now();
		let mut accepted = 0;
		for _ in 0..50 {
			if let Ok(Some(_)) = listener.poll_accept() {
				accepted += 1;
			}
			if start.elapsed() > Duration::from_secs(5) {
				break;
			}
			tokio::task::yield_now().await;
		}
		// We don't require all 50 to be accepted — just no deadlock
		assert!(
			start.elapsed() < Duration::from_secs(5),
			"possible deadlock"
		);
		let _ = accepted;
	}

	#[tokio::test]
	async fn test_any_poll_accept_none_initially() {
		let (_stack, mut listener) = make_stack_with_any_listener();
		let result = listener.poll_accept().expect("poll_accept");
		assert!(result.is_none());
	}

	#[tokio::test]
	async fn test_any_no_duplicate_handle() {
		let (stack, mut listener) = make_stack_with_any_listener();

		complete_handshake(&stack, 6666).await;

		// Wait for connection to be ready
		let start = Instant::now();
		let first = loop {
			if let Ok(Some(s)) = listener.poll_accept() {
				break s;
			}
			assert!(start.elapsed() <= Duration::from_secs(2), "Timeout");
			tokio::task::yield_now().await;
		};

		// Second poll_accept should return None (same handle was already seen)
		let second = listener.poll_accept().expect("poll_accept");
		assert!(second.is_none(), "should not return duplicate handle");

		let _ = first;
	}
}
