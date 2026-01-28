pub mod device;

use smoltcp::{
	iface::{Config, Interface, SocketHandle, SocketSet, SocketStorage},
	phy::Device,
	socket::tcp,
	time::Instant,
	wire::HardwareAddress,
};

use crate::{config::StackConfig, error::Error};

/// Synchronous, poll-based TCP/IP stack wrapping smoltcp.
///
/// `InnerStack` is generic over any smoltcp [`Device`] and owns the
/// [`Interface`], [`SocketSet`], and device. It contains no async runtime
/// dependencies and is designed for deterministic testing (e.g. pcap replay).
///
/// # Examples
///
/// ```
/// use netstack::inner::InnerStack;
/// use netstack::inner::device::VecDevice;
/// use netstack::config::StackConfig;
/// use smoltcp::wire::{IpCidr, Ipv4Address};
///
/// let config = StackConfig {
///     ip_addrs: vec![IpCidr::new(Ipv4Address::new(10, 0, 0, 1).into(), 24)],
///     ..StackConfig::default()
/// };
/// let device = VecDevice::new(1500);
/// let stack = InnerStack::new(device, config);
/// ```
pub struct InnerStack<D: Device> {
	device: D,
	iface: Interface,
	sockets: SocketSet<'static>,
	tcp_rx_buffer_size: usize,
	tcp_tx_buffer_size: usize,
}

impl<D: Device> InnerStack<D> {
	/// Create a new `InnerStack` from a device and configuration.
	///
	/// # Panics
	///
	/// Panics if the device medium does not match [`HardwareAddress::Ip`]
	/// (the stack is designed for L3 / TUN devices only).
	///
	/// # Examples
	///
	/// ```
	/// use netstack::inner::InnerStack;
	/// use netstack::inner::device::VecDevice;
	/// use netstack::config::StackConfig;
	/// use smoltcp::wire::{IpCidr, Ipv4Address};
	///
	/// let config = StackConfig {
	///     ip_addrs: vec![IpCidr::new(Ipv4Address::new(10, 0, 0, 1).into(), 24)],
	///     ..StackConfig::default()
	/// };
	/// let device = VecDevice::new(1500);
	/// let stack = InnerStack::new(device, config);
	/// ```
	#[allow(clippy::needless_pass_by_value)] // constructor consumes config intentionally
	pub fn new(mut device: D, config: StackConfig) -> Self {
		let mut iface_config = Config::new(HardwareAddress::Ip);
		iface_config.random_seed = config.random_seed;

		let now = Instant::from_millis(0);
		let mut iface = Interface::new(iface_config, &mut device, now);

		iface.update_ip_addrs(|addrs| {
			for cidr in &config.ip_addrs {
				addrs
					.push(*cidr)
					.expect("too many IP addresses for interface");
			}
		});

		let sockets = SocketSet::new(Vec::<SocketStorage<'static>>::new());

		Self {
			device,
			iface,
			sockets,
			tcp_rx_buffer_size: config.tcp_rx_buffer_size,
			tcp_tx_buffer_size: config.tcp_tx_buffer_size,
		}
	}

	/// Advance the stack state machine.
	///
	/// Processes all pending ingress packets and generates egress packets.
	/// Returns `true` if any socket state changed.
	pub fn poll(&mut self, timestamp: Instant) -> bool {
		self.iface
			.poll(timestamp, &mut self.device, &mut self.sockets)
			!= smoltcp::iface::PollResult::None
	}

	/// Returns the next time the stack should be polled, if any.
	///
	/// Returns [`None`] if there is no pending timer and the stack only
	/// needs to be polled on new ingress.
	pub fn poll_at(&mut self, timestamp: Instant) -> Option<Instant> {
		self.iface.poll_at(timestamp, &self.sockets)
	}

	/// Create a TCP socket in the LISTEN state on the given port.
	///
	/// # Errors
	///
	/// Returns [`Error::InvalidPort`] if `port` is 0.
	/// Returns [`Error::Listen`] if the socket cannot enter the listen state.
	pub fn tcp_listen(&mut self, port: u16) -> Result<SocketHandle, Error> {
		if port == 0 {
			return Err(Error::InvalidPort { port });
		}

		let rx_buf = tcp::SocketBuffer::new(vec![0u8; self.tcp_rx_buffer_size]);
		let tx_buf = tcp::SocketBuffer::new(vec![0u8; self.tcp_tx_buffer_size]);
		let mut socket = tcp::Socket::new(rx_buf, tx_buf);
		socket.listen(port)?;

		let handle = self.sockets.add(socket);
		Ok(handle)
	}

	/// Get an immutable reference to a TCP socket by handle.
	///
	/// # Panics
	///
	/// Panics if the handle does not refer to a valid TCP socket.
	#[must_use]
	pub fn tcp_socket(&self, handle: SocketHandle) -> &tcp::Socket<'static> {
		self.sockets.get(handle)
	}

	/// Get a mutable reference to a TCP socket by handle.
	///
	/// # Panics
	///
	/// Panics if the handle does not refer to a valid TCP socket.
	pub fn tcp_socket_mut(&mut self, handle: SocketHandle) -> &mut tcp::Socket<'static> {
		self.sockets.get_mut(handle)
	}

	/// Remove a socket from the set and return it.
	///
	/// # Panics
	///
	/// Panics if the handle does not refer to a valid socket.
	pub fn remove_socket(&mut self, handle: SocketHandle) -> smoltcp::socket::Socket<'static> {
		self.sockets.remove(handle)
	}

	/// Returns a reference to the underlying device.
	#[must_use]
	pub fn device(&self) -> &D {
		&self.device
	}

	/// Returns a mutable reference to the underlying device.
	pub fn device_mut(&mut self) -> &mut D {
		&mut self.device
	}

	/// Returns a reference to the smoltcp [`Interface`].
	#[must_use]
	pub fn iface(&self) -> &Interface {
		&self.iface
	}

	/// Returns a mutable reference to the smoltcp [`Interface`].
	pub fn iface_mut(&mut self) -> &mut Interface {
		&mut self.iface
	}

	/// Returns a reference to the [`SocketSet`].
	#[must_use]
	pub fn sockets(&self) -> &SocketSet<'static> {
		&self.sockets
	}

	/// Returns a mutable reference to the [`SocketSet`].
	pub fn sockets_mut(&mut self) -> &mut SocketSet<'static> {
		&mut self.sockets
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::inner::device::VecDevice;
	use smoltcp::wire::{IpCidr, Ipv4Address};

	fn test_config() -> StackConfig {
		StackConfig {
			ip_addrs: vec![IpCidr::new(Ipv4Address::new(10, 0, 0, 1).into(), 24)],
			random_seed: 42,
			..StackConfig::default()
		}
	}

	#[test]
	fn create_stack_and_listen() {
		let device = VecDevice::new(1500);
		let mut stack = InnerStack::new(device, test_config());

		let handle = stack.tcp_listen(80).expect("listen on port 80");
		let socket = stack.tcp_socket(handle);
		assert_eq!(socket.state(), tcp::State::Listen);
	}

	#[test]
	fn listen_on_port_zero_fails() {
		let device = VecDevice::new(1500);
		let mut stack = InnerStack::new(device, test_config());

		let result = stack.tcp_listen(0);
		assert!(result.is_err());
	}

	#[test]
	fn inject_syn_produces_syn_ack() {
		use smoltcp::wire::{
			IpProtocol, Ipv4Packet, Ipv4Repr, TcpControl, TcpPacket, TcpRepr, TcpSeqNumber,
		};

		let device = VecDevice::new(1500);
		let mut stack = InnerStack::new(device, test_config());
		let _handle = stack.tcp_listen(80).expect("listen on port 80");

		// Build a SYN packet: 10.0.0.2:12345 -> 10.0.0.1:80
		let tcp_repr = TcpRepr {
			src_port: 12345,
			dst_port: 80,
			control: TcpControl::Syn,
			seq_number: TcpSeqNumber(1000),
			ack_number: None,
			window_len: 65535,
			window_scale: Some(7),
			max_seg_size: Some(1460),
			sack_permitted: false,
			sack_ranges: [None; 3],
			payload: &[],
			timestamp: None,
		};

		let ip_repr = Ipv4Repr {
			src_addr: Ipv4Address::new(10, 0, 0, 2),
			dst_addr: Ipv4Address::new(10, 0, 0, 1),
			next_header: IpProtocol::Tcp,
			payload_len: tcp_repr.header_len(),
			hop_limit: 64,
		};

		let mut packet_buf = vec![0u8; ip_repr.buffer_len() + tcp_repr.header_len()];
		let mut ipv4_pkt = Ipv4Packet::new_unchecked(&mut packet_buf);
		ip_repr.emit(&mut ipv4_pkt, &smoltcp::phy::ChecksumCapabilities::default());

		let ip_hdr_len = ipv4_pkt.header_len() as usize;
		let mut tcp_pkt = TcpPacket::new_unchecked(&mut packet_buf[ip_hdr_len..]);
		tcp_repr.emit(
			&mut tcp_pkt,
			&Ipv4Address::new(10, 0, 0, 2).into(),
			&Ipv4Address::new(10, 0, 0, 1).into(),
			&smoltcp::phy::ChecksumCapabilities::default(),
		);

		// Re-emit IP header (checksum may depend on total length)
		let mut ipv4_pkt = Ipv4Packet::new_unchecked(&mut packet_buf);
		ip_repr.emit(&mut ipv4_pkt, &smoltcp::phy::ChecksumCapabilities::default());

		// Inject the SYN and poll
		let now = Instant::from_millis(0);
		stack.device_mut().inject(packet_buf);
		stack.poll(now);

		// Should have produced a SYN-ACK in the egress
		let egress = stack.device_mut().drain_egress();
		assert!(
			!egress.is_empty(),
			"expected at least one egress packet (SYN-ACK)"
		);

		// Parse the first egress packet and verify it's a SYN-ACK
		let reply = &egress[0];
		let reply_ip = Ipv4Packet::new_checked(reply).expect("valid IPv4");
		assert_eq!(reply_ip.next_header(), IpProtocol::Tcp);

		let reply_tcp = TcpPacket::new_checked(reply_ip.payload()).expect("valid TCP");
		assert!(reply_tcp.syn(), "expected SYN flag set");
		assert!(reply_tcp.ack(), "expected ACK flag set");
		assert_eq!(reply_tcp.src_port(), 80);
		assert_eq!(reply_tcp.dst_port(), 12345);
		assert_eq!(reply_tcp.ack_number(), TcpSeqNumber(1001));
	}
}
