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
version 6, with arrays and bus wiring), and read it back with `view`. Store tests with a chip \
(edit op `tests`) and run them all with `test`; run a whole experiment in one call with `run`. Parts are named by their \
label; a chip is made by editing a circuit name that does not exist yet and placed with \
{\"use\": name}. Positions may be left out: parts are laid out automatically. Keep circuits \
small and build larger ones from chips.";

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct WorkspaceArgs {
    /// One of: list, open, new, save, status, demos (list the built-in
    /// examples), open_demo (open an editable copy of one; name is its id).
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
    /// One of: outline (default), diagnostics, stats, groups (parts that
    /// would make chips, as lists for make_chip).
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

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ImportArgs {
    /// A Verilog file (.v, .sv: read with Yosys, which must be on PATH)
    /// or a Yosys JSON netlist.
    pub file: String,
    /// The top module. Left out: the one no other module uses.
    pub top: Option<String>,
    /// "open": the modules become chips of the open workspace (the
    /// default when one is open). "new": a new workspace.
    pub into: Option<String>,
    /// For "new": the workspace's name (default: the top module's).
    pub name: Option<String>,
    /// For "open": replace chips that have the name of an imported
    /// module. Their placements keep their wires, by port name.
    pub replace: Option<bool>,
    /// For "new": drop the unsaved changes of the open workspace.
    pub discard: Option<bool>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct TransportArgs {
    /// One of: status (default), run, pause, step, reset, rate, set.
    pub action: Option<String>,
    /// For step: how many ticks (default 1).
    pub ticks: Option<u64>,
    /// The tick rate in Hz, or "max" (with any action).
    pub hz: Option<Value>,
    /// Root inputs to drive, by label: {"En": 1} (with any action; they
    /// are set before a step).
    pub inputs: Option<Map<String, Value>>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct AppArgs {
    /// What to read: state (default).
    pub what: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct TestArgs {
    /// Run the tests of this circuit only (a chip's name, or "main").
    /// Left out: every circuit of the workspace.
    pub circuit: Option<String>,
    /// Run only the tests of this name.
    pub name: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct RunArgs {
    /// The circuit to simulate: a chip's name, or leave out for the root.
    pub circuit: Option<String>,
    /// Keep the simulation and its snapshots under this name: the next
    /// call with the same name continues where this one stopped.
    pub session: Option<String>,
    /// The steps, in order. Each is an object:
    /// {"set": {input: value}}, {"ticks": n},
    /// {"poke": {state: value, memory: [[address, value], ...]}} (registers, latches, chips wired straight to them, arrays of these, "ram@0x10"; settles at once, the tick stays),
    /// {"until": "expression", "max_ticks": n}, {"expect": {signal: value}},
    /// {"read": [signals, "ram@0x00..0x10"]},
    /// {"load": {"part": memory, "file": path, "skip": n, "length": n, "at": addr}},
    /// {"store": {"part": memory, "file": path}},
    /// {"record": [signals], "when": "expression"},
    /// {"compare": {"file": path, "columns": [...], "radix": 16, "context": 5}} or {"compare": {"rows": [[...]]}},
    /// {"snapshot": name}, {"restore": name}, {"reset": true}.
    /// A signal is a path of labels: "cpu/alu/sum.y:0..4"; "ram@0x10" is a memory word.
    pub steps: Option<Vec<Value>>,
    /// Short names for signal paths, usable in expressions:
    /// {"pc": "cpu/pc.q"}.
    pub signals: Option<Map<String, Value>>,
    /// Write every recorded row to this file (.csv, or .vcd).
    pub trace: Option<String>,
    /// The most ticks this call may run (default 1,000,000).
    pub max_ticks: Option<u64>,
    /// Read the script (steps, signals, ...) from this JSON file.
    pub file: Option<String>,
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

    #[tool(
        description = "Import Verilog (through Yosys) or a Yosys JSON netlist: each module \
                       becomes a chip of word-level parts, laid out automatically. Use an \
                       imported chip as the reference a gate-level chip is tested against \
                       (a stored test {\"equivalent\": {\"to\": chip}})."
    )]
    fn import_rtl(&self, Parameters(args): Parameters<ImportArgs>) -> Result<String, String> {
        self.session().import_rtl_tool(
            std::path::Path::new(&args.file),
            args.top.as_deref(),
            args.name.as_deref(),
            args.into.as_deref(),
            args.replace.unwrap_or(false),
            args.discard.unwrap_or(false),
        )
    }

    #[tool(
        description = "Run the tests stored with the chips of the open workspace (set them with \
                       edit: {\"tests\": [...]}). Reports each failure with its first failing \
                       case, and a count of what passed."
    )]
    fn test(&self, Parameters(args): Parameters<TestArgs>) -> Result<String, String> {
        self.session()
            .test_tool(args.circuit.as_deref(), args.name.as_deref())
    }

    #[tool(
        description = "Drive the simulation the game's editor shows (only when the server runs \
                       in the game): run, pause, step, reset, set the tick rate, drive inputs. \
                       For tests and experiments use `run`, which has its own simulation."
    )]
    fn transport(&self, Parameters(args): Parameters<TransportArgs>) -> Result<String, String> {
        let mut command = Map::new();
        if let Some(action) = args.action {
            command.insert("action".into(), action.into());
        }
        if let Some(ticks) = args.ticks {
            command.insert("ticks".into(), ticks.into());
        }
        if let Some(hz) = args.hz {
            command.insert("hz".into(), hz);
        }
        if let Some(inputs) = args.inputs {
            command.insert("inputs".into(), Value::Object(inputs));
        }
        self.session().transport_tool(command)
    }

    #[tool(
        description = "Read what the game's editor shows, as text (only when the server runs in \
                       the game): the open workspace, the view, the transport, the build, undo \
                       steps, the selection, input and output values, messages."
    )]
    fn app(&self, Parameters(args): Parameters<AppArgs>) -> Result<String, String> {
        self.session()
            .app_tool(args.what.as_deref().unwrap_or("state"))
    }

    #[tool(
        description = "Run one whole experiment on a private simulation of a circuit: set \
                       inputs, load memories from files, run ticks or until a condition, expect \
                       values, record signals to a trace file, compare them with an expected \
                       trace (stops at the first difference), snapshot and restore. The circuit \
                       shown in the editor is not disturbed."
    )]
    fn run(&self, Parameters(args): Parameters<RunArgs>) -> Result<String, String> {
        let mut script = Map::new();
        if let Some(steps) = args.steps {
            script.insert("steps".into(), steps.into());
        }
        if let Some(signals) = args.signals {
            script.insert("signals".into(), Value::Object(signals));
        }
        if let Some(trace) = args.trace {
            script.insert("trace".into(), trace.into());
        }
        if let Some(max) = args.max_ticks {
            script.insert("max_ticks".into(), max.into());
        }
        let file = args.file.map(PathBuf::from);
        self.session().run_tool(
            args.circuit.as_deref(),
            args.session.as_deref(),
            script,
            file.as_deref(),
        )
    }
}

#[tool_handler]
impl ServerHandler for Server {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_instructions(INSTRUCTIONS)
    }
}
