//! Shared REPL infrastructure for all node types.

use std::{io::IsTerminal, time::Instant};

use tokio::sync::mpsc;

/// Node start time for uptime reporting (shared across node types).
static NODE_STARTED_AT: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();

/// Record the node start time. Call once at startup.
pub fn mark_started() {
	NODE_STARTED_AT.get_or_init(Instant::now);
}

/// Print version and uptime.
pub fn print_ping(printer: &Printer) {
	let uptime = NODE_STARTED_AT
		.get()
		.map_or_else(|| "unknown".to_string(), |t| format_duration(t.elapsed()));
	printer.print(format!(
		"wallhack {} - uptime: {uptime}",
		crate::version::built_info::PKG_VERSION
	));
}

/// Wrapper for printing to terminal without disrupting readline.
#[derive(Clone)]
pub struct Printer {
	tx: mpsc::UnboundedSender<String>,
}

impl Printer {
	/// Create a new printer with the given channel.
	#[must_use]
	pub fn new(tx: mpsc::UnboundedSender<String>) -> Self {
		Self { tx }
	}

	/// Print a message (async-safe).
	pub fn print(&self, msg: impl Into<String>) {
		let _ = self.tx.send(msg.into());
	}
}

/// Check if stdin is a terminal and REPL should be interactive.
#[must_use]
pub fn is_interactive() -> bool {
	std::io::stdin().is_terminal()
}

/// Check if an error is terminal and should not be retried.
///
/// Authentication failures and certificate mismatches indicate a configuration
/// problem — retrying won't help and just creates noise.
#[must_use]
pub fn is_nonretryable_error(err: &impl std::fmt::Display) -> bool {
	let msg = err.to_string();
	msg.contains("Fingerprint mismatch")
		|| msg.contains("PSK authentication failed")
		|| msg.contains("certificate")
		|| msg.contains("CertificateRequired")
		|| msg.contains("HandshakeFailure")
}

/// Format a duration as a human-readable string (e.g. "2m 30s", "1h 5m 0s").
#[must_use]
pub fn format_duration(d: std::time::Duration) -> String {
	let total_secs = d.as_secs();
	let hours = total_secs / 3600;
	let mins = (total_secs % 3600) / 60;
	let secs = total_secs % 60;

	if hours > 0 {
		format!("{hours}h {mins}m {secs}s")
	} else if mins > 0 {
		format!("{mins}m {secs}s")
	} else if secs > 0 {
		format!("{secs}s")
	} else {
		format!("{}ms", d.as_millis())
	}
}

/// Format bytes in human-readable form.
#[must_use]
pub fn format_bytes(bytes: u64) -> String {
	let units = ["B", "KB", "MB", "GB", "TB", "PB"];
	#[allow(clippy::cast_precision_loss)]
	let mut value = bytes as f64;
	let mut i = 0;

	while value >= 1024.0 && i < units.len() - 1 {
		value /= 1024.0;
		i += 1;
	}

	if i == 0 {
		format!("{} {}", bytes, units[0])
	} else {
		format!("{:.2} {}", value, units[i])
	}
}
