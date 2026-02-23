//! CLI for the wallhack binary.
//!
//! Node role is declared explicitly via subcommand (`entry`, `exit`, `relay`).
//! Transport direction (`--listen` / `--connect`) is independent of role.
//!
//! # Examples
//!
//! ```text
//! wallhack
//! wallhack entry --listen :6565
//! wallhack entry --connect host:443
//! wallhack exit --connect host:6565
//! wallhack exit --listen :443
//! wallhack relay --connect up:443 --listen :6565
//! ```

use std::path::PathBuf;

use argh::FromArgs;

/// Network pivoting and tunneling tool.
///
/// Defaults to entry mode listening on the default port when invoked without a subcommand.
#[allow(clippy::struct_excessive_bools)] // Independent CLI flags, not related state
#[derive(FromArgs, Debug, Clone)]
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

	/// pre-shared key for tunnel authentication (or set `WALLHACK_PSK` env var)
	#[argh(option)]
	pub psk: Option<String>,

	/// verbose output
	#[argh(switch, short = 'v')]
	pub verbose: bool,

	/// extra verbose (debug level tracing)
	#[argh(switch)]
	pub debug: bool,

	/// comma-separated module substring filters for debug tracing
	#[argh(option)]
	pub debug_filter: Option<String>,

	/// trace level tracing (most verbose)
	#[argh(switch)]
	pub trace: bool,

	/// comma-separated module substring filters for trace tracing
	#[argh(option)]
	pub trace_filter: Option<String>,

	/// quiet mode (errors only)
	#[argh(switch, short = 'q')]
	pub quiet: bool,

	/// print version information and exit
	#[argh(switch)]
	pub version: bool,

	#[argh(subcommand)]
	pub command: Option<Command>,
}

/// Subcommand that determines the node role.
#[derive(FromArgs, Debug, Clone)]
#[argh(subcommand)]
pub enum Command {
	Entry(EntryCommand),
	Exit(ExitCommand),
	Relay(RelayCommand),
}

/// Entry node: creates TUN interface, routes traffic, runs interactive REPL.
#[derive(FromArgs, Debug, Clone)]
#[argh(subcommand, name = "entry")]
pub struct EntryCommand {
	/// name for this node; used for identification (random if omitted)
	#[argh(option, short = 'n')]
	pub name: Option<String>,

	/// listen address for incoming connections (e.g. ":6565")
	#[argh(option, short = 'l')]
	pub listen: Option<String>,

	/// connect to a peer (e.g. "host:6565")
	#[argh(option, short = 'c')]
	pub connect: Option<String>,

	/// REST API address (e.g. "127.0.0.1:6566")
	#[argh(option)]
	pub api: Option<String>,

	/// REST API username for basic auth (default: admin)
	#[argh(option)]
	pub api_user: Option<String>,

	/// REST API secret for basic auth (default: auto-generated, printed on startup)
	#[argh(option)]
	pub api_secret: Option<String>,

	/// maximum number of concurrent peer connections
	#[argh(option)]
	pub max_peers: Option<usize>,

	/// skip SYN proxy verification (optimistic JIT, faster but less accurate port scanning)
	#[argh(switch)]
	pub fast: bool,
}

/// Exit node: makes syscalls to the local network on behalf of the tunnel.
#[derive(FromArgs, Debug, Clone)]
#[argh(subcommand, name = "exit")]
pub struct ExitCommand {
	/// listen address for incoming connections (e.g. ":443")
	#[argh(option, short = 'l')]
	pub listen: Option<String>,

	/// connect to a peer (e.g. "host:6565")
	#[argh(option, short = 'c')]
	pub connect: Option<String>,

	/// name for this peer; used for TUN naming and identification (random if omitted)
	#[argh(option, short = 'n')]
	pub name: Option<String>,

	/// accept server certificate by fingerprint (e.g. "sha256:abc123...")
	#[argh(option)]
	pub accept_fingerprint: Option<String>,
}

/// Relay node: forwards traffic between peers.
#[derive(FromArgs, Debug, Clone)]
#[argh(subcommand, name = "relay")]
pub struct RelayCommand {
	/// node name (default: random 8-char hex)
	#[argh(option, short = 'n')]
	pub name: Option<String>,

	/// listen address for relay connections (e.g. ":6565")
	#[argh(option, short = 'l')]
	pub listen: Option<String>,

	/// connect to a peer (e.g. "host:6565")
	#[argh(option, short = 'c')]
	pub connect: Option<String>,

	/// accept server certificate by fingerprint (e.g. "sha256:abc123...")
	#[argh(option)]
	pub accept_fingerprint: Option<String>,
}

/// Generate a random node name (8-character hex ID).
fn generate_node_name() -> String {
	use rand::Rng;
	let mut rng = rand::rng();
	let id: u32 = rng.random();
	format!("{id:08x}")
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
	/// Node has both connect and listen (relay capability).
	Both {
		connect: AddressSpec,
		listen: AddressSpec,
	},
}

impl EntryCommand {
	/// Returns the node name, generating a random one if not specified.
	#[must_use]
	pub fn name(&self) -> String {
		self.name.clone().unwrap_or_else(generate_node_name)
	}

	/// Resolve the transport direction.
	///
	/// Defaults to listening on the default port when neither flag is provided.
	///
	/// # Errors
	///
	/// Returns error if both `--listen` and `--connect` are specified.
	pub fn transport(&self) -> Result<TransportDir, String> {
		match (&self.listen, &self.connect) {
			(Some(_), Some(_)) => Err("entry requires exactly one of --listen or --connect".into()),
			(Some(addr), None) => Ok(TransportDir::Listen(AddressSpec::parse(addr))),
			(None, Some(addr)) => Ok(TransportDir::Connect(AddressSpec::parse(addr))),
			(None, None) => {
				let default_port = wallhack_core::server::config::DEFAULT_LISTEN_PORT;
				Ok(TransportDir::Listen(AddressSpec::parse(&format!(
					":{default_port}"
				))))
			}
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
	/// No default — one or both of `--listen` or `--connect` is required. When
	/// both are specified, the exit node gains relay capability.
	///
	/// # Errors
	///
	/// Returns error if neither flag is specified.
	pub fn transport(&self) -> Result<TransportDir, String> {
		match (&self.listen, &self.connect) {
			(Some(listen), Some(connect)) => Ok(TransportDir::Both {
				connect: AddressSpec::parse(connect),
				listen: AddressSpec::parse(listen),
			}),
			(Some(addr), None) => Ok(TransportDir::Listen(AddressSpec::parse(addr))),
			(None, Some(addr)) => Ok(TransportDir::Connect(AddressSpec::parse(addr))),
			(None, None) => Err("exit requires --listen or --connect".into()),
		}
	}

	/// Returns the peer name, generating a random one if not specified.
	#[must_use]
	pub fn name(&self) -> String {
		self.name.clone().unwrap_or_else(generate_node_name)
	}
}

impl RelayCommand {
	/// Returns the node name, generating a random one if not specified.
	#[must_use]
	pub fn name(&self) -> String {
		self.name.clone().unwrap_or_else(generate_node_name)
	}

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

/// Known subcommand names.
const SUBCOMMANDS: &[&str] = &["entry", "exit", "relay"];

/// Global switches that belong before the subcommand.
const GLOBAL_FLAGS: &[&str] = &[
	"--debug",
	"--trace",
	"-v",
	"--verbose",
	"-q",
	"--quiet",
	"--version",
];

/// Flags that belong to a subcommand, used by `suggest_subcommand` for detection.
const SUBCOMMAND_FLAGS: &[&str] = &["--listen", "-l", "--connect", "-c"];

/// Reorder global flags that appear after the subcommand to before it.
fn reorder_global_flags(args: Vec<String>) -> Vec<String> {
	// Find the subcommand position (skip argv[0])
	let sub_pos = args[1..]
		.iter()
		.position(|a| SUBCOMMANDS.contains(&a.as_str()))
		.map(|i| i + 1);

	let Some(sub_pos) = sub_pos else {
		return args;
	};

	let mut before: Vec<String> = args[..sub_pos].to_vec();
	let subcommand = args[sub_pos].clone();
	let mut after: Vec<String> = Vec::new();

	for arg in &args[sub_pos + 1..] {
		if GLOBAL_FLAGS.contains(&arg.as_str()) {
			before.push(arg.clone());
		} else {
			after.push(arg.clone());
		}
	}

	before.push(subcommand);
	before.extend(after);
	before
}

/// Suggest a subcommand when subcommand-level flags are used at the top level.
fn suggest_subcommand(args: &[&str]) -> Option<String> {
	use std::fmt::Write;

	let has_subcommand = args.iter().any(|a| SUBCOMMANDS.contains(a));
	if has_subcommand {
		return None;
	}

	let has_sub_flag = args.iter().any(|a| SUBCOMMAND_FLAGS.contains(a));
	if !has_sub_flag {
		return None;
	}

	let has_listen = args.iter().any(|a| *a == "--listen" || *a == "-l");

	let flag_str: String = args
		.iter()
		.map(|a| (*a).to_string())
		.collect::<Vec<_>>()
		.join(" ");

	let mut lines = String::new();
	if has_listen {
		let _ = writeln!(lines, "  wallhack entry {flag_str}");
		let _ = writeln!(lines, "  wallhack exit {flag_str}");
	} else {
		let _ = writeln!(lines, "  wallhack exit {flag_str}");
		let _ = writeln!(lines, "  wallhack entry {flag_str}");
	}

	let flag_name = if has_listen { "--listen" } else { "--connect" };
	Some(format!(
		"The {flag_name} flag requires a subcommand. Did you mean:\n\n{lines}"
	))
}

/// Extract the binary name from argv[0], like argh does internally.
fn binary_name(argv0: &str) -> &str {
	std::path::Path::new(argv0)
		.file_name()
		.and_then(|s| s.to_str())
		.unwrap_or(argv0)
}

/// Parse CLI from command line arguments.
///
/// Wraps argh with:
/// - Global flag reordering (allows `wallhack entry --debug` in addition to `wallhack --debug entry`)
/// - Better error messages when subcommand-level flags are used without a subcommand
#[must_use]
pub fn parse_cli() -> WallhackCli {
	let args: Vec<String> = std::env::args().collect();
	let reordered = reorder_global_flags(args);
	let cmd = binary_name(&reordered[0]);
	let strs: Vec<&str> = reordered.iter().map(String::as_str).collect();

	match WallhackCli::from_args(&[cmd], &strs[1..]) {
		Ok(cli) => cli,
		Err(early_exit) => {
			if early_exit.status.is_err()
				&& let Some(hint) = suggest_subcommand(&strs[1..])
			{
				eprintln!("{hint}");
				eprintln!("Run {cmd} --help for more information.");
				std::process::exit(1);
			}
			std::process::exit(if let Ok(()) = early_exit.status {
				println!("{}", early_exit.output);
				0
			} else {
				eprintln!(
					"{}\nRun {cmd} --help for more information.",
					early_exit.output
				);
				1
			})
		}
	}
}

impl WallhackCli {
	/// Resolve PSK from `--psk` flag or `WALLHACK_PSK` environment variable.
	#[must_use]
	pub fn resolve_psk(&self) -> Option<String> {
		self.psk
			.clone()
			.or_else(|| std::env::var("WALLHACK_PSK").ok())
	}
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
	/// - `hostname` → UDP, default port (6565)
	#[must_use]
	pub fn parse(s: &str) -> Self {
		if let Some(addr) = s.strip_suffix("/tcp") {
			Self {
				addr: Self::with_default_port(addr),
				protocol: Protocol::Tcp,
			}
		} else if let Some(addr) = s.strip_suffix("/udp") {
			Self {
				addr: Self::with_default_port(addr),
				protocol: Protocol::Udp,
			}
		} else {
			Self {
				addr: Self::with_default_port(s),
				protocol: Protocol::Udp,
			}
		}
	}

	/// Append the default port if `addr` has no port specified.
	///
	/// Handles bracketed IPv6 (`[::1]`, `[::1]:port`) separately from
	/// hostnames and IPv4 (`host`, `host:port`, `:port`).
	fn with_default_port(addr: &str) -> String {
		let has_port = if addr.starts_with('[') {
			// Bracketed IPv6: port is present only when `]:` follows the closing bracket.
			addr.contains("]:")
		} else {
			// Hostname or IPv4: any `:` means a port is already specified.
			addr.contains(':')
		};

		if has_port {
			addr.to_string()
		} else {
			format!(
				"{}:{}",
				addr,
				wallhack_core::server::config::DEFAULT_LISTEN_PORT
			)
		}
	}
}

#[cfg(test)]
mod tests {
	use super::{AddressSpec, Protocol};

	#[test]
	fn address_spec_with_port_unchanged() {
		let spec = AddressSpec::parse("attacker:443");
		assert_eq!(spec.addr, "attacker:443");
		assert_eq!(spec.protocol, Protocol::Udp);
	}

	#[test]
	fn address_spec_no_port_gets_default() {
		let spec = AddressSpec::parse("attacker");
		assert_eq!(
			spec.addr,
			format!(
				"attacker:{}",
				wallhack_core::server::config::DEFAULT_LISTEN_PORT
			)
		);
		assert_eq!(spec.protocol, Protocol::Udp);
	}

	#[test]
	fn address_spec_listen_shorthand_unchanged() {
		let spec = AddressSpec::parse(":6565");
		assert_eq!(spec.addr, ":6565");
		assert_eq!(spec.protocol, Protocol::Udp);
	}

	#[test]
	fn address_spec_tcp_suffix_with_port() {
		let spec = AddressSpec::parse("host:443/tcp");
		assert_eq!(spec.addr, "host:443");
		assert_eq!(spec.protocol, Protocol::Tcp);
	}

	#[test]
	fn address_spec_tcp_suffix_no_port_gets_default() {
		let spec = AddressSpec::parse("host/tcp");
		assert_eq!(
			spec.addr,
			format!(
				"host:{}",
				wallhack_core::server::config::DEFAULT_LISTEN_PORT
			)
		);
		assert_eq!(spec.protocol, Protocol::Tcp);
	}

	#[test]
	fn address_spec_bracketed_ipv6_no_port_gets_default() {
		let spec = AddressSpec::parse("[::1]");
		assert_eq!(
			spec.addr,
			format!(
				"[::1]:{}",
				wallhack_core::server::config::DEFAULT_LISTEN_PORT
			)
		);
		assert_eq!(spec.protocol, Protocol::Udp);
	}

	#[test]
	fn address_spec_bracketed_ipv6_with_port_unchanged() {
		let spec = AddressSpec::parse("[::1]:443");
		assert_eq!(spec.addr, "[::1]:443");
		assert_eq!(spec.protocol, Protocol::Udp);
	}
}
