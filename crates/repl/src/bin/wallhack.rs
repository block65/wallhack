//! Unified wallhack binary entry point.
//!
//! Usage:
//!   wallhack                                    # Entry node with REPL (default)
//!   wallhack --listen :7777                     # Entry node on custom port
//!   wallhack --connect host:6565 --listen :7575 # Relay node
//!   wallhack --connect host:6565                # Exit node

use anyhow::Result;
use repl::{NodeRole, WallhackCli, parse_wallhack, run_entry, run_exit, run_relay};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
	let cli = parse_wallhack();

	// Setup tracing based on verbosity flags
	setup_tracing(&cli);

	let role = cli.node_role();

	match role {
		NodeRole::Entry => {
			repl::info!("Starting as entry node");
			run_entry(cli).await
		}
		NodeRole::Relay => {
			repl::info!("Starting as relay node");
			run_relay(cli).await
		}
		NodeRole::Exit => {
			repl::info!("Starting as exit node");
			run_exit(cli).await
		}
	}
}

fn setup_tracing(cli: &WallhackCli) {
	// Determine default level based on CLI flags
	let default_level = if cli.debug {
		"debug"
	} else if cli.verbose {
		"info"
	} else if cli.quiet {
		"error"
	} else {
		"warn"
	};

	// Build filter that respects RUST_LOG env var, falls back to CLI flags
	let filter = EnvFilter::builder()
		.with_default_directive(default_level.parse().expect("valid directive"))
		.from_env_lossy();

	tracing_subscriber::fmt().with_env_filter(filter).init();
}
