//! Wallhack binary entry point.
//!
//! Usage:
//!   wallhack
//!   wallhack entry --listen :6565
//!   wallhack entry --connect host:443
//!   wallhack exit --connect host:6565
//!   wallhack exit --listen :443
//!   wallhack relay --connect upstream:443 --listen :6565

use std::io::IsTerminal;

use anyhow::Result;
use tracing::level_filters::LevelFilter;
use wallhack_cli::{Command, EntryCommand, parse_cli, start_entry, start_exit, start_relay};

#[tokio::main]
async fn main() -> Result<()> {
	let cli = parse_cli();

	// Initialize output config: enable colour only when stderr is a terminal.
	wallhack_cli::output::initialize_output_config(
		wallhack_cli::output::OutputFormat::Plain,
		wallhack_cli::OutputStyles::default(),
		std::io::stderr().is_terminal(),
	);

	// Handle --version flag
	if cli.version {
		if cli.verbose {
			wallhack_cli::version::print_version_verbose();
		} else {
			wallhack_cli::version::print_version_short();
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

	handle.wait().await
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
		wallhack_cli::warn!("Entropy pool not yet seeded — startup may stall.");
	}
}

fn setup_tracing(cli: &wallhack_cli::WallhackCli) {
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
		// No internal tracing by default — user-facing output uses wallhack_cli::info!/error!
		(LevelFilter::OFF, "")
	};

	let subscriber = wallhack_cli::subscriber::SimpleSubscriber::new(level, filter_str);
	tracing::subscriber::set_global_default(subscriber).expect("setting default subscriber");
}
