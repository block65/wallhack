//! Wallhack CLI — thin control client for the wallhack daemon.
//!
//! Communicates with a running `wallhackd` over a Unix domain socket.

use wallhack_cli::{
	cli::{CtlCommand, RouteAction},
	ipc, output,
};
use wallhack_wire::management::{
	AddRouteRequest, ConnectRequest, DisconnectRequest, ListenRequest, PeersRequest, PingRequest,
	RemoveRouteRequest, RoutesRequest, ShutdownRequest, StatsRequest, StatusRequest,
	management_request,
};

#[tokio::main]
async fn main() {
	let cli: wallhack_cli::cli::Cli = argh::from_env();

	if let Err(e) = run(cli).await {
		eprintln!("error: {e}");
		std::process::exit(1);
	}
}

async fn run(cli: wallhack_cli::cli::Cli) -> Result<(), output::CtlError> {
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
