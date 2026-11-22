//! `gatequest-mcp`: the GateQuest MCP server on stdio, with no window
//! (`docs/mcp-server.md`).
//!
//! ```sh
//! cargo run -p gate_quest --features mcp --bin gatequest-mcp
//! cargo run -p gate_quest --features mcp --bin gatequest-mcp -- --app
//! ```
//!
//! It works in the player's workspace folder (`GATEQUEST_DATA_DIR`
//! overrides it, as for the game). Standard output carries the protocol;
//! messages for people go to standard error.
//!
//! By default the server holds its workspace as a document and never
//! starts the game's systems. With `--app` it runs the game's simulation
//! layer with no window and works on the workspace open in it, as the
//! game started with `--mcp` does: edits are undo steps, and the
//! `transport` and `app` tools have a simulation to drive.

use std::process::ExitCode;
use std::time::Duration;

use bevy::app::App;
use gate_quest::editor::workspace::WorkspaceStore;
use gate_quest::mcp::app::{McpAppPlugin, link};
use gate_quest::mcp::server::Server;
use gate_quest::mcp::session::Session;
use gate_quest::sim::{SimRuntime, SimulationRuntimePlugin};
use rmcp::ServiceExt;

/// Serve `session` on stdio until the client goes away.
fn serve(session: Session) -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    runtime.block_on(async {
        let service = Server::new(session)
            .serve(rmcp::transport::stdio())
            .await
            .map_err(|e| e.to_string())?;
        service.waiting().await.map_err(|e| e.to_string())?;
        Ok(())
    })
}

fn main() -> ExitCode {
    let store = WorkspaceStore::player().unwrap_or_else(|| WorkspaceStore::in_dir("./workspaces"));
    eprintln!("gatequest-mcp: workspaces in {}", store.dir().display());
    let served = if std::env::args().any(|arg| arg == "--app") {
        // The simulation layer of the game, with no window: its frames
        // run here while the server answers on another thread.
        let (link, inbox) = link();
        let mut app = App::new();
        app.add_plugins(SimulationRuntimePlugin {
            runtime: SimRuntime::platform_default(),
        })
        .add_plugins(McpAppPlugin::new(inbox));
        let server = std::thread::spawn(move || serve(Session::with_app(store, Box::new(link))));
        while !server.is_finished() {
            app.update();
            std::thread::sleep(Duration::from_millis(4));
        }
        server
            .join()
            .unwrap_or_else(|_| Err("the server thread panicked".to_string()))
    } else {
        serve(Session::new(store))
    };
    match served {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("gatequest-mcp: {e}");
            ExitCode::FAILURE
        }
    }
}
