//! Wallhack — unified multi-call binary.
//!
//! # Dispatch rules
//!
//! Invoked as `wallhack` (no args, with repl feature): starts daemon in entry
//! mode and attaches the interactive REPL.
//! Invoked as `wallhack` (no args, slim build): starts daemon engine directly.
//! Invoked as `wallhack --connect HOST [...]`: daemon in auto-negotiated mode.
//! Invoked as `wallhack --listen ADDR [...]`: daemon in auto-negotiated mode.
//! Invoked as `wallhack --role ROLE [...]`: daemon with fixed role hint.
//! Invoked as `wallhack entry/exit/relay [...]`: daemon with explicit role override.
//! Invoked as `wallhackd` (or `wallhack daemon`): daemon engine directly.
//! Invoked as `wallhackctl`: IPC control client only; fails if daemon not running.
//! Invoked as `wallhack <control-subcommand>`: IPC control client.
//!
//! The dispatch heuristic: if the first argument starts with `-` it is a flag
//! destined for the daemon CLI (auto-negotiation or global options). Control
//! client subcommands are always bare words (`route`, `peers`, `ping`, etc.).

use wallhack_cli::{
    cli::{CtlCommand, RouteAction},
    ipc, output,
};
use wallhack_wire::management::{
    AddRouteRequest, ClearHintsRequest, ConnectRequest, DisconnectPeerRequest, DisconnectRequest,
    HintLevel, ListenRequest, NodeRole, PeersRequest, PingRequest, RemoveRouteRequest,
    RoutesRequest, SetHintRequest, ShutdownRequest, StatsRequest, StatusRequest,
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

    if args.iter().any(|a| a == "--version") {
        println!("{}", wallhack_cli::version::version());
        std::process::exit(0);
    }

    let is_ctl = bin_name == "wallhackctl";
    let is_daemon = bin_name == DAEMON_BIN_NAME
        || (!is_ctl
            && args.get(1).is_some_and(|a| {
                // "wallhack daemon" passthrough.
                a == "daemon"
                // Any flag argument: auto-negotiation or global daemon options.
                // Control client commands are always bare words, never flags.
                || a.starts_with('-')
            }));

    if is_daemon {
        run_daemon(args, &bin_name);
    } else if is_ctl || args.len() > 1 {
        // Control client: wallhackctl (any args) OR wallhack <control-subcommand>
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
    // "wallhack daemon ..." or "wallhackd ..." = explicit headless mode.
    #[cfg(feature = "repl")]
    let explicit_daemon = bin_name == DAEMON_BIN_NAME || args.get(1).is_some_and(|a| a == "daemon");

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

    let cli = match wallhack_cli::daemon_cli::parse_cli_from_args(&daemon_args) {
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
        println!("{}", wallhack_cli::version::version());
        std::process::exit(0);
    }

    let config = match wallhack_cli::daemon_cli::build_daemon_config(&cli) {
        Ok(config) => config,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    };

    // When the repl feature is enabled, stdout is a TTY, and the user
    // did NOT explicitly invoke "wallhack daemon" or "wallhackd", attach
    // the REPL. "wallhack daemon" means headless — the user opted out.
    #[cfg(feature = "repl")]
    if std::io::IsTerminal::is_terminal(&std::io::stdout()) && !explicit_daemon {
        run_daemon_repl(&cli, &config);
    }

    // Headless path: no REPL, just run the daemon engine.
    tracing::subscriber::set_global_default(wallhack_cli::subscriber::SimpleSubscriber::from(&cli))
        .expect("setting default subscriber");

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("failed to build tokio runtime");

    let socket_override = cli.host.as_deref().map(wallhack_cli::ipc::resolve_host);
    let exit_code = rt.block_on(async {
        match wallhackd::run_daemon_engine(config, socket_override).await {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("error: {e}");
                1
            }
        }
    });

    std::process::exit(exit_code);
}

#[cfg(feature = "repl")]
#[allow(clippy::too_many_lines)]
fn run_daemon_repl(
    cli: &wallhack_cli::daemon_cli::WallhackCli,
    config: &wallhackd::daemon_config::DaemonConfig,
) -> ! {
    let subscriber = if cli.trace || cli.trace_filter.is_some() {
        wallhack_cli::subscriber::SimpleSubscriber::new(
            tracing::level_filters::LevelFilter::TRACE,
            cli.trace_filter.as_deref().unwrap_or(""),
        )
    } else if cli.debug || cli.debug_filter.is_some() {
        wallhack_cli::subscriber::SimpleSubscriber::new(
            tracing::level_filters::LevelFilter::DEBUG,
            cli.debug_filter.as_deref().unwrap_or(""),
        )
    } else if cli.quiet {
        wallhack_cli::subscriber::SimpleSubscriber::new(
            tracing::level_filters::LevelFilter::WARN,
            "",
        )
    } else {
        wallhack_cli::subscriber::SimpleSubscriber::new(
            tracing::level_filters::LevelFilter::INFO,
            "",
        )
    };
    let writer = subscriber.writer();
    tracing::subscriber::set_global_default(subscriber).expect("setting default subscriber");

    let printer = reedline::ExternalPrinter::<String>::default();
    let sender = printer.sender();
    *writer.write().unwrap() = Box::new(move |tag, msg| {
        let _ = sender.send(format!("{tag}: {msg}"));
    });

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("failed to build tokio runtime");

    let exit_code = rt.block_on(async {
        let handle = match wallhackd::start_node(config) {
            Ok(h) => h,
            Err(e) => {
                eprintln!("error: {e}");
                std::process::exit(1);
            }
        };

        // Start IPC listener so wallhackctl can still connect.
        let socket_path = cli.host.as_deref().map_or_else(
            || wallhack_core::ipc::socket_path(Some(config.mode.name())),
            wallhack_cli::ipc::resolve_host,
        );
        let ipc_api = handle.api_arc();
        let peer_events = handle.peer_events_sender();
        let shutdown_rx = handle.shutdown_rx();
        tokio::spawn(async move {
            if let Err(e) = wallhack_core::ipc::run_ipc_listener(
                ipc_api,
                peer_events,
                &socket_path,
                shutdown_rx,
            )
            .await
            {
                tracing::error!("IPC listener error: {e}");
            }
        });

        #[cfg(feature = "vsock")]
        {
            let ipc_api_vsock = handle.api_arc();
            let peer_events_vsock = handle.peer_events_sender();
            let shutdown_rx_vsock = handle.shutdown_rx();
            tokio::spawn(async move {
                if let Err(e) = wallhack_core::ipc::run_vsock_listener(
                    ipc_api_vsock,
                    peer_events_vsock,
                    wallhack_core::ipc::VSOCK_IPC_PORT,
                    shutdown_rx_vsock,
                )
                .await
                {
                    tracing::warn!("vsock IPC listener unavailable: {e}");
                }
            });
        }

        // In-process connection for the REPL (with notifications).
        let (client, server) = tokio::io::duplex(4096);
        let api = handle.api_arc();
        let repl_events = handle.subscribe_peer_events();
        tokio::spawn(async move {
            if let Err(e) =
                wallhack_core::ipc::handle_connection(server, api, Some(repl_events)).await
            {
                tracing::debug!("REPL IPC connection ended: {e}");
            }
        });

        let conn = wallhack_cli::ipc::IpcConnection::new(client);

        // Forward notifications to the REPL printer.
        let notif_sender = printer.sender();
        let mut notif_rx = conn.subscribe_notifications();
        tokio::spawn(async move {
            wallhack_cli::output::forward_notifications(&mut notif_rx, |line| {
                let _ = notif_sender.send(line);
            })
            .await;
        });

        let repl_result = tokio::select! {
            result = wallhack_cli::repl::run(conn, printer) => result,
            _ = tokio::signal::ctrl_c() => Ok(()),
        };

        // Gracefully shut down the daemon node.
        if let Err(e) = handle.shutdown().await {
            tracing::debug!("Node shutdown: {e}");
        }

        match repl_result {
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
    let mut stream = match &cli.host {
        Some(host) => ipc::connect_to(&ipc::resolve_host(host))
            .await
            .map_err(ipc::IpcError::from)?,
        None => ipc::connect().await.map_err(ipc::IpcError::from)?,
    };

    let Some(command) = cli.command else {
        eprintln!("error: missing subcommand\nRun with --help for usage information.");
        std::process::exit(1);
    };
    let request = match command {
        CtlCommand::Ping(cmd) => management_request::Request::Ping(PingRequest {
            peer: cmd.peer.unwrap_or_default(),
        }),
        CtlCommand::Info(_) => management_request::Request::Status(StatusRequest {}),
        CtlCommand::Stats(_) => management_request::Request::Stats(StatsRequest {}),
        #[cfg(feature = "json")]
        CtlCommand::Peers(ref cmd) if cmd.json => {
            // JSON output: make the request and short-circuit the standard response path.
            let request = management_request::Request::Peers(PeersRequest {});
            let resp = ipc::send_request(&mut stream, request).await?;
            if let Some(wallhack_wire::management::management_response::Response::Peers(p)) =
                resp.response
            {
                output::print_peers_json(&p.peers);
            } else {
                return Err(output::CtlError::EmptyResponse);
            }
            return Ok(());
        }
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
        CtlCommand::DisconnectPeer(cmd) => {
            management_request::Request::DisconnectPeer(DisconnectPeerRequest {
                peer: cmd.peer,
                exact: false,
            })
        }
        CtlCommand::Role(cmd) => {
            if let Some(target) = cmd.target {
                let role = parse_ctl_role(&target);
                management_request::Request::SetHint(SetHintRequest {
                    level: HintLevel::Fixed.into(),
                    role: role.into(),
                })
            } else {
                management_request::Request::Status(StatusRequest {})
            }
        }
        CtlCommand::Hint(cmd) => match cmd.action {
            wallhack_cli::cli::HintAction::Prefer(h) => {
                management_request::Request::SetHint(SetHintRequest {
                    level: HintLevel::Prefer.into(),
                    role: parse_ctl_role(&h.role).into(),
                })
            }
            wallhack_cli::cli::HintAction::Exclude(h) => {
                management_request::Request::SetHint(SetHintRequest {
                    level: HintLevel::Exclude.into(),
                    role: parse_ctl_role(&h.role).into(),
                })
            }
            wallhack_cli::cli::HintAction::Fixed(h) => {
                management_request::Request::SetHint(SetHintRequest {
                    level: HintLevel::Fixed.into(),
                    role: parse_ctl_role(&h.role).into(),
                })
            }
            wallhack_cli::cli::HintAction::Clear(_) => {
                management_request::Request::ClearHints(ClearHintsRequest {})
            }
        },
        CtlCommand::Shutdown(_) => management_request::Request::Shutdown(ShutdownRequest {}),
    };

    let resp = ipc::send_request(&mut stream, request).await?;
    output::print_response(&resp)
}

fn parse_ctl_role(s: &str) -> NodeRole {
    match s {
        "entry" => NodeRole::Entry,
        "exit" => NodeRole::Exit,
        "relay" => NodeRole::Relay,
        _ => {
            eprintln!("error: invalid role '{s}' (expected: entry, exit, relay)");
            std::process::exit(1);
        }
    }
}

#[cfg(feature = "repl")]
fn run_repl() -> ! {
    use tracing::level_filters::LevelFilter;

    let subscriber = wallhack_cli::subscriber::SimpleSubscriber::new(LevelFilter::WARN, "");
    let writer = subscriber.writer();
    tracing::subscriber::set_global_default(subscriber).expect("setting default subscriber");

    let printer = reedline::ExternalPrinter::<String>::default();
    let sender = printer.sender();
    *writer.write().unwrap() = Box::new(move |tag, msg| {
        let _ = sender.send(format!("{tag}: {msg}"));
    });

    let daemon_cli = wallhack_cli::daemon_cli::parse_cli_from_args(&[DAEMON_BIN_NAME.to_string()])
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

        // In-process connection with notifications.
        let (client, server) = tokio::io::duplex(4096);
        let api = handle.api_arc();
        let repl_events = handle.subscribe_peer_events();
        tokio::spawn(async move {
            if let Err(e) =
                wallhack_core::ipc::handle_connection(server, api, Some(repl_events)).await
            {
                tracing::debug!("REPL IPC connection ended: {e}");
            }
        });

        let conn = wallhack_cli::ipc::IpcConnection::new(client);

        // Forward notifications to the REPL printer.
        let notif_sender = printer.sender();
        let mut notif_rx = conn.subscribe_notifications();
        tokio::spawn(async move {
            wallhack_cli::output::forward_notifications(&mut notif_rx, |line| {
                let _ = notif_sender.send(line);
            })
            .await;
        });

        let repl_result = tokio::select! {
            result = wallhack_cli::repl::run(conn, printer) => result,
            _ = tokio::signal::ctrl_c() => Ok(()),
        };

        // Gracefully shut down the daemon node.
        if let Err(e) = handle.shutdown().await {
            tracing::debug!("Node shutdown: {e}");
        }

        match repl_result {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("error: {e}");
                1
            }
        }
    });

    std::process::exit(exit_code);
}
