//! Shared REPL infrastructure for all node types.

use std::{
	io::{BufRead, IsTerminal, Write},
	time::Instant,
};

use tokio::sync::mpsc;

#[cfg(feature = "repl")]
use reedline::{
	DefaultPrompt, DefaultPromptSegment, ExternalPrinter, FileBackedHistory, Reedline, Signal,
};

/// Node start time for uptime reporting (shared across node types).
static NODE_STARTED_AT: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();

/// Global sink for tracing output when the interactive REPL is active.
///
/// When set, the tracing subscriber routes log lines through this channel
/// instead of writing directly to stderr, preventing terminal corruption.
static LOG_SINK: std::sync::OnceLock<mpsc::UnboundedSender<PrintMsg>> = std::sync::OnceLock::new();

/// Install the global log sink. Call once when entering interactive REPL mode.
///
/// After this is called, [`emit_log`] routes messages through the channel
/// instead of writing to stderr.
pub fn install_log_sink(tx: mpsc::UnboundedSender<PrintMsg>) {
	let _ = LOG_SINK.set(tx);
}

/// Emit a log line. Routes through the REPL printer if active, otherwise stderr.
///
/// Called by the tracing subscriber so log output goes through reedline's
/// `ExternalPrinter` rather than writing raw bytes to the terminal.
pub fn emit_log(line: String) {
	if let Some(tx) = LOG_SINK.get() {
		let _ = tx.send(PrintMsg::Text(line));
	} else {
		eprintln!("{line}");
	}
}

/// Record the node start time. Call once at startup.
pub fn mark_started() {
	NODE_STARTED_AT.get_or_init(Instant::now);
}

/// Returns the current uptime as a formatted string.
#[must_use]
pub fn uptime() -> String {
	NODE_STARTED_AT
		.get()
		.map_or_else(|| "unknown".to_string(), |t| format_duration(t.elapsed()))
}

/// Message type for the print channel, allowing commands to signal completion.
pub enum PrintMsg {
	/// A line of text to display.
	Text(String),
	/// Signals that the current REPL command has finished printing.
	Done,
}

/// Print just the version (used by the `version` REPL command).
pub fn print_version_info(printer: &Printer) {
	printer.print(crate::version::built_info::PKG_VERSION);
}

/// Print the unified help text (identical on all node types).
pub fn print_help(printer: &Printer) {
	use std::io::Write as _;
	let mut tw = tabwriter::TabWriter::new(vec![]).padding(2);
	let _ = writeln!(tw, "Available commands:");
	let _ = writeln!(tw, "  version");
	let _ = writeln!(tw, "  info\tNode state (role, listen address)");
	let _ = writeln!(tw, "  peers\tList connected peers");
	let _ = writeln!(
		tw,
		"  ping [peer]\tPing a peer (optional if only one connected)"
	);
	let _ = writeln!(tw, "  stats\tTraffic statistics");
	let _ = writeln!(
		tw,
		"  route add <cidr> [via <peer>]\tAdd a route (peer optional if only one connected)"
	);
	let _ = writeln!(tw, "  route del <cidr>\tRemove a route");
	let _ = writeln!(tw, "  route list\tList all routes");
	let _ = writeln!(tw, "  connect <addr>\tConnect to a peer");
	let _ = writeln!(tw, "  listen [addr]\tStart listening for peers");
	let _ = writeln!(tw, "  disconnect [peer]\tDisconnect a peer");
	let _ = writeln!(tw, "  help\tShow this help");
	let _ = writeln!(tw, "  quit\tExit wallhack");
	let _ = tw.flush();
	let buf = tw.into_inner().unwrap_or_default();
	let output = String::from_utf8_lossy(&buf);
	for line in output.trim_end().lines() {
		printer.print(line.trim_end());
	}
}

/// Wrapper for printing to terminal without disrupting reedline.
#[derive(Clone)]
pub struct Printer {
	tx: mpsc::UnboundedSender<PrintMsg>,
}

impl Printer {
	/// Create a new printer with the given channel.
	#[must_use]
	pub fn new(tx: mpsc::UnboundedSender<PrintMsg>) -> Self {
		Self { tx }
	}

	/// Print a message (async-safe).
	pub fn print(&self, msg: impl Into<String>) {
		let _ = self.tx.send(PrintMsg::Text(msg.into()));
	}

	/// Signal that the current REPL command has finished producing output.
	///
	/// This is consumed by the reedline thread to know all responses are queued
	/// before it draws the next prompt.
	pub fn done(&self) {
		let _ = self.tx.send(PrintMsg::Done);
	}

	/// Print an error message using the standard output formatting (reedline-safe).
	pub fn error(&self, msg: impl Into<String>) {
		self.print_level(crate::output::Level::Error, msg);
	}

	/// Print a warning message using the standard output formatting (reedline-safe).
	pub fn warn(&self, msg: impl Into<String>) {
		self.print_level(crate::output::Level::Warn, msg);
	}

	/// Print an info message using the standard output formatting (reedline-safe).
	pub fn info(&self, msg: impl Into<String>) {
		self.print_level(crate::output::Level::Info, msg);
	}

	fn print_level(&self, level: crate::output::Level, msg: impl Into<String>) {
		let message = msg.into();
		let formatted = match crate::output::OUTPUT_CONFIG.read() {
			Ok(config) => config.format_message(&crate::output::StatusMessage { level, message }),
			Err(_) => format!("{level} {message}"),
		};
		let _ = self.tx.send(PrintMsg::Text(formatted));
	}
}

/// RAII guard that calls [`Printer::done`] when dropped.
///
/// Place at the top of each REPL command dispatch arm so that `continue`,
/// `break`, and early `return` all reliably signal command completion to the
/// reedline thread.
pub struct DoneGuard<'a>(pub &'a Printer);

impl Drop for DoneGuard<'_> {
	fn drop(&mut self) {
		self.0.done();
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

/// A row in the peers table, used by both entry and exit nodes.
pub struct PeerRow {
	pub name: String,
	pub role: String,
	pub addr: String,
	pub latency: String,
	pub uptime: String,
	pub device: Option<String>,
}

/// Print a column-aligned peers table using elastic tabstops.
///
/// The DEVICE column is only shown if any row has a device value.
pub fn print_peer_table(printer: &Printer, rows: &[PeerRow]) {
	if rows.is_empty() {
		printer.print("No connected peers.");
		return;
	}

	printer.print(format!("Connected peers ({}):", rows.len()));

	let show_device = rows.iter().any(|r| r.device.is_some());

	let mut tw = tabwriter::TabWriter::new(vec![]).padding(2);

	if show_device {
		let _ = writeln!(tw, "  PEER\tROLE\tADDRESS\tLATENCY\tUPTIME\tDEVICE");
		for r in rows {
			let _ = writeln!(
				tw,
				"  {}\t{}\t{}\t{}\t{}\t{}",
				r.name,
				r.role,
				r.addr,
				r.latency,
				r.uptime,
				r.device.as_deref().unwrap_or("-"),
			);
		}
	} else {
		let _ = writeln!(tw, "  PEER\tROLE\tADDRESS\tLATENCY\tUPTIME");
		for r in rows {
			let _ = writeln!(
				tw,
				"  {}\t{}\t{}\t{}\t{}",
				r.name, r.role, r.addr, r.latency, r.uptime,
			);
		}
	}

	let _ = tw.flush();
	let buf = tw.into_inner().unwrap_or_default();
	let output = String::from_utf8_lossy(&buf);
	for line in output.trim_end().lines() {
		printer.print(line.trim_end());
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

/// Run the REPL input loop in a blocking thread.
///
/// If `repl` feature is enabled and stdin is a TTY, uses `reedline` for a rich
/// interactive experience. Otherwise falls back to simple stdin reading.
pub fn run_repl_input<T, F>(
	prompt_name: &str,
	tx: &mpsc::Sender<T>,
	mut print_rx: mpsc::UnboundedReceiver<PrintMsg>,
	parser: F,
	is_quit: impl Fn(&T) -> bool,
) where
	T: Send + 'static,
	F: Fn(&str) -> T + Send + 'static,
{
	#[cfg(feature = "repl")]
	if is_interactive() {
		let external_printer = ExternalPrinter::default();
		let ep = external_printer.clone();

		// Print thread: relay Text messages into reedline's ExternalPrinter.
		std::thread::spawn(move || {
			while let Some(msg) = print_rx.blocking_recv() {
				match msg {
					PrintMsg::Text(s) => {
						let _ = ep.print(s);
					}
					PrintMsg::Done => {
						// reedline flushes ExternalPrinter at the start of
						// each read_line() call; the Done sentinel is not needed here.
					}
				}
			}
		});

		let mut rl = Reedline::create().with_external_printer(external_printer);
		if let Some(history) = std::env::var("HOME").ok().and_then(|home| {
			FileBackedHistory::with_file(1000, (home + "/.wallhack_history").into()).ok()
		}) {
			rl = rl.with_history(Box::new(history));
		}
		let mut line_editor = rl;

		let prompt = DefaultPrompt::new(
			DefaultPromptSegment::Basic(prompt_name.to_string()),
			DefaultPromptSegment::Empty,
		);

		loop {
			match line_editor.read_line(&prompt) {
				Ok(Signal::Success(line)) => {
					let line = line.trim();
					if line.is_empty() {
						continue;
					}
					let cmd = parser(line);
					let quit = is_quit(&cmd);
					if tx.blocking_send(cmd).is_err() || quit {
						break;
					}
				}
				Ok(Signal::CtrlC) => {}
				Ok(Signal::CtrlD) => {
					let cmd = parser("quit");
					let _ = tx.blocking_send(cmd);
					break;
				}
				Err(e) => {
					tracing::debug!("Readline error: {e}");
					let cmd = parser("quit");
					let _ = tx.blocking_send(cmd);
					break;
				}
			}
		}
		return;
	}

	// Fallback/Headless mode: simple stdin reading
	let stdin = std::io::stdin();
	let mut stdout = std::io::stdout();

	loop {
		// In headless mode, we still need to process PrintMsg to avoid channel overflow
		// and to know when a command is finished.
		if is_interactive() {
			print!("{prompt_name}> ");
			let _ = stdout.flush();
		}

		let mut line = String::new();
		match stdin.lock().read_line(&mut line) {
			Ok(0) | Err(_) => {
				let cmd = parser("quit");
				let _ = tx.blocking_send(cmd);
				break;
			}
			Ok(_) => {
				let line = line.trim();
				if line.is_empty() {
					continue;
				}

				let cmd = parser(line);
				let quit = is_quit(&cmd);
				if tx.blocking_send(cmd).is_err() || quit {
					break;
				}

				// Wait for the command to finish printing before showing the next prompt
				while let Some(msg) = print_rx.blocking_recv() {
					match msg {
						PrintMsg::Text(s) => println!("{s}"),
						PrintMsg::Done => break,
					}
				}
			}
		}
	}
}
