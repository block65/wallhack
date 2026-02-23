#![warn(unused_extern_crates)]

pub mod cli;
pub mod dns;
pub mod subscriber;
pub mod version;

mod entry;
mod exit;
mod net;
mod relay;

pub use cli::{Command, EntryCommand, ExitCommand, RelayCommand, WallhackCli, parse_cli_from_args};

use std::sync::Arc;

use tokio::sync::watch;
use tracing::level_filters::LevelFilter;
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
// Error type
// ============================================================================

/// Errors from the daemon engine.
#[derive(Debug, thiserror::Error)]
pub enum DaemonError {
	/// CLI produced output and exited early (help text or parse error).
	#[error("{message}")]
	Cli {
		message: String,
		/// 0 for informational output (--help), 1 for parse errors.
		exit_code: i32,
	},

	/// Node setup or runtime failure.
	#[error(transparent)]
	Runtime(#[from] anyhow::Error),
}

// ============================================================================
// Daemon engine entry point
// ============================================================================

/// Run the daemon engine from CLI arguments.
///
/// Parses `args` (including argv\[0\]), configures tracing, starts the
/// appropriate node, and blocks until shutdown.
///
/// # Errors
///
/// Returns [`DaemonError::Cli`] for parse errors or informational output
/// (--help, --version). Returns [`DaemonError::Runtime`] for node failures.
pub async fn run_daemon_engine(args: Vec<String>) -> Result<(), DaemonError> {
	let cli = cli::parse_cli_from_args(args)?;

	if cli.version {
		let message = if cli.verbose {
			version::version_verbose()
		} else {
			version::version_short()
		};
		return Err(DaemonError::Cli {
			message,
			exit_code: 0,
		});
	}

	setup_tracing(&cli);

	#[cfg(target_os = "linux")]
	check_entropy_ready();

	let handle = match &cli.command {
		Some(Command::Entry(cmd)) => start_entry(&cli, cmd)?,
		Some(Command::Relay(cmd)) => start_relay(&cli, cmd)?,
		Some(Command::Exit(cmd)) => start_exit(&cli, cmd)?,
		None => {
			// Default: entry node listening on default port
			let cmd = EntryCommand {
				name: None,
				listen: None,
				connect: None,
				api: None,
				api_user: None,
				api_secret: None,
				max_peers: None,
				fast: false,
			};
			start_entry(&cli, &cmd)?
		}
	};

	// Start IPC listener for the management protocol.
	let socket_path = wallhack_core::ipc::socket_path();
	let api = handle.api_arc();
	let shutdown_rx = handle.shutdown_rx();

	let ipc_task = tokio::spawn(async move {
		if let Err(e) = wallhack_core::ipc::run_ipc_listener(api, &socket_path, shutdown_rx).await {
			tracing::error!("IPC listener error: {e}");
		}
	});

	tokio::select! {
		result = handle.wait() => Ok(result?),
		_ = ipc_task => Ok(()),
	}
}

// ============================================================================
// Tracing setup
// ============================================================================

fn setup_tracing(cli: &WallhackCli) {
	let (level, filter_str) = if cli.trace || cli.trace_filter.is_some() {
		(
			LevelFilter::TRACE,
			cli.trace_filter.as_deref().unwrap_or(""),
		)
	} else if cli.debug || cli.debug_filter.is_some() {
		(
			LevelFilter::DEBUG,
			cli.debug_filter.as_deref().unwrap_or(""),
		)
	} else {
		// No internal tracing by default
		(LevelFilter::OFF, "")
	};

	let subscriber = subscriber::SimpleSubscriber::new(level, filter_str);
	tracing::subscriber::set_global_default(subscriber).expect("setting default subscriber");
}

/// Warn if the kernel entropy pool isn't seeded yet.
#[cfg(target_os = "linux")]
fn check_entropy_ready() {
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

// ============================================================================
// Utilities
// ============================================================================

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
