//! `gatequest-mcp`: the GateQuest MCP server on stdio, with no window
//! (`docs/mcp-server.md`).
//!
//! ```sh
//! cargo run -p gate_quest --features mcp --bin gatequest-mcp
//! ```
//!
//! It works in the player's workspace folder (`GATEQUEST_DATA_DIR`
//! overrides it, as for the game). Standard output carries the protocol;
//! messages for people go to standard error.

use std::process::ExitCode;

use gate_quest::editor::workspace::WorkspaceStore;
use gate_quest::mcp::server::Server;
use gate_quest::mcp::session::Session;
use rmcp::ServiceExt;

fn main() -> ExitCode {
    let store = WorkspaceStore::player().unwrap_or_else(|| WorkspaceStore::in_dir("./workspaces"));
    eprintln!("gatequest-mcp: workspaces in {}", store.dir().display());
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(e) => {
            eprintln!("gatequest-mcp: {e}");
            return ExitCode::FAILURE;
        }
    };
    let served = runtime.block_on(async {
        let service = Server::new(Session::new(store))
            .serve(rmcp::transport::stdio())
            .await
            .map_err(|e| e.to_string())?;
        service.waiting().await.map_err(|e| e.to_string())
    });
    match served {
        Ok(_) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("gatequest-mcp: {e}");
            ExitCode::FAILURE
        }
    }
}
