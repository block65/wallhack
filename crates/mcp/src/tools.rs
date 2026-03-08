//! MCP tool definitions — one per management protocol operation.

use rmcp::{handler::server::wrapper::Parameters, schemars, tool};
use wallhack_wire::management::{
    AddRouteRequest, ConnectRequest, DisconnectPeerRequest, DisconnectRequest, ListenRequest,
    PeersRequest, PingRequest, RemoveRouteRequest, RoutesRequest, ShutdownRequest, StatsRequest,
    StatusRequest, management_request,
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
pub struct PeerParams {
    /// Peer name (or unambiguous prefix)
    pub peer: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct AddrParams {
    /// Address, e.g. "1.2.3.4:4433" or "0.0.0.0:4433"
    pub addr: String,
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
        description = "Get node status: role, version, uptime, listen/peer addresses, capabilities"
    )]
    async fn wallhack_status(&self) -> Result<String, rmcp::ErrorData> {
        ipc_call(management_request::Request::Status(StatusRequest {})).await
    }

    #[tool(description = "Ping the daemon (or a specific peer by name prefix)")]
    async fn wallhack_ping(
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
    async fn wallhack_stats(&self) -> Result<String, rmcp::ErrorData> {
        ipc_call(management_request::Request::Stats(StatsRequest {})).await
    }

    #[tool(
        description = "List all connected peers with their name, address, status, latency, and capabilities"
    )]
    async fn wallhack_peers(&self) -> Result<String, rmcp::ErrorData> {
        ipc_call(management_request::Request::Peers(PeersRequest {})).await
    }

    #[tool(description = "List all configured routes (CIDR to peer mappings)")]
    async fn wallhack_routes(&self) -> Result<String, rmcp::ErrorData> {
        ipc_call(management_request::Request::Routes(RoutesRequest {})).await
    }

    #[tool(description = "Add a route: forward traffic for a CIDR range to a peer")]
    async fn wallhack_add_route(
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
    async fn wallhack_remove_route(
        &self,
        Parameters(params): Parameters<RemoveRouteParams>,
    ) -> Result<String, rmcp::ErrorData> {
        ipc_call(management_request::Request::RemoveRoute(
            RemoveRouteRequest { cidr: params.cidr },
        ))
        .await
    }

    #[tool(description = "Disconnect a peer by name")]
    async fn wallhack_disconnect_peer(
        &self,
        Parameters(params): Parameters<PeerParams>,
    ) -> Result<String, rmcp::ErrorData> {
        ipc_call(management_request::Request::DisconnectPeer(
            DisconnectPeerRequest { peer: params.peer },
        ))
        .await
    }

    #[tool(description = "Connect to a remote peer by address")]
    async fn wallhack_connect(
        &self,
        Parameters(params): Parameters<AddrParams>,
    ) -> Result<String, rmcp::ErrorData> {
        ipc_call(management_request::Request::Connect(ConnectRequest {
            addr: params.addr,
        }))
        .await
    }

    #[tool(description = "Start listening for incoming peer connections on an address")]
    async fn wallhack_listen(
        &self,
        Parameters(params): Parameters<AddrParams>,
    ) -> Result<String, rmcp::ErrorData> {
        ipc_call(management_request::Request::Listen(ListenRequest {
            addr: params.addr,
        }))
        .await
    }

    #[tool(description = "Disconnect from the current transport (stop connecting/listening)")]
    async fn wallhack_disconnect(&self) -> Result<String, rmcp::ErrorData> {
        ipc_call(management_request::Request::Disconnect(
            DisconnectRequest {},
        ))
        .await
    }

    #[tool(description = "Gracefully shut down the wallhack daemon")]
    async fn wallhack_shutdown(&self) -> Result<String, rmcp::ErrorData> {
        ipc_call(management_request::Request::Shutdown(ShutdownRequest {})).await
    }
}

impl rmcp::ServerHandler for WallhackServer {
    fn get_info(&self) -> rmcp::model::ServerInfo {
        rmcp::model::ServerInfo::new(
            rmcp::model::ServerCapabilities::builder()
                .enable_tools()
                .build(),
        )
        .with_instructions(
            "Use these tools to manage a running wallhack daemon. \
             The daemon must be running and reachable via its Unix socket.",
        )
    }
}
