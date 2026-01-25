#![warn(unused_extern_crates)]

pub mod config;
pub mod cli;
pub mod dns;
pub mod output;

mod completer;
mod helper;
mod repl_commands;

#[cfg(feature = "color")]
mod styles;

// Re-exports for convenience
pub use cli::{AgentCli, HostCli, parse_agent, parse_host};
pub use config::{
	AgentConfig, Command, ConnectConfig, GlobalConfig, HostConfig, ListenConfig, OutputFormat,
};
pub use styles::OutputStyles;
