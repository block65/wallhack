//! Wallhack binary entry point.
//!
//! Usage:
//!   wallhack                                              # Entry, listen :6565
//!   wallhack entry --listen :6565                         # Entry, listen
//!   wallhack entry --connect host:443                     # Entry, reverse tunnel
//!   wallhack exit --connect host:6565                     # Exit, connect
//!   wallhack exit --listen :443                           # Exit, reverse tunnel
//!   wallhack relay --connect upstream:443 --listen :6565  # Relay

use anyhow::Result;
use repl::{Command, EntryCommand, parse_wallhack, run_entry, run_exit, run_relay};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
	let cli = parse_wallhack();

	// Handle --version flag
	if cli.version {
		repl::version::print_version();
		return Ok(());
	}

	setup_tracing(&cli);

	match &cli.command {
		Some(Command::Entry(cmd)) => {
			repl::info!("Starting as entry node");
			run_entry(&cli, cmd).await
		}
		Some(Command::Relay(cmd)) => {
			repl::info!("Starting as relay node");
			run_relay(&cli, cmd).await
		}
		Some(Command::Exit(cmd)) => {
			repl::info!("Starting as exit node");
			run_exit(&cli, cmd).await
		}
		None => {
			// Default: entry node listening on :6565
			repl::info!("Starting as entry node (default)");
			let cmd = EntryCommand {
				listen: None,
				connect: None,
				api: None,
				api_user: None,
				api_pass: None,
			};
			run_entry(&cli, &cmd).await
		}
	}
}

fn setup_tracing(cli: &repl::WallhackCli) {
	let default_level = if cli.debug {
		"debug"
	} else if cli.verbose {
		"info"
	} else if cli.quiet {
		"error"
	} else {
		"warn"
	};

	let filter = EnvFilter::builder()
		.with_default_directive(default_level.parse().expect("valid directive"))
		.from_env_lossy();

	tracing_subscriber::fmt().with_env_filter(filter).init();
}
