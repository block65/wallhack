use std::collections::VecDeque;

use smoltcp::phy::Device;

/// Extension trait for devices that can peek ingress packets.
pub trait PeekDevice: Device {
	/// Returns the next ingress packet without consuming it.
	///
	/// Implementations should drain all available packets from the underlying
	/// device into an internal buffer, then return a reference to the first one.
	fn peek_ingress(&mut self) -> Option<&[u8]>;

	/// Returns a reference to all buffered ingress packets.
	///
	/// This allows processing all pending packets (e.g., for JIT listener creation)
	/// before calling `poll()` which will consume them. Returns a reference to
	/// the internal queue to avoid cloning packet data.
	fn peek_all_ingress(&mut self) -> &VecDeque<Vec<u8>>;
}
