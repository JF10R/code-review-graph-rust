//! MCP server wiring via rmcp.
//!
//! Registers all 9 tools and runs the server over stdio transport.
//! All tool handlers delegate to `tools::*` via `tokio::task::spawn_blocking`
//! to avoid blocking the async event loop during heavy operations.

use crate::error::Result;

/// Run the MCP server via stdio.
pub async fn run_server(repo_root: Option<String>) -> Result<()> {
    let _ = repo_root;
    todo!("Implement rmcp server with #[tool] handlers")
}
