//! REPL command parsing using argh.

use std::{net::SocketAddr, path::PathBuf};

use argh::FromArgs;

/// REPL commands - parsed from user input at runtime.
#[derive(Debug, FromArgs)]
#[argh(subcommand)]
pub enum ReplCommands {
	Listen(ListenCommand),
	Connect(ConnectCommand),
	Stats(StatsCommand),
	Peers(PeersCommand),
	Quit(QuitCommand),
	Clear(ClearCommand),
}

/// Listen for incoming connections from agents
#[derive(Debug, FromArgs)]
#[argh(subcommand, name = "listen")]
pub struct ListenCommand {
	/// local address and port to listen on
	#[argh(positional, default = "\"[::]:6565\".to_string()")]
	pub addr: String,

	/// path to the TLS certificate file
	#[argh(option, short = 'c')]
	pub cert: Option<PathBuf>,

	/// path to the TLS private key file
	#[argh(option, short = 'k')]
	pub key: Option<PathBuf>,

	/// path to the CA roots file for mTLS
	#[argh(option)]
	pub ca: Option<PathBuf>,
}

/// Connect to a remote host or agent
#[derive(Debug, FromArgs)]
#[argh(subcommand, name = "connect")]
pub struct ConnectCommand {
	/// path to the TLS certificate file
	#[argh(option, short = 'c')]
	pub cert: Option<PathBuf>,

	/// path to the TLS private key file
	#[argh(option, short = 'k')]
	pub private_key: Option<PathBuf>,

	/// address and port to connect to
	#[argh(option)]
	pub connect: Option<SocketAddr>,
}

/// Statistics for nerds
#[derive(Debug, FromArgs)]
#[argh(subcommand, name = "stats")]
pub struct StatsCommand {}

/// List all active peers
#[derive(Debug, FromArgs)]
#[argh(subcommand, name = "peers")]
pub struct PeersCommand {}

/// Gracefully shut down and exit
#[derive(Debug, FromArgs)]
#[argh(subcommand, name = "quit")]
pub struct QuitCommand {}

/// Clear the console
#[derive(Debug, FromArgs)]
#[argh(subcommand, name = "clear")]
pub struct ClearCommand {}

/// Root command for REPL parsing
#[derive(Debug, FromArgs)]
pub struct Repl {
	#[argh(subcommand)]
	pub command: ReplCommands,
}

impl Repl {
	/// Parse a REPL command from a line of input.
	pub fn parse_line(line: &str) -> Result<Self, String> {
		let parts: Vec<&str> = line.split_whitespace().collect();
		if parts.is_empty() {
			return Err("empty command".to_string());
		}

		// argh expects argv[0] to be the program name
		let mut args: Vec<&str> = vec!["repl"];
		args.extend(parts);

		let args: Vec<String> = args.iter().map(|s| (*s).to_string()).collect();
		let args_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();

		Repl::from_args(&args_refs[..1], &args_refs[1..]).map_err(|e| e.output)
	}

	/// Get available command names for completion.
	pub fn command_names() -> Vec<&'static str> {
		vec!["listen", "connect", "stats", "peers", "quit", "clear"]
	}
}
