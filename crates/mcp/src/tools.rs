//! MCP tool definitions — one per management protocol operation.

use rmcp::{handler::server::wrapper::Parameters, schemars, tool};
use wallhack_wire::management::{
    AddRouteRequest, ClearHintsRequest, ConnectRequest, DisconnectPeerRequest, DisconnectRequest,
    HintLevel, ListenRequest, NodeRole, PeersRequest, PingRequest, RemoveRouteRequest,
    RoutesRequest, SetHintRequest, ShutdownRequest, StatsRequest, StatusRequest,
    management_request,
};

use crate::convert;

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct PingParams {
    /// Peer name prefix to ping. Omit to ping the daemon itself.
    pub peer: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct AddRouteParams {
    /// CIDR range, e.g. "10.0.0.0/8"
    pub cidr: String,
    /// Target peer name
    pub peer: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct RemoveRouteParams {
    /// CIDR range to remove, e.g. "10.0.0.0/8"
    pub cidr: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct DisconnectParams {
    /// Peer name (or unambiguous prefix). Omit to disconnect the transport.
    pub peer: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct AddrParams {
    /// Address, e.g. "1.2.3.4:4433" or "0.0.0.0:4433"
    pub addr: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SetHintParams {
    /// Hint level: "prefer", "exclude", or "fixed"
    pub level: String,
    /// Target role: "entry", "exit", or "relay"
    pub role: String,
}

/// Wallhack MCP server — exposes daemon management as MCP tools.
#[derive(Debug, Clone)]
pub struct WallhackServer;

/// Send an IPC request over a fresh Unix socket and format the response.
async fn ipc_call(request: management_request::Request) -> Result<String, rmcp::ErrorData> {
    let mut stream = wallhack_ipc::client::connect().await.map_err(|e| {
        rmcp::ErrorData::internal_error(format!("cannot connect to daemon: {e}"), None)
    })?;

    let resp = wallhack_ipc::client::send_request(&mut stream, request)
        .await
        .map_err(|e| rmcp::ErrorData::internal_error(e.to_string(), None))?;

    convert::format_response(&resp).map_err(|msg| rmcp::ErrorData::internal_error(msg, None))
}

#[rmcp::tool_router(vis = "pub")]
impl WallhackServer {
    #[tool(
        description = "Get node info: role, version, uptime, listen/peer addresses, capabilities"
    )]
    async fn info(&self) -> Result<String, rmcp::ErrorData> {
        ipc_call(management_request::Request::Status(StatusRequest {})).await
    }

    #[tool(description = "Ping the daemon (or a specific peer by name prefix)")]
    async fn ping(
        &self,
        Parameters(params): Parameters<PingParams>,
    ) -> Result<String, rmcp::ErrorData> {
        ipc_call(management_request::Request::Ping(PingRequest {
            peer: params.peer.unwrap_or_default(),
        }))
        .await
    }

    #[tool(
        description = "Get traffic statistics: bytes/packets in/out, active connections and flows"
    )]
    async fn stats(&self) -> Result<String, rmcp::ErrorData> {
        ipc_call(management_request::Request::Stats(StatsRequest {})).await
    }

    #[tool(
        description = "List all connected peers with their name, address, status, latency, and capabilities"
    )]
    async fn peers(&self) -> Result<String, rmcp::ErrorData> {
        ipc_call(management_request::Request::Peers(PeersRequest {})).await
    }

    #[tool(description = "List all configured routes (CIDR to peer mappings)")]
    async fn routes(&self) -> Result<String, rmcp::ErrorData> {
        ipc_call(management_request::Request::Routes(RoutesRequest {})).await
    }

    #[tool(description = "Add a route: forward traffic for a CIDR range to a peer")]
    async fn add_route(
        &self,
        Parameters(params): Parameters<AddRouteParams>,
    ) -> Result<String, rmcp::ErrorData> {
        ipc_call(management_request::Request::AddRoute(AddRouteRequest {
            cidr: params.cidr,
            peer: params.peer,
        }))
        .await
    }

    #[tool(description = "Remove a route by CIDR")]
    async fn remove_route(
        &self,
        Parameters(params): Parameters<RemoveRouteParams>,
    ) -> Result<String, rmcp::ErrorData> {
        ipc_call(management_request::Request::RemoveRoute(
            RemoveRouteRequest { cidr: params.cidr },
        ))
        .await
    }

    #[tool(description = "Connect to a remote peer by address")]
    async fn connect(
        &self,
        Parameters(params): Parameters<AddrParams>,
    ) -> Result<String, rmcp::ErrorData> {
        ipc_call(management_request::Request::Connect(ConnectRequest {
            addr: params.addr,
        }))
        .await
    }

    #[tool(description = "Start listening for incoming peer connections on an address")]
    async fn listen(
        &self,
        Parameters(params): Parameters<AddrParams>,
    ) -> Result<String, rmcp::ErrorData> {
        ipc_call(management_request::Request::Listen(ListenRequest {
            addr: params.addr,
        }))
        .await
    }

    #[tool(
        description = "Disconnect a peer by name, or disconnect the transport if no peer specified"
    )]
    async fn disconnect(
        &self,
        Parameters(params): Parameters<DisconnectParams>,
    ) -> Result<String, rmcp::ErrorData> {
        match params.peer {
            Some(peer) => {
                ipc_call(management_request::Request::DisconnectPeer(
                    DisconnectPeerRequest { peer, exact: false },
                ))
                .await
            }
            None => {
                ipc_call(management_request::Request::Disconnect(
                    DisconnectRequest {},
                ))
                .await
            }
        }
    }

    #[tool(description = "Gracefully shut down the wallhack daemon")]
    async fn shutdown(&self) -> Result<String, rmcp::ErrorData> {
        ipc_call(management_request::Request::Shutdown(ShutdownRequest {})).await
    }

    #[tool(
        description = "Set a role hint to influence auto-negotiation (prefer/exclude/fixed + entry/exit/relay)"
    )]
    async fn set_hint(
        &self,
        Parameters(params): Parameters<SetHintParams>,
    ) -> Result<String, rmcp::ErrorData> {
        let level = match params.level.as_str() {
            "prefer" => HintLevel::Prefer,
            "exclude" => HintLevel::Exclude,
            "fixed" => HintLevel::Fixed,
            other => {
                return Err(rmcp::ErrorData::invalid_params(
                    format!("invalid hint level '{other}' (expected: prefer, exclude, fixed)"),
                    None,
                ));
            }
        };
        let role = match params.role.as_str() {
            "entry" => NodeRole::Entry,
            "exit" => NodeRole::Exit,
            "relay" => NodeRole::Relay,
            other => {
                return Err(rmcp::ErrorData::invalid_params(
                    format!("invalid role '{other}' (expected: entry, exit, relay)"),
                    None,
                ));
            }
        };
        ipc_call(management_request::Request::SetHint(SetHintRequest {
            level: level.into(),
            role: role.into(),
        }))
        .await
    }

    #[tool(description = "Clear all role hints, returning to pure capability-based negotiation")]
    async fn clear_hints(&self) -> Result<String, rmcp::ErrorData> {
        ipc_call(management_request::Request::ClearHints(
            ClearHintsRequest {},
        ))
        .await
    }
}

impl rmcp::ServerHandler for WallhackServer {
    fn get_info(&self) -> rmcp::model::ServerInfo {
        let sha = &env!("VERGEN_GIT_SHA")[..7];
        let dirty = if env!("VERGEN_GIT_DIRTY") == "true" {
            "-dirty"
        } else {
            ""
        };
        let ts = env!("VERGEN_BUILD_TIMESTAMP");
        let compact_ts = ts.get(..19).unwrap_or(ts).replace(['-', ':'], "");
        let profile = env!("WALLHACK_BUILD_PROFILE");
        let version = format!(
            "{}+{}{}.{}.{}",
            env!("CARGO_PKG_VERSION"),
            sha,
            dirty,
            compact_ts,
            profile
        );
        rmcp::model::ServerInfo::new(
            rmcp::model::ServerCapabilities::builder()
                .enable_tools()
                .build(),
        )
        .with_server_info(rmcp::model::Implementation::new("wallhack-mcp", version))
        .with_instructions(
            "Use these tools to manage a running wallhack daemon. \
             The daemon must be running and reachable via its Unix socket.",
        )
    }
}
