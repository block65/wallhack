use clap::{Args, Parser, Subcommand};
use clap_complete::Shell;
use clap_verbosity_flag::Verbosity;
use std::{net::SocketAddr, path::PathBuf};

use crate::{
	dns::{ResolvableAddress, parse_str_to_addr},
	output::OutputFormat,
	styles::CLAP_STYLES,
};

/// Arguments for the listen command.
#[derive(Args, Debug)]
pub struct ListenArgs {
	/// Local address and port to listen on
	#[arg(default_value = "[::]:6565")]
	pub addr: ResolvableAddress, // should this really be a plain SocketAddr?

	#[command(flatten)]
	pub tls: Option<ServerTlsArgs>,

	/// Path to the CA roots file for mutual TLS (mTLS) client authentication.
	#[arg(long = "ca", short = None, value_name = "FILE_PATH")]
	pub ca_roots: Option<PathBuf>,
}

#[derive(Args, Debug)]
#[group(requires = "cert_pem_file", requires = "key_pem_file")]
pub struct ServerTlsArgs {
	/// Path to the TLS certificate file
	#[arg(
		long = "cert",
		short = 'c',
		value_name = "FILE_PATH",
		required = false,
		requires = "key_pem_file"
	)]
	pub cert_pem_file: PathBuf,

	/// Path to the TLS private key file
	#[arg(
		long = "key",
		short = 'k',
		value_name = "FILE_PATH",
		required = false,
		requires = "cert_pem_file"
	)]
	pub key_pem_file: PathBuf,
}

/// Common TLS arguments for client and server authentication.
#[derive(Args, Debug, Clone)]
#[group(requires = "cert_pem_file", requires = "key_pem_file")]
pub struct ClientTlsArgs {
	/// Path to the TLS certificate file. For mutual TLS (mTLS)
	#[arg(
		long = "cert",
		short = 'c',
		value_name = "FILE_PATH",
		required = false,
		requires = "key_pem_file"
	)]
	pub cert_pem_file: PathBuf,

	/// Path to the TLS private key file. For mutual TLS (mTLS)
	#[arg(
		long = "key",
		short = 'k',
		value_name = "FILE_PATH",
		required = false,
		requires = "cert_pem_file"
	)]
	pub key_pem_file: PathBuf,

	/// Path to the CA roots file for mutual TLS (mTLS) client authentication.
	#[arg(long = "ca", short = None, value_name = "FILE_PATH")]
	pub ca_roots: Option<PathBuf>,

	/// Hostname for the server certificate verification.
	pub hostname: Option<String>,
}

/// Arguments for the connect command.
#[derive(Args, Debug, Clone)]
pub struct ConnectArgs {
	/// Target address in the format `<hostname_or_ip>:<port>`
	pub target: ResolvableAddress,

	/// DNS server to use for hostname lookup (e.g., `8.8.8.8` or
	/// `1.1.1.1:53`)
	#[clap(long = "dns", short = 'd', value_parser = parse_str_to_addr)]
	pub dns_server: Option<SocketAddr>,

	#[command(flatten)]
	pub tls: Option<ClientTlsArgs>,

	/// Connect timeout
	#[arg(long, short = 't', value_name = "SECONDS", default_value_t = 30)]
	pub timeout: u64,
}

/// A versatile tunneling and pivoting tool for network penetration
/// testing.
#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
#[clap(propagate_version = true)]
#[clap(styles = CLAP_STYLES)]
pub struct AgentCli {
	/// The main command to execute.
	#[command(subcommand)]
	pub command: CliCommands,

	#[command(flatten)]
	pub globals: GlobalArgs,
}

/// A versatile tunneling and pivoting tool for network penetration
/// testing.
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
#[command(propagate_version = true)]
#[clap(styles = CLAP_STYLES)]
pub struct HostCli {
	/// The main command to execute.
	#[command(subcommand)]
	pub command: CliCommands,

	/// tun interface to receive packets on
	#[arg(long, short = 't', value_name = "IFACE")]
	pub tun: Option<String>,

	#[command(flatten)]
	pub globals: GlobalArgs,
}

#[derive(Args, Debug)]
pub struct GlobalArgs {
	/// Output format.
	#[arg(long, global = true, value_enum, default_value_t = OutputFormat::Plain)]
	pub output_format: OutputFormat,

	#[command(flatten)]
	pub verbosity: Verbosity, // Use the standard Verbosity type

	/// Configures color output (auto, always, never).
	#[arg(long, global = true, default_value = "auto", value_name = "WHEN")]
	pub color: String, // Consider using an enum if you validate values

	/// If provided, outputs the completion file for the given shell.
	#[arg(hide = true, long = "generate", value_enum, global = true)]
	pub generator: Option<Shell>,
}

#[derive(Subcommand, Debug)]
pub enum CliCommands {
	/// Start a listener for incoming agent connections.
	Listen(ListenArgs),
	/// Connect to a listening agent.
	Connect(ConnectArgs),
	// You might add other top-level commands here later (e.g., Inject, etc.)
}
