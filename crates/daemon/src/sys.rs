//! Platform-specific system checks.

/// Warn if the kernel entropy pool isn't seeded yet.
#[cfg(target_os = "linux")]
pub(crate) fn check_entropy_ready() {
    use std::{io::Read, os::unix::fs::OpenOptionsExt};

    let Ok(mut f) = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(0x800)
        .open("/dev/random")
    else {
        return;
    };

    let mut buf = [0u8; 1];
    if let Err(e) = f.read(&mut buf)
        && e.kind() == std::io::ErrorKind::WouldBlock
    {
        tracing::warn!("Entropy pool not yet seeded — startup may stall.");
    }
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn check_entropy_ready() {}
