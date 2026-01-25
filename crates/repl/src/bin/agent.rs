use repl::{AgentConfig, Command, parse_agent};
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
	#[error("Orchestrator error: {0}")]
	Orchestrator(#[from] agent::orchestrator::Error),

	#[error("Client QUIC error: {0}")]
	ClientQuic(#[from] client::quic::Error),

	#[error("Server QUIC error: {0}")]
	ServerQuic(#[from] server::quic::Error),

	#[error("DNS resolution error: {0}")]
	DnsResolve(#[from] repl::dns::ResolveError),

	#[error("Tracing subscriber filter parse error: {0}")]
	TracingFilterParse(#[from] tracing_subscriber::filter::ParseError),
}

async fn agent_client(config: repl::ConnectConfig) -> Result<(), Error> {
	repl::info!("resolving {}...", config.target);

	let endpoint = repl::dns::resolve(config.target, config.dns_server).await?;
	tracing::debug!("resolved endpoint: {:#?}", endpoint);

	let client_config = match config.tls {
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

async fn agent_server(config: repl::ListenConfig) -> Result<(), Error> {
	let mut server = server::quic::QuicServer::try_new(ServerConfig::default())?;

	repl::info!("agent server listening on {}", config.addr);

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

async fn run(config: AgentConfig) -> Result<(), Error> {
	#[cfg(feature = "tokio-console")]
	console_subscriber::init();

	match config.command {
		Command::Connect(args) => {
			agent_client(args).await?;
		}
		Command::Listen(args) => {
			agent_server(args).await?;
		}
	}

	Ok(())
}

#[tokio::main]
async fn main() {
	let cli = parse_agent();

	let config = match cli.into_config() {
		Ok(config) => config,
		Err(e) => {
			eprintln!("Error: {e}");
			std::process::exit(1);
		}
	};

	let fut = run(config);

	if let Err(err) = fut.await {
		repl::error!("Error: {err}");

		let mut source = std::error::Error::source(&err);
		let mut cause_level = 1;
		while let Some(inner_err) = source {
			repl::error!("  caused by ({cause_level}): {inner_err}");
			source = inner_err.source();
			cause_level += 1;
		}

		std::process::exit(1);
	}
}
