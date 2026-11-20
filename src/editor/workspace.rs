//! Workspace files (save format versions 5 and 6; see
//! `docs/save-format.md`).
//!
//! The editor writes version 5, the expanded form. Version 6, the
//! hand-editable authoring form of the built-in demos
//! ([`crate::sim::source`]), is also read: it is expanded on load, with
//! `"demo/circuit"` references resolved against the built-in demos.
//!
//! A workspace is a folder `workspaces/<name>/` holding one
//! `manifest.json`:
//!
//! ```json
//! {
//!   "format": "gatequest-workspace",
//!   "version": 5,
//!   "name": "My circuit",
//!   "root": "<uuid of the root circuit>",
//!   "circuits": [ { "id": "...", "name": "...", "nodes": [...], "wires": [...] } ]
//! }
//! ```
//!
//! Files from earlier versions (manifest versions 1–4 and legacy single
//! `<name>.json` files) are refused with [`WorkspaceError::UnsupportedVersion`];
//! they are not migrated.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use bevy::prelude::ResMut;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::editor::EditorState;
use crate::sim::fixtures::fixture_workspace_catalog;
use crate::sim::source::{self, Externals};
use crate::sim::{CircuitDocument, CircuitLibrary, WorkspaceDocument, demos};
use crate::{DEFAULT_WORKSPACE_EXT, WORKSPACE_DIR};

pub const WORKSPACE_MANIFEST_FILE: &str = "manifest.json";
/// Format tag written to every manifest.
pub const WORKSPACE_FORMAT: &str = "gatequest-workspace";
/// The version the editor writes: the expanded form. Version 5 is the
/// first written for the simulation v2 circuit model; earlier versions are
/// refused.
pub const WORKSPACE_VERSION: u32 = 5;
/// The hand-editable authoring form, also read (see [`crate::sim::source`]).
pub const SOURCE_VERSION: u32 = source::SOURCE_VERSION as u32;

#[derive(Debug)]
pub enum WorkspaceError {
    Io(io::Error),
    Parse(serde_json::Error),
    /// A workspace in an older (or unknown) format.
    UnsupportedVersion {
        name: String,
        found: Option<u64>,
    },
    Invalid(String),
}

impl fmt::Display for WorkspaceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WorkspaceError::Io(e) => write!(f, "{e}"),
            WorkspaceError::Parse(e) => write!(f, "workspace file is not valid: {e}"),
            WorkspaceError::UnsupportedVersion { name, found } => {
                let found = found.map_or_else(
                    || "an unknown version".to_string(),
                    |v| format!("version {v}"),
                );
                write!(
                    f,
                    "Workspace \"{name}\" was saved in save format {found}, which this version of \
                     GateQuest cannot open (it reads versions {WORKSPACE_VERSION} and {SOURCE_VERSION}). \
                     Workspaces saved before the simulation v2 update are not converted; rebuild the circuit and save \
                     it again."
                )
            }
            WorkspaceError::Invalid(e) => write!(f, "workspace is invalid: {e}"),
        }
    }
}

impl std::error::Error for WorkspaceError {}

impl From<io::Error> for WorkspaceError {
    fn from(e: io::Error) -> Self {
        WorkspaceError::Io(e)
    }
}

impl From<serde_json::Error> for WorkspaceError {
    fn from(e: serde_json::Error) -> Self {
        WorkspaceError::Parse(e)
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct WorkspaceFile {
    format: String,
    version: u32,
    name: String,
    root: Uuid,
    circuits: Vec<CircuitDocument>,
}

pub fn catalog_init_system(mut editor_state: ResMut<EditorState>) {
    if editor_state.workspace_catalog.is_empty() {
        editor_state.workspace_catalog = fixture_workspace_catalog();
    }
}

pub fn load_saved_workspace_names() -> io::Result<Vec<String>> {
    list_workspaces_in(Path::new(WORKSPACE_DIR))
}

pub fn read_workspace(name: &str) -> Result<WorkspaceDocument, WorkspaceError> {
    read_workspace_in(Path::new(WORKSPACE_DIR), name)
}

pub fn save_workspace(name: &str, workspace: &WorkspaceDocument) -> Result<(), WorkspaceError> {
    save_workspace_in(Path::new(WORKSPACE_DIR), name, workspace)
}

/// Names of the workspaces in `dir`: folders with a manifest, and legacy
/// single-file workspaces (listed so that opening one explains why it is
/// refused).
pub fn list_workspaces_in(dir: &Path) -> io::Result<Vec<String>> {
    let mut names = HashSet::new();
    if !dir.exists() {
        return Ok(Vec::new());
    }
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        let name = if path.is_dir() {
            path.join(WORKSPACE_MANIFEST_FILE)
                .exists()
                .then(|| {
                    path.file_name()
                        .and_then(|v| v.to_str())
                        .map(str::to_string)
                })
                .flatten()
        } else if path.extension().and_then(|e| e.to_str())
            == Some(DEFAULT_WORKSPACE_EXT.trim_start_matches('.'))
        {
            path.file_stem()
                .and_then(|v| v.to_str())
                .map(str::to_string)
        } else {
            None
        };
        if let Some(name) = name {
            names.insert(name);
        }
    }
    let mut names: Vec<String> = names.into_iter().collect();
    names.sort_unstable();
    Ok(names)
}

fn version_of(value: &serde_json::Value) -> Option<u64> {
    value.get("version").and_then(serde_json::Value::as_u64)
}

pub fn read_workspace_in(dir: &Path, name: &str) -> Result<WorkspaceDocument, WorkspaceError> {
    let folder = workspace_path_in(dir, name);
    let manifest = folder.join(WORKSPACE_MANIFEST_FILE);
    if !manifest.exists() {
        let legacy = folder.with_extension(DEFAULT_WORKSPACE_EXT.trim_start_matches('.'));
        if legacy.exists() {
            // Pre-bundle single-file save (format version 1).
            let value: serde_json::Value =
                serde_json::from_slice(&fs::read(legacy)?).unwrap_or(serde_json::Value::Null);
            return Err(WorkspaceError::UnsupportedVersion {
                name: name.to_string(),
                found: version_of(&value).or(Some(1)),
            });
        }
        return Err(WorkspaceError::Io(io::Error::new(
            io::ErrorKind::NotFound,
            format!("workspace \"{name}\" not found"),
        )));
    }

    let text = fs::read_to_string(manifest)?;
    let value: serde_json::Value = serde_json::from_str(&text)?;
    let found = version_of(&value);
    let format_ok = value.get("format").and_then(|v| v.as_str()) == Some(WORKSPACE_FORMAT);
    if format_ok && found == Some(u64::from(SOURCE_VERSION)) {
        return read_source_workspace(name, &text);
    }
    if !format_ok || found != Some(u64::from(WORKSPACE_VERSION)) {
        return Err(WorkspaceError::UnsupportedVersion {
            name: name.to_string(),
            found,
        });
    }
    let file: WorkspaceFile = serde_json::from_value(value)?;

    let mut circuits: HashMap<Uuid, CircuitDocument> = HashMap::new();
    for circuit in file.circuits {
        if circuits.insert(circuit.id, circuit).is_some() {
            return Err(WorkspaceError::Invalid("two circuits share an id".into()));
        }
    }
    let root = circuits
        .remove(&file.root)
        .ok_or_else(|| WorkspaceError::Invalid(format!("root circuit {} is missing", file.root)))?;
    for circuit in std::iter::once(&root).chain(circuits.values()) {
        for d in circuit.instanced_definitions() {
            if !circuits.contains_key(&d) {
                return Err(WorkspaceError::Invalid(format!(
                    "circuit \"{}\" instances missing definition {d}",
                    circuit.name
                )));
            }
        }
    }
    let workspace = WorkspaceDocument {
        root,
        definitions: circuits
            .into_iter()
            .map(|(id, c)| (id, Arc::new(c)))
            .collect(),
    };
    let library = CircuitLibrary {
        definitions: workspace.definitions.clone(),
    };
    if let Some(id) = library.find_cycle() {
        let name = library.get(id).map_or_else(String::new, |d| d.name.clone());
        return Err(WorkspaceError::Invalid(format!(
            "subcircuit \"{name}\" ({id}) contains itself"
        )));
    }
    validate_required_io_labels(&workspace)?;
    Ok(workspace)
}

/// Key that derives the ids of a version 6 workspace file.
pub fn workspace_key(name: &str) -> String {
    format!("workspaces/{}", sanitize_workspace_name(name))
}

/// Read a version 6 workspace: expand it, resolving `"demo/circuit"`
/// references against the built-in demos.
fn read_source_workspace(name: &str, text: &str) -> Result<WorkspaceDocument, WorkspaceError> {
    let invalid = |e: source::SourceError| WorkspaceError::Invalid(e.to_string());
    let file = source::parse(text).map_err(invalid)?;
    let externals = demos::catalog()
        .map(|c| c.externals())
        .unwrap_or_else(|_| Externals::default());
    let expansion = source::expand(&file, &workspace_key(name), &externals).map_err(invalid)?;
    validate_required_io_labels(&expansion.workspace)?;
    Ok(expansion.workspace)
}

pub fn save_workspace_in(
    dir: &Path,
    name: &str,
    workspace: &WorkspaceDocument,
) -> Result<(), WorkspaceError> {
    validate_required_io_labels(workspace)?;
    let folder = workspace_path_in(dir, name);
    if folder.is_file() {
        fs::remove_file(&folder)?;
    }
    fs::create_dir_all(&folder)?;

    let mut definitions: Vec<&Arc<CircuitDocument>> = workspace.definitions.values().collect();
    definitions.sort_by_key(|d| d.id);
    let file = WorkspaceFile {
        format: WORKSPACE_FORMAT.to_string(),
        version: WORKSPACE_VERSION,
        name: name.to_string(),
        root: workspace.root.id,
        circuits: std::iter::once(workspace.root.clone())
            .chain(definitions.into_iter().map(|d| (**d).clone()))
            .collect(),
    };
    let json = serde_json::to_vec_pretty(&file)?;
    let manifest = folder.join(WORKSPACE_MANIFEST_FILE);
    let tmp = folder.join("manifest.json.tmp");
    fs::write(&tmp, json)?;
    fs::rename(tmp, manifest)?;
    Ok(())
}

/// Input, Output and Display nodes must be labelled (labels name ports).
fn validate_required_io_labels(workspace: &WorkspaceDocument) -> Result<(), WorkspaceError> {
    let mut missing = Vec::new();
    for circuit in
        std::iter::once(&workspace.root).chain(workspace.definitions.values().map(|d| &**d))
    {
        for node in &circuit.nodes {
            if node.kind.is_port() && node.label.as_deref().is_none_or(|v| v.trim().is_empty()) {
                missing.push(format!(
                    "{} node {} in circuit \"{}\"",
                    node.kind.display_name(),
                    node.id,
                    circuit.name
                ));
            }
        }
    }
    if missing.is_empty() {
        return Ok(());
    }
    let preview = if missing.len() > 12 {
        format!("{}, ...", missing[..12].join(", "))
    } else {
        missing.join(", ")
    };
    Err(WorkspaceError::Invalid(format!(
        "Input/Output/Display nodes need labels; {} unlabeled: {preview}",
        missing.len()
    )))
}

pub(crate) fn workspace_path_in(dir: &Path, name: &str) -> PathBuf {
    dir.join(sanitize_workspace_name(name))
}

pub(crate) fn sanitize_workspace_name(name: &str) -> String {
    name.chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c => c,
        })
        .collect()
}
