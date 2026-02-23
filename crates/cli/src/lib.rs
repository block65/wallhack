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

use std::sync::Arc;

use tokio::sync::watch;
use wallhack_core::{
	NodeRole,
	control::{
		handler::{Handler, HandlerConfig},
		metrics::Metrics,
		peers::Registry,
		routes::RouteTable,
	},
	daemon::DaemonHandle,
	node_api::NodeApi,
};

// ============================================================================
// Daemon handle constructors
// ============================================================================

/// Start an entry node and return a [`DaemonHandle`].
///
/// Spawns the node into a background task. Use [`DaemonHandle::wait`] to
/// block until the node exits, or [`DaemonHandle::shutdown`] to stop it.
///
/// # Errors
///
/// Returns error if entry node setup fails.
pub fn start_entry(global: &WallhackCli, cmd: &EntryCommand) -> anyhow::Result<DaemonHandle> {
	let metrics = Arc::new(Metrics::default());
	let peers = Arc::new(Registry::new());
	let routes = RouteTable::shared();

	let handler = Handler::new(
		HandlerConfig::new(NodeRole::Entry),
		Arc::clone(&metrics),
		Arc::clone(&peers),
		Arc::clone(&routes),
	);
	let node_api: Arc<dyn NodeApi> = Arc::new(handler);

	let (shutdown_tx, _shutdown_rx) = watch::channel(());

	let global = global.clone();
	let cmd = cmd.clone();
	let task = tokio::spawn(async move { entry::run(&global, &cmd, metrics, peers, routes).await });

	Ok(DaemonHandle::new(node_api, shutdown_tx, task))
}

/// Start an exit node and return a [`DaemonHandle`].
///
/// Spawns the node into a background task. Use [`DaemonHandle::wait`] to
/// block until the node exits, or [`DaemonHandle::shutdown`] to stop it.
///
/// # Errors
///
/// Returns error if exit node setup fails.
pub fn start_exit(global: &WallhackCli, cmd: &ExitCommand) -> anyhow::Result<DaemonHandle> {
	let metrics = Arc::new(Metrics::default());
	let peers = Arc::new(Registry::new());
	let routes = RouteTable::shared();

	let handler = Handler::new(
		HandlerConfig::new(NodeRole::Exit),
		Arc::clone(&metrics),
		Arc::clone(&peers),
		Arc::clone(&routes),
	);
	let node_api: Arc<dyn NodeApi> = Arc::new(handler);

	let (shutdown_tx, _shutdown_rx) = watch::channel(());

	let global = global.clone();
	let cmd = cmd.clone();
	let task = tokio::spawn(async move { exit::run(&global, &cmd, metrics).await });

	Ok(DaemonHandle::new(node_api, shutdown_tx, task))
}

/// Start a relay node and return a [`DaemonHandle`].
///
/// Spawns the node into a background task. Use [`DaemonHandle::wait`] to
/// block until the node exits, or [`DaemonHandle::shutdown`] to stop it.
///
/// # Errors
///
/// Returns error if relay node setup fails.
pub fn start_relay(global: &WallhackCli, cmd: &RelayCommand) -> anyhow::Result<DaemonHandle> {
	let metrics = Arc::new(Metrics::default());
	let peers = Arc::new(Registry::new());
	let routes = RouteTable::shared();

	let handler = Handler::new(
		HandlerConfig::new(NodeRole::Relay),
		Arc::clone(&metrics),
		Arc::clone(&peers),
		Arc::clone(&routes),
	);
	let node_api: Arc<dyn NodeApi> = Arc::new(handler);

	let (shutdown_tx, _shutdown_rx) = watch::channel(());

	let global = global.clone();
	let cmd = cmd.clone();
	let task = tokio::spawn(async move { relay::run(&global, &cmd, metrics).await });

	Ok(DaemonHandle::new(node_api, shutdown_tx, task))
}

// ============================================================================
// Convenience wrappers (block until node exits)
// ============================================================================

/// Run as an entry node (blocks until exit).
///
/// # Errors
///
/// Returns error if entry node setup or operation fails.
pub async fn run_entry(global: &WallhackCli, cmd: &EntryCommand) -> anyhow::Result<()> {
	start_entry(global, cmd)?.wait().await
}

/// Run as a relay node (blocks until exit).
///
/// # Errors
///
/// Returns error if relay node setup or operation fails.
pub async fn run_relay(global: &WallhackCli, cmd: &RelayCommand) -> anyhow::Result<()> {
	start_relay(global, cmd)?.wait().await
}

/// Run as an exit node (blocks until exit).
///
/// # Errors
///
/// Returns error if exit node setup or operation fails.
pub async fn run_exit(global: &WallhackCli, cmd: &ExitCommand) -> anyhow::Result<()> {
	start_exit(global, cmd)?.wait().await
}
