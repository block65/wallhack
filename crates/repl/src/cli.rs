//! CLI argument parsing using argh.
//!
//! This module is the only place that depends on the argument parsing library.
//! It converts parsed arguments into the internal config types defined in `config.rs`.

use std::{path::PathBuf, str::FromStr};

use argh::FromArgs;

use crate::{
	config::{
		AgentConfig, ClientTlsConfig, ColorChoice, Command, ConnectConfig, GlobalConfig,
		HostConfig, ListenConfig, OutputFormat, ServerTlsConfig, Verbosity,
	},
	dns::{ResolvableAddress, parse_str_to_addr},
};

/// A versatile tunneling and pivoting tool for network penetration testing.
#[derive(FromArgs, Debug)]
pub struct HostCli {
	#[argh(subcommand)]
	command: CliCommand,

	/// tun interface to receive packets on
	#[argh(option, short = 't')]
	tun: Option<String>,

	/// output format: plain, json
	#[argh(option, default = "\"plain\".to_string()")]
	output_format: String,

	/// verbosity: -v for verbose, -vv for debug, -q for quiet
	#[argh(switch, short = 'v')]
	verbose: bool,

	/// extra verbose (debug level)
	#[argh(switch)]
	debug: bool,

	/// quiet mode
	#[argh(switch, short = 'q')]
	quiet: bool,

	/// color output: auto, always, never
	#[argh(option, default = "\"auto\".to_string()")]
	color: String,
}

impl HostCli {
	pub fn into_config(self) -> Result<HostConfig, String> {
		let globals = parse_globals(&self.output_format, self.verbose, self.debug, self.quiet, &self.color)?;
		let command = self.command.into_command()?;

		Ok(HostConfig {
			command,
			tun: self.tun,
			globals,
		})
	}
}

/// A versatile tunneling and pivoting tool for network penetration testing.
#[derive(FromArgs, Debug)]
pub struct AgentCli {
	#[argh(subcommand)]
	command: CliCommand,

	/// output format: plain, json
	#[argh(option, default = "\"plain\".to_string()")]
	output_format: String,

	/// verbosity: -v for verbose, -vv for debug, -q for quiet
	#[argh(switch, short = 'v')]
	verbose: bool,

	/// extra verbose (debug level)
	#[argh(switch)]
	debug: bool,

	/// quiet mode
	#[argh(switch, short = 'q')]
	quiet: bool,

	/// color output: auto, always, never
	#[argh(option, default = "\"auto\".to_string()")]
	color: String,
}

impl AgentCli {
	pub fn into_config(self) -> Result<AgentConfig, String> {
		let globals = parse_globals(&self.output_format, self.verbose, self.debug, self.quiet, &self.color)?;
		let command = self.command.into_command()?;

		Ok(AgentConfig { command, globals })
	}
}

fn parse_globals(
	output_format: &str,
	verbose: bool,
	debug: bool,
	quiet: bool,
	color: &str,
) -> Result<GlobalConfig, String> {
	let output_format = match output_format {
		"plain" => OutputFormat::Plain,
		"json" => OutputFormat::Json,
		other => return Err(format!("invalid output format: {other}")),
	};

	let verbosity = if quiet {
		Verbosity::Quiet
	} else if debug {
		Verbosity::Debug
	} else if verbose {
		Verbosity::Verbose
	} else {
		Verbosity::Normal
	};

	let color = match color {
		"auto" => ColorChoice::Auto,
		"always" => ColorChoice::Always,
		"never" => ColorChoice::Never,
		other => return Err(format!("invalid color choice: {other}")),
	};

	Ok(GlobalConfig {
		output_format,
		verbosity,
		color,
	})
}

#[derive(FromArgs, Debug)]
#[argh(subcommand)]
enum CliCommand {
	Listen(ListenCmd),
	Connect(ConnectCmd),
}

impl CliCommand {
	fn into_command(self) -> Result<Command, String> {
		match self {
			Self::Listen(cmd) => cmd.into_config().map(Command::Listen),
			Self::Connect(cmd) => cmd.into_config().map(Command::Connect),
		}
	}
}

/// Start a listener for incoming agent connections.
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "listen")]
struct ListenCmd {
	/// local address and port to listen on (default: [::]:6565)
	#[argh(positional, default = "\"[::]:6565\".to_string()")]
	addr: String,

	/// path to the TLS certificate file
	#[argh(option, short = 'c')]
	cert: Option<PathBuf>,

	/// path to the TLS private key file
	#[argh(option, short = 'k')]
	key: Option<PathBuf>,

	/// path to the CA roots file for mTLS client authentication
	#[argh(option)]
	ca: Option<PathBuf>,
}

impl ListenCmd {
	fn into_config(self) -> Result<ListenConfig, String> {
		let addr = ResolvableAddress::from_str(&self.addr)?;

		let tls = match (self.cert, self.key) {
			(Some(cert), Some(key)) => Some(ServerTlsConfig {
				cert_pem_file: cert,
				key_pem_file: key,
				ca_roots: self.ca,
			}),
			(None, None) => None,
			_ => return Err("--cert and --key must both be provided for TLS".to_string()),
		};

		Ok(ListenConfig { addr, tls })
	}
}

/// Connect to a listening agent.
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "connect")]
struct ConnectCmd {
	/// target address in the format <hostname_or_ip>:<port>
	#[argh(positional)]
	target: String,

	/// DNS server to use for hostname lookup (e.g., 8.8.8.8 or 1.1.1.1:53)
	#[argh(option, short = 'd')]
	dns: Option<String>,

	/// path to the TLS certificate file (for mTLS)
	#[argh(option, short = 'c')]
	cert: Option<PathBuf>,

	/// path to the TLS private key file (for mTLS)
	#[argh(option, short = 'k')]
	key: Option<PathBuf>,

	/// path to the CA roots file for mTLS
	#[argh(option)]
	ca: Option<PathBuf>,

	/// hostname for server certificate verification
	#[argh(option)]
	hostname: Option<String>,

	/// connect timeout in seconds (default: 30)
	#[argh(option, short = 't', default = "30")]
	timeout: u64,
}

impl ConnectCmd {
	fn into_config(self) -> Result<ConnectConfig, String> {
		let target = ResolvableAddress::from_str(&self.target)?;

		let dns_server = self
			.dns
			.map(|s| parse_str_to_addr(&s))
			.transpose()?;

		let tls = match (self.cert, self.key) {
			(Some(cert), Some(key)) => Some(ClientTlsConfig {
				cert_pem_file: cert,
				key_pem_file: key,
				ca_roots: self.ca,
				hostname: self.hostname,
			}),
			(None, None) => {
				if self.hostname.is_some() || self.ca.is_some() {
					return Err(
						"--hostname and --ca require --cert and --key for mTLS".to_string(),
					);
				}
				None
			}
			_ => return Err("--cert and --key must both be provided for mTLS".to_string()),
		};

		Ok(ConnectConfig {
			target,
			dns_server,
			tls,
			timeout_secs: self.timeout,
		})
	}
}

/// Parse CLI arguments. Returns the parsed CLI or prints help/error and exits.
pub fn parse_host() -> HostCli {
	argh::from_env()
}

/// Parse CLI arguments. Returns the parsed CLI or prints help/error and exits.
pub fn parse_agent() -> AgentCli {
	argh::from_env()
}
