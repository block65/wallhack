use smoltcp::phy::Device;

/// Extension trait for devices that can peek ingress packets.
pub trait PeekDevice: Device {
	/// Returns the next ingress packet without consuming it.
	///
	/// Implementations should drain all available packets from the underlying
	/// device into an internal buffer, then return a reference to the first one.
	fn peek_ingress(&mut self) -> Option<&[u8]>;

	/// Returns an iterator over all buffered ingress packets.
	///
	/// This allows processing all pending packets (e.g., for JIT listener creation)
	/// before calling `poll()` which will consume them.
	fn peek_all_ingress(&mut self) -> Vec<Vec<u8>> {
		// Default implementation: just return the single peeked packet
		self.peek_ingress()
			.map(|p| vec![p.to_vec()])
			.unwrap_or_default()
	}
}
