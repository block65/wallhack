use anyhow::Result;
use clap::{CommandFactory, Parser};

use repl::{CliCommands, ConnectArgs, HostCli, ListenArgs};
use wallhack::{
	client::{
		self,
		client::{Client, ClientRole},
		config::{ClientConfig, MtlsConfig},
	},
	host::{net::tun, orchestrator::HostOrchestrator},
	server::{
		self,
		config::ServerConfig,
		server::{Server, ServerRole},
	},
};

pub fn print_completions<G: clap_complete::Generator>(generator: G, cmd: &mut clap::Command) {
	clap_complete::generate(
		generator,
		cmd,
		cmd.get_name().to_string(),
		&mut std::io::stdout(),
	);
}

async fn host_client(args: ConnectArgs, tun_name: Option<String>) -> Result<()> {
	repl::info!("connecting to {}", args.target);

	repl::verbose!("resolving {}", args.target);

	let endpoint = repl::dns::resolve(args.target, args.dns_server).await?;
	repl::verbose!("resolved as {:#?}", endpoint);

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

	let reconnect = true;

	let adapter = tun::adapter::TunAdapter::try_new(tun_name)?;
	let mut client = client::quic::QuicClient::try_new(client_config)?;

	// this is essentially a reconnect loop
	loop {
		let connect_result = client.connect(ClientRole::Host).await?;

		repl::info!(
			"connection established with {}",
			connect_result.client_ident()
		);

		let orchestrator = HostOrchestrator::new(adapter.clone());

		let (instr, resp) = connect_result.channels();
		match orchestrator.drive((instr, resp.subscribe())).await {
			Ok(()) => {
				repl::info!("Orchestrator finished successfully");
			}
			Err(e) => {
				repl::error!("Orchestrator encountered an error: {e}");
			}
		}

		if !reconnect {
			repl::info!("Connection closed, exiting...");
			break;
		}
	}
	Ok(())
}

async fn host_server(args: ListenArgs, tun_name: Option<String>) -> Result<()> {
	let mut server = server::quic::QuicServer::try_new(ServerConfig::default())?;

	repl::info!("host server listening on {}", args.addr);

	loop {
		match server.accept(ServerRole::Host).await {
			Ok(Some(accept_result)) => {
				let if_name = tun_name.clone();

				repl::info!("accepted client from {}", accept_result.client_ident());

				let (instr, resp) = accept_result.channels();

				tokio::spawn(async move {
					let adapter = match tun::adapter::TunAdapter::try_new(if_name) {
						Ok(adapter) => {
							repl::info!("Adapter {} created successfully", adapter.name);
							adapter
						}
						Err(e) => {
							repl::error!("Failed to create adapter: {e}");
							return;
						}
					};

					let orchestrator = HostOrchestrator::new(adapter);

					match orchestrator.drive((instr, resp.subscribe())).await {
						Ok(()) => {
							repl::info!("Orchestrator finished successfully");
						}
						Err(e) => {
							repl::error!("Orchestrator encountered an error: {e}");
						}
					}
				});
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

async fn run(cli: HostCli) -> Result<()> {
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
		let mut cmd = HostCli::command();
		print_completions(generator, &mut cmd);
		return Ok(());
	}

	match cli.command {
		CliCommands::Connect(args) => {
			host_client(args, cli.tun).await?;
		}
		CliCommands::Listen(args) => {
			host_server(args, cli.tun).await?;
		}
	}

	Ok(())
}

#[tokio::main]
async fn main() {
	let cli = HostCli::parse();

	let fut = run(cli);

	if let Err(err) = fut.await {
		repl::error!("  Error: {err}");

		let mut source = err.source();
		let mut cause_level = 1;
		while let Some(inner_err) = source {
			eprintln!("  caused by ({cause_level}): {inner_err}");
			source = inner_err.source();
			cause_level += 1;
		}

		std::process::exit(1); // Exit with a non-zero status code
	}
}
