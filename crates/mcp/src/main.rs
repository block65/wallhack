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
    let server = tools::WallhackServer
        .serve(transport)
        .await
        .expect("failed to start MCP server");
    server.waiting().await.expect("MCP server error");
}
