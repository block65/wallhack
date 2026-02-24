//! Wallhack — unified multi-call binary.
//!
//! Invoked as `wallhack entry [...]`: launches the daemon engine in entry mode.
//! Invoked as `wallhack exit [...]`: launches the daemon engine in exit mode.
//! Invoked as `wallhack relay [...]`: launches the daemon engine in relay mode.
//! Invoked as `wallhackd` (or `wallhack daemon`): launches the daemon engine directly (equivalent to wallhackd).
//! Invoked as `wallhackctl`: IPC control client only; fails if daemon not running.
//! Invoked as `wallhack` (no args, with repl feature): interactive REPL.
//! Invoked as `wallhack` (no args, slim build): auto-starts daemon engine.
//! Invoked as `wallhack <control-subcommand>`: IPC control client.

use wallhack_cli::{
    cli::{CtlCommand, RouteAction},
    ipc, output,
};
use wallhack_wire::management::{
    AddRouteRequest, ConnectRequest, DisconnectRequest, ListenRequest, PeersRequest, PingRequest,
    RemoveRouteRequest, RoutesRequest, ShutdownRequest, StatsRequest, StatusRequest,
    management_request,
};

const DAEMON_BIN_NAME: &str = "wallhackd";

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let bin_name = std::path::Path::new(&args[0])
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("wallhack")
        .to_string();

    let is_ctl = bin_name == "wallhackctl";
    let is_daemon = bin_name == DAEMON_BIN_NAME
        || (!is_ctl
            && args
                .get(1)
                .is_some_and(|a| matches!(a.as_str(), "entry" | "exit" | "relay" | "daemon")));

    if is_daemon {
        run_daemon(args, &bin_name);
    } else if is_ctl || args.len() > 1 {
        // Control client: wallhackctl (any args) OR wallhack <subcommand>
        let cli: wallhack_cli::cli::Cli = argh::from_env();
        run_ctl(cli);
    } else {
        // wallhack with no arguments
        #[cfg(feature = "repl")]
        {
            run_repl();
        }
        #[cfg(not(feature = "repl"))]
        {
            run_daemon(args, &bin_name);
        }
    }
}

fn run_daemon(args: Vec<String>, bin_name: &str) -> ! {
    // Strip "daemon" passthrough prefix if present, so argh sees the daemon CLI directly.
    let daemon_args: Vec<String> =
        if bin_name != DAEMON_BIN_NAME && args.get(1).is_some_and(|a| a == "daemon") {
            // wallhack daemon [entry|exit|relay] ... → wallhackd [entry|exit|relay] ...
            let mut new_args = vec![DAEMON_BIN_NAME.to_string()];
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

    tracing::subscriber::set_global_default(wallhack_cli::subscriber::SimpleSubscriber::from(&cli))
        .expect("setting default subscriber");

    let config = match wallhack_cli::daemon_cli::build_daemon_config(&cli) {
        Ok(config) => config,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    };

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
        CtlCommand::Ping(cmd) => management_request::Request::Ping(PingRequest {
            peer: cmd.peer.unwrap_or_default(),
        }),
        CtlCommand::Info(_) => management_request::Request::Status(StatusRequest {}),
        CtlCommand::Stats(_) => management_request::Request::Stats(StatsRequest {}),
        CtlCommand::Peers(_) => management_request::Request::Peers(PeersRequest {}),
        CtlCommand::Route(cmd) => match cmd.action {
            RouteAction::List(_) => management_request::Request::Routes(RoutesRequest {}),
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
    use tracing::level_filters::LevelFilter;

    let subscriber = wallhack_cli::subscriber::SimpleSubscriber::new(LevelFilter::INFO, "");
    let writer = subscriber.writer();
    tracing::subscriber::set_global_default(subscriber).expect("setting default subscriber");

    let printer = reedline::ExternalPrinter::<String>::default();
    let sender = printer.sender();
    *writer.write().unwrap() = Box::new(move |tag, msg| {
        let _ = sender.send(format!("{tag}: {msg}"));
    });

    let daemon_cli =
        wallhack_cli::daemon_cli::parse_cli_from_args(vec![DAEMON_BIN_NAME.to_string()])
            .expect("default daemon cli");
    let config =
        wallhack_cli::daemon_cli::build_daemon_config(&daemon_cli).expect("default daemon config");

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("failed to build tokio runtime");

    let exit_code = rt.block_on(async {
        let handle = match wallhackd::start_node(&config) {
            Ok(h) => h,
            Err(e) => {
                eprintln!("error: {e}");
                std::process::exit(1);
            }
        };

        // In-process connection: no filesystem socket needed.
        let (client, server) = tokio::io::duplex(4096);
        let api = handle.api_arc();
        tokio::spawn(async move {
            if let Err(e) = wallhack_core::ipc::handle_connection(server, api).await {
                tracing::debug!("REPL IPC connection ended: {e}");
            }
        });

        match wallhack_cli::repl::run(client, printer).await {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("error: {e}");
                1
            }
        }
    });

    std::process::exit(exit_code);
}
