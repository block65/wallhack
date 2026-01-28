use smoltcp::wire::IpCidr;

/// Configuration for creating an [`InnerStack`](crate::inner::InnerStack).
///
/// # Examples
///
/// ```
/// use netstack::config::StackConfig;
/// use smoltcp::wire::{IpCidr, Ipv4Address};
///
/// let config = StackConfig {
///     ip_addrs: vec![IpCidr::new(Ipv4Address::new(10, 0, 0, 1).into(), 24)],
///     ..StackConfig::default()
/// };
/// ```
#[derive(Debug, Clone)]
pub struct StackConfig {
	/// IP addresses assigned to the interface (with prefix length).
	pub ip_addrs: Vec<IpCidr>,

	/// Random seed for the smoltcp interface.
	///
	/// It is recommended to use a different seed on each run
	/// to avoid TCP sequence/port collisions.
	pub random_seed: u64,

	/// Maximum transmission unit in bytes.
	///
	/// For a TUN (L3) device this is the IP MTU, typically 1500.
	pub mtu: usize,

	/// Size of TCP socket receive buffers in bytes.
	pub tcp_rx_buffer_size: usize,

	/// Size of TCP socket transmit buffers in bytes.
	pub tcp_tx_buffer_size: usize,
}

impl Default for StackConfig {
	fn default() -> Self {
		Self {
			ip_addrs: Vec::new(),
			random_seed: 0,
			mtu: 1500,
			tcp_rx_buffer_size: 65535,
			tcp_tx_buffer_size: 65535,
		}
	}
}
