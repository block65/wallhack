use smoltcp::wire::IpCidr;

/// Configuration for creating an [`InnerStack`](crate::inner::InnerStack).
///
/// # Examples
///
/// ```
/// use wallhack_netstack::config::StackConfig;
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
	///
	/// 256 KiB balances memory per socket against throughput. smoltcp's TCP
	/// window is limited to the buffer size, so at high link speeds a 64 KiB
	/// buffer becomes the bottleneck (bandwidth-delay product). 256 KiB
	/// sustains ~2 Gbps at 1 ms RTT through the local TUN/veth path.
	pub tcp_rx_buffer_size: usize,

	/// Size of TCP socket transmit buffers in bytes.
	///
	/// Same rationale as `tcp_rx_buffer_size`. The TX buffer limits how much
	/// data `copy_bidirectional` can enqueue before the poll loop transmits
	/// it through the TUN device. Too small and reverse-direction throughput
	/// (target-to-client) stalls waiting for ACKs to free buffer space.
	pub tcp_tx_buffer_size: usize,

	/// Enable "any IP" mode (promiscuous mode for IP).
	///
	/// In this mode, the interface will accept packets destined to any IP address.
	/// Required for transparent proxying or capturing all traffic on a TUN interface.
	/// Note: Default routes are automatically added when this is enabled.
	pub any_ip: bool,

	/// Maximum number of concurrent sockets (TCP + UDP combined).
	///
	/// JIT binding is rejected once this limit is reached, preventing OOM under
	/// SYN flood or exhaustive port scans. Defaults to `usize::MAX` (no limit).
	pub max_sockets: usize,
}

impl Default for StackConfig {
	fn default() -> Self {
		Self {
			ip_addrs: Vec::new(),
			random_seed: 0,
			mtu: 1500,
			tcp_rx_buffer_size: 256 * 1024,
			tcp_tx_buffer_size: 256 * 1024,
			any_ip: false,
			max_sockets: usize::MAX,
		}
	}
}
