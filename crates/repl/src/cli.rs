//! CLI argument parsing using argh.
//!
//! This module is the only place that depends on the argument parsing library.
//! It converts parsed arguments into the internal config types defined in `config.rs`.

use std::{path::PathBuf, str::FromStr};

use anyhow::{Result, anyhow};
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
	/// Converts the CLI arguments into a configuration.
	///
	/// # Errors
	///
	/// Returns an error if:
	/// - The global arguments (like output format or color) are invalid.
	/// - The specific command cannot be parsed or converted.
	pub fn into_config(self) -> Result<HostConfig> {
		let globals = parse_globals(
			&self.output_format,
			self.verbose,
			self.debug,
			self.quiet,
			&self.color,
		)?;
		let command = self.command.into_command()?;

		Ok(HostConfig { command, globals })
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
	/// Converts the CLI arguments into a configuration.
	///
	/// # Errors
	/// Returns an error if:
	/// - The global arguments (like output format or color) are invalid.
	/// - The specific command cannot be parsed or converted.
	pub fn into_config(self) -> Result<AgentConfig> {
		let globals = parse_globals(
			&self.output_format,
			self.verbose,
			self.debug,
			self.quiet,
			&self.color,
		)?;
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
) -> Result<GlobalConfig> {
	let output_format = match output_format {
		"plain" => OutputFormat::Plain,
		"json" => OutputFormat::Json,
		other => return Err(anyhow!("invalid output format: {other}")),
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
		other => return Err(anyhow!("invalid color choice: {other}")),
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
	fn into_command(self) -> Result<Command> {
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
	fn into_config(self) -> Result<ListenConfig> {
		let addr = ResolvableAddress::from_str(&self.addr)?;

		let tls = match (self.cert, self.key) {
			(Some(cert), Some(key)) => Some(ServerTlsConfig {
				cert_pem_file: cert,
				key_pem_file: key,
				ca_roots: self.ca,
			}),
			(None, None) => None,
			_ => return Err(anyhow!("--cert and --key must both be provided for TLS")),
		};

		Ok(ListenConfig { addr, tls })
	}
}

/// Connect to a listening agent.
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "connect")]
struct ConnectCmd {
	/// target address in the format <`hostname_or_ip>`:<port>
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
	fn into_config(self) -> Result<ConnectConfig> {
		let target = ResolvableAddress::from_str(&self.target)?;

		let dns_server = self.dns.map(|s| parse_str_to_addr(&s)).transpose()?;

		let tls = match (self.cert, self.key) {
			(Some(cert), Some(key)) => Some(ClientTlsConfig {
				cert_pem_file: cert,
				key_pem_file: key,
				ca_roots: self.ca,
				hostname: self.hostname,
			}),
			(None, None) => {
				if self.hostname.is_some() || self.ca.is_some() {
					return Err(anyhow!(
						"--hostname and --ca require --cert and --key for mTLS"
					));
				}
				None
			}
			_ => return Err(anyhow!("--cert and --key must both be provided for mTLS")),
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
#[must_use]
pub fn parse_host() -> HostCli {
	argh::from_env()
}

/// Parse CLI arguments. Returns the parsed CLI or prints help/error and exits.
#[must_use]
pub fn parse_agent() -> AgentCli {
	argh::from_env()
}

// ============================================================================
// New unified CLI (Single binary architecture)
// ============================================================================

/// Node role determined by flag combinations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeRole {
	/// Entry node: has TUN, listens for connections, runs interactive REPL
	Entry,
	/// Relay node: connects upstream, listens for downstream
	Relay,
	/// Exit node: connects only, makes syscalls to local network
	Exit,
}

/// Unified CLI for wallhack binary.
#[derive(FromArgs, Debug)]
#[argh(description = "Network pivoting and tunneling tool")]
pub struct WallhackCli {
	/// listen address for incoming connections (e.g., ":6565" or "0.0.0.0:6565")
	#[argh(option, short = 'l')]
	pub listen: Option<String>,

	/// target to connect to (host:port)
	#[argh(option, short = 'c')]
	pub connect: Option<String>,

	/// TLS certificate file
	#[argh(option)]
	pub cert: Option<PathBuf>,

	/// TLS private key file
	#[argh(option)]
	pub key: Option<PathBuf>,

	/// CA roots file for mTLS verification
	#[argh(option)]
	pub ca: Option<PathBuf>,

	/// DNS server for target resolution
	#[argh(option, short = 'd')]
	pub dns: Option<String>,

	/// TLS hostname for verification (defaults to target host)
	#[argh(option)]
	pub hostname: Option<String>,

	/// connection timeout in seconds
	#[argh(option, short = 't', default = "10")]
	pub timeout: u64,

	/// verbose output (-v for verbose, -vv for debug)
	#[argh(switch, short = 'v')]
	pub verbose: bool,

	/// extra verbose (debug level)
	#[argh(switch)]
	pub debug: bool,

	/// quiet mode (errors only)
	#[argh(switch, short = 'q')]
	pub quiet: bool,

	/// agent identifier for stable TUN naming (exit nodes only)
	/// If not specified, a random ID is generated.
	#[argh(option, short = 'i')]
	pub agent_id: Option<String>,
}

impl WallhackCli {
	/// Determine the node role based on flag combinations.
	///
	/// - Entry: `--listen` only (or no args) - creates TUN, accepts agents
	/// - Exit: `--connect` only - connects to entry/relay, executes syscalls
	/// - Relay: `--listen` AND `--connect` - forwards between upstream and downstream
	#[must_use]
	pub fn node_role(&self) -> NodeRole {
		let has_listen = self.listen.is_some();
		let has_connect = self.connect.is_some();

		match (has_listen, has_connect) {
			// Entry: listen only, or no args (defaults to listen on :6565)
			(true, false) | (false, false) => NodeRole::Entry,

			// Relay: both listen and connect
			(true, true) => NodeRole::Relay,

			// Exit: connect only
			(false, true) => NodeRole::Exit,
		}
	}

	/// Returns the listen address, defaulting to ":6565" for entry nodes.
	#[must_use]
	pub fn listen_addr(&self) -> &str {
		self.listen.as_deref().unwrap_or(":6565")
	}

	/// Returns the agent ID for exit nodes.
	/// Uses user-provided ID or generates a random 8-character hex string.
	#[must_use]
	pub fn agent_id(&self) -> String {
		self.agent_id.clone().unwrap_or_else(|| {
			use rand::Rng;
			let mut rng = rand::rng();
			let id: u32 = rng.random();
			format!("{id:08x}")
		})
	}
}

/// Parse unified CLI from command line arguments.
#[must_use]
pub fn parse_wallhack() -> WallhackCli {
	argh::from_env()
}

// ============================================================================
// Protocol parsing (docker-style port specs)
// ============================================================================

/// Network protocol for transport selection.
///
/// Determined from docker-style port specs:
/// - `host:6565` or `host:6565/udp` → UDP (QUIC)
/// - `host:6565/tcp` → TCP (WebSocket)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Protocol {
	/// UDP transport (QUIC) - default, better performance
	#[default]
	Udp,
	/// TCP transport (WebSocket) - for proxy traversal
	Tcp,
}

/// Parsed address with protocol.
#[derive(Debug, Clone)]
pub struct AddressSpec {
	/// The address without protocol suffix
	pub addr: String,
	/// The transport protocol
	pub protocol: Protocol,
}

impl AddressSpec {
	/// Parse an address spec in docker-style format.
	///
	/// Formats:
	/// - `host:port` → UDP (default)
	/// - `host:port/udp` → UDP
	/// - `host:port/tcp` → TCP
	/// - `:port` → listen on all interfaces, UDP
	/// - `:port/tcp` → listen on all interfaces, TCP
	#[must_use]
	pub fn parse(s: &str) -> Self {
		if let Some(addr) = s.strip_suffix("/tcp") {
			Self {
				addr: addr.to_string(),
				protocol: Protocol::Tcp,
			}
		} else if let Some(addr) = s.strip_suffix("/udp") {
			Self {
				addr: addr.to_string(),
				protocol: Protocol::Udp,
			}
		} else {
			Self {
				addr: s.to_string(),
				protocol: Protocol::Udp,
			}
		}
	}
}

impl WallhackCli {
	/// Returns the parsed listen address spec.
	#[must_use]
	pub fn listen_spec(&self) -> AddressSpec {
		AddressSpec::parse(self.listen.as_deref().unwrap_or(":6565"))
	}

	/// Returns the parsed connect address spec, if present.
	#[must_use]
	pub fn connect_spec(&self) -> Option<AddressSpec> {
		self.connect.as_deref().map(AddressSpec::parse)
	}
}
