//! RTL import for the server: Verilog through Yosys, or a Yosys JSON
//! netlist, into a new workspace or as chips of the open one
//! (`docs/mcp-server.md`, "`import_rtl`"; the mapping itself is
//! [`crate::sim::rtl`], `docs/rtl-import.md`).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use uuid::Uuid;

use super::batch;
use crate::sim::rtl::{ImportOptions, import_yosys_json};
use crate::sim::source::{self, Externals, NodeBody};
use crate::sim::{CircuitDocument, GateKind, WorkspaceDocument};

/// The Yosys passes between reading the Verilog and writing the netlist
/// (`docs/rtl-import.md`, "Producing the netlist"): processes become
/// flip-flops and multiplexers, memories stay whole.
pub const YOSYS_PASSES: &str = "proc; opt; memory -nomap; opt";

/// An imported netlist, ready for a workspace.
#[derive(Debug, Clone)]
pub struct Imported {
    /// The top module's circuit as the root, the other modules as chips.
    /// Every part is labelled with its name in the netlist.
    pub workspace: WorkspaceDocument,
    pub warnings: Vec<String>,
}

/// Run Yosys on Verilog files and return the JSON netlist it writes.
pub fn yosys(files: &[PathBuf], top: Option<&str>) -> Result<String, String> {
    let quoted = |path: &Path| -> Result<String, String> {
        let text = path.display().to_string();
        if text.contains(['"', ';', '\n']) {
            return Err(format!(
                "{text}: a file name with \" or ; cannot be given to Yosys"
            ));
        }
        if !path.is_file() {
            return Err(format!("{text}: no such file"));
        }
        Ok(format!("\"{text}\""))
    };
    if let Some(top) = top
        && !top
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
    {
        return Err(format!("\"{top}\" is not a module name"));
    }
    let out =
        std::env::temp_dir().join(format!("gatequest-yosys-{}.json", Uuid::new_v4().simple()));
    let mut script = String::new();
    for file in files {
        script.push_str(&format!("read_verilog -sv {}; ", quoted(file)?));
    }
    script.push_str(&match top {
        Some(top) => format!("hierarchy -check -top {top}; "),
        None => "hierarchy -check -auto-top; ".to_string(),
    });
    script.push_str(&format!("{YOSYS_PASSES}; write_json \"{}\"", out.display()));
    let run = Command::new("yosys").args(["-q", "-p", &script]).output();
    let run = match run {
        Ok(run) => run,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(
                "Yosys is not installed (no `yosys` on PATH). Install it (the Nix flake \
                 provides it), or run it yourself and pass the JSON netlist: yosys -p \
                 'read_verilog top.v; hierarchy -auto-top; proc; opt; memory -nomap; opt; \
                 write_json top.json'"
                    .to_string(),
            );
        }
        Err(e) => return Err(format!("could not run yosys: {e}")),
    };
    if !run.status.success() {
        let _ = std::fs::remove_file(&out);
        let mut said = String::from_utf8_lossy(&run.stderr).trim().to_string();
        if said.is_empty() {
            said = String::from_utf8_lossy(&run.stdout).trim().to_string();
        }
        // The last lines hold the error.
        let lines: Vec<&str> = said.lines().collect();
        let tail = lines[lines.len().saturating_sub(12)..].join("\n");
        return Err(format!("yosys failed:\n{tail}"));
    }
    let text = std::fs::read_to_string(&out).map_err(|e| format!("{}: {e}", out.display()));
    let _ = std::fs::remove_file(&out);
    text
}

/// Import a Yosys JSON netlist as a workspace named `name` (by default
/// the top module's name).
pub fn netlist(text: &str, top: Option<&str>, name: Option<&str>) -> Result<Imported, String> {
    let options = ImportOptions {
        top: top.map(str::to_string),
        name: name.map(str::to_string),
    };
    let import = import_yosys_json(text, &options).map_err(|e| e.0)?;
    // Expand the file again under our own key, and label every part with
    // its name, so that signals and later edits can name it.
    let key = format!("imports/{}", import.name);
    let expansion = source::expand(&import.source, &key, &Externals::default()).map_err(|e| e.0)?;
    let mut labelled: HashMap<Uuid, CircuitDocument> = HashMap::new();
    for (circuit, (_, doc)) in import.source.circuits.iter().zip(&expansion.circuits) {
        let mut doc = (**doc).clone();
        let mut names: HashMap<Uuid, String> = HashMap::new();
        for node in &circuit.nodes {
            // Ports and instances have their labels already.
            if let NodeBody::Use(_) = node.body {
                continue;
            }
            match node.array {
                None => {
                    names.insert(
                        source::item_id(doc.id, &format!("node/{}", node.name)),
                        node.name.clone(),
                    );
                }
                Some(array) => {
                    for i in 0..array.count {
                        let name = format!("{}[{i}]", node.name);
                        names.insert(source::item_id(doc.id, &format!("node/{name}")), name);
                    }
                }
            }
        }
        let taken: HashSet<String> = doc.nodes.iter().filter_map(|n| n.label.clone()).collect();
        for node in &mut doc.nodes {
            if node.label.is_none()
                && let Some(name) = names.remove(&node.id)
                && !taken.contains(&name)
            {
                node.label = Some(name);
            }
        }
        labelled.insert(doc.id, doc);
    }
    let mut workspace = expansion.workspace;
    workspace.root = labelled
        .remove(&workspace.root.id)
        .unwrap_or(workspace.root);
    workspace.root.name = import.name.clone();
    for (id, def) in &mut workspace.definitions {
        if let Some(doc) = labelled.remove(id) {
            *def = Arc::new(doc);
        }
    }
    Ok(Imported {
        workspace,
        warnings: import.warnings,
    })
}

/// Read `file` as Verilog (`.v`, `.sv`: through Yosys) or as a Yosys
/// JSON netlist (anything else), and import it.
pub fn file(path: &Path, top: Option<&str>, name: Option<&str>) -> Result<Imported, String> {
    let verilog = matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("v" | "sv" | "vh" | "verilog")
    );
    let text = if verilog {
        yosys(&[path.to_path_buf()], top)?
    } else {
        std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?
    };
    netlist(&text, top, name).map_err(|e| format!("{}: {e}", path.display()))
}

/// Add the circuits of `imported` to `ws` as chips: the top module too.
/// A chip of `ws` with the name of an imported one is an error, or with
/// `replace` is replaced, keeping its identity so that its placements
/// now use the new one (their wires follow the ports by label). Returns
/// what was done, one line per chip, and wires that had to be dropped.
pub fn add_as_chips(
    ws: &mut WorkspaceDocument,
    imported: &WorkspaceDocument,
    replace: bool,
) -> Result<Vec<String>, String> {
    let mut circuits: Vec<&CircuitDocument> = imported.definitions.values().map(|d| &**d).collect();
    circuits.sort_by(|a, b| (&a.name, a.id).cmp(&(&b.name, b.id)));
    circuits.insert(0, &imported.root);

    let mut remap: HashMap<Uuid, Uuid> = HashMap::new();
    let mut clashes = Vec::new();
    for doc in &circuits {
        let existing: Vec<Uuid> = ws
            .definitions
            .values()
            .filter(|d| d.name == doc.name)
            .map(|d| d.id)
            .collect();
        match existing.as_slice() {
            [] => {
                if ws.definitions.contains_key(&doc.id) || ws.root.id == doc.id {
                    return Err(format!(
                        "\"{}\" was imported before under another name; remove that chip first",
                        doc.name
                    ));
                }
            }
            [id] if replace => {
                remap.insert(doc.id, *id);
            }
            _ => clashes.push(doc.name.clone()),
        }
    }
    if !clashes.is_empty() {
        return Err(format!(
            "the workspace already has chips named {}; pass \"replace\": true to replace them",
            clashes.join(", ")
        ));
    }

    let mut done = Vec::new();
    for doc in circuits {
        let mut doc = doc.clone();
        let replaced = remap.get(&doc.id).copied();
        doc.id = replaced.unwrap_or(doc.id);
        for node in &mut doc.nodes {
            if let GateKind::SubCircuit { definition } = &mut node.kind
                && let Some(&id) = remap.get(definition)
            {
                *definition = id;
            }
        }
        let line = format!(
            "{} chip {}: {} parts, {} wires",
            if replaced.is_some() {
                "replaced"
            } else {
                "added"
            },
            doc.name,
            doc.nodes.len(),
            doc.wires.len()
        );
        match replaced.and_then(|id| ws.definitions.get(&id).cloned()) {
            Some(old) => {
                doc.revision = old.revision + 1;
                let dropped = batch::rebind_instances(ws, &old, &doc);
                ws.definitions.insert(doc.id, Arc::new(doc));
                done.push(line);
                done.extend(dropped.into_iter().map(|d| format!("  dropped wire {d}")));
            }
            None => {
                ws.definitions.insert(doc.id, Arc::new(doc));
                done.push(line);
            }
        }
    }
    Ok(done)
}
