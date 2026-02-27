//! CLI argument parsing for the wallhack control client.

use argh::FromArgs;

/// Control the wallhack daemon.
#[derive(FromArgs, Debug)]
pub struct Cli {
    /// print version information and exit
    #[argh(switch)]
    pub version: bool,

    #[argh(subcommand)]
    pub command: Option<CtlCommand>,
}

/// Available control commands.
#[derive(FromArgs, Debug)]
#[argh(subcommand)]
pub enum CtlCommand {
    Ping(PingCmd),
    Info(InfoCmd),
    Stats(StatsCmd),
    Peers(PeersCmd),
    Route(RouteCmd),
    Connect(ConnectCmd),
    Listen(ListenCmd),
    Disconnect(DisconnectCmd),
    Shutdown(ShutdownCmd),
}

/// Ping a peer.
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "ping")]
pub struct PingCmd {
    /// peer name prefix to ping (auto-selects sole peer if omitted)
    #[argh(positional)]
    pub peer: Option<String>,
}

/// Show daemon info.
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "info")]
pub struct InfoCmd {}

/// Show traffic statistics.
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "stats")]
pub struct StatsCmd {}

/// List connected peers.
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "peers")]
pub struct PeersCmd {}

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
    List(RouteListCmd),
    Add(RouteAddCmd),
    Remove(RouteRemoveCmd),
}

/// List routes.
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "list")]
pub struct RouteListCmd {}

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
