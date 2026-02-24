//! Tests for [`Netstack`] lifecycle and configuration, performance baselines,
//! and regression tests for previously identified correctness bugs.

use std::{sync::Arc, time::Duration};

use parking_lot::deadlock;
use smoltcp::wire::{
	IpCidr, IpProtocol, Ipv4Packet, Ipv6Address, Ipv6Packet, Ipv6Repr, TcpPacket, TcpSeqNumber,
};
use tokio::{io::AsyncWriteExt, time::Instant};

use super::{CacheEntry, Netstack, SynProxyState, test_helpers::*};
use crate::{
	config::StackConfig,
	inner::{InnerStack, device::VecDevice},
};

// ============================================================================
// Netstack — creation, lifecycle, and configuration
// ============================================================================

#[tokio::test]
async fn test_creation() {
	let config = StackConfig {
		ip_addrs: vec![IpCidr::new(STACK_IP.into(), 24)],
		..test_config()
	};
	let device = VecDevice::new(1500);
	let _stack = Netstack::new(device, config);
}

#[tokio::test]
async fn test_wake_triggers_poll() {
	let config = StackConfig {
		ip_addrs: vec![IpCidr::new(STACK_IP.into(), 24)],
		..test_config()
	};
	let device = VecDevice::new(1500);
	let mut stack = Netstack::new(device, config);
	stack.enable_tcp_listen_any();

	// Inject a SYN and wake — SYN-ACK should appear after poll processes it
	{
		let mut inner = stack.shared.inner.lock();
		inner.device_mut().inject(create_syn_packet(8080));
	}
	stack.wake();

	// Wait for SYN-ACK in egress
	let start = tokio::time::Instant::now();
	loop {
		{
			let mut inner = stack.shared.inner.lock();
			let egress = inner.device_mut().drain_egress();
			if !egress.is_empty() {
				break;
			}
		}
		assert!(
			start.elapsed() <= Duration::from_secs(2),
			"Timeout: wake did not trigger poll producing SYN-ACK"
		);
		tokio::task::yield_now().await;
	}
}

#[tokio::test]
async fn test_drop_aborts_poll() {
	let config = StackConfig {
		ip_addrs: vec![IpCidr::new(STACK_IP.into(), 24)],
		..test_config()
	};
	let device = VecDevice::new(1500);
	let stack = Netstack::new(device, config);

	drop(stack);

	// If we get here without hanging, the abort worked
	tokio::time::sleep(Duration::from_millis(10)).await;
}

#[tokio::test]
async fn test_tcp_listen_returns_listener() {
	let config = StackConfig {
		ip_addrs: vec![IpCidr::new(STACK_IP.into(), 24)],
		..test_config()
	};
	let device = VecDevice::new(1500);
	let stack = Netstack::new(device, config);
	let listener = stack.tcp_listen(8080, 128);
	assert_eq!(listener.port(), 8080);
}

#[tokio::test]
async fn test_tcp_listen_any_enables_jit() {
	let config = StackConfig {
		ip_addrs: vec![IpCidr::new(STACK_IP.into(), 24)],
		..test_config()
	};
	let device = VecDevice::new(1500);
	let mut stack = Netstack::new(device, config);
	let _listener = stack.tcp_listen_any().expect("tcp_listen_any");

	// Inject SYN to arbitrary port — JIT should bind it
	{
		let mut inner = stack.shared.inner.lock();
		inner.device_mut().inject(create_syn_packet(12345));
	}
	stack.wake();

	let start = tokio::time::Instant::now();
	loop {
		{
			let inner = stack.shared.inner.lock();
			if inner.socket_count() > 0 {
				break;
			}
		}
		assert!(
			start.elapsed() <= Duration::from_secs(2),
			"Timeout: JIT did not bind port 12345"
		);
		tokio::task::yield_now().await;
	}
}

#[tokio::test]
async fn test_udp_bind_any_enables_jit() {
	let config = StackConfig {
		ip_addrs: vec![IpCidr::new(STACK_IP.into(), 24)],
		..test_config()
	};
	let device = VecDevice::new(1500);
	let mut stack = Netstack::new(device, config);
	let _socket = stack.udp_bind_any().expect("udp_bind_any");

	{
		let mut inner = stack.shared.inner.lock();
		inner
			.device_mut()
			.inject(create_udp_packet(50000, 9999, b"test"));
	}
	stack.wake();

	let start = tokio::time::Instant::now();
	loop {
		{
			let inner = stack.shared.inner.lock();
			if inner.socket_count() > 0 {
				break;
			}
		}
		assert!(
			start.elapsed() <= Duration::from_secs(2),
			"Timeout: JIT did not bind UDP port 9999"
		);
		tokio::task::yield_now().await;
	}
}

#[tokio::test]
async fn test_syn_proxy_state() {
	let state = SynProxyState::new(false);
	assert!(!state.is_fast_mode());

	state.mark_probing(80);
	assert_eq!(state.get(80), Some(CacheEntry::Probing));

	state.mark_open(80);
	assert_eq!(state.get(80), Some(CacheEntry::Open));
	assert!(!state.is_closed(80));

	state.mark_closed(443);
	assert_eq!(state.get(443), Some(CacheEntry::Closed));
	assert!(state.is_closed(443));

	assert_eq!(state.get(9999), None);

	state.clear_cache();
	assert_eq!(state.get(80), None);
	assert_eq!(state.get(443), None);

	state.set_fast_mode(true);
	assert!(state.is_fast_mode());
}

// ============================================================================
// Performance baselines
//
// Thresholds are intentionally 10–20× slack from observed values on a
// developer machine (debug build) to tolerate CI runner variance.
// Tighten when the implementation gets measurably faster; loosen only with
// written justification in the commit message.
// ============================================================================

/// Minimum acceptable write throughput through a virtual TCP socket (MB/s).
///
/// Observed on a developer machine (debug build): 600–850 MB/s.
/// Assumes CI runners may be up to 5× slower → ~120 MB/s worst case.
const THROUGHPUT_FLOOR_MB_S: f64 = 100.0;

/// Maximum acceptable per-call latency for `poll_accept` (µs).
///
/// Observed (debug build): 6–138 µs across 10/50/100-port scales.
/// Assumes CI runners may be up to 5× slower → ~690 µs worst case.
const POLL_ACCEPT_CEIL_US: u128 = 1_000;

/// Maximum acceptable worst-case average lock-acquisition wait (µs).
///
/// Observed (debug build, 64 tasks): 0–1 µs.
/// Ceiling is set at 50 µs to remain meaningful on slow CI while still
/// catching any accidental O(n) work introduced inside the lock.
const LOCK_CONTENTION_CEIL_US: u64 = 50;

/// Spawn a background OS thread that polls `parking_lot::deadlock` every
/// 200 ms. If a lock cycle is detected, prints diagnostics and aborts.
///
/// Requires `parking_lot` compiled with the `deadlock_detection` feature
/// (enabled via `[dev-dependencies]` in `Cargo.toml`).
fn start_deadlock_watcher() {
	std::thread::Builder::new()
		.name("deadlock-watcher".into())
		.spawn(|| {
			loop {
				std::thread::sleep(Duration::from_millis(200));
				let cycles = deadlock::check_deadlock();
				if cycles.is_empty() {
					continue;
				}
				eprintln!("DEADLOCK DETECTED — {} cycle(s):", cycles.len());
				for (i, threads) in cycles.iter().enumerate() {
					eprintln!("  Cycle #{i}:");
					for t in threads {
						eprintln!("    thread_id={:?}", t.thread_id());
						eprintln!("    backtrace={:?}", t.backtrace());
					}
				}
				std::process::abort();
			}
		})
		.expect("deadlock watcher thread spawn failed");
}

#[tokio::test]
async fn test_smoke_jit_handshake() {
	let config = StackConfig {
		ip_addrs: vec![IpCidr::new(STACK_IP.into(), 24)],
		..StackConfig::default()
	};

	let device = VecDevice::new(1500);
	let mut stack = Netstack::new(device, config);
	let mut listener = stack.tcp_listen_any().expect("failed to create listener");

	let port = 8080;
	{
		let mut inner = stack.shared.inner.lock();
		inner.device_mut().inject(create_syn_packet(port));
	}
	stack.wake();

	// Wait for SYN-ACK
	let start = Instant::now();
	let server_seq = loop {
		let egress = {
			let mut inner = stack.shared.inner.lock();
			inner.device_mut().drain_egress()
		};
		if let Some(pkt) = egress.first() {
			let ip_pkt = Ipv4Packet::new_checked(pkt).unwrap();
			let tcp_pkt = TcpPacket::new_checked(ip_pkt.payload()).unwrap();
			if tcp_pkt.syn() && tcp_pkt.ack() {
				break tcp_pkt.seq_number();
			}
		}
		assert!(
			start.elapsed() <= Duration::from_secs(1),
			"Timeout waiting for SYN-ACK"
		);
		tokio::task::yield_now().await;
	};

	// Send ACK
	{
		let mut inner = stack.shared.inner.lock();
		inner.device_mut().inject(create_ack_packet(
			port,
			TcpSeqNumber(CLIENT_ISN + 1),
			server_seq + 1,
		));
	}
	stack.wake();

	// Accept connection
	let mut stream = loop {
		if let Ok(Some(s)) = listener.poll_accept() {
			break s;
		}
		assert!(
			start.elapsed() <= Duration::from_secs(2),
			"Timeout waiting for accept"
		);
		tokio::task::yield_now().await;
	};

	// Send data
	stream.write_all(b"hello").await.expect("failed to write");

	// Drive the stack one last time
	{
		let mut inner = stack.shared.inner.lock();
		let now = inner.now();
		inner.poll(now);
		let egress = inner.device_mut().drain_egress();
		assert!(!egress.is_empty(), "expected data packet in egress");
	}
}

/// Verify that `poll_accept` stays under [`POLL_ACCEPT_CEIL_US`] per call as
/// the number of JIT-bound ports grows. Guards against O(n) regressions in
/// the hot path.
#[tokio::test]
async fn test_jit_any_performance_scaling() {
	let config = StackConfig {
		ip_addrs: vec![IpCidr::new(STACK_IP.into(), 24)],
		..StackConfig::default()
	};

	let scales = [10, 50, 100];

	for &num_ports in &scales {
		let device = VecDevice::new(1500);
		let mut stack = Netstack::new(device, config.clone());
		let mut listener = stack.tcp_listen_any().expect("failed to create listener");

		for port in 1..=num_ports {
			let pkt = create_syn_packet(port);
			{
				let mut inner = stack.shared.inner.lock();
				inner.device_mut().inject(pkt);
			}
			stack.wake();
		}

		let start_wait = Instant::now();
		loop {
			{
				let inner = stack.shared.inner.lock();
				if inner.socket_count() >= num_ports as usize {
					break;
				}
			}
			assert!(
				start_wait.elapsed().as_secs() <= 5,
				"Timed out waiting for JIT listeners"
			);
			tokio::task::yield_now().await;
		}

		let start = Instant::now();
		let iterations = 100u32;
		for _ in 0..iterations {
			let _ = listener.poll_accept().unwrap();
		}
		let elapsed = start.elapsed() / iterations;

		println!(
			"Scale: {num_ports} ports, {} total sockets, poll_accept avg: {elapsed:?}",
			stack.shared.inner.lock().socket_count(),
		);

		assert!(
			elapsed.as_micros() < POLL_ACCEPT_CEIL_US,
			"poll_accept too slow at {num_ports} ports: {elapsed:?} per call \
             exceeds {POLL_ACCEPT_CEIL_US} µs ceiling — possible O(n) regression"
		);
	}
}

/// Establish a virtual TCP connection and write 1 MB through it, asserting
/// that throughput stays above [`THROUGHPUT_FLOOR_MB_S`].
#[tokio::test]
async fn test_throughput_baseline() {
	let config = StackConfig {
		ip_addrs: vec![IpCidr::new(STACK_IP.into(), 24)],
		tcp_rx_buffer_size: 1024 * 1024,
		tcp_tx_buffer_size: 1024 * 1024,
		..StackConfig::default()
	};

	let device = VecDevice::new(1500);
	let mut stack = Netstack::new(device, config);
	let mut listener = stack.tcp_listen_any().unwrap();

	let port = 8080;
	{
		let mut inner = stack.shared.inner.lock();
		inner.device_mut().inject(create_syn_packet(port));
	}
	stack.wake();

	let start_hs = Instant::now();
	let server_seq = loop {
		let egress = {
			let mut inner = stack.shared.inner.lock();
			inner.device_mut().drain_egress()
		};
		if let Some(pkt) = egress.first() {
			let ip_pkt = Ipv4Packet::new_checked(pkt).unwrap();
			let tcp_pkt = TcpPacket::new_checked(ip_pkt.payload()).unwrap();
			if tcp_pkt.syn() && tcp_pkt.ack() {
				break tcp_pkt.seq_number();
			}
		}
		assert!(
			start_hs.elapsed() <= Duration::from_secs(2),
			"Handshake timeout (SYN-ACK)"
		);
		tokio::task::yield_now().await;
	};

	{
		let mut inner = stack.shared.inner.lock();
		inner.device_mut().inject(create_ack_packet(
			port,
			TcpSeqNumber(CLIENT_ISN + 1),
			server_seq + 1,
		));
	}
	stack.wake();

	let mut stream = loop {
		if let Ok(Some(s)) = listener.poll_accept() {
			break s;
		}
		assert!(
			start_hs.elapsed() <= Duration::from_secs(5),
			"Handshake timeout (accept)"
		);
		tokio::task::yield_now().await;
	};

	let data_size = 1024 * 1024; // 1 MB
	let chunk_size = 128 * 1024;
	let chunk = vec![0u8; chunk_size];
	let start_io = Instant::now();

	let mut written = 0;
	while written < data_size {
		stream.write_all(&chunk).await.expect("write failed");
		written += chunk_size;

		let mut inner = stack.shared.inner.lock();
		inner.device_mut().drain_egress();
		let now = inner.now();
		inner.poll(now);
	}

	let elapsed = start_io.elapsed();
	#[allow(clippy::cast_precision_loss)]
	let mb_ps = (data_size as f64 / 1024.0 / 1024.0) / elapsed.as_secs_f64();
	println!("Throughput baseline: {mb_ps:.2} MB/s ({elapsed:?} for 1 MB)");

	assert!(
		mb_ps >= THROUGHPUT_FLOOR_MB_S,
		"Throughput {mb_ps:.2} MB/s is below the {THROUGHPUT_FLOOR_MB_S} MB/s floor — \
         possible performance regression"
	);
}

/// Synthetic mutex pressure test.
///
/// Spawns [`N_TASKS`] Tokio tasks — significantly more than the worker thread
/// count — each executing [`ITERS`] tight lock-acquire/work/release cycles
/// against the shared inner stack mutex. Measures per-acquisition wait time
/// as a proxy for lock contention and asserts it stays within
/// [`LOCK_CONTENTION_CEIL_US`].
///
/// A [`start_deadlock_watcher`] thread is active for the test duration. If any
/// circular lock dependency forms, the process is aborted with diagnostics.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_lock_contention_pressure() {
	const N_TASKS: usize = 64; // >> worker_threads; forces real contention
	const ITERS: usize = 500;

	start_deadlock_watcher();

	let config = StackConfig {
		ip_addrs: vec![IpCidr::new(STACK_IP.into(), 24)],
		..test_config()
	};
	let device = VecDevice::new(1500);
	let mut stack = Netstack::new(device, config);
	let _listener = stack.tcp_listen_any().expect("tcp_listen_any");

	// Inject a handful of SYNs so the stack has non-trivial socket state.
	// We use sleep rather than a yield/drain spin so the poll loop gets
	// guaranteed CPU time in multi-thread mode without racing on the inner lock.
	for port in 8080..8085u16 {
		let mut inner = stack.shared.inner.lock();
		inner.device_mut().inject(create_syn_packet(port));
	}
	stack.wake();
	tokio::time::sleep(Duration::from_millis(100)).await;

	let shared = Arc::clone(&stack.shared);

	let handles: Vec<_> = (0..N_TASKS)
		.map(|_| {
			let shared = Arc::clone(&shared);
			tokio::spawn(async move {
				let mut total_wait_ns: u64 = 0;
				for _ in 0..ITERS {
					// Guard dropped inside the block so the future stays Send.
					let acq_ns = {
						let t0 = std::time::Instant::now();
						let guard = shared.inner.lock();
						let ns = u64::try_from(t0.elapsed().as_nanos()).unwrap_or(u64::MAX);
						let _ = guard.socket_count();
						ns
					};
					total_wait_ns = total_wait_ns.saturating_add(acq_ns);
					tokio::task::yield_now().await;
				}
				total_wait_ns / ITERS as u64
			})
		})
		.collect();

	let mut max_avg_wait_nanos: u64 = 0;
	for handle in handles {
		let avg_ns = handle.await.expect("pressure task panicked");
		max_avg_wait_nanos = max_avg_wait_nanos.max(avg_ns);
	}

	let max_avg_wait_micros = max_avg_wait_nanos / 1_000;
	println!(
		"Lock contention ({N_TASKS} tasks × {ITERS} iters, 4 workers): \
		 worst-case avg acquisition wait = {max_avg_wait_micros} µs"
	);

	assert!(
		max_avg_wait_micros < LOCK_CONTENTION_CEIL_US,
		"Lock contention too high: worst-case avg wait {max_avg_wait_micros} µs exceeds \
		 {LOCK_CONTENTION_CEIL_US} µs ceiling — possible performance regression or \
		 unexpected lock hold-time growth"
	);
}

// ============================================================================
// Regression tests for previously identified correctness bugs
// ============================================================================

/// `max_sockets` must be enforced.
///
/// When `StackConfig::max_sockets` is set to a small value, JIT binding must
/// stop creating new sockets once the limit is reached. A SYN flood of 500
/// packets must not produce more than `max_sockets` sockets.
#[tokio::test]
async fn test_max_sockets_enforced() {
	const LIMIT: usize = 10;

	let config = StackConfig {
		ip_addrs: vec![IpCidr::new(STACK_IP.into(), 24)],
		max_sockets: LIMIT,
		..test_config()
	};
	let device = VecDevice::new(1500);
	let mut stack = Netstack::new(device, config);
	let _listener = stack.tcp_listen_any().expect("tcp_listen_any");

	for port in 1u16..=500 {
		let mut inner = stack.shared.inner.lock();
		inner.device_mut().inject(create_syn_packet_from(
			50000 + (port % 10000),
			port,
			TcpSeqNumber(1000 + i32::from(port)),
		));
	}
	stack.wake();

	tokio::time::sleep(Duration::from_millis(100)).await;

	let final_count = stack.shared.inner.lock().socket_count();
	assert!(
		final_count <= LIMIT,
		"Socket count {final_count} exceeded max_sockets limit {LIMIT} — \
		 enforce the limit in jit_bind_port"
	);
}

/// Idle LISTEN sockets must be pruned.
///
/// SYNs that never complete the handshake (e.g. from a port scan) leave
/// sockets in the LISTEN or `SYN_RECEIVED` state. These must be removed by
/// the pruning mechanism so the socket set doesn't grow indefinitely.
#[tokio::test]
async fn test_idle_listen_sockets_pruned() {
	let config = StackConfig {
		ip_addrs: vec![IpCidr::new(STACK_IP.into(), 24)],
		..test_config()
	};
	let device = VecDevice::new(1500);
	let mut stack = Netstack::new(device, config);
	let _listener = stack.tcp_listen_any().expect("tcp_listen_any");

	let count = 50u16;
	for port in 1..=count {
		let mut inner = stack.shared.inner.lock();
		inner.device_mut().inject(create_syn_packet(port));
	}
	stack.wake();

	let start = tokio::time::Instant::now();
	loop {
		{
			let inner = stack.shared.inner.lock();
			if inner.socket_count() >= count as usize {
				break;
			}
		}
		assert!(
			start.elapsed() <= Duration::from_secs(5),
			"Timeout waiting for sockets"
		);
		tokio::task::yield_now().await;
	}

	{
		let mut inner = stack.shared.inner.lock();
		inner.prune_stale_syn_received(Duration::ZERO);
	}

	let remaining = stack.shared.inner.lock().socket_count();
	assert!(
		remaining == 0,
		"Expected 0 sockets after pruning idle listeners, got {remaining} — \
		 implement TTL/LRU pruning for LISTEN sockets"
	);
}

/// The `tcp_ports` set must shrink when idle ports are pruned.
///
/// Ports added via JIT binding must be evictable once their sockets are idle
/// and have been pruned. The port set growing without bound causes O(S) scans
/// in `TcpListenerAny` to slow indefinitely.
#[tokio::test]
async fn test_idle_ports_pruned_from_set() {
	let config = StackConfig {
		ip_addrs: vec![IpCidr::new(STACK_IP.into(), 24)],
		..test_config()
	};
	let device = VecDevice::new(1500);
	let mut stack = Netstack::new(device, config);
	let _listener = stack.tcp_listen_any().expect("tcp_listen_any");

	let count = 100u16;
	for port in 1..=count {
		let mut inner = stack.shared.inner.lock();
		inner.device_mut().inject(create_syn_packet(port));
	}
	stack.wake();

	let start = tokio::time::Instant::now();
	loop {
		if stack.tcp_ports.lock().len() >= count as usize {
			break;
		}
		assert!(
			start.elapsed() <= Duration::from_secs(5),
			"Timeout waiting for ports"
		);
		tokio::task::yield_now().await;
	}

	{
		let mut inner = stack.shared.inner.lock();
		inner.prune_stale_syn_received(Duration::ZERO);
	}

	let port_count = stack.tcp_ports.lock().len();
	assert!(
		port_count == 0,
		"Expected tcp_ports to be empty after pruning idle listeners, \
		 got {port_count} — implement port set eviction on prune"
	);
}

/// IPv6 extension headers must be traversed by the JIT parser.
///
/// A TCP SYN inside an IPv6 packet that uses a Hop-by-Hop Options extension
/// header must still be JIT-bound. The parser must walk the extension header
/// chain to find the real L4 payload.
#[tokio::test]
async fn test_ipv6_extension_header_traversed() {
	let config = StackConfig {
		ip_addrs: vec![IpCidr::new(
			smoltcp::wire::IpAddress::Ipv6(Ipv6Address::new(0xfd00, 0, 0, 0, 0, 0, 0, 1)),
			64,
		)],
		any_ip: true,
		..test_config()
	};
	let device = VecDevice::new(1500);
	let mut stack = Netstack::new(device, config);
	let _listener = stack.tcp_listen_any().expect("tcp_listen_any");

	let src = Ipv6Address::new(0xfd00, 0, 0, 0, 0, 0, 0, 2);
	let dst = Ipv6Address::new(0xfd00, 0, 0, 0, 0, 0, 0, 1);
	let dst_port = 7777u16;

	let tcp_repr = smoltcp::wire::TcpRepr {
		src_port: 12345,
		dst_port,
		control: smoltcp::wire::TcpControl::Syn,
		seq_number: TcpSeqNumber(1000),
		ack_number: None,
		window_len: 65535,
		window_scale: None,
		max_seg_size: None,
		sack_permitted: false,
		sack_ranges: [None; 3],
		payload: &[],
		timestamp: None,
	};
	let tcp_len = tcp_repr.header_len();

	// Hop-by-Hop Options header (8 bytes minimum):
	//   next_header: 6 (TCP)
	//   hdr_ext_len: 0 (= 8 bytes total)
	//   padding: 6 bytes of PadN option
	let hop_by_hop = [
		6, // next_header = TCP
		0, // hdr_ext_len = 0 (8 bytes)
		1, 4, // PadN option: type=1, len=4
		0, 0, 0, 0, // padding
	];

	let ipv6_repr = Ipv6Repr {
		src_addr: src,
		dst_addr: dst,
		next_header: IpProtocol::HopByHop,
		payload_len: hop_by_hop.len() + tcp_len,
		hop_limit: 64,
	};

	let total_len = ipv6_repr.buffer_len() + hop_by_hop.len() + tcp_len;
	let mut packet_buf = vec![0u8; total_len];

	let mut ipv6_pkt = Ipv6Packet::new_unchecked(&mut packet_buf);
	ipv6_repr.emit(&mut ipv6_pkt);

	let ipv6_hdr_len = 40;
	packet_buf[ipv6_hdr_len..ipv6_hdr_len + hop_by_hop.len()].copy_from_slice(&hop_by_hop);

	let tcp_offset = ipv6_hdr_len + hop_by_hop.len();
	let mut tcp_pkt = smoltcp::wire::TcpPacket::new_unchecked(&mut packet_buf[tcp_offset..]);
	tcp_repr.emit(
		&mut tcp_pkt,
		&src.into(),
		&dst.into(),
		&smoltcp::phy::ChecksumCapabilities::default(),
	);

	{
		let mut inner = stack.shared.inner.lock();
		inner.device_mut().inject(packet_buf);
	}
	stack.wake();

	tokio::time::sleep(Duration::from_millis(100)).await;

	let port_bound = stack.tcp_ports.lock().contains(&dst_port);
	assert!(
		port_bound,
		"Port {dst_port} was NOT JIT-bound despite valid TCP SYN behind IPv6 \
		 Hop-by-Hop extension header — refactor parse_ipv6_l4 to traverse the \
		 extension header chain"
	);
}

/// Control: standard IPv6+TCP SYN (no extension headers) must still JIT-bind.
/// Ensures the extension-header traversal doesn't break the common path.
#[tokio::test]
async fn test_ipv6_basic_still_works() {
	let config = StackConfig {
		ip_addrs: vec![IpCidr::new(
			smoltcp::wire::IpAddress::Ipv6(Ipv6Address::new(0xfd00, 0, 0, 0, 0, 0, 0, 1)),
			64,
		)],
		any_ip: true,
		..test_config()
	};
	let device = VecDevice::new(1500);
	let mut stack = Netstack::new(device, config);
	let _listener = stack.tcp_listen_any().expect("tcp_listen_any");

	let src = Ipv6Address::new(0xfd00, 0, 0, 0, 0, 0, 0, 2);
	let dst = Ipv6Address::new(0xfd00, 0, 0, 0, 0, 0, 0, 1);
	let dst_port = 8888u16;

	let tcp_repr = smoltcp::wire::TcpRepr {
		src_port: 12345,
		dst_port,
		control: smoltcp::wire::TcpControl::Syn,
		seq_number: TcpSeqNumber(2000),
		ack_number: None,
		window_len: 65535,
		window_scale: None,
		max_seg_size: None,
		sack_permitted: false,
		sack_ranges: [None; 3],
		payload: &[],
		timestamp: None,
	};
	let tcp_len = tcp_repr.header_len();

	let ipv6_repr = Ipv6Repr {
		src_addr: src,
		dst_addr: dst,
		next_header: IpProtocol::Tcp,
		payload_len: tcp_len,
		hop_limit: 64,
	};

	let total_len = ipv6_repr.buffer_len() + tcp_len;
	let mut packet_buf = vec![0u8; total_len];

	let mut ipv6_pkt = Ipv6Packet::new_unchecked(&mut packet_buf);
	ipv6_repr.emit(&mut ipv6_pkt);

	let mut tcp_pkt = smoltcp::wire::TcpPacket::new_unchecked(&mut packet_buf[40..]);
	tcp_repr.emit(
		&mut tcp_pkt,
		&src.into(),
		&dst.into(),
		&smoltcp::phy::ChecksumCapabilities::default(),
	);

	{
		let mut inner = stack.shared.inner.lock();
		inner.device_mut().inject(packet_buf);
	}
	stack.wake();

	let start = tokio::time::Instant::now();
	loop {
		if stack.tcp_ports.lock().contains(&dst_port) {
			break;
		}
		assert!(
			start.elapsed() <= Duration::from_secs(2),
			"Timeout: standard IPv6 TCP SYN did not JIT-bind port {dst_port}"
		);
		tokio::task::yield_now().await;
	}
}

/// `SynReceived` sockets must NOT be pruned by `prune_closed_tcp_sockets`.
///
/// Regression: prune running while a handshake is in progress deleted the
/// socket, causing the stack to respond with RST to the final ACK.
/// The fix is that `prune_closed_tcp_sockets` must ignore `SynReceived`
/// sockets; use `prune_stale_syn_received` with a TTL for those.
#[test]
fn test_prune_syn_received_disrupts_valid_handshake() {
	let config = StackConfig {
		ip_addrs: vec![IpCidr::new(STACK_IP.into(), 24)],
		..test_config()
	};
	let device = VecDevice::new(1500);
	let mut stack = InnerStack::new(device, config);
	stack.tcp_listen(80).expect("listen");

	// Step 1: SYN in, poll → SYN-ACK out, socket is now SynReceived
	stack.device_mut().inject(create_syn_packet(80));
	let now = stack.now();
	stack.poll(now);

	let egress = stack.device_mut().drain_egress();
	let server_isn = egress
		.iter()
		.find_map(|p| {
			let ip = Ipv4Packet::new_checked(p.as_slice()).ok()?;
			let tcp = TcpPacket::new_checked(ip.payload()).ok()?;
			(tcp.syn() && tcp.ack()).then(|| tcp.seq_number())
		})
		.expect("expected SYN-ACK in egress");

	// Step 2: Prune while the socket is still SynReceived
	stack.prune_closed_tcp_sockets();

	// Step 3: Client sends the final ACK to complete the handshake
	stack.device_mut().inject(create_ack_packet(
		80,
		TcpSeqNumber(CLIENT_ISN) + 1,
		server_isn + 1,
	));
	let now2 = stack.now();
	stack.poll(now2);

	// If the socket was pruned the stack sends RST
	let post_ack_egress = stack.device_mut().drain_egress();
	let got_rst = post_ack_egress.iter().any(|p| {
		if let Ok(ip) = Ipv4Packet::new_checked(p.as_slice())
			&& let Ok(tcp) = TcpPacket::new_checked(ip.payload())
		{
			return tcp.rst();
		}
		false
	});
	assert!(
		!got_rst,
		"stack sent RST in response to the final handshake ACK — \
		 prune_closed_tcp_sockets() must not remove SynReceived sockets; \
		 use prune_stale_syn_received() with a TTL instead"
	);
}

/// `now()` must use a monotonic clock, not wall time.
///
/// A freshly created `InnerStack` should report an elapsed time of roughly
/// zero milliseconds — not ~1.7 trillion milliseconds (Unix epoch).
#[test]
fn test_now_uses_monotonic_clock() {
	let config = StackConfig {
		ip_addrs: vec![IpCidr::new(STACK_IP.into(), 24)],
		..test_config()
	};
	let device = VecDevice::new(1500);
	let stack = InnerStack::new(device, config);
	let t = stack.now();
	assert!(
		t.total_millis() < 60_000,
		"now() returned {}ms, which looks like a Unix wall-clock timestamp — \
		 replace SystemTime with std::time::Instant in InnerStack::now()",
		t.total_millis()
	);
}
