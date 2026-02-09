//! Shared REPL infrastructure for all node types.

use std::io::IsTerminal;

use tokio::sync::mpsc;

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
