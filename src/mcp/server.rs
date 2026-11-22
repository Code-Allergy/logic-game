//! The tools over the Model Context Protocol (`docs/mcp-server.md`).
//!
//! A thin layer: each tool hands its arguments to the [`Session`] and
//! returns its text, or its message as a tool error. The session is
//! behind a mutex, so calls run one at a time.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{ServerCapabilities, ServerConfig};
use rmcp::{ServerHandler, schemars, tool, tool_handler, tool_router};
use serde::Deserialize;
use serde_json::{Map, Value};

use super::session::Session;

const INSTRUCTIONS: &str = "\
Build and inspect GateQuest logic circuits. Open or make a workspace with `workspace`, change it \
with `edit` (one call can build a whole chip: node and wire entries are those of save format \
version 6, with arrays and bus wiring), and read it back with `view`. Parts are named by their \
label; a chip is made by editing a circuit name that does not exist yet and placed with \
{\"use\": name}. Positions may be left out: parts are laid out automatically. Keep circuits \
small and build larger ones from chips.";

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct WorkspaceArgs {
    /// One of: list, open, new, save, status.
    pub action: String,
    /// The workspace's name: required for open and new; for save, a new
    /// name saves a copy under it.
    pub name: Option<String>,
    /// For open and new: drop the unsaved changes of the open workspace.
    pub discard: Option<bool>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ViewArgs {
    /// The circuit: a chip's name, or leave out for the root ("main").
    pub circuit: Option<String>,
    /// One of: outline (default), diagnostics, stats.
    pub what: Option<String>,
    /// Outline: only parts whose name or kind starts with this.
    pub filter: Option<String>,
    /// Outline: the most part lines to return (default 200).
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct EditArgs {
    /// The circuit the ops apply to: a chip's name (made if it does not
    /// exist), or leave out for the root.
    pub circuit: Option<String>,
    /// Operations, applied in order. Each is an object with one field:
    /// {"add": [node entries]}, {"wire": [[source, sink, ...], ...]},
    /// {"unwire": [sink references]}, {"remove": [part references]},
    /// {"set": {"part": reference, ...parameters}}, {"layout": true}.
    /// A node entry is {"name": n, "type": kind, ...parameters} or
    /// {"name": n, "use": chip}, with "count" for an array. A reference
    /// is name[index].pin:bits, e.g. "fa[0..7].Cout" or "a:0..4".
    pub ops: Option<Vec<Value>>,
    /// Several {"circuit", "ops"} groups in one call, instead of
    /// "circuit" and "ops". All of them apply, or none.
    pub edits: Option<Vec<Value>>,
    /// Read the request ({"circuit", "ops"} or {"edits"}) from this JSON
    /// file instead: for large generated circuits.
    pub file: Option<String>,
    /// Report what would change, and change nothing.
    pub dry_run: Option<bool>,
}

/// The server: one session shared by its tools.
#[derive(Clone)]
pub struct Server {
    session: Arc<Mutex<Session>>,
}

impl Server {
    pub fn new(session: Session) -> Self {
        Self {
            session: Arc::new(Mutex::new(session)),
        }
    }

    fn session(&self) -> std::sync::MutexGuard<'_, Session> {
        // A tool that panicked leaves the workspace as it was before the
        // call (edits are applied to a copy), so the session is still good.
        self.session.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[tool_router]
impl Server {
    #[tool(
        description = "List, open, make or save workspaces, or get the status of the open one \
                       (its circuits, their sizes, whether it builds)."
    )]
    fn workspace(&self, Parameters(args): Parameters<WorkspaceArgs>) -> Result<String, String> {
        self.session().workspace_tool(
            &args.action,
            args.name.as_deref(),
            args.discard.unwrap_or(false),
        )
    }

    #[tool(
        description = "Read a circuit of the open workspace as text: one line per part with what \
                       drives each input (outline), build errors and unconnected inputs \
                       (diagnostics), or part counts (stats)."
    )]
    fn view(&self, Parameters(args): Parameters<ViewArgs>) -> Result<String, String> {
        self.session().view_tool(
            args.circuit.as_deref(),
            args.what.as_deref(),
            args.filter.as_deref(),
            args.limit,
        )
    }

    #[tool(
        description = "Change the open workspace: add parts, wire them, remove or change them. \
                       All operations of a call apply, or none; the result says what changed and \
                       whether the workspace builds."
    )]
    fn edit(&self, Parameters(args): Parameters<EditArgs>) -> Result<String, String> {
        let mut request = Map::new();
        if let Some(circuit) = args.circuit {
            request.insert("circuit".into(), circuit.into());
        }
        if let Some(ops) = args.ops {
            request.insert("ops".into(), ops.into());
        }
        if let Some(edits) = args.edits {
            request.insert("edits".into(), edits.into());
        }
        let file = args.file.map(PathBuf::from);
        self.session()
            .edit_tool(request, file.as_deref(), args.dry_run.unwrap_or(false))
    }
}

#[tool_handler]
impl ServerHandler for Server {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_instructions(INSTRUCTIONS)
    }
}
