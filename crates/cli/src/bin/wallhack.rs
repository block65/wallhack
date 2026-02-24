//! Wallhack — unified multi-call binary.
//!
//! Invoked as `wallhackd` (or `wallhack daemon`): launches the daemon engine.
//! Invoked as `wallhack <subcommand>`: IPC control client.
//! Invoked as `wallhack` (no args, with repl feature): interactive REPL.

use wallhack_cli::{
	cli::{CtlCommand, RouteAction},
	ipc, output,
};
use wallhack_wire::management::{
	AddRouteRequest, ConnectRequest, DisconnectRequest, ListenRequest, PeersRequest, PingRequest,
	RemoveRouteRequest, RoutesRequest, ShutdownRequest, StatsRequest, StatusRequest,
	management_request,
};

fn main() {
	let args: Vec<String> = std::env::args().collect();
	let bin_name = std::path::Path::new(&args[0])
		.file_name()
		.and_then(|s| s.to_str())
		.unwrap_or("wallhack")
		.to_string();

	// Multi-call: if invoked as "wallhackd", run the daemon engine directly.
	let is_daemon = bin_name == "wallhackd" || args.get(1).is_some_and(|a| a == "daemon");

	if is_daemon {
		run_daemon(args, &bin_name);
	} else if args.len() <= 1 {
		// No arguments: launch REPL if available, otherwise show help.
		#[cfg(feature = "repl")]
		{
			run_repl();
		}
		#[cfg(not(feature = "repl"))]
		{
			let cli: wallhack_cli::cli::Cli = argh::from_env();
			run_ctl(cli);
		}
	} else {
		let cli: wallhack_cli::cli::Cli = argh::from_env();
		run_ctl(cli);
	}
}

fn run_daemon(args: Vec<String>, bin_name: &str) -> ! {
	// Strip "daemon" subcommand if present, so argh sees the daemon CLI.
	let daemon_args: Vec<String> =
		if bin_name != "wallhackd" && args.get(1).is_some_and(|a| a == "daemon") {
			// Replace: wallhack daemon entry ... → wallhackd entry ...
			let mut new_args = vec!["wallhackd".to_string()];
			new_args.extend(args[2..].iter().cloned());
			new_args
		} else {
			args
		};

	let cli = match wallhack_cli::daemon_cli::parse_cli_from_args(daemon_args) {
		Ok(cli) => cli,
		Err(e) => {
			if e.exit_code == 0 {
				print!("{}", e.message);
			} else {
				eprint!("{}", e.message);
			}
			std::process::exit(e.exit_code);
		}
	};

	if cli.version {
		let message = if cli.verbose {
			wallhack_cli::version::version_verbose()
		} else {
			wallhack_cli::version::version_short()
		};
		println!("{message}");
		std::process::exit(0);
	}

	// Set up tracing based on CLI flags.
	setup_tracing(&cli);

	let config = match wallhack_cli::daemon_cli::build_daemon_config(&cli) {
		Ok(config) => config,
		Err(e) => {
			eprintln!("error: {e}");
			std::process::exit(1);
		}
	};

	// Log the node name and version.
	let name = match &config.mode {
		wallhackd::daemon_config::ModeConfig::Entry(c) => &c.name,
		wallhackd::daemon_config::ModeConfig::Exit(c) => &c.name,
		wallhackd::daemon_config::ModeConfig::Relay(c) => &c.name,
	};
	tracing::info!(
		"wallhack {}  {name}",
		wallhack_cli::version::built_info::PKG_VERSION
	);

	let rt = tokio::runtime::Builder::new_multi_thread()
		.enable_all()
		.build()
		.expect("failed to build tokio runtime");

	let exit_code = rt.block_on(async {
		match wallhackd::run_daemon_engine(config).await {
			Ok(()) => 0,
			Err(e) => {
				eprintln!("error: {e}");
				1
			}
		}
	});

	std::process::exit(exit_code);
}

fn setup_tracing(cli: &wallhack_cli::daemon_cli::WallhackCli) {
	use tracing::level_filters::LevelFilter;

	let (level, filter_str) = if cli.trace || cli.trace_filter.is_some() {
		(
			LevelFilter::TRACE,
			cli.trace_filter.as_deref().unwrap_or(""),
		)
	} else if cli.debug || cli.debug_filter.is_some() {
		(
			LevelFilter::DEBUG,
			cli.debug_filter.as_deref().unwrap_or(""),
		)
	} else {
		// No internal tracing by default
		(LevelFilter::OFF, "")
	};

	let subscriber = wallhack_cli::subscriber::SimpleSubscriber::new(level, filter_str);
	tracing::subscriber::set_global_default(subscriber).expect("setting default subscriber");
}

fn run_ctl(cli: wallhack_cli::cli::Cli) -> ! {
	let rt = tokio::runtime::Builder::new_multi_thread()
		.enable_all()
		.build()
		.expect("failed to build tokio runtime");

	let exit_code = rt.block_on(async {
		match run_ctl_async(cli).await {
			Ok(()) => 0,
			Err(e) => {
				eprintln!("error: {e}");
				1
			}
		}
	});

	std::process::exit(exit_code);
}

async fn run_ctl_async(cli: wallhack_cli::cli::Cli) -> Result<(), output::CtlError> {
	let mut stream = ipc::connect().await.map_err(ipc::IpcError::from)?;

	let request = match cli.command {
		CtlCommand::Ping(_) => management_request::Request::Ping(PingRequest {}),
		CtlCommand::Status(_) => management_request::Request::Status(StatusRequest {}),
		CtlCommand::Stats(_) => management_request::Request::Stats(StatsRequest {}),
		CtlCommand::Peers(_) => management_request::Request::Peers(PeersRequest {}),
		CtlCommand::Routes(_) => management_request::Request::Routes(RoutesRequest {}),
		CtlCommand::Route(cmd) => match cmd.action {
			RouteAction::Add(add) => management_request::Request::AddRoute(AddRouteRequest {
				cidr: add.cidr,
				peer: add.peer,
			}),
			RouteAction::Remove(rm) => {
				management_request::Request::RemoveRoute(RemoveRouteRequest { cidr: rm.cidr })
			}
		},
		CtlCommand::Connect(cmd) => {
			management_request::Request::Connect(ConnectRequest { addr: cmd.addr })
		}
		CtlCommand::Listen(cmd) => {
			management_request::Request::Listen(ListenRequest { addr: cmd.addr })
		}
		CtlCommand::Disconnect(_) => management_request::Request::Disconnect(DisconnectRequest {}),
		CtlCommand::Shutdown(_) => management_request::Request::Shutdown(ShutdownRequest {}),
	};

	let resp = ipc::send_request(&mut stream, request).await?;
	output::print_response(&resp)
}

#[cfg(feature = "repl")]
fn run_repl() -> ! {
	let rt = tokio::runtime::Builder::new_multi_thread()
		.enable_all()
		.build()
		.expect("failed to build tokio runtime");

	let exit_code = rt.block_on(async {
		match wallhack_cli::repl::run().await {
			Ok(()) => 0,
			Err(e) => {
				eprintln!("error: {e}");
				1
			}
		}
	});

	std::process::exit(exit_code);
}
