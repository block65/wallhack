//! TUN capability detection.
//!
//! Probes whether the current process can create a TUN interface. The result
//! is used to populate the `tun_capable` field of the handshake so peers can
//! negotiate roles automatically.
//!
//! The check uses actual kernel probes rather than heuristics like `geteuid()`,
//! giving correct answers for non-root users with `CAP_NET_ADMIN` and for root
//! inside containers that lack the capability.

/// Detect whether this process can create a TUN interface.
///
/// **Linux:** Opens `/dev/net/tun` with read+write access and immediately
/// closes it. This is one syscall and gives the actual kernel answer.
///
/// **macOS:** Returns `false` pending proper `utun` probing via
/// `socket(PF_SYSTEM, SOCK_DGRAM, SYSPROTO_CONTROL)`. Conservative default
/// until macOS TUN support is implemented.
///
/// **All other platforms:** Returns `false`.
#[must_use]
pub fn detect_tun_capable() -> bool {
    detect_tun_capable_impl()
}

#[cfg(target_os = "linux")]
fn detect_tun_capable_impl() -> bool {
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/net/tun")
        .is_ok()
}

#[cfg(not(target_os = "linux"))]
fn detect_tun_capable_impl() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_tun_capable_returns_bool() {
        // Just check it doesn't panic.
        let _ = detect_tun_capable();
    }
}
