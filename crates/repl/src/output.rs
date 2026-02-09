use std::sync::{LazyLock, RwLock};

use derive_more::Display;

use crate::styles::OutputStyles;

/// Output format for the application.
#[derive(Debug, Clone, Copy, Default)]
pub enum OutputFormat {
	#[default]
	Plain,
	Json,
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
		match self.format {
			OutputFormat::Plain => {
				let level_text = match message.level {
					Level::Info => {
						format!(
							"{style}{:6}{style:#}",
							message.level,
							style = self.styles.get_literal(),
						)
					}
					Level::Verbose => format!(
						"{:5}",
						format!(
							"{style}{:6}{style:#}",
							message.level,
							style = self.styles.get_literal(),
						)
					),
					Level::Error => format!(
						"{style}{:6}{style:#}",
						message.level,
						style = self.styles.get_error(),
					),
					Level::Warn => format!(
						"{style}{:6}{style:#}",
						message.level,
						style = self.styles.get_warning(),
					),
				};
				eprintln!("{} {}", level_text, message.message);
			}
			OutputFormat::Json => {
				// let json_output = serde_json::to_string(&message)
				// 	.expect("Failed to serialize status message to JSON");
				println!("{{ json_output }}");
			}
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

	#[display("-")]
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
