//! wallhack-mcp — MCP server for AI assistants (Claude Code/Desktop).
//!
//! Connects to the wallhack daemon via Unix socket IPC and exposes
//! all management operations as MCP tools over stdio transport.

// The `#[tool_router]` / `#[tool]` macros generate dispatch code that
// references param structs and helpers, but dead-code analysis misses them.
#[allow(dead_code)]
mod convert;
#[allow(dead_code)]
mod tools;

use rmcp::ServiceExt;

#[tokio::main]
async fn main() {
    let transport = rmcp::transport::io::stdio();
    let mut router = rmcp::handler::server::router::Router::new(tools::WallhackServer);
    router.tool_router = tools::WallhackServer::tool_router();
    let server = match router.serve(transport).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("wallhack-mcp: {e}");
            std::process::exit(1);
        }
    };
    if let Err(e) = server.waiting().await {
        eprintln!("wallhack-mcp: {e}");
    }
}
