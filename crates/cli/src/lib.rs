#![warn(unused_extern_crates)]

pub mod cli;
pub mod dns;
pub mod output;
pub mod repl_common;
pub mod subscriber;
pub mod version;

mod entry;
mod exit;
mod net;
mod relay;

mod styles;

pub use cli::{Command, EntryCommand, ExitCommand, RelayCommand, WallhackCli, parse_cli};
pub use styles::OutputStyles;

// ============================================================================
// Node mode implementations
// ============================================================================

/// Run as an entry node with interactive REPL.
///
/// # Errors
///
/// Returns error if entry node setup or operation fails.
pub async fn run_entry(global: &WallhackCli, cmd: &EntryCommand) -> anyhow::Result<()> {
	entry::run(global, cmd).await
}

/// Run as a relay node.
///
/// # Errors
///
/// Returns error if relay node setup or operation fails.
pub async fn run_relay(global: &WallhackCli, cmd: &RelayCommand) -> anyhow::Result<()> {
	relay::run(global, cmd).await
}

/// Run as an exit node.
///
/// # Errors
///
/// Returns error if exit node setup or operation fails.
pub async fn run_exit(global: &WallhackCli, cmd: &ExitCommand) -> anyhow::Result<()> {
	exit::run(global, cmd).await
}
