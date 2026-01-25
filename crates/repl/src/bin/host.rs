use anyhow::Result;

use repl::{Command, HostConfig, parse_host};
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

async fn host_client(config: repl::ConnectConfig, tun_name: Option<String>) -> Result<()> {
	repl::info!("connecting to {}", config.target);

	repl::verbose!("resolving {}", config.target);

	let endpoint = repl::dns::resolve(config.target, config.dns_server).await?;
	repl::verbose!("resolved as {:#?}", endpoint);

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

async fn host_server(config: repl::ListenConfig, tun_name: Option<String>) -> Result<()> {
	let mut server = server::quic::QuicServer::try_new(ServerConfig::default())?;

	repl::info!("host server listening on {}", config.addr);

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

async fn run(config: HostConfig) -> Result<()> {
	#[cfg(feature = "tokio-console")]
	console_subscriber::init();

	match config.command {
		Command::Connect(args) => {
			host_client(args, config.tun).await?;
		}
		Command::Listen(args) => {
			host_server(args, config.tun).await?;
		}
	}

	Ok(())
}

#[tokio::main]
async fn main() {
	let cli = parse_host();

	let config = match cli.into_config() {
		Ok(config) => config,
		Err(e) => {
			eprintln!("Error: {e}");
			std::process::exit(1);
		}
	};

	let fut = run(config);

	if let Err(err) = fut.await {
		repl::error!("  Error: {err}");

		let mut source = err.source();
		let mut cause_level = 1;
		while let Some(inner_err) = source {
			eprintln!("  caused by ({cause_level}): {inner_err}");
			source = inner_err.source();
			cause_level += 1;
		}

		std::process::exit(1);
	}
}
