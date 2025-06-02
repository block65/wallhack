use clap::{CommandFactory, Parser};

use repl::{AgentCli, CliCommands, ConnectArgs, ListenArgs};
use wallhack::{
	agent::{self, net::SyscallAgentAdapter},
	client::{
		self,
		client::{Client, ClientRole},
		config::{ClientConfig, MtlsConfig},
	},
	server::{
		self,
		config::ServerConfig,
		server::{Server, ServerRole},
	},
};

#[derive(Debug, thiserror::Error)]
pub enum Error {
	// wallhack::agent::orchestrator::Error
	#[error("Orchestrator error: {0}")]
	Orchestrator(#[from] agent::orchestrator::Error),

	// wallhack::client::quic::Error
	#[error("Client QUIC error: {0}")]
	ClientQuic(#[from] client::quic::Error),

	// wallhack::server::quic::Error
	#[error("Server QUIC error: {0}")]
	ServerQuic(#[from] server::quic::Error),

	// repl::dns::ResolveError
	#[error("DNS resolution error: {0}")]
	DnsResolve(#[from] repl::dns::ResolveError),

	// tracing_subscriber::filter::ParseError
	#[error("Tracing subscriber filter parse error: {0}")]
	TracingFilterParse(#[from] tracing_subscriber::filter::ParseError),
}

pub fn print_completions<G: clap_complete::Generator>(generator: G, cmd: &mut clap::Command) {
	clap_complete::generate(
		generator,
		cmd,
		cmd.get_name().to_string(),
		&mut std::io::stdout(),
	);
}

async fn agent_client(args: ConnectArgs) -> Result<(), Error> {
	repl::info!("resolving {}...", args.target);

	let endpoint = repl::dns::resolve(args.target, args.dns_server).await?;
	tracing::debug!("resolved endpoint: {:#?}", endpoint);

	let client_config = match args.tls {
		Some(tls) => ClientConfig {
			addr: endpoint,
			hostname: tls.hostname,
			mtls: Some(MtlsConfig {
				cert_pem_file: tls.cert_pem_file,
				key_pem_file: tls.key_pem_file,
				ca_roots: tls.ca_roots,
			}),
			..Default::default()
		},
		None => ClientConfig {
			addr: endpoint,
			..Default::default()
		},
	};

	let mut client = client::quic::QuicClient::try_new(client_config)?;

	let connect_result = client.connect(ClientRole::Agent).await?;
	repl::info!("connected to {}", connect_result.client_ident());

	let (instr, responses) = connect_result.channels();

	let adapter = SyscallAgentAdapter::new();
	let orchestrator = agent::Orchestrator::new(std::sync::Arc::new(adapter));
	orchestrator.drive(responses, instr.subscribe()).await?;
	Ok(())
}

async fn agent_server(args: ListenArgs) -> Result<(), Error> {
	let mut server = server::quic::QuicServer::try_new(ServerConfig::default())?;

	repl::info!("agent server listening on {}", args.addr);

	loop {
		match server.accept(ServerRole::Agent).await {
			Ok(Some(accept_result)) => {
				repl::info!("New connection from {}", accept_result.client_ident());

				let (instr, responses) = accept_result.channels();

				let fut = async move {
					let adapter = SyscallAgentAdapter::new();
					let orchestrator: agent::Orchestrator<SyscallAgentAdapter> =
						agent::Orchestrator::new(std::sync::Arc::new(adapter));
					match orchestrator.drive(responses, instr.subscribe()).await {
						Ok(()) => {
							repl::info!("Orchestrator finished successfully");
						}
						Err(e) => {
							repl::error!("Orchestrator encountered an error: {e}");
						}
					}
				};

				tokio::spawn(fut);
			}
			Ok(None) => {
				repl::info!("Server closed");
				break;
			}
			Err(e) => {
				repl::error!("Failed to accept connection: {e}");
			}
		}
	}
	Ok(())
}

async fn run(cli: AgentCli) -> Result<(), Error> {
	#[cfg(debug_assertions)]
	console_subscriber::init();

	// let filter = tracing_subscriber::EnvFilter::from_default_env();
	// tracing_subscriber::fmt()
	// 	.compact()
	// 	.with_env_filter(filter)
	// 	.with_file(false)
	// 	// .with_thread_ids(true)
	// 	.with_target(true)
	// 	.init();

	// Handle completions generation (same as before)
	if let Some(generator) = cli.globals.generator {
		let mut cmd = AgentCli::command();
		print_completions(generator, &mut cmd);
		return Ok(());
	}

	// Handle completions generation (same as before)
	if let Some(generator) = cli.globals.generator {
		let mut cmd = AgentCli::command();
		print_completions(generator, &mut cmd);
		return Ok(());
	}

	match cli.command {
		CliCommands::Connect(args) => {
			agent_client(args).await?;
		}
		CliCommands::Listen(args) => {
			agent_server(args).await?;
		}
	}

	Ok(())
}

#[tokio::main]
async fn main() {
	let cli = AgentCli::parse();

	let fut = run(cli);

	if let Err(err) = fut.await {
		repl::error!("Oopsies");
		repl::error!("  Error: {err}");

		let mut source = std::error::Error::source(&err);
		let mut cause_level = 1;
		while let Some(inner_err) = source {
			repl::error!("  caused by ({cause_level}): {inner_err}");
			source = inner_err.source();
			cause_level += 1;
		}

		std::process::exit(1); // Exit with a non-zero status code
	}
}
