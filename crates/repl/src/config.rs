//! Internal configuration types - parser agnostic.
//!
//! These types are used throughout the application and have no dependency
//! on any CLI parsing library. The CLI layer converts parsed arguments
//! into these types at the boundary.

use std::{net::SocketAddr, path::PathBuf};

use crate::dns::ResolvableAddress;

/// TLS configuration for client connections (mTLS).
#[derive(Debug, Clone)]
pub struct ClientTlsConfig {
	pub cert_pem_file: PathBuf,
	pub key_pem_file: PathBuf,
	pub ca_roots: Option<PathBuf>,
	pub hostname: Option<String>,
}

/// TLS configuration for server (listener).
#[derive(Debug, Clone)]
pub struct ServerTlsConfig {
	pub cert_pem_file: PathBuf,
	pub key_pem_file: PathBuf,
	pub ca_roots: Option<PathBuf>,
}

/// Configuration for the listen command.
#[derive(Debug, Clone)]
pub struct ListenConfig {
	pub addr: ResolvableAddress,
	pub tls: Option<ServerTlsConfig>,
}

/// Configuration for the connect command.
#[derive(Debug, Clone)]
pub struct ConnectConfig {
	pub target: ResolvableAddress,
	pub dns_server: Option<SocketAddr>,
	pub tls: Option<ClientTlsConfig>,
	pub timeout_secs: u64,
}

/// Global configuration options.
#[derive(Debug, Clone)]
pub struct GlobalConfig {
	pub output_format: OutputFormat,
	pub verbosity: Verbosity,
	pub color: ColorChoice,
}

impl Default for GlobalConfig {
	fn default() -> Self {
		Self {
			output_format: OutputFormat::Plain,
			verbosity: Verbosity::Normal,
			color: ColorChoice::Auto,
		}
	}
}

/// Output format for the application.
#[derive(Debug, Clone, Copy, Default)]
pub enum OutputFormat {
	#[default]
	Plain,
	Json,
}

/// Verbosity level.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum Verbosity {
	Quiet,
	#[default]
	Normal,
	Verbose,
	Debug,
}

/// Color output choice.
#[derive(Debug, Clone, Copy, Default)]
pub enum ColorChoice {
	#[default]
	Auto,
	Always,
	Never,
}

/// Top-level command for the CLI.
#[derive(Debug, Clone)]
pub enum Command {
	Listen(ListenConfig),
	Connect(ConnectConfig),
}

/// Host CLI configuration (combines command + host-specific options).
#[derive(Debug, Clone)]
pub struct HostConfig {
	pub command: Command,
	pub tun: Option<String>,
	pub globals: GlobalConfig,
}

/// Agent CLI configuration.
#[derive(Debug, Clone)]
pub struct AgentConfig {
	pub command: Command,
	pub globals: GlobalConfig,
}