//! Tests for socket accumulation and cleanup.
//!
//! These tests verify that sockets are properly cleaned up after use
//! and don't accumulate over time.

use smoltcp::{
	socket::tcp,
	time::Instant,
	wire::{IpCidr, Ipv4Address},
};
use wallhack_netstack::{
	config::StackConfig,
	inner::{InnerStack, device::VecDevice},
};

const STACK_IPV4: Ipv4Address = Ipv4Address::new(10, 0, 0, 1);

fn test_config() -> StackConfig {
	StackConfig {
		ip_addrs: vec![IpCidr::new(STACK_IPV4.into(), 24)],
		random_seed: 0xdead_beef,
		..StackConfig::default()
	}
}

fn now() -> Instant {
	Instant::from_millis(0)
}

#[test]
fn test_socket_count_after_listen() {
	let device = VecDevice::new(1500);
	let mut stack = InnerStack::new(device, test_config());

	assert_eq!(stack.socket_count(), 0, "Should start with no sockets");

	stack.tcp_listen(8080).expect("listen failed");
	assert_eq!(stack.socket_count(), 1, "Should have 1 socket after listen");

	stack.tcp_listen(8081).expect("listen failed");
	assert_eq!(
		stack.socket_count(),
		2,
		"Should have 2 sockets after second listen"
	);
}

#[test]
fn test_ensure_tcp_listener_idempotent() {
	let device = VecDevice::new(1500);
	let mut stack = InnerStack::new(device, test_config());

	stack.ensure_tcp_listener(8080).expect("ensure failed");
	assert_eq!(stack.socket_count(), 1);

	// Calling again should not create another socket
	stack.ensure_tcp_listener(8080).expect("ensure failed");
	assert_eq!(
		stack.socket_count(),
		1,
		"ensure_tcp_listener should be idempotent"
	);
}

#[test]
fn test_tcp_find_or_listen_skips_closed_sockets() {
	let device = VecDevice::new(1500);
	let mut stack = InnerStack::new(device, test_config());

	// Create a listener
	let handle1 = stack.tcp_find_or_listen(8080).expect("listen failed");
	assert_eq!(stack.socket_count(), 1);

	// Simulate the socket becoming closed (abort it)
	{
		let socket: &mut tcp::Socket<'_> = stack.sockets_mut().get_mut(handle1);
		socket.abort();
	}
	stack.poll(now());

	// Verify the socket is now closed
	{
		let socket: &tcp::Socket<'_> = stack.sockets().get(handle1);
		assert_eq!(
			socket.state(),
			tcp::State::Closed,
			"Socket should be closed after abort"
		);
	}

	// tcp_find_or_listen should create a NEW socket, not reuse the closed one
	let handle2 = stack.tcp_find_or_listen(8080).expect("listen failed");
	assert_ne!(
		handle1, handle2,
		"Should get a new handle, not reuse closed socket"
	);
	assert_eq!(
		stack.socket_count(),
		2,
		"Should have 2 sockets (old closed + new)"
	);

	// New socket should be in Listen state
	{
		let socket: &tcp::Socket<'_> = stack.sockets().get(handle2);
		assert_eq!(
			socket.state(),
			tcp::State::Listen,
			"New socket should be listening"
		);
	}
}

#[test]
fn test_prune_removes_closed_sockets() {
	let device = VecDevice::new(1500);
	let mut stack = InnerStack::new(device, test_config());

	// Create multiple listeners
	let handle1 = stack.tcp_listen(8080).expect("listen failed");
	let handle2 = stack.tcp_listen(8081).expect("listen failed");
	let _handle3 = stack.tcp_listen(8082).expect("listen failed");
	assert_eq!(stack.socket_count(), 3);

	// Close some of them
	{
		let socket: &mut tcp::Socket<'_> = stack.sockets_mut().get_mut(handle1);
		socket.abort();
	}
	{
		let socket: &mut tcp::Socket<'_> = stack.sockets_mut().get_mut(handle2);
		socket.abort();
	}
	stack.poll(now());

	// Should still have 3 sockets (closed ones not yet pruned)
	assert_eq!(stack.socket_count(), 3);

	// Now prune
	let pruned = stack.prune_closed_tcp_sockets();
	assert_eq!(pruned, 2, "Should have pruned 2 closed sockets");
	assert_eq!(stack.socket_count(), 1, "Should have 1 socket remaining");
}

#[test]
fn test_multiple_sequential_connections_same_port() {
	let device = VecDevice::new(1500);
	let mut stack = InnerStack::new(device, test_config());

	// Simulate multiple sequential connections to the same port
	// This mimics what happens with iperf3's control connection
	for i in 0..10 {
		let handle = stack.tcp_find_or_listen(5201).expect("listen failed");

		// Verify socket is listening
		{
			let socket: &tcp::Socket<'_> = stack.sockets().get(handle);
			assert_eq!(
				socket.state(),
				tcp::State::Listen,
				"Iteration {i}: should be listening"
			);
		}

		// Simulate connection then close
		{
			let socket: &mut tcp::Socket<'_> = stack.sockets_mut().get_mut(handle);
			socket.abort();
		}
		stack.poll(now());

		// Prune closed sockets
		stack.prune_closed_tcp_sockets();
	}

	// After all iterations, should have minimal sockets
	// (at most 1 from the last iteration if it wasn't pruned)
	assert!(
		stack.socket_count() <= 1,
		"Should not accumulate sockets: got {}",
		stack.socket_count()
	);
}

#[test]
fn test_socket_accumulation_without_pruning() {
	let device = VecDevice::new(1500);
	let mut stack = InnerStack::new(device, test_config());

	// Create and close many sockets WITHOUT pruning
	for _ in 0..100 {
		let handle = stack.tcp_find_or_listen(5201).expect("listen failed");

		// Close it
		{
			let socket: &mut tcp::Socket<'_> = stack.sockets_mut().get_mut(handle);
			socket.abort();
		}
		stack.poll(now());
		// Note: NOT calling prune_closed_tcp_sockets()
	}

	// Without pruning, we'll accumulate closed sockets
	// The exact count depends on whether tcp_find_or_listen creates new ones
	println!("Socket count without pruning: {}", stack.socket_count());

	// Now prune and verify cleanup
	let pruned = stack.prune_closed_tcp_sockets();
	println!("Pruned {pruned} sockets");

	// After pruning, should have 0 sockets (all were closed)
	assert_eq!(stack.socket_count(), 0, "All sockets should be pruned");
}

#[test]
fn test_high_volume_connections() {
	// Simulate what happens during high bandwidth iperf tests
	// Many connections over a short period
	let device = VecDevice::new(1500);
	let mut stack = InnerStack::new(device, test_config());

	let mut poll_count = 0;

	for i in 0..1000 {
		// Create connection
		let handle = stack.tcp_find_or_listen(5201).expect("listen failed");

		// Simulate short-lived connection
		{
			let socket: &mut tcp::Socket<'_> = stack.sockets_mut().get_mut(handle);
			socket.abort();
		}
		stack.poll(now());

		// Prune every 100 polls (matching the async code)
		poll_count += 1;
		if poll_count % 100 == 0 {
			stack.prune_closed_tcp_sockets();
		}

		// Check we're not accumulating too many
		assert!(
			stack.socket_count() <= 200,
			"Socket accumulation at iteration {}: {} sockets",
			i,
			stack.socket_count()
		);
	}

	// Final cleanup
	stack.prune_closed_tcp_sockets();
	println!(
		"Final socket count after 1000 connections: {}",
		stack.socket_count()
	);
	assert!(
		stack.socket_count() < 10,
		"Should have minimal sockets after cleanup"
	);
}
