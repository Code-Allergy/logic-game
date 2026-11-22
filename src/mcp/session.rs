//! The tools as plain functions on one open workspace
//! (`docs/mcp-server.md`, "Tools"): what the server calls, with no
//! protocol in it, so that tests and other hosts can call it too.
//!
//! A [`Session`] holds the workspace it has open as a document. Edits
//! change that document; nothing reaches the disk until `save`.

use std::path::Path;

use serde_json::{Map, Value};

use super::batch::{self, Place};
use super::view;
use crate::editor::workspace::{OnConflict, WorkspaceStore};
use crate::sim::demos::catalog;
use crate::sim::source::Externals;
use crate::sim::{CircuitDocument, WorkspaceDocument};

/// Largest request file read, in bytes.
pub const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;

struct Open {
    /// The name it is saved under (or will be).
    name: String,
    workspace: WorkspaceDocument,
    /// Changed since it was opened or saved.
    unsaved: bool,
}

/// One client's state: the store it reads and saves in, and the open
/// workspace.
pub struct Session {
    store: WorkspaceStore,
    open: Option<Open>,
}

fn externals() -> Externals<'static> {
    catalog().map(|c| c.externals()).unwrap_or_default()
}

impl Session {
    pub fn new(store: WorkspaceStore) -> Self {
        Self { store, open: None }
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

    /// The `workspace` tool: `list`, `open`, `new`, `save`, `status`.
    pub fn workspace_tool(
        &mut self,
        action: &str,
        name: Option<&str>,
        discard: bool,
    ) -> Result<String, String> {
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
                self.open = Some(Open {
                    name: name.to_string(),
                    workspace,
                    unsaved: false,
                });
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
                self.open = Some(Open {
                    name: name.to_string(),
                    workspace: WorkspaceDocument::new(CircuitDocument::new(name)),
                    unsaved: true,
                });
                Ok(format!("new workspace \"{name}\" (not saved yet)\n"))
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
                "unknown action \"{other}\" (actions: list, open, new, save, status)"
            )),
        }
    }

    /// The `view` tool: `outline` (the default), `diagnostics`, `stats`.
    pub fn view_tool(
        &self,
        circuit: Option<&str>,
        what: Option<&str>,
        filter: Option<&str>,
        limit: Option<usize>,
    ) -> Result<String, String> {
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
            other => Err(format!(
                "unknown view \"{other}\" (views: outline, diagnostics, stats)"
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
        let open = self
            .open
            .as_mut()
            .ok_or("no workspace is open; call workspace with action \"open\" or \"new\" first")?;
        let (workspace, report) =
            batch::apply(&open.workspace, &request, &externals()).map_err(|e| e.0)?;
        if dry_run {
            return Ok(format!("dry run, nothing changed\n{report}"));
        }
        if workspace != open.workspace {
            open.workspace = workspace;
            open.unsaved = true;
        }
        Ok(report.to_string())
    }
}
