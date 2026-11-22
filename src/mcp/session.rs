//! The tools as plain functions on one open workspace
//! (`docs/mcp-server.md`, "Tools"): what the server calls, with no
//! protocol in it, so that tests and other hosts can call it too.
//!
//! A [`Session`] holds the workspace it has open as a document. Edits
//! change that document; nothing reaches the disk until `save`.

use std::path::Path;

use serde_json::{Map, Value};

use super::batch::{self, Place};
use super::rig::Rig;
use super::run::{self, RunState};
use super::{testing, view};
use crate::editor::workspace::{OnConflict, WorkspaceStore};
use crate::sim::demos::catalog;
use crate::sim::source::Externals;
use crate::sim::{CircuitDocument, WorkspaceDocument};

/// Largest request file read, in bytes.
pub const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;

/// The workspace open in a running app, as a document.
#[derive(Debug, Clone)]
pub struct AppWorkspace {
    pub name: String,
    /// The root circuit and every definition of the app's library.
    pub workspace: WorkspaceDocument,
    pub unsaved: bool,
    /// A built-in demo: it cannot be edited until it is copied.
    pub read_only: bool,
    /// Changes whenever the circuit is edited: a commit names the stamp
    /// it was computed from, and is refused if the circuit moved on.
    pub stamp: u64,
}

/// A running app (the game, or the same app with no window) whose open
/// workspace the session works on: `docs/mcp-server.md`, "How an edit
/// reaches the editor". Every call blocks until the app has answered.
pub trait AppLink: Send {
    /// The open workspace as it is now.
    fn read(&mut self) -> Result<AppWorkspace, String>;
    /// Make the open workspace `workspace` (one undo step), if it is
    /// still at `stamp`.
    fn commit(&mut self, workspace: WorkspaceDocument, stamp: u64) -> Result<(), String>;
    /// Replace the open workspace.
    fn open(
        &mut self,
        name: String,
        workspace: WorkspaceDocument,
        unsaved: bool,
    ) -> Result<(), String>;
    /// Save through the editor (`None`: under its own name).
    fn save(&mut self, name: Option<String>) -> Result<String, String>;
    /// Drive the simulation the editor shows.
    fn transport(&mut self, command: Map<String, Value>) -> Result<String, String>;
    /// What the editor shows, as text.
    fn state(&mut self) -> Result<String, String>;
}

struct Open {
    /// The name it is saved under (or will be).
    name: String,
    workspace: WorkspaceDocument,
    /// Changed since it was opened or saved.
    unsaved: bool,
}

/// Simulations kept between `run` calls, at most.
pub const MAX_SIM_SESSIONS: usize = 8;

/// A simulation kept between `run` calls.
struct SimSession {
    name: String,
    /// The circuit it simulates, and the workspace change it was built at.
    circuit: uuid::Uuid,
    generation: u64,
    state: RunState,
}

/// One client's state: the store it reads and saves in, the open
/// workspace, and the simulations kept between `run` calls.
pub struct Session {
    store: WorkspaceStore,
    open: Option<Open>,
    /// Counts changes of the open workspace: a kept simulation is of the
    /// workspace as it was at one of them.
    generation: u64,
    sims: Vec<SimSession>,
    /// The app whose open workspace this session works on, if any; else
    /// the session holds its workspace itself.
    link: Option<Box<dyn AppLink>>,
    /// Of the workspace last read from the app.
    stamp: u64,
    read_only: bool,
}

const NO_APP: &str = "this server runs without the game; this tool works on the simulation and \
                      the editor of a running game (start it with --mcp)";

fn externals() -> Externals<'static> {
    catalog().map(|c| c.externals()).unwrap_or_default()
}

impl Session {
    pub fn new(store: WorkspaceStore) -> Self {
        Self {
            store,
            open: None,
            generation: 0,
            sims: Vec::new(),
            link: None,
            stamp: 0,
            read_only: false,
        }
    }

    /// A session on the workspace open in a running app.
    pub fn with_app(store: WorkspaceStore, link: Box<dyn AppLink>) -> Self {
        Self {
            link: Some(link),
            ..Self::new(store)
        }
    }

    /// With an app: take its open workspace as ours, as it is now.
    fn sync(&mut self) -> Result<(), String> {
        let Some(link) = &mut self.link else {
            return Ok(());
        };
        let app = link.read()?;
        let same = self
            .open
            .as_ref()
            .is_some_and(|o| o.name == app.name && o.workspace == app.workspace);
        if !same {
            self.generation += 1;
        }
        self.stamp = app.stamp;
        self.read_only = app.read_only;
        self.open = Some(Open {
            name: app.name,
            workspace: app.workspace,
            unsaved: app.unsaved,
        });
        Ok(())
    }

    /// Make `workspace` the open workspace's content: in the app as one
    /// undo step, or here.
    fn commit(&mut self, workspace: WorkspaceDocument) -> Result<(), String> {
        if let Some(link) = &mut self.link {
            if self.read_only {
                return Err(
                    "this is a read-only demo; open an editable copy with workspace \
                     {\"action\": \"open_demo\"}"
                        .into(),
                );
            }
            link.commit(workspace.clone(), self.stamp)?;
        }
        let open = self.open.as_mut().ok_or("no workspace is open")?;
        open.workspace = workspace;
        open.unsaved = true;
        self.generation += 1;
        Ok(())
    }

    /// Replace the open workspace: in the app, or here.
    fn replace(
        &mut self,
        name: String,
        workspace: WorkspaceDocument,
        unsaved: bool,
    ) -> Result<(), String> {
        if let Some(link) = &mut self.link {
            link.open(name.clone(), workspace.clone(), unsaved)?;
        }
        self.generation += 1;
        self.open = Some(Open {
            name,
            workspace,
            unsaved,
        });
        Ok(())
    }

    /// The `transport` tool: the simulation the editor shows.
    pub fn transport_tool(&mut self, command: Map<String, Value>) -> Result<String, String> {
        self.link.as_mut().ok_or(NO_APP)?.transport(command)
    }

    /// The `app` tool: `state`, what the editor shows as text.
    pub fn app_tool(&mut self, what: &str) -> Result<String, String> {
        let link = self.link.as_mut().ok_or(NO_APP)?;
        match what {
            "state" => link.state(),
            other => Err(format!(
                "unknown \"{other}\" (the app tool has: state; screenshots and input are not \
                 built yet)"
            )),
        }
    }

    /// The open workspace, for hosts and tests.
    pub fn workspace(&self) -> Option<&WorkspaceDocument> {
        self.open.as_ref().map(|o| &o.workspace)
    }

    fn opened(&self) -> Result<&Open, String> {
        self.open.as_ref().ok_or_else(|| {
            "no workspace is open; call workspace with action \"open\" or \"new\" first".to_string()
        })
    }

    fn may_replace(&self, discard: bool) -> Result<(), String> {
        match &self.open {
            Some(open) if open.unsaved && !discard => Err(format!(
                "\"{}\" has unsaved changes; save it first, or pass \"discard\": true",
                open.name
            )),
            _ => Ok(()),
        }
    }

    /// The `workspace` tool: `list`, `open`, `new`, `save`, `status`,
    /// `demos`, `open_demo`.
    pub fn workspace_tool(
        &mut self,
        action: &str,
        name: Option<&str>,
        discard: bool,
    ) -> Result<String, String> {
        self.sync()?;
        let named = || {
            name.map(str::trim)
                .filter(|n| !n.is_empty())
                .ok_or_else(|| format!("\"{action}\" needs a \"name\""))
        };
        match action {
            "list" => {
                let names = self.store.list().map_err(|e| e.to_string())?;
                let mut out = format!("workspaces in {}:\n", self.store.dir().display());
                for n in &names {
                    out.push_str(&format!("  {n}\n"));
                }
                if names.is_empty() {
                    out.push_str("  (none)\n");
                }
                if let Some(open) = &self.open {
                    out.push_str(&format!(
                        "open: {}{}\n",
                        open.name,
                        if open.unsaved {
                            " (unsaved changes)"
                        } else {
                            ""
                        }
                    ));
                }
                Ok(out)
            }
            "open" => {
                let name = named()?;
                self.may_replace(discard)?;
                let workspace = self.store.read(name).map_err(|e| e.to_string())?;
                let out = format!("opened \"{name}\"\n{}", view::status(&workspace));
                self.replace(name.to_string(), workspace, false)?;
                Ok(out)
            }
            "demos" => {
                let catalog = catalog().map_err(str::to_string)?;
                let mut out = String::from("demos and gallery items (open one with open_demo):\n");
                for demo in catalog.all() {
                    out.push_str(&format!("  {}  {}\n", demo.id, demo.title));
                }
                Ok(out)
            }
            "open_demo" => {
                let id = named()?;
                self.may_replace(discard)?;
                let catalog = catalog().map_err(str::to_string)?;
                let demo = catalog
                    .get(id)
                    .ok_or_else(|| format!("no demo \"{id}\" (list them with action \"demos\")"))?;
                let name = self.store.unique_name(id);
                let workspace = crate::sim::demos::editable_copy(&demo.workspace, &name);
                let out = format!(
                    "opened a copy of \"{id}\" as \"{name}\" (not saved yet)\n{}",
                    view::status(&workspace)
                );
                self.replace(name, workspace, true)?;
                Ok(out)
            }
            "new" => {
                let name = named()?;
                self.may_replace(discard)?;
                if self.store.contains(name) {
                    return Err(format!(
                        "a workspace named \"{name}\" is already saved; open it, or pick another name"
                    ));
                }
                self.replace(
                    name.to_string(),
                    WorkspaceDocument::new(CircuitDocument::new(name)),
                    true,
                )?;
                Ok(format!("new workspace \"{name}\" (not saved yet)\n"))
            }
            "save" if self.link.is_some() => {
                // The editor saves: its rules, its messages.
                let requested = name.map(str::trim).filter(|n| !n.is_empty());
                let link = self.link.as_mut().expect("checked above");
                let said = link.save(requested.map(str::to_string))?;
                Ok(format!("{said}\n"))
            }
            "save" => {
                let open = self.open.as_mut().ok_or("no workspace is open")?;
                let requested = name.map(str::trim).filter(|n| !n.is_empty());
                let saved = self.store.save_open(
                    requested.unwrap_or(&open.name),
                    &open.name,
                    open.workspace.clone(),
                    OnConflict::Refuse,
                )?;
                open.workspace.root.id = saved.root;
                open.workspace.root.name = saved.name.clone();
                open.name = saved.name.clone();
                open.unsaved = false;
                let mut out = format!(
                    "saved \"{}\" in {}\n",
                    saved.name,
                    self.store.dir().display()
                );
                for port in saved.report.named_ports {
                    out.push_str(&format!("  named port {port}\n"));
                }
                Ok(out)
            }
            "status" => {
                let open = self.opened()?;
                Ok(format!(
                    "workspace \"{}\"{}\n{}",
                    open.name,
                    if open.unsaved {
                        " (unsaved changes)"
                    } else {
                        ""
                    },
                    view::status(&open.workspace)
                ))
            }
            other => Err(format!(
                "unknown action \"{other}\" (actions: list, open, new, save, status, demos, open_demo)"
            )),
        }
    }

    /// The `view` tool: `outline` (the default), `diagnostics`, `stats`,
    /// `groups`.
    pub fn view_tool(
        &mut self,
        circuit: Option<&str>,
        what: Option<&str>,
        filter: Option<&str>,
        limit: Option<usize>,
    ) -> Result<String, String> {
        self.sync()?;
        let ws = &self.opened()?.workspace;
        let doc = match batch::find_circuit(ws, circuit)? {
            Some(place @ (Place::Root | Place::Definition(_))) => batch::circuit_at(ws, place),
            None => {
                let mut names: Vec<&str> =
                    ws.definitions.values().map(|d| d.name.as_str()).collect();
                names.sort_unstable();
                return Err(format!(
                    "no circuit \"{}\" (circuits: main, {})",
                    circuit.unwrap_or_default(),
                    names.join(", ")
                ));
            }
        };
        match what.unwrap_or("outline") {
            "outline" => Ok(view::outline(
                ws,
                doc,
                filter,
                limit.unwrap_or(view::DEFAULT_LIMIT),
            )),
            "diagnostics" => Ok(view::diagnostics(ws)),
            "stats" => Ok(view::stats(ws, doc)),
            "groups" => Ok(view::groups(ws, doc)),
            "status" => Ok(view::status(ws)),
            other => Err(format!(
                "unknown view \"{other}\" (views: outline, diagnostics, stats, groups, status)"
            )),
        }
    }

    /// The `edit` tool. `request` holds `circuit` and `ops`, or `edits`;
    /// with `file` it is read from that JSON file instead. With `dry_run`
    /// the report is made and the workspace is left as it is.
    pub fn edit_tool(
        &mut self,
        mut request: Map<String, Value>,
        file: Option<&Path>,
        dry_run: bool,
    ) -> Result<String, String> {
        let request = match file {
            Some(path) => {
                if !request.is_empty() {
                    return Err("give \"file\", or \"circuit\"/\"ops\"/\"edits\", not both".into());
                }
                let size = std::fs::metadata(path)
                    .map_err(|e| format!("{}: {e}", path.display()))?
                    .len();
                if size > MAX_FILE_BYTES {
                    return Err(format!(
                        "{} is {size} bytes; the limit is {MAX_FILE_BYTES}",
                        path.display()
                    ));
                }
                let text = std::fs::read_to_string(path)
                    .map_err(|e| format!("{}: {e}", path.display()))?;
                serde_json::from_str::<Value>(&text)
                    .map_err(|e| format!("{}: not valid JSON: {e}", path.display()))?
            }
            None => {
                request.retain(|_, v| !v.is_null());
                Value::Object(request)
            }
        };
        self.sync()?;
        let open = self.opened()?;
        let (workspace, report) =
            batch::apply(&open.workspace, &request, &externals()).map_err(|e| e.0)?;
        if dry_run {
            return Ok(format!("dry run, nothing changed\n{report}"));
        }
        if workspace != open.workspace {
            self.commit(workspace)?;
        }
        Ok(report.to_string())
    }

    /// The `import_rtl` tool: Verilog (`.v`, `.sv`; through Yosys) or a
    /// Yosys JSON netlist. `into` is `"new"` (a new workspace named `name`
    /// or after the top module) or `"open"` (the modules become chips of
    /// the open workspace; with `replace`, chips of the same name are
    /// replaced). The default is `"open"` when a workspace is open.
    pub fn import_rtl_tool(
        &mut self,
        file: &Path,
        top: Option<&str>,
        name: Option<&str>,
        into: Option<&str>,
        replace: bool,
        discard: bool,
    ) -> Result<String, String> {
        self.sync()?;
        let into = into.unwrap_or(if self.open.is_some() { "open" } else { "new" });
        if !matches!(into, "new" | "open") {
            return Err(format!("\"into\" is \"new\" or \"open\", not \"{into}\""));
        }
        if into == "new" {
            self.may_replace(discard)?;
        } else {
            self.opened()?;
        }
        let imported = super::import::file(file, top, name)?;
        let mut out = String::new();
        if into == "new" {
            let name = self.store.unique_name(&imported.workspace.root.name);
            let mut workspace = imported.workspace;
            workspace.root.name = name.clone();
            out.push_str(&format!(
                "imported {} as new workspace \"{name}\" (not saved yet)\n{}",
                file.display(),
                view::status(&workspace)
            ));
            self.replace(name, workspace, true)?;
        } else {
            let open = self.open.as_ref().expect("checked above");
            let mut workspace = open.workspace.clone();
            let done = super::import::add_as_chips(&mut workspace, &imported.workspace, replace)?;
            out.push_str(&format!(
                "imported {} into \"{}\"\n",
                file.display(),
                open.name
            ));
            for line in done {
                out.push_str(&line);
                out.push('\n');
            }
            out.push_str(&view::diagnostics(&workspace));
            self.commit(workspace)?;
        }
        for warning in imported.warnings {
            out.push_str(&format!("warning: {warning}\n"));
        }
        Ok(out)
    }

    /// The `test` tool: run the stored tests of one circuit, or of every
    /// circuit of the workspace; with `name`, only tests of that name.
    /// `Err` holds the report when a test failed.
    pub fn test_tool(
        &mut self,
        circuit: Option<&str>,
        name: Option<&str>,
    ) -> Result<String, String> {
        self.sync()?;
        let report = testing::run_suite(&self.opened()?.workspace, circuit, name)?;
        if report.failed() > 0 {
            Err(report.text())
        } else {
            Ok(report.text())
        }
    }

    /// The `run` tool: an experiment script on a private simulation of
    /// `circuit`. With `session` the simulation and its snapshots are
    /// kept for the next call with the same name. With `file` the script
    /// is read from that JSON file. `Err` holds the report when the
    /// experiment failed.
    pub fn run_tool(
        &mut self,
        circuit: Option<&str>,
        session: Option<&str>,
        mut script: Map<String, Value>,
        file: Option<&Path>,
    ) -> Result<String, String> {
        script.retain(|_, v| !v.is_null());
        if let Some(path) = file {
            let text =
                std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
            let Value::Object(from_file) = serde_json::from_str::<Value>(&text)
                .map_err(|e| format!("{}: not valid JSON: {e}", path.display()))?
            else {
                return Err(format!("{}: a script is an object", path.display()));
            };
            for (k, v) in from_file {
                script.entry(k).or_insert(v);
            }
        }
        // A script file may name its circuit and session too.
        let take = |script: &mut Map<String, Value>, key: &str| match script.shift_remove(key) {
            Some(Value::String(s)) => Some(s),
            _ => None,
        };
        let circuit = circuit
            .map(str::to_string)
            .or_else(|| take(&mut script, "circuit"));
        let session = session
            .map(str::to_string)
            .or_else(|| take(&mut script, "session"));
        script.shift_remove("circuit");
        script.shift_remove("session");

        self.sync()?;
        let generation = self.generation;
        let ws = &self
            .open
            .as_ref()
            .ok_or("no workspace is open; call workspace with action \"open\" or \"new\" first")?
            .workspace;
        let place = batch::find_circuit(ws, circuit.as_deref())?
            .ok_or_else(|| format!("no circuit \"{}\"", circuit.clone().unwrap_or_default()))?;
        let circuit_id = batch::circuit_at(ws, place).id;

        let mut note = String::new();
        let kept = session.as_deref().and_then(|name| {
            let i = self.sims.iter().position(|s| s.name == name)?;
            let kept = self.sims.remove(i);
            if kept.circuit == circuit_id && kept.generation == generation {
                note = format!(
                    "session \"{name}\" continues at tick {}\n",
                    kept.state.rig.tick_count()
                );
                Some(kept.state)
            } else {
                note = format!(
                    "session \"{name}\" was dropped (the workspace or the circuit changed); \
                     starting from power-on\n"
                );
                None
            }
        });
        let mut state = match kept {
            Some(state) => state,
            None => RunState::new(Rig::build(ws, place)?),
        };
        let outcome = run::run(&mut state, &script);
        if let Some(name) = session {
            if self.sims.len() >= MAX_SIM_SESSIONS {
                let dropped = self.sims.remove(0);
                note.push_str(&format!(
                    "session \"{}\" was dropped (at most {MAX_SIM_SESSIONS} are kept)\n",
                    dropped.name
                ));
            }
            self.sims.push(SimSession {
                name,
                circuit: circuit_id,
                generation,
                state,
            });
        }
        let outcome = outcome?;
        let text = format!("{note}{}", outcome.text);
        if outcome.failed { Err(text) } else { Ok(text) }
    }
}
