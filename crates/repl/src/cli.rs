//! CLI for the wallhack binary.
//!
//! Node role is declared explicitly via subcommand (`entry`, `exit`, `relay`).
//! Transport direction (`--listen` / `--connect`) is independent of role.
//!
//! # Examples
//!
//! ```text
//! wallhack                                         # entry, listen :6565
//! wallhack entry --listen :6565                    # entry, listen
//! wallhack entry --connect host:443                # entry, reverse tunnel
//! wallhack exit --connect host:6565                # exit, connect
//! wallhack exit --listen :443                      # exit, reverse tunnel
//! wallhack relay --connect up:443 --listen :6565   # relay, both required
//! ```

use std::path::PathBuf;

use argh::FromArgs;

/// Network pivoting and tunneling tool.
///
/// Defaults to entry mode listening on :6565 when invoked without a subcommand.
#[derive(FromArgs, Debug)]
pub struct WallhackCli {
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

	/// TLS hostname for verification (defaults to target hostname)
	#[argh(option)]
	pub hostname: Option<String>,

	/// connection timeout in seconds
	#[argh(option, short = 't', default = "10")]
	pub timeout: u64,

	/// verbose output
	#[argh(switch, short = 'v')]
	pub verbose: bool,

	/// extra verbose (debug level)
	#[argh(switch)]
	pub debug: bool,

	/// quiet mode (errors only)
	#[argh(switch, short = 'q')]
	pub quiet: bool,

	#[argh(subcommand)]
	pub command: Option<Command>,
}

/// Subcommand that determines the node role.
#[derive(FromArgs, Debug)]
#[argh(subcommand)]
pub enum Command {
	Entry(EntryCommand),
	Exit(ExitCommand),
	Relay(RelayCommand),
}

/// Entry node: creates TUN interface, routes traffic, runs interactive REPL.
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "entry")]
pub struct EntryCommand {
	/// listen address for incoming connections (e.g. ":6565")
	#[argh(option, short = 'l')]
	pub listen: Option<String>,

	/// connect to a peer (e.g. "host:6565") for reverse tunnels
	#[argh(option, short = 'c')]
	pub connect: Option<String>,

	/// REST API address (e.g. "127.0.0.1:6566")
	#[argh(option)]
	pub api: Option<String>,

	/// REST API username for basic auth
	#[argh(option)]
	pub api_user: Option<String>,

	/// REST API password for basic auth
	#[argh(option)]
	pub api_pass: Option<String>,
}

/// Exit node: makes syscalls to the local network on behalf of the tunnel.
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "exit")]
pub struct ExitCommand {
	/// listen address for incoming connections (e.g. ":443") for reverse tunnels
	#[argh(option, short = 'l')]
	pub listen: Option<String>,

	/// connect to a peer (e.g. "host:6565")
	#[argh(option, short = 'c')]
	pub connect: Option<String>,

	/// stable identifier for TUN naming; random if omitted
	#[argh(option, short = 'i')]
	pub exit_id: Option<String>,
}

/// Relay node: forwards traffic between upstream and downstream peers.
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "relay")]
pub struct RelayCommand {
	/// listen address for downstream connections (e.g. ":6565")
	#[argh(option, short = 'l')]
	pub listen: Option<String>,

	/// connect to upstream peer (e.g. "host:6565")
	#[argh(option, short = 'c')]
	pub connect: Option<String>,
}

// ============================================================================
// Transport direction
// ============================================================================

/// The resolved transport direction for a node.
#[derive(Debug, Clone)]
pub enum TransportDir {
	/// Node listens for incoming connections.
	Listen(AddressSpec),
	/// Node connects to a remote peer.
	Connect(AddressSpec),
}

impl EntryCommand {
	/// Resolve the transport direction.
	///
	/// Defaults to `Listen(":6565")` when neither flag is provided.
	///
	/// # Errors
	///
	/// Returns error if both `--listen` and `--connect` are specified.
	pub fn transport(&self) -> Result<TransportDir, String> {
		match (&self.listen, &self.connect) {
			(Some(_), Some(_)) => Err("entry requires exactly one of --listen or --connect".into()),
			(Some(addr), None) => Ok(TransportDir::Listen(AddressSpec::parse(addr))),
			(None, Some(addr)) => Ok(TransportDir::Connect(AddressSpec::parse(addr))),
			(None, None) => Ok(TransportDir::Listen(AddressSpec::parse(":6565"))),
		}
	}

	/// Returns the API address if specified, parsing to [`std::net::SocketAddr`].
	#[must_use]
	pub fn api_addr(&self) -> Option<std::net::SocketAddr> {
		self.api.as_ref().map(|addr| {
			addr.parse()
				.unwrap_or_else(|_| std::net::SocketAddr::from(([127, 0, 0, 1], 6566)))
		})
	}
}

impl ExitCommand {
	/// Resolve the transport direction.
	///
	/// No default — one of `--listen` or `--connect` is required.
	///
	/// # Errors
	///
	/// Returns error if neither or both flags are specified.
	pub fn transport(&self) -> Result<TransportDir, String> {
		match (&self.listen, &self.connect) {
			(Some(_), Some(_)) => Err("exit requires exactly one of --listen or --connect".into()),
			(Some(addr), None) => Ok(TransportDir::Listen(AddressSpec::parse(addr))),
			(None, Some(addr)) => Ok(TransportDir::Connect(AddressSpec::parse(addr))),
			(None, None) => Err("exit requires --listen or --connect".into()),
		}
	}

	/// Returns the exit node ID, generating a random one if not specified.
	#[must_use]
	pub fn exit_id(&self) -> String {
		self.exit_id.clone().unwrap_or_else(|| {
			use rand::Rng;
			let mut rng = rand::rng();
			let id: u32 = rng.random();
			format!("{id:08x}")
		})
	}
}

impl RelayCommand {
	/// Resolve both transport directions.
	///
	/// Relay requires **both** `--listen` and `--connect`.
	///
	/// # Errors
	///
	/// Returns error if either flag is missing.
	pub fn transport(&self) -> Result<(AddressSpec, AddressSpec), String> {
		match (&self.connect, &self.listen) {
			(Some(connect), Some(listen)) => {
				Ok((AddressSpec::parse(connect), AddressSpec::parse(listen)))
			}
			(None, _) => Err("relay requires --connect (upstream peer)".into()),
			(_, None) => Err("relay requires --listen (downstream port)".into()),
		}
	}
}

/// Parse CLI from command line arguments.
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
/// - `hostname:6565` or `hostname:6565/udp` → UDP (QUIC)
/// - `hostname:6565/tcp` → TCP (WebSocket)
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
	/// - `hostname:port` → UDP (default)
	/// - `hostname:port/udp` → UDP
	/// - `hostname:port/tcp` → TCP
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
