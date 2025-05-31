use std::{net::SocketAddr, path::PathBuf};

use clap::{Args, CommandFactory, Parser, Subcommand};

use crate::ListenArgs;

#[derive(Args, Debug)]
pub struct ListenCommandArgs {
	#[command(flatten)]
	pub listen: ListenArgs,
}

#[derive(Args, Debug)]
pub struct ConnectArgs {
	/// Path to the TLS certificate file (for server authentication).
	#[arg(long, short = 'c', value_name = "FILE_PATH", requires = "tls_key")]
	pub cert: Option<PathBuf>,

	/// Path to the TLS private key file (for server authentication).
	#[arg(long, short = 'k', value_name = "FILE_PATH", requires = "tls_cert")]
	pub private_key: Option<PathBuf>,

	/// Specifies the address and port of the agent to connect to.
	#[arg(long, value_name = "ADDRESS:PORT", conflicts_with = "listen")]
	pub connect: Option<SocketAddr>,
}

#[derive(Debug, Parser)]
#[command(multicall = true)]
pub struct Repl {
	#[command(subcommand)]
	pub command: ReplCommands,
}

#[derive(Debug, Subcommand)]
pub enum ReplCommands {
	/// Listen for incoming connections from agents
	Listen(ListenCommandArgs),

	/// Connect to a remote host or agent
	Connect(ConnectArgs),

	/// Statistics for nerds
	#[clap(name = "stats")]
	Statistics,

	/// List all active peers
	Peers,

	// Gracefully shut down and exit
	Quit,

	/// Clear the console
	Clear,
}

impl Repl {
	pub fn get_command_names() -> Vec<String> {
		Repl::command() // Get the clap::Command definition
			.get_subcommands() // Get an iterator over the subcommands
			.filter(|cmd| !cmd.is_hide_set()) // Optionally filter out hidden commands like "stats"
			.map(|cmd| cmd.get_name().to_string()) // Get the name of each subcommand
			.collect()
	}
}
