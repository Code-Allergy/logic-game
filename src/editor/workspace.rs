//! Workspace files (save format versions 5 and 6; see
//! `docs/save-format.md`).
//!
//! The editor writes version 5, the expanded form. Version 6, the
//! hand-editable authoring form of the built-in demos
//! ([`crate::sim::source`]), is also read: it is expanded on load, with
//! `"demo/circuit"` references resolved against the built-in demos.
//!
//! A workspace is a folder `<workspaces>/<name>/` holding one
//! `manifest.json` (and `manifest.json.bak`, the previous save). The
//! workspaces folder is in the player's data folder (see
//! [`WorkspaceStore::player`]):
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
//! they are not migrated. So are files from a newer version of the game.
//!
//! Names: a workspace name becomes a folder name through
//! [`sanitize_workspace_name`] (no path separators, no `..`, no leading
//! dot, no Windows device names, no trailing dots or spaces), and two
//! names are the same workspace slot when their [`name_key`]s match
//! (case-insensitively, as on Windows and macOS file systems). A save
//! never replaces a *different* workspace (another root circuit id): see
//! [`WorkspaceStore::plan_save`].

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::sim::document::CircuitTests;
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
    /// A workspace in an older (or unknown) format, or made by a newer
    /// version of the game (`newer`).
    UnsupportedVersion {
        name: String,
        found: Option<u64>,
        newer: bool,
    },
    Invalid(String),
}

impl fmt::Display for WorkspaceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WorkspaceError::Io(e) => write!(f, "{e}"),
            WorkspaceError::Parse(e) => write!(f, "workspace file is not valid: {e}"),
            WorkspaceError::UnsupportedVersion {
                name,
                found: Some(found),
                newer: true,
            } => write!(
                f,
                "Workspace \"{name}\" was made by a newer version of GateQuest (save format \
                 version {found}); this version reads versions {WORKSPACE_VERSION} and \
                 {SOURCE_VERSION}. Update GateQuest to open it. The file is left as it is."
            ),
            WorkspaceError::UnsupportedVersion { name, found, .. } => {
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
    /// Stored tests by circuit id (left out when there are none).
    #[serde(default, skip_serializing_if = "CircuitTests::is_empty")]
    tests: CircuitTests,
}

/// Environment variable that overrides the player's data folder (the
/// workspaces go in its `workspaces` subfolder). Tests and tools use it so
/// they never touch the player's files.
pub const DATA_DIR_ENV: &str = "GATEQUEST_DATA_DIR";

/// Message for file operations in the web build.
pub const WEB_SAVE_UNAVAILABLE: &str = "Saving isn't available in the web version yet.";

/// The player's data folder, from the environment:
///
/// - `GATEQUEST_DATA_DIR` when set;
/// - Windows: `%APPDATA%\GateQuest`;
/// - macOS: `~/Library/Application Support/GateQuest`;
/// - other systems: `$XDG_DATA_HOME/gatequest`, else
///   `~/.local/share/gatequest`.
///
/// `None` when none of these is set (to an absolute path).
pub fn player_data_dir() -> Option<PathBuf> {
    data_dir_from(|key| std::env::var_os(key))
}

/// [`player_data_dir`] with the environment read through `var`.
pub fn data_dir_from(var: impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    if let Some(dir) = var(DATA_DIR_ENV).filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(dir));
    }
    let absolute = |key: &str| {
        var(key)
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
    };
    if cfg!(target_os = "windows") {
        absolute("APPDATA").map(|dir| dir.join("GateQuest"))
    } else if cfg!(target_os = "macos") {
        absolute("HOME").map(|home| home.join("Library/Application Support/GateQuest"))
    } else {
        absolute("XDG_DATA_HOME")
            .or_else(|| absolute("HOME").map(|home| home.join(".local/share")))
            .map(|dir| dir.join("gatequest"))
    }
}

/// What is saved under a name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stored {
    Missing,
    /// A workspace with this root circuit id.
    Root(Uuid),
    /// Something this version cannot open (an old or newer format, a
    /// damaged file): never overwritten.
    Unreadable,
}

/// What a save does when the name holds a different workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnConflict {
    /// Refuse, with a message (an explicit save: the player picks another
    /// name).
    Refuse,
    /// Save under a free name instead (saving on exit: nothing is lost).
    Rename,
}

/// Where and how a save goes (see [`WorkspaceStore::plan_save`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavePlan {
    pub name: String,
    /// A save under a new name ("Save As"): the copy gets a fresh root id,
    /// so it is a workspace of its own (chips are keyed by root id).
    pub fresh_root: bool,
}

/// What a save did besides writing the file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SaveReport {
    /// Ports that had no label and were named in the file, as
    /// `"in0" (Input in "Adder")`.
    pub named_ports: Vec<String>,
}

/// A finished save of the open workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Saved {
    pub name: String,
    /// The root id written (fresh after a Save As).
    pub root: Uuid,
    pub save_as: bool,
    pub report: SaveReport,
}

impl Saved {
    /// The status line for the player.
    pub fn message(&self, previous: &str) -> String {
        let mut message = if self.save_as && !previous.trim().is_empty() {
            format!(
                "Saved as a new workspace \"{}\" (\"{previous}\" is unchanged).",
                self.name
            )
        } else {
            format!("Saved workspace \"{}\".", self.name)
        };
        if !self.report.named_ports.is_empty() {
            message.push_str(&format!(
                " Unlabelled ports were named {}.",
                self.report.named_ports.join(", ")
            ));
        }
        message
    }
}

/// The player's saved workspaces: a folder of workspace folders, plus,
/// read-only, the folder older versions used (`./workspaces` in the
/// launch folder) so that nothing saved there goes missing. Workspaces are
/// always written to the main folder; one found only in the old folder is
/// moved over by its next save (the old copy stays where it is).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceStore {
    dir: PathBuf,
    legacy: Option<PathBuf>,
}

impl WorkspaceStore {
    /// The workspaces in `dir` only.
    pub fn in_dir(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            legacy: None,
        }
    }

    /// The workspaces in `dir`, falling back to `legacy` for reading.
    pub fn with_legacy(dir: impl Into<PathBuf>, legacy: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            legacy: Some(legacy.into()),
        }
    }

    /// The player's workspaces: `workspaces/` in [`player_data_dir`] (or
    /// `./workspaces` when there is no data folder), reading the old
    /// `./workspaces` too when it exists. `None` in the web build, which
    /// has no file system.
    pub fn player() -> Option<Self> {
        if cfg!(target_arch = "wasm32") {
            return None;
        }
        let old = Path::new(WORKSPACE_DIR);
        Some(match player_data_dir() {
            Some(data) => {
                let dir = data.join("workspaces");
                // An explicit data folder (tests, tools) stands alone.
                let overridden = std::env::var_os(DATA_DIR_ENV).is_some_and(|v| !v.is_empty());
                let same = fs::canonicalize(old).ok() == fs::canonicalize(&dir).ok();
                if !overridden && old.is_dir() && !same {
                    Self::with_legacy(dir, old)
                } else {
                    Self::in_dir(dir)
                }
            }
            None => Self::in_dir(old),
        })
    }

    /// The folder saves go to.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The older folder also read, if any.
    pub fn legacy(&self) -> Option<&Path> {
        self.legacy.as_deref()
    }

    fn dirs(&self) -> impl Iterator<Item = &Path> {
        std::iter::once(self.dir.as_path()).chain(self.legacy.as_deref())
    }

    /// Workspace names, sorted; a name in both folders is listed once.
    pub fn list(&self) -> io::Result<Vec<String>> {
        let mut keys = HashSet::new();
        let mut names = Vec::new();
        for (i, dir) in self.dirs().enumerate() {
            let listed = match list_workspaces_in(dir) {
                Ok(listed) => listed,
                // The old folder is a courtesy: its errors are not the
                // player's problem.
                Err(_) if i > 0 => continue,
                Err(e) => return Err(e),
            };
            for name in listed {
                if keys.insert(name_key(&name)) {
                    names.push(name);
                }
            }
        }
        names.sort_unstable();
        Ok(names)
    }

    /// The saved entry for `name` (compared by [`name_key`]): its folder
    /// path and its name on disk, main folder first.
    fn find(&self, name: &str) -> Option<(PathBuf, String)> {
        let key = name_key(name);
        self.dirs().find_map(|dir| {
            list_workspaces_in(dir)
                .ok()?
                .into_iter()
                .find(|entry| name_key(entry) == key)
                .map(|entry| (dir.join(&entry), entry))
        })
    }

    /// Whether a workspace (or anything that looks like one) is saved
    /// under `name`.
    pub fn contains(&self, name: &str) -> bool {
        self.find(name).is_some()
    }

    pub fn read(&self, name: &str) -> Result<WorkspaceDocument, WorkspaceError> {
        match self.find(name) {
            Some((folder, entry)) => read_workspace_at(&folder, &entry),
            None => Err(WorkspaceError::Io(io::Error::new(
                io::ErrorKind::NotFound,
                format!("workspace \"{name}\" not found"),
            ))),
        }
    }

    /// What is saved under `name`.
    pub fn stored(&self, name: &str) -> Stored {
        match self.find(name) {
            None => Stored::Missing,
            Some((folder, entry)) => match read_workspace_at(&folder, &entry) {
                Ok(ws) => Stored::Root(ws.root.id),
                Err(_) => Stored::Unreadable,
            },
        }
    }

    /// Write `workspace` under `name` in the main folder, as it is (see
    /// [`WorkspaceStore::save_open`] for the checks). An existing folder
    /// whose name differs only in case is reused.
    pub fn save(
        &self,
        name: &str,
        workspace: &WorkspaceDocument,
    ) -> Result<SaveReport, WorkspaceError> {
        let key = name_key(name);
        let existing = list_workspaces_in(&self.dir)
            .ok()
            .and_then(|names| names.into_iter().find(|entry| name_key(entry) == key))
            .map(|entry| self.dir.join(entry))
            .filter(|path| path.is_dir());
        let folder = existing.unwrap_or_else(|| workspace_path_in(&self.dir, name));
        save_workspace_at(&folder, name, workspace)
    }

    /// `base`, or the first free name after it: `"Adder (2)"`, or for a
    /// copy `"Adder (copy 2)"`.
    pub fn unique_name(&self, base: &str) -> String {
        let taken: HashSet<String> = self
            .list()
            .unwrap_or_default()
            .iter()
            .map(|name| name_key(name))
            .collect();
        unique_name_among(base, |name| taken.contains(&name_key(name)))
    }

    /// Where a save of the open workspace (named `current`, root `root`)
    /// asked under `requested` goes:
    ///
    /// - the same name (by [`name_key`]): a plain save, allowed over the
    ///   same workspace (same root id) or a free name;
    /// - another name: a Save As, to a free name only, with a fresh root
    ///   id.
    ///
    /// A name holding a different workspace (or a file this version cannot
    /// read) is refused, or with [`OnConflict::Rename`] replaced by a free
    /// one.
    pub fn plan_save(
        &self,
        requested: &str,
        current: &str,
        root: Uuid,
        on_conflict: OnConflict,
    ) -> Result<SavePlan, String> {
        let requested = requested.trim();
        if requested.is_empty() {
            return Err("Type a name for the workspace first.".into());
        }
        let save_as = name_key(requested) != name_key(current.trim());
        let fits = match self.stored(requested) {
            Stored::Missing => true,
            Stored::Root(saved) => !save_as && saved == root,
            Stored::Unreadable => false,
        };
        if fits {
            return Ok(SavePlan {
                name: requested.to_string(),
                fresh_root: save_as,
            });
        }
        match on_conflict {
            OnConflict::Refuse => Err(format!(
                "A different workspace named \"{requested}\" is already saved. Pick another \
                 name, or load that one to change it."
            )),
            OnConflict::Rename => Ok(SavePlan {
                name: self.unique_name(requested),
                fresh_root: save_as,
            }),
        }
    }

    /// Save the open workspace `workspace` (named `current`) under
    /// `requested` following [`WorkspaceStore::plan_save`]: a Save As
    /// gives the root a fresh id, and the root takes the name it is saved
    /// under.
    pub fn save_open(
        &self,
        requested: &str,
        current: &str,
        mut workspace: WorkspaceDocument,
        on_conflict: OnConflict,
    ) -> Result<Saved, String> {
        let plan = self.plan_save(requested, current, workspace.root.id, on_conflict)?;
        if plan.fresh_root {
            let old = workspace.root.id;
            workspace.root.id = Uuid::new_v4();
            // The root's tests follow it to its new id.
            if let Some(tests) = workspace.tests.remove(&old) {
                workspace.tests.insert(workspace.root.id, tests);
            }
        }
        workspace.root.name = plan.name.clone();
        let report = self
            .save(&plan.name, &workspace)
            .map_err(|e| format!("Could not save \"{}\": {e}", plan.name))?;
        Ok(Saved {
            name: plan.name,
            root: workspace.root.id,
            save_as: plan.fresh_root,
            report,
        })
    }
}

/// `base`, or the first name after it that `taken` does not hold:
/// `"base (2)"`, `"base (3)"`, ...; a name ending in `" (copy)"` goes on
/// `" (copy 2)"`, `" (copy 3)"`.
pub fn unique_name_among(base: &str, taken: impl Fn(&str) -> bool) -> String {
    let base = base.trim();
    let base = if base.is_empty() { "workspace" } else { base };
    if !taken(base) {
        return base.to_string();
    }
    let (stem, copy) = match base.strip_suffix(" (copy)") {
        Some(stem) => (stem, true),
        None => (base, false),
    };
    // A saved name keeps its first `MAX_NAME_CHARS` characters
    // (`sanitize_workspace_name`): a long stem is cut so that the number
    // is kept, or every candidate would be the same saved name.
    let candidate = |suffix: String| {
        let room = MAX_NAME_CHARS.saturating_sub(suffix.chars().count());
        let stem: String = stem.chars().take(room).collect();
        format!("{}{suffix}", stem.trim_end())
    };
    (2..=u32::MAX)
        .map(|n| {
            candidate(if copy {
                format!(" (copy {n})")
            } else {
                format!(" ({n})")
            })
        })
        .find(|name| !taken(name))
        .unwrap_or_else(|| candidate(format!(" ({})", Uuid::new_v4())))
}

/// Whether the open workspace must be saved when the app exits, and
/// where: `None` when nothing would be lost (no file system, a read-only
/// demo, no unsaved change, or an empty workspace that was never saved).
/// A name that holds a different workspace is never overwritten: the save
/// goes to a free name instead.
pub fn exit_save_plan(
    store: Option<&WorkspaceStore>,
    name: &str,
    root: Uuid,
    read_only: bool,
    dirty: bool,
    empty: bool,
) -> Option<SavePlan> {
    let store = store?;
    if read_only || !dirty {
        return None;
    }
    let name = if name.trim().is_empty() {
        "workspace"
    } else {
        name
    };
    if empty && store.stored(name) != Stored::Root(root) {
        return None;
    }
    store.plan_save(name, name, root, OnConflict::Rename).ok()
}

/// The player's saved workspace names (none in the web build).
pub fn load_saved_workspace_names() -> io::Result<Vec<String>> {
    match WorkspaceStore::player() {
        Some(store) => store.list(),
        None => Ok(Vec::new()),
    }
}

/// Read one of the player's saved workspaces.
pub fn read_workspace(name: &str) -> Result<WorkspaceDocument, WorkspaceError> {
    match WorkspaceStore::player() {
        Some(store) => store.read(name),
        None => Err(WorkspaceError::Io(io::Error::new(
            io::ErrorKind::Unsupported,
            WEB_SAVE_UNAVAILABLE,
        ))),
    }
}

/// Names of the workspaces in `dir`: folders with a manifest, and legacy
/// single-file workspaces (listed so that opening one explains why it is
/// refused). Hidden entries (a leading dot, such as the demo map's
/// progress file) are not workspaces.
pub fn list_workspaces_in(dir: &Path) -> io::Result<Vec<String>> {
    let mut names = HashSet::new();
    if !dir.exists() {
        return Ok(Vec::new());
    }
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with('.'))
        {
            continue;
        }
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
    read_workspace_at(&workspace_path_in(dir, name), name)
}

/// Read the workspace in `folder` (or the legacy single file beside it).
pub fn read_workspace_at(folder: &Path, name: &str) -> Result<WorkspaceDocument, WorkspaceError> {
    let manifest = folder.join(WORKSPACE_MANIFEST_FILE);
    if !manifest.exists() {
        let legacy = folder.file_name().map(|file| {
            let mut file = file.to_os_string();
            file.push(DEFAULT_WORKSPACE_EXT);
            folder.with_file_name(file)
        });
        if let Some(legacy) = legacy.filter(|path| path.is_file()) {
            // Pre-bundle single-file save (format version 1).
            let value: serde_json::Value =
                serde_json::from_slice(&fs::read(legacy)?).unwrap_or(serde_json::Value::Null);
            return Err(WorkspaceError::UnsupportedVersion {
                name: name.to_string(),
                found: version_of(&value).or(Some(1)),
                newer: false,
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
        let newest = u64::from(WORKSPACE_VERSION.max(SOURCE_VERSION));
        return Err(WorkspaceError::UnsupportedVersion {
            name: name.to_string(),
            found,
            newer: format_ok && found.is_some_and(|v| v > newest),
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
    let mut workspace = WorkspaceDocument {
        root,
        definitions: circuits
            .into_iter()
            .map(|(id, c)| (id, Arc::new(c)))
            .collect(),
        tests: file.tests,
    };
    workspace.tests = workspace.kept_tests();
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

/// Write `workspace` to `dir` under `name` (see [`save_workspace_at`]).
pub fn save_workspace_in(
    dir: &Path,
    name: &str,
    workspace: &WorkspaceDocument,
) -> Result<SaveReport, WorkspaceError> {
    save_workspace_at(&workspace_path_in(dir, name), name, workspace)
}

/// Write `workspace` as the workspace folder `folder`: the manifest is
/// written atomically, the previous one kept as `manifest.json.bak`.
/// Unlabelled ports are named in the file (see [`label_unlabelled_ports`]).
pub fn save_workspace_at(
    folder: &Path,
    name: &str,
    workspace: &WorkspaceDocument,
) -> Result<SaveReport, WorkspaceError> {
    let (workspace, named_ports) = label_unlabelled_ports(workspace);
    if folder.is_file() {
        fs::remove_file(folder)?;
    }
    fs::create_dir_all(folder)?;
    let json = workspace_json(name, &workspace)?;
    write_atomic(&folder.join(WORKSPACE_MANIFEST_FILE), json.as_bytes())?;
    Ok(SaveReport { named_ports })
}

/// Write `data` to `path` without ever leaving a half-written file: write
/// and flush `<file>.tmp` to disk, keep the current file as `<file>.bak`,
/// then rename the new file into place (and flush the folder, on Unix).
pub fn write_atomic(path: &Path, data: &[u8]) -> io::Result<()> {
    let file_name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no file name"))?
        .to_os_string();
    let sibling = |suffix: &str| {
        let mut name = file_name.clone();
        name.push(suffix);
        path.with_file_name(name)
    };
    let (tmp, bak) = (sibling(".tmp"), sibling(".bak"));
    {
        let mut file = fs::File::create(&tmp)?;
        file.write_all(data)?;
        file.sync_all()?;
    }
    if path.is_file() {
        // A copy, not a rename: the file stays in place until the new one
        // replaces it.
        fs::copy(path, &bak)?;
        if let Ok(file) = fs::OpenOptions::new().write(true).open(&bak) {
            file.sync_all().ok();
        }
    }
    fs::rename(&tmp, path)?;
    #[cfg(unix)]
    if let Some(dir) = path.parent()
        && let Ok(dir) = fs::File::open(if dir.as_os_str().is_empty() {
            Path::new(".")
        } else {
            dir
        })
    {
        dir.sync_all().ok();
    }
    Ok(())
}

/// The version 5 `manifest.json` text of `workspace` (pretty-printed; the
/// root first, then the definitions sorted by id). Unlabelled ports are
/// named as a save names them.
pub fn workspace_to_json(
    name: &str,
    workspace: &WorkspaceDocument,
) -> Result<String, WorkspaceError> {
    workspace_json(name, &label_unlabelled_ports(workspace).0)
}

fn workspace_json(name: &str, workspace: &WorkspaceDocument) -> Result<String, WorkspaceError> {
    validate_required_io_labels(workspace)?;
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
        tests: workspace.kept_tests(),
    };
    Ok(serde_json::to_string_pretty(&file)?)
}

fn is_unlabelled_port(node: &crate::sim::DocNode) -> bool {
    node.kind.is_port() && node.label.as_deref().is_none_or(|v| v.trim().is_empty())
}

/// Labels name a chip's pins, so every Input, Output and Display needs
/// one. Name those without: `in0`, `in1`, ... for inputs, `out0`, ... for
/// outputs, `display0`, ... for displays (skipping labels in use in that
/// circuit). Returns the workspace to write and the names given, as
/// `"in0" (Input in "Adder")`.
pub fn label_unlabelled_ports(
    workspace: &WorkspaceDocument,
) -> (Cow<'_, WorkspaceDocument>, Vec<String>) {
    let needs = |doc: &CircuitDocument| doc.nodes.iter().any(is_unlabelled_port);
    if !needs(&workspace.root) && !workspace.definitions.values().any(|d| needs(d)) {
        return (Cow::Borrowed(workspace), Vec::new());
    }
    let mut named = Vec::new();
    let mut label = |doc: &mut CircuitDocument| {
        let mut used: HashSet<String> = doc
            .nodes
            .iter()
            .filter_map(|n| n.label.as_deref())
            .map(|l| l.trim().to_string())
            .collect();
        for node in doc.nodes.iter_mut().filter(|n| is_unlabelled_port(n)) {
            let prefix = match node.kind {
                crate::sim::GateKind::Input { .. } => "in",
                crate::sim::GateKind::Output { .. } => "out",
                _ => "display",
            };
            let label = (0..)
                .map(|n| format!("{prefix}{n}"))
                .find(|l| !used.contains(l))
                .unwrap_or_default();
            used.insert(label.clone());
            named.push(format!(
                "\"{label}\" ({} in \"{}\")",
                crate::editor::parts::kind_title(&node.kind),
                doc.name
            ));
            node.label = Some(label);
        }
    };
    let mut out = workspace.clone();
    label(&mut out.root);
    let mut ids: Vec<Uuid> = out.definitions.keys().copied().collect();
    ids.sort_unstable();
    for id in ids {
        if let Some(def) = out.definitions.get_mut(&id)
            && needs(def)
        {
            label(Arc::make_mut(def));
        }
    }
    (Cow::Owned(out), named)
}

/// Input, Output and Display nodes must be labelled (labels name ports).
fn validate_required_io_labels(workspace: &WorkspaceDocument) -> Result<(), WorkspaceError> {
    let mut missing = Vec::new();
    for circuit in
        std::iter::once(&workspace.root).chain(workspace.definitions.values().map(|d| &**d))
    {
        for node in circuit.nodes.iter().filter(|n| is_unlabelled_port(n)) {
            missing.push(format!(
                "{} in \"{}\"",
                crate::editor::parts::kind_title(&node.kind),
                circuit.name
            ));
        }
    }
    if missing.is_empty() {
        return Ok(());
    }
    let count = missing.len();
    missing.sort();
    missing.dedup();
    let preview = if missing.len() > 6 {
        format!("{}, ...", missing[..6].join(", "))
    } else {
        missing.join(", ")
    };
    Err(WorkspaceError::Invalid(format!(
        "{count} {} without a label ({preview}). Labels name a chip's pins: give each \
         Input, Output and Readout a name.",
        if count == 1 { "port is" } else { "ports are" }
    )))
}

pub(crate) fn workspace_path_in(dir: &Path, name: &str) -> PathBuf {
    dir.join(sanitize_workspace_name(name))
}

/// Longest folder name written (in characters), well below file system
/// limits.
const MAX_NAME_CHARS: usize = 80;

/// The folder name for workspace `name`, safe on every desktop system:
/// path separators, `:*?"<>|` and control characters become `_`; it never
/// starts with a dot (no `..`, no hidden folder) nor ends with a dot or a
/// space (Windows drops those); Windows device names (`CON`, `NUL`,
/// `COM1`, ...) get a `_` prefix; it is at most 80 characters, and never
/// empty.
pub fn sanitize_workspace_name(name: &str) -> String {
    let mapped: String = name
        .trim()
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .take(MAX_NAME_CHARS)
        .collect();
    let trimmed = mapped.trim_end_matches(['.', ' ']);
    let mut out = match trimmed.strip_prefix('.') {
        Some(rest) => format!("_{rest}"),
        None => trimmed.to_string(),
    };
    if out.is_empty() {
        out = "_".to_string();
    }
    let stem = out.split('.').next().unwrap_or_default().trim_end();
    if is_windows_device_name(stem) {
        out.insert(0, '_');
    }
    out
}

fn is_windows_device_name(stem: &str) -> bool {
    let upper = stem.to_ascii_uppercase();
    matches!(
        upper.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) || ["COM", "LPT"].iter().any(|prefix| {
        upper.strip_prefix(prefix).is_some_and(|n| {
            matches!(
                n,
                "0" | "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
            )
        })
    })
}

/// Two names are the same workspace slot when their keys match: the
/// sanitized folder name, lower-cased (file systems on Windows and macOS
/// ignore case).
pub fn name_key(name: &str) -> String {
    sanitize_workspace_name(name).to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::{DocNode, GateKind};
    use bevy::math::Vec2;

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gatequest-ws-{}", Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A small editable workspace named `name`.
    fn workspace(name: &str) -> WorkspaceDocument {
        let mut root = CircuitDocument::new(name);
        root.add(DocNode::labeled(GateKind::Input { w: 1 }, Vec2::ZERO, "A"));
        WorkspaceDocument::new(root)
    }

    #[test]
    fn names_never_leave_the_folder_or_hide() {
        for (name, folder) in [
            ("Adder", "Adder"),
            ("  Adder  ", "Adder"),
            ("a/b\\c:d*e?f\"g<h>i|j", "a_b_c_d_e_f_g_h_i_j"),
            ("..", "_"),
            (".", "_"),
            ("", "_"),
            ("../escape", "_._escape"),
            (".hidden", "_hidden"),
            ("trailing. . ", "trailing"),
            ("tab\there", "tab_here"),
            ("CON", "_CON"),
            ("con", "_con"),
            ("Nul.txt", "_Nul.txt"),
            ("COM1", "_COM1"),
            ("lpt9", "_lpt9"),
            ("COM10", "COM10"),
            ("CONSOLE", "CONSOLE"),
            ("v1.2", "v1.2"),
        ] {
            assert_eq!(sanitize_workspace_name(name), folder, "{name:?}");
        }
        assert_eq!(
            sanitize_workspace_name(&"x".repeat(300)).chars().count(),
            MAX_NAME_CHARS
        );
        let dir = Path::new("/data/workspaces");
        for name in ["..", "../..", "/etc", "a/../../b", "..\\..\\x"] {
            let path = workspace_path_in(dir, name);
            assert_eq!(path.parent(), Some(dir), "{name:?} -> {path:?}");
        }
        // Case does not make a different workspace.
        assert_eq!(name_key("Adder"), name_key("adder"));
        assert_eq!(name_key("a/b"), name_key("A_B"));
        assert_ne!(name_key("Adder"), name_key("Adder 2"));
    }

    #[test]
    fn data_dir_comes_from_the_environment() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |key: &str| {
                pairs
                    .iter()
                    .find(|(k, _)| *k == key)
                    .map(|(_, v)| OsString::from(*v))
            }
        };
        assert_eq!(
            data_dir_from(env(&[(DATA_DIR_ENV, "/tmp/gq"), ("HOME", "/home/p")])),
            Some(PathBuf::from("/tmp/gq"))
        );
        assert_eq!(data_dir_from(env(&[])), None);
        if cfg!(all(unix, not(target_os = "macos"))) {
            assert_eq!(
                data_dir_from(env(&[("HOME", "/home/p")])),
                Some(PathBuf::from("/home/p/.local/share/gatequest"))
            );
            assert_eq!(
                data_dir_from(env(&[("HOME", "/home/p"), ("XDG_DATA_HOME", "/xdg")])),
                Some(PathBuf::from("/xdg/gatequest"))
            );
            // A relative XDG_DATA_HOME is ignored, as the spec says.
            assert_eq!(
                data_dir_from(env(&[("HOME", "/home/p"), ("XDG_DATA_HOME", "rel")])),
                Some(PathBuf::from("/home/p/.local/share/gatequest"))
            );
            assert_eq!(data_dir_from(env(&[("HOME", "relative")])), None);
        }
        if cfg!(target_os = "macos") {
            assert_eq!(
                data_dir_from(env(&[("HOME", "/Users/p")])),
                Some(PathBuf::from(
                    "/Users/p/Library/Application Support/GateQuest"
                ))
            );
        }
    }

    #[test]
    fn save_as_gets_a_fresh_root_and_the_new_name() {
        let dir = temp_dir();
        let store = WorkspaceStore::in_dir(&dir);
        let ws = workspace("Adder");
        let root = ws.root.id;
        let first = store
            .save_open("Adder", "Adder", ws.clone(), OnConflict::Refuse)
            .unwrap();
        assert_eq!((first.root, first.save_as), (root, false));
        let before = fs::read(dir.join("Adder/manifest.json")).unwrap();

        let saved = store
            .save_open("Adder v2", "Adder", ws.clone(), OnConflict::Refuse)
            .unwrap();
        assert!(saved.save_as);
        assert_eq!(saved.name, "Adder v2");
        assert_ne!(saved.root, root, "a workspace of its own");
        let copy = store.read("Adder v2").unwrap();
        assert_eq!(copy.root.id, saved.root);
        assert_eq!(copy.root.name, "Adder v2");
        assert_eq!(
            fs::read(dir.join("Adder/manifest.json")).unwrap(),
            before,
            "the old workspace is left as it was"
        );
        assert!(saved.message("Adder").contains("\"Adder\" is unchanged"));

        // Saving again under the same name (any case) is a plain save.
        let again = store
            .save_open("adder v2", "Adder v2", copy.clone(), OnConflict::Refuse)
            .unwrap();
        assert_eq!((again.root, again.save_as), (saved.root, false));
        assert_eq!(store.list().unwrap(), ["Adder", "Adder v2"]);
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn a_different_workspace_is_never_overwritten() {
        let dir = temp_dir();
        let store = WorkspaceStore::in_dir(&dir);
        let mine = workspace("workspace");
        store
            .save_open("workspace", "workspace", mine.clone(), OnConflict::Refuse)
            .unwrap();
        let before = fs::read(dir.join("workspace/manifest.json")).unwrap();

        // Another workspace with the same name (a new empty one, say).
        let other = workspace("workspace");
        let err = store
            .save_open("Workspace", "workspace", other.clone(), OnConflict::Refuse)
            .unwrap_err();
        assert!(err.contains("different workspace"), "{err}");
        // Save As onto a saved name is refused too.
        let err = store
            .save_open("workspace", "elsewhere", other.clone(), OnConflict::Refuse)
            .unwrap_err();
        assert!(err.contains("different workspace"), "{err}");
        assert_eq!(
            fs::read(dir.join("workspace/manifest.json")).unwrap(),
            before
        );

        // On exit it goes to a free name instead.
        let saved = store
            .save_open("workspace", "workspace", other.clone(), OnConflict::Rename)
            .unwrap();
        assert_eq!(saved.name, "workspace (2)");
        assert_eq!(saved.root, other.root.id);
        assert_eq!(
            fs::read(dir.join("workspace/manifest.json")).unwrap(),
            before
        );

        // A file this version cannot read is not overwritten either.
        fs::create_dir_all(dir.join("future")).unwrap();
        fs::write(
            dir.join("future/manifest.json"),
            r#"{"format":"gatequest-workspace","version":99}"#,
        )
        .unwrap();
        assert_eq!(store.stored("future"), Stored::Unreadable);
        assert!(
            store
                .save_open("future", "future", other, OnConflict::Refuse)
                .is_err()
        );
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn free_names_count_up() {
        let taken = ["Adder", "adder (2)", "Counter (copy)", "counter (copy 2)"];
        let taken = |name: &str| taken.iter().any(|t| name_key(t) == name_key(name));
        assert_eq!(unique_name_among("Fresh", taken), "Fresh");
        assert_eq!(unique_name_among("ADDER", taken), "ADDER (3)");
        assert_eq!(
            unique_name_among("Counter (copy)", taken),
            "Counter (copy 3)"
        );
        assert_eq!(unique_name_among("  ", taken), "workspace");
        // A name as long as a saved name may be: the number still fits.
        let long = "n".repeat(MAX_NAME_CHARS);
        let saved = |name: &str| name_key(name) == name_key(&long);
        let free = unique_name_among(&long, saved);
        assert_eq!(free, format!("{} (2)", "n".repeat(MAX_NAME_CHARS - 4)));
        assert_eq!(sanitize_workspace_name(&free), free);
    }

    #[test]
    fn exit_saves_only_what_would_be_lost() {
        let dir = temp_dir();
        let store = WorkspaceStore::in_dir(&dir);
        let root = Uuid::new_v4();
        let plan = |name: &str, read_only, dirty, empty| {
            exit_save_plan(Some(&store), name, root, read_only, dirty, empty)
        };
        let save = |name: &str| SavePlan {
            name: name.to_string(),
            fresh_root: false,
        };
        assert_eq!(plan("mine", false, true, false), Some(save("mine")));
        assert_eq!(plan("mine", true, true, false), None, "a read-only demo");
        assert_eq!(plan("mine", false, false, false), None, "nothing changed");
        assert_eq!(
            plan("workspace", false, true, true),
            None,
            "an empty workspace that was never saved"
        );
        assert_eq!(plan("  ", false, true, false), Some(save("workspace")));
        assert_eq!(
            exit_save_plan(None, "mine", root, false, true, false),
            None,
            "no file system (web)"
        );

        // Emptied after a save: that is an edit to keep.
        let mut ws = workspace("mine");
        ws.root.id = root;
        store.save("mine", &ws).unwrap();
        assert_eq!(plan("mine", false, true, true), Some(save("mine")));
        // Another workspace holds the name: a free one.
        let other = Uuid::new_v4();
        assert_eq!(
            exit_save_plan(Some(&store), "Mine", other, false, true, false),
            Some(save("Mine (2)"))
        );
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn saves_keep_a_backup_and_leave_no_temp_file() {
        let dir = temp_dir();
        let store = WorkspaceStore::in_dir(&dir);
        let mut ws = workspace("Adder");
        store.save("Adder", &ws).unwrap();
        assert!(
            !dir.join("Adder/manifest.json.bak").exists(),
            "nothing before"
        );
        let first = fs::read_to_string(dir.join("Adder/manifest.json")).unwrap();
        ws.root
            .add(DocNode::labeled(GateKind::Output { w: 1 }, Vec2::ZERO, "Y"));
        store.save("adder", &ws).unwrap();
        assert_eq!(
            fs::read_to_string(dir.join("Adder/manifest.json.bak")).unwrap(),
            first
        );
        assert_eq!(store.read("Adder").unwrap(), ws);
        assert!(!dir.join("Adder/manifest.json.tmp").exists());
        assert_eq!(
            store.list().unwrap(),
            ["Adder"],
            "one folder, whatever the case"
        );
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn newer_files_say_so() {
        let dir = temp_dir();
        fs::create_dir_all(dir.join("new")).unwrap();
        fs::write(
            dir.join("new/manifest.json"),
            r#"{"format":"gatequest-workspace","version":7,"root":"x","circuits":[]}"#,
        )
        .unwrap();
        let err = read_workspace_in(&dir, "new").unwrap_err();
        assert!(matches!(
            err,
            WorkspaceError::UnsupportedVersion {
                found: Some(7),
                newer: true,
                ..
            }
        ));
        let message = err.to_string();
        assert!(message.contains("newer version of GateQuest"), "{message}");
        assert!(message.contains("versions 5 and 6"), "{message}");
        assert!(!message.contains("rebuild"), "{message}");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn the_old_folder_is_read_and_saves_go_to_the_new_one() {
        let (new, old) = (temp_dir(), temp_dir());
        let ws = workspace("Old one");
        WorkspaceStore::in_dir(&old).save("Old one", &ws).unwrap();
        WorkspaceStore::in_dir(&old)
            .save("Both", &workspace("Both"))
            .unwrap();
        let both = workspace("Both");
        WorkspaceStore::in_dir(&new).save("both", &both).unwrap();

        let store = WorkspaceStore::with_legacy(&new, &old);
        assert_eq!(store.list().unwrap(), ["Old one", "both"]);
        assert_eq!(store.read("old one").unwrap(), ws);
        assert_eq!(store.read("Both").unwrap(), both, "the new folder wins");
        // Saving the old workspace moves it over (same root: allowed).
        let saved = store
            .save_open("Old one", "Old one", ws.clone(), OnConflict::Refuse)
            .unwrap();
        assert!(!saved.save_as);
        assert!(new.join("Old one/manifest.json").exists());
        assert!(old.join("Old one/manifest.json").exists(), "left in place");
        fs::remove_dir_all(new).ok();
        fs::remove_dir_all(old).ok();
    }
}
