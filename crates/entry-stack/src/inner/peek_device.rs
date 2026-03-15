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

    /// Remove pending packets that do not satisfy the predicate.
    ///
    /// Packets for which `f` returns `false` are silently dropped before the
    /// smoltcp poll loop processes them. Used to suppress RSTs for TCP
    /// segments that arrive with no matching socket.
    fn retain_pending<F: FnMut(&[u8]) -> bool>(&mut self, f: F);

    /// Inject a raw packet into the pending ingress queue.
    ///
    /// Used by the SYN intercept path to re-inject held SYN packets after the
    /// exit node confirms (or denies) reachability.
    fn inject_pending(&mut self, packet: Vec<u8>);
}
