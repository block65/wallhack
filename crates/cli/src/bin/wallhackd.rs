//! Wallhack daemon entry point.
//!
//! Usage:
//!   wallhackd
//!   wallhackd entry --listen :6565
//!   wallhackd entry --connect host:443
//!   wallhackd exit --connect host:6565
//!   wallhackd exit --listen :443
//!   wallhackd relay --connect upstream:443 --listen :6565

use std::io::IsTerminal;

use anyhow::Result;
use tracing::level_filters::LevelFilter;
use wallhackd::{Command, EntryCommand, parse_cli, start_entry, start_exit, start_relay};

#[tokio::main]
async fn main() -> Result<()> {
	let cli = parse_cli();

	// Initialize output config: enable colour only when stderr is a terminal.
	wallhackd::output::initialize_output_config(
		wallhackd::output::OutputFormat::Plain,
		wallhackd::OutputStyles::default(),
		std::io::stderr().is_terminal(),
	);

	// Handle --version flag
	if cli.version {
		if cli.verbose {
			wallhackd::version::print_version_verbose();
		} else {
			wallhackd::version::print_version_short();
		}
		return Ok(());
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
		result = handle.wait() => result,
		_ = ipc_task => Ok(()),
	}
}

/// Warn if the kernel entropy pool isn't seeded yet.
///
/// `getrandom(2)` blocks until the CRNG has 256 bits of entropy, which can take a
/// long time on systems with limited entropy sources — causing silent hangs in crypto
/// startup code. Probing once here makes the wait visible.
#[cfg(target_os = "linux")]
fn check_entropy_ready() {
	use std::{io::Read, os::unix::fs::OpenOptionsExt};

	// O_NONBLOCK (0x800) on the /dev/random fd is the same CRNG-readiness check
	// that getrandom(GRND_NONBLOCK) uses internally, with no unsafe required.
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
		wallhackd::warn!("Entropy pool not yet seeded — startup may stall.");
	}
}

fn setup_tracing(cli: &wallhackd::WallhackCli) {
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
		// No internal tracing by default — user-facing output uses wallhackd::info!/error!
		(LevelFilter::OFF, "")
	};

	let subscriber = wallhackd::subscriber::SimpleSubscriber::new(level, filter_str);
	tracing::subscriber::set_global_default(subscriber).expect("setting default subscriber");
}
