#![warn(unused_extern_crates)]

pub mod cli;
pub mod dns;
pub mod subscriber;
pub mod version;

mod config;
mod mode;
mod net;
mod transport;

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
	entry::actor,
	node_api::NodeApi,
};

// ============================================================================
// Error types
// ============================================================================

/// Errors from the daemon engine (public API).
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
	Node(#[from] NodeError),

	/// Error from the runtime (e.g. spawned task panicked).
	#[error(transparent)]
	Runtime(#[from] anyhow::Error),
}

/// Typed errors for node operations.
#[derive(Debug, thiserror::Error)]
pub enum NodeError {
	/// Required transport feature not compiled in.
	#[error("{0} transport not available (compile with --features {0})")]
	TransportUnavailable(&'static str),

	/// Invalid CLI or node configuration.
	#[error("{0}")]
	Config(String),

	/// PSK authentication failure.
	#[error("PSK authentication failed for peer {0}")]
	PskAuth(String),

	/// Control channel unexpectedly closed.
	#[error("control channel closed")]
	ChannelClosed,

	/// Address resolution produced no results.
	#[error("no addresses resolved for {0}")]
	NoAddresses(String),

	/// Address parse error.
	#[error(transparent)]
	AddrParse(#[from] std::net::AddrParseError),

	#[error("TUN subsystem error: {0}")]
	TunActor(#[from] crate::actor::Error),

	#[error("connection manager error: {0}")]
	ConnectionManager(#[from] wallhack_core::entry::manager::Error),

	#[error("runtime task error: {0}")]
	Runtime(#[from] tokio::task::JoinError),

	/// I/O error.
	#[error(transparent)]
	Io(#[from] std::io::Error),

	/// WebSocket Server Error
	#[error(transparent)]
	WebSocketServer(#[from] wallhack_core::server::ws::Error),

	/// DNS resolution failure.
	#[error("DNS resolution failed: {0}")]
	DnsResolution(#[source] Box<dyn std::error::Error + Send + Sync>),

	/// Transport creation or connection failure.
	#[error("transport error: {0}")]
	Transport(#[source] Box<dyn std::error::Error + Send + Sync>),

	/// Stream-level I/O error (bi-stream read/write).
	#[error("stream error: {0}")]
	Stream(#[source] Box<dyn std::error::Error + Send + Sync>),
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
/// (--help, --version). Returns [`DaemonError::Node`] for node failures.
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

	let command = cli.command.clone().unwrap_or(Command::Entry(EntryCommand {
		name: None,
		listen: None,
		connect: None,
		api: None,
		api_user: None,
		api_secret: None,
		max_peers: None,
		fast: false,
	}));

	let handle = start_node(&cli, &command)?;

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

/// Check whether an error looks terminal (auth/cert failure) and should not be
/// retried.  Uses string matching because upstream error types are opaque.
pub(crate) fn is_nonretryable_error(err: &impl std::fmt::Display) -> bool {
	let msg = err.to_string();
	msg.contains("Fingerprint mismatch")
		|| msg.contains("PSK authentication failed")
		|| msg.contains("certificate")
		|| msg.contains("CertificateRequired")
		|| msg.contains("HandshakeFailure")
}

// ============================================================================
// Unified node constructor
// ============================================================================

/// Start a node in the given mode and return a [`DaemonHandle`].
///
/// Spawns the node into a background task. Use [`DaemonHandle::wait`] to
/// block until the node exits, or [`DaemonHandle::shutdown`] to stop it.
///
/// # Errors
///
/// Returns error if node setup fails.
pub fn start_node(global: &WallhackCli, command: &Command) -> Result<DaemonHandle, NodeError> {
	let role = match command {
		Command::Entry(_) => NodeRole::Entry,
		Command::Exit(_) => NodeRole::Exit,
		Command::Relay(_) => NodeRole::Relay,
	};

	let metrics = Arc::new(Metrics::default());
	let peers = Arc::new(Registry::new());
	let routes = RouteTable::shared();

	let handler = Handler::new(
		HandlerConfig::new(role),
		Arc::clone(&metrics),
		Arc::clone(&peers),
		Arc::clone(&routes),
	);
	let node_api: Arc<dyn NodeApi> = Arc::new(handler);

	let (shutdown_tx, _shutdown_rx) = watch::channel(());

	let global = global.clone();
	let command = command.clone();
	let resources = mode::NodeResources {
		metrics,
		peers,
		routes,
	};
	let task = tokio::spawn(async move {
		mode::run(&global, &command, resources)
			.await
			.map_err(Into::into)
	});

	Ok(DaemonHandle::new(node_api, shutdown_tx, task))
}
