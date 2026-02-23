use std::sync::{LazyLock, RwLock};

use derive_more::Display;

use crate::styles::OutputStyles;

/// Output format for the application.
#[derive(Debug, Clone, Copy, Default)]
pub enum OutputFormat {
	#[default]
	Plain,
}

pub static OUTPUT_CONFIG: LazyLock<RwLock<Output>> = LazyLock::new(|| {
	RwLock::new(Output {
		format: OutputFormat::Plain,
		styles: OutputStyles::default(),
		use_color: true,
	})
});

pub fn initialize_output_config(format: OutputFormat, styles: OutputStyles, use_color: bool) {
	if let Ok(mut config_guard) = OUTPUT_CONFIG.write() {
		config_guard.format = format;
		config_guard.styles = styles;
		config_guard.use_color = use_color;
	} else {
		eprintln!("FATAL ERROR: Output config lock poisoned.");
	}
}

pub struct Output {
	pub format: OutputFormat,

	pub styles: OutputStyles,

	pub use_color: bool,
}

impl Output {
	pub fn print(&self, message: &StatusMessage) {
		let OutputFormat::Plain = self.format;
		eprintln!("{} {}", self.format_level(message.level), message.message);
	}

	/// Format a message as a plain-text string without printing it.
	#[must_use]
	pub fn format_message(&self, message: &StatusMessage) -> String {
		format!("{} {}", self.format_level(message.level), message.message)
	}

	fn format_level(&self, level: Level) -> String {
		if !self.use_color {
			return format!("{level}");
		}
		match level {
			Level::Info => format!(
				"{style}{:6}{style:#}",
				level,
				style = self.styles.get_literal(),
			),
			Level::Verbose => format!(
				"{:5}",
				format!(
					"{style}{:6}{style:#}",
					level,
					style = self.styles.get_literal(),
				)
			),
			Level::Error => format!(
				"{style}{:6}{style:#}",
				level,
				style = self.styles.get_error(),
			),
			Level::Warn => format!(
				"{style}{:6}{style:#}",
				level,
				style = self.styles.get_warning(),
			),
		}
	}
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Display)]
pub enum Level {
	#[display("[+]")]
	Info,

	#[display("[~]")]
	Verbose,

	#[display("[!]")]
	Warn,

	#[display("[-]")]
	Error,
}

#[derive(Debug)]
pub struct StatusMessage {
	pub level: Level,
	pub message: String,
	// Potentially add timestamp, source, etc.
	// #[serde(skip_serializing_if = "Option::is_none")]
	// pub details: Option<String>, // For structured details in JSON
}

#[macro_export]
macro_rules! info {
	($($arg:tt)*) => ({
		let message_content = format!($($arg)*);
		// Access the global config, lock it, and call the print method
		match $crate::output::OUTPUT_CONFIG.read() {
			Ok(config_guard) => {
				config_guard.print(
					&$crate::output::StatusMessage {
						level: $crate::output::Level::Info,
						message: message_content,
					}
				);
			}
			Err(e) => {
				eprintln!("FATAL ERROR: Output config lock poisoned: {}. Original message: INFO: {}", e, message_content);
			}
		}
	})
}

#[macro_export]
macro_rules! verbose {
	($($arg:tt)*) => ({
		let message_content = format!($($arg)*);
		// Access the global config, lock it, and call the print method
		match $crate::output::OUTPUT_CONFIG.read() {
			Ok(config_guard) => {
				config_guard.print(
					&$crate::output::StatusMessage {
						level: $crate::output::Level::Verbose,
						message: message_content,
					}
				);
			}
			Err(e) => {
				eprintln!("FATAL ERROR: Output config lock poisoned: {}. Original message: VERBOSE: {}", e, message_content);
			}
		}
	})
}

#[macro_export]
macro_rules! error {
	($($arg:tt)*) => ({
		let message_content = format!($($arg)*);
		// Access the global config, lock it, and call the print method
		match $crate::output::OUTPUT_CONFIG.read() {
			Ok(config_guard) => {
				config_guard.print(
					&$crate::output::StatusMessage {
						level: $crate::output::Level::Error,
						message: message_content,
					}
				);
			}
			Err(e) => {
				eprintln!("FATAL ERROR: Output config lock poisoned: {}. Original message: ERROR: {}", e, message_content);
			}
		}
	})
}

#[macro_export]
macro_rules! warn {
	($($arg:tt)*) => ({
		let message_content = format!($($arg)*);
		// Access the global config, lock it, and call the print method
		match $crate::output::OUTPUT_CONFIG.read() {
			Ok(config_guard) => {
				config_guard.print(
					&$crate::output::StatusMessage {
						level: $crate::output::Level::Warn,
						message: message_content,
					}
				);
			}
			Err(e) => {
				eprintln!("FATAL ERROR: Output config lock poisoned: {}. Original message: WARN: {}", e, message_content);
			}
		}
	})
}

/// Route an info message through the printer when interactive, or the output
/// system when headless. `$printer` must be `Option<&Printer>`.
#[macro_export]
macro_rules! route_info {
	($printer:expr, $($arg:tt)*) => {
		if let Some(__p) = $printer {
			__p.info(format!($($arg)*));
		} else {
			$crate::info!($($arg)*);
		}
	};
}

/// Route a warn message through the printer when interactive, or the output
/// system when headless. `$printer` must be `Option<&Printer>`.
#[macro_export]
macro_rules! route_warn {
	($printer:expr, $($arg:tt)*) => {
		if let Some(__p) = $printer {
			__p.warn(format!($($arg)*));
		} else {
			$crate::warn!($($arg)*);
		}
	};
}
