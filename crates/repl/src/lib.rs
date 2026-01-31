#![warn(unused_extern_crates)]

pub mod cli;
pub mod dns;
pub mod output;

mod entry;
mod exit;
mod relay;

#[cfg(feature = "color")]
mod styles;

pub use cli::{NodeRole, WallhackCli, parse_wallhack};
pub use styles::OutputStyles;

// ============================================================================
// Node mode implementations
// ============================================================================

/// Run as an entry node with interactive REPL.
///
/// # Errors
///
/// Returns error if entry node setup or operation fails.
pub async fn run_entry(cli: WallhackCli) -> anyhow::Result<()> {
	entry::run(cli).await
}

/// Run as a relay node (connect + listen).
///
/// # Errors
///
/// Returns error if relay node setup or operation fails.
pub async fn run_relay(cli: WallhackCli) -> anyhow::Result<()> {
	relay::run(cli).await
}

/// Run as an exit node (connect only).
///
/// # Errors
///
/// Returns error if exit node setup or operation fails.
pub async fn run_exit(cli: WallhackCli) -> anyhow::Result<()> {
	exit::run(cli).await
}
