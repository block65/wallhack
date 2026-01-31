//! Unified CLI for wallhack binary.
//!
//! This module contains the argument parsing logic for the wallhack binary. It
//! uses `argh` to parse command line arguments into a `WallhackCli` struct.

use std::path::PathBuf;

use argh::FromArgs;

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

	/// target to connect to (hostname:port)
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

	/// TLS hostname for verification (defaults to target hostname)
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

	/// exit node identifier for stable TUN naming (exit nodes only) If not
	/// specified, a random ID is generated.
	#[argh(option, short = 'i')]
	pub exit_id: Option<String>,
}

impl WallhackCli {
	/// Determine the node role based on flag combinations.
	///
	/// - Entry: `--listen` only (or no args) - creates TUN, accepts exit nodes
	/// - Exit: `--connect` only - connects to entry/relay, executes syscalls
	/// - Relay: `--listen` AND `--connect` - forwards between upstream and
	///   downstream
	#[must_use]
	pub fn node_role(&self) -> NodeRole {
		let has_listen = self.listen.is_some();
		let has_connect = self.connect.is_some();

		match (has_listen, has_connect) {
			// Entry: listen only, or no args (defaults to listen on :6565)
			(true | false, false) => NodeRole::Entry,

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

	/// Returns the ID for exit nodes. Uses user-provided ID or generates a random
	/// 8-character hex string.
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
