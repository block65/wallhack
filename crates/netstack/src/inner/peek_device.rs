use smoltcp::phy::Device;

/// Extension trait for devices that can peek ingress packets.
pub trait PeekDevice: Device {
	/// Returns the next ingress packet without consuming it.
	fn peek_ingress(&mut self) -> Option<&[u8]>;
}
