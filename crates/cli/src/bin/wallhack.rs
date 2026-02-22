//! Wallhack binary entry point.
//!
//! Usage:
//!   wallhack                                              # Entry, listen default port
//!   wallhack entry --listen :6565                         # Entry, listen
//!   wallhack entry --connect host:443                     # Entry, reverse tunnel
//!   wallhack exit --connect host:6565                     # Exit, connect
//!   wallhack exit --listen :443                           # Exit, reverse tunnel
//!   wallhack relay --connect upstream:443 --listen :6565  # Relay

use anyhow::Result;
use cli::{Command, EntryCommand, parse_cli, run_entry, run_exit, run_relay};
use tracing::level_filters::LevelFilter;

#[tokio::main]
async fn main() -> Result<()> {
	let cli = parse_cli();

	// Handle --version flag
	if cli.version {
		cli::version::print_version();
		return Ok(());
	}

	setup_tracing(&cli);

	#[cfg(target_os = "linux")]
	check_entropy_ready();

	match &cli.command {
		Some(Command::Entry(cmd)) => {
			cli::info!("Starting as entry node");
			run_entry(&cli, cmd).await
		}
		Some(Command::Relay(cmd)) => {
			cli::info!("Starting as relay node");
			run_relay(&cli, cmd).await
		}
		Some(Command::Exit(cmd)) => {
			cli::info!("Starting as exit node");
			run_exit(&cli, cmd).await
		}
		None => {
			// Default: entry node listening on default port
			let cmd = EntryCommand {
				listen: None,
				connect: None,
				api: None,
				api_user: None,
				api_secret: None,
				max_peers: None,
				fast: false,
			};
			run_entry(&cli, &cmd).await
		}
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
		cli::warn!("Entropy pool not yet seeded — startup may stall.");
	}
}

fn setup_tracing(cli: &cli::WallhackCli) {
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
		// No internal tracing by default — user-facing output uses cli::info!/error!
		(LevelFilter::OFF, "")
	};

	let subscriber = cli::subscriber::SimpleSubscriber::new(level, filter_str);
	tracing::subscriber::set_global_default(subscriber).expect("setting default subscriber");
}
