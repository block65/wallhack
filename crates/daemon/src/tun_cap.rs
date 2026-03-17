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
/// **Linux:** Checks for `CAP_NET_ADMIN` in the effective capability set
/// via `/proc/self/status`. Opening `/dev/net/tun` alone is insufficient —
/// the `TUNSETIFF` ioctl requires `CAP_NET_ADMIN`.
///
/// **macOS:** Returns `false` pending proper `utun` probing.
///
/// **All other platforms:** Returns `false`.
#[must_use]
pub fn detect_tun_capable() -> bool {
    detect_tun_capable_impl()
}

#[cfg(target_os = "linux")]
fn detect_tun_capable_impl() -> bool {
    // Opening /dev/net/tun succeeds for most users — the actual TUNSETIFF
    // ioctl that creates the interface requires CAP_NET_ADMIN. Check for
    // the capability directly so negotiation gets the real answer.
    has_cap_net_admin()
}

/// Check if the current process has `CAP_NET_ADMIN` in its effective set.
///
/// Reads `/proc/self/status` for the `CapEff` bitmask and tests bit 12.
#[cfg(target_os = "linux")]
fn has_cap_net_admin() -> bool {
    let Ok(status) = std::fs::read_to_string("/proc/self/status") else {
        return false;
    };
    for line in status.lines() {
        if let Some(hex) = line.strip_prefix("CapEff:\t") {
            let Ok(caps) = u64::from_str_radix(hex.trim(), 16) else {
                return false;
            };
            return caps & (1 << 12) != 0;
        }
    }
    false
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
