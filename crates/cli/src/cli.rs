//! CLI argument parsing for the wallhack control client.

use argh::FromArgs;

/// Control the wallhack daemon.
#[derive(FromArgs, Debug)]
pub struct Cli {
    /// daemon socket path (overrides `WALLHACK_HOST` env var)
    #[argh(option, short = 'H')]
    pub host: Option<String>,

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
    Role(RoleCmd),
    Hint(HintCmd),
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
pub struct PeersCmd {
    /// output as JSON
    #[argh(switch, short = 'j')]
    pub json: bool,
}

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
    Del(RouteDelCmd),
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

/// Delete a route.
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "del")]
pub struct RouteDelCmd {
    /// CIDR to delete (e.g. "10.0.0.0/8")
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

/// Disconnect a peer or the transport session.
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "disconnect")]
pub struct DisconnectCmd {
    /// peer name (or unambiguous prefix). Omit to disconnect the transport.
    #[argh(positional)]
    pub peer: Option<String>,
}

/// Show or set the node role.
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "role")]
pub struct RoleCmd {
    /// target role (entry, exit, relay). Omit to show current role.
    #[argh(positional)]
    pub target: Option<String>,
}

/// Manage role hints.
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "hint")]
pub struct HintCmd {
    #[argh(subcommand)]
    pub action: HintAction,
}

/// Hint sub-commands.
#[derive(FromArgs, Debug)]
#[argh(subcommand)]
pub enum HintAction {
    Prefer(HintPreferCmd),
    Exclude(HintExcludeCmd),
    Fixed(HintFixedCmd),
    Auto(HintAutoCmd),
}

/// Set a prefer hint.
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "prefer")]
pub struct HintPreferCmd {
    /// target role (entry, exit, relay)
    #[argh(positional)]
    pub role: String,
}

/// Set an exclude hint.
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "exclude")]
pub struct HintExcludeCmd {
    /// target role (entry, exit, relay)
    #[argh(positional)]
    pub role: String,
}

/// Set a fixed hint.
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "fixed")]
pub struct HintFixedCmd {
    /// target role (entry, exit, relay)
    #[argh(positional)]
    pub role: String,
}

/// Return to capability-based negotiation.
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "auto")]
pub struct HintAutoCmd {}

/// Shut down the daemon.
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "shutdown")]
pub struct ShutdownCmd {}
