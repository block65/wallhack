//! CLI argument parsing for the wallhack control client.

use argh::FromArgs;

/// Control the wallhack daemon.
#[derive(FromArgs, Debug)]
pub struct Cli {
    #[argh(subcommand)]
    pub command: CtlCommand,
}

/// Available control commands.
#[derive(FromArgs, Debug)]
#[argh(subcommand)]
pub enum CtlCommand {
    Ping(PingCmd),
    Status(StatusCmd),
    Stats(StatsCmd),
    Peers(PeersCmd),
    Routes(RoutesCmd),
    Route(RouteCmd),
    Connect(ConnectCmd),
    Listen(ListenCmd),
    Disconnect(DisconnectCmd),
    Shutdown(ShutdownCmd),
}

/// Ping the daemon.
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "ping")]
pub struct PingCmd {}

/// Show daemon status.
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "status")]
pub struct StatusCmd {}

/// Show traffic statistics.
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "stats")]
pub struct StatsCmd {}

/// List connected peers.
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "peers")]
pub struct PeersCmd {}

/// List routes.
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "routes")]
pub struct RoutesCmd {}

/// Manage routes.
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "route")]
pub struct RouteCmd {
    #[argh(subcommand)]
    pub action: RouteAction,
}

/// Route sub-commands.
#[derive(FromArgs, Debug)]
#[argh(subcommand)]
pub enum RouteAction {
    Add(RouteAddCmd),
    Remove(RouteRemoveCmd),
}

/// Add a route.
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "add")]
pub struct RouteAddCmd {
    /// CIDR to route (e.g. "10.0.0.0/8")
    #[argh(positional)]
    pub cidr: String,

    /// target peer name
    #[argh(option, long = "peer")]
    pub peer: String,
}

/// Remove a route.
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "remove")]
pub struct RouteRemoveCmd {
    /// CIDR to remove (e.g. "10.0.0.0/8")
    #[argh(positional)]
    pub cidr: String,
}

/// Connect to a peer.
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "connect")]
pub struct ConnectCmd {
    /// address to connect to (e.g. "host:6565")
    #[argh(positional)]
    pub addr: String,
}

/// Start listening for connections.
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "listen")]
pub struct ListenCmd {
    /// address to listen on (e.g. ":6565")
    #[argh(positional)]
    pub addr: String,
}

/// Disconnect from upstream peer.
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "disconnect")]
pub struct DisconnectCmd {}

/// Shut down the daemon.
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "shutdown")]
pub struct ShutdownCmd {}
