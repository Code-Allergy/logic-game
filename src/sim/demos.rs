//! Built-in demos, loaded from the files in `assets/demos/`.
//!
//! `manifest.json` lists the demos in pyramid order (level, title,
//! description, and the lower-level definitions each one uses); each demo
//! is a save format version 6 file `<id>.json` (see [`super::source`] and
//! `docs/save-format.md`). No demo is built in code.
//!
//! The files are embedded with `include_str!`, so the demos work on wasm
//! (no filesystem). Native builds read `assets/demos/` from the working
//! directory instead when it is there, so hand edits show up without a
//! rebuild, and fall back to the embedded copies if that fails.
//!
//! A demo's circuits get ids derived from `demos/<id>/<circuit>` (UUID v5,
//! [`super::source::circuit_id`]), so loading a demo twice, or a demo and
//! another demo that uses it, yields the same shared definitions.

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, OnceLock};

use serde::Deserialize;
use uuid::Uuid;

use super::source::{self, Externals, SourceFile};
use super::{CircuitDocument, CircuitLibrary, GateKind, WorkspaceDocument};

/// Folder of the demo files, relative to the working directory.
pub const DEMO_DIR: &str = "assets/demos";
pub const MANIFEST_FILE: &str = "manifest.json";
/// Format tag of `manifest.json`.
pub const MANIFEST_FORMAT: &str = "gatequest-demos";
pub const MANIFEST_VERSION: u32 = 1;

macro_rules! embed {
    ($($file:literal),* $(,)?) => {
        &[$(($file, include_str!(concat!("../../assets/demos/", $file)))),*]
    };
}

/// Every file of `assets/demos/`, embedded (a test checks that this list
/// matches the folder).
pub const EMBEDDED: &[(&str, &str)] = embed!(
    "manifest.json",
    "gates.json",
    "universal.json",
    "half_adder.json",
    "full_adder.json",
    "ripple_adder.json",
    "decoder.json",
    "mux.json",
    "mux_tree.json",
    "sr_latch.json",
    "d_latch.json",
    "flip_flop.json",
    "register.json",
    "counter.json",
    "register_file.json",
    "alu.json",
    "cpu.json",
);

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestFile {
    format: String,
    version: u32,
    levels: Vec<Level>,
    demos: Vec<ManifestDemo>,
}

/// A level of the pyramid.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Level {
    pub level: u32,
    pub title: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestDemo {
    id: String,
    level: u32,
    title: String,
    description: String,
    #[serde(default)]
    uses: Vec<String>,
}

/// One loaded demo.
#[derive(Debug, Clone)]
pub struct Demo {
    pub id: String,
    pub level: u32,
    pub title: String,
    pub description: String,
    /// The `"demo/circuit"` definitions from other demos it instances.
    pub uses: Vec<String>,
    /// The file as written.
    pub source: SourceFile,
    /// The root circuit (named after the title) and every definition it
    /// reaches, expanded.
    pub workspace: Arc<WorkspaceDocument>,
    /// Every circuit of the file by name, expanded.
    pub circuits: Vec<(String, Arc<CircuitDocument>)>,
}

/// The demos in manifest (pyramid) order.
#[derive(Debug, Default)]
pub struct DemoCatalog {
    pub levels: Vec<Level>,
    pub demos: Vec<Demo>,
    /// `"demo/circuit"` -> definition id, for every circuit of every demo.
    names: HashMap<String, Uuid>,
    library: CircuitLibrary,
}

/// The key that derives a demo's ids.
pub fn demo_key(id: &str) -> String {
    format!("demos/{id}")
}

impl DemoCatalog {
    /// Load from the embedded files.
    pub fn embedded() -> Result<Self, String> {
        Self::load(|file| {
            EMBEDDED
                .iter()
                .find(|(name, _)| *name == file)
                .map(|(_, text)| (*text).to_string())
                .ok_or_else(|| format!("{file} is not embedded"))
        })
    }

    /// Load from a folder (native only).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn from_dir(dir: &std::path::Path) -> Result<Self, String> {
        Self::load(|file| {
            std::fs::read_to_string(dir.join(file)).map_err(|e| format!("{file}: {e}"))
        })
    }

    /// Load with `read` giving the text of a file by name.
    pub fn load(read: impl Fn(&str) -> Result<String, String>) -> Result<Self, String> {
        let manifest: ManifestFile = serde_json::from_str(&read(MANIFEST_FILE)?)
            .map_err(|e| format!("{MANIFEST_FILE}: {e}"))?;
        if manifest.format != MANIFEST_FORMAT || manifest.version != MANIFEST_VERSION {
            return Err(format!(
                "{MANIFEST_FILE}: expected format \"{MANIFEST_FORMAT}\" version {MANIFEST_VERSION}"
            ));
        }
        let mut catalog = DemoCatalog {
            levels: manifest.levels,
            ..Default::default()
        };
        let mut last_level = 0;
        for entry in manifest.demos {
            let file = format!("{}.json", entry.id);
            let place = |e: String| format!("demo \"{}\" ({file}): {e}", entry.id);
            if catalog.get(&entry.id).is_some() {
                return Err(place("listed twice".into()));
            }
            if entry.level < last_level {
                return Err(place("levels must not decrease in manifest order".into()));
            }
            if !catalog.levels.iter().any(|l| l.level == entry.level) {
                return Err(place(format!("level {} is not in \"levels\"", entry.level)));
            }
            last_level = entry.level;

            let source = source::parse(&read(&file)?).map_err(|e| place(e.to_string()))?;
            // The manifest's `uses` must be exactly what the file instances,
            // and only from demos listed earlier at the same or a lower level.
            let used = source.external_uses();
            let listed: BTreeSet<String> = entry.uses.iter().cloned().collect();
            if used != listed || listed.len() != entry.uses.len() {
                return Err(place(format!(
                    "\"uses\" in the manifest is {:?} but the file uses {:?}",
                    entry.uses, used
                )));
            }
            for r in &used {
                let demo = r.split('/').next().unwrap_or_default();
                match catalog.get(demo) {
                    Some(d) if d.level <= entry.level => {}
                    Some(_) => return Err(place(format!("\"{r}\" is from a higher level"))),
                    None => {
                        return Err(place(format!(
                            "\"{r}\": demo \"{demo}\" is not listed before it"
                        )));
                    }
                }
            }

            let expansion = source::expand(
                &source,
                &demo_key(&entry.id),
                &Externals {
                    names: Some(&catalog.names),
                    library: Some(&catalog.library),
                },
            )
            .map_err(|e| place(e.to_string()))?;
            for (name, doc) in &expansion.circuits {
                catalog.names.insert(format!("{}/{name}", entry.id), doc.id);
                catalog.library.definitions.insert(doc.id, Arc::clone(doc));
            }
            catalog.demos.push(Demo {
                id: entry.id,
                level: entry.level,
                title: entry.title,
                description: entry.description,
                uses: entry.uses,
                source,
                workspace: Arc::new(expansion.workspace),
                circuits: expansion.circuits,
            });
        }
        Ok(catalog)
    }

    pub fn get(&self, id: &str) -> Option<&Demo> {
        self.demos.iter().find(|d| d.id == id)
    }

    /// The demo whose root circuit has this id.
    pub fn by_root(&self, root: Uuid) -> Option<&Demo> {
        self.demos.iter().find(|d| d.workspace.root.id == root)
    }

    pub fn level_title(&self, level: u32) -> &str {
        self.levels
            .iter()
            .find(|l| l.level == level)
            .map_or("", |l| l.title.as_str())
    }

    /// What `"demo/circuit"` references in other files resolve to.
    pub fn externals(&self) -> Externals<'_> {
        Externals {
            names: Some(&self.names),
            library: Some(&self.library),
        }
    }
}

/// The demo catalog, loaded once: on native from [`DEMO_DIR`] when it has a
/// manifest (falling back to the embedded files on error), otherwise from
/// the embedded files.
pub fn catalog() -> Result<&'static DemoCatalog, &'static str> {
    static CATALOG: OnceLock<Result<DemoCatalog, String>> = OnceLock::new();
    CATALOG
        .get_or_init(|| {
            #[cfg(not(target_arch = "wasm32"))]
            {
                let dir = std::path::Path::new(DEMO_DIR);
                if dir.join(MANIFEST_FILE).exists() {
                    match DemoCatalog::from_dir(dir) {
                        Ok(catalog) => return Ok(catalog),
                        Err(e) => bevy::log::warn!(
                            "Cannot load the demos in {DEMO_DIR}: {e}; using the built-in copies"
                        ),
                    }
                }
            }
            DemoCatalog::embedded()
        })
        .as_ref()
        .map_err(String::as_str)
}

/// An editable copy of a demo: the same circuits under fresh ids (UUID v4)
/// for the root and every definition, so editing the copy never changes
/// the demo or another copy. Node and wire ids are kept (they are per
/// circuit).
pub fn editable_copy(workspace: &WorkspaceDocument, name: &str) -> WorkspaceDocument {
    let remap: HashMap<Uuid, Uuid> = workspace
        .definitions
        .keys()
        .map(|&id| (id, Uuid::new_v4()))
        .collect();
    let rewrite = |doc: &CircuitDocument, id: Uuid| {
        let mut doc = doc.clone();
        doc.id = id;
        for node in &mut doc.nodes {
            if let GateKind::SubCircuit { definition } = &mut node.kind
                && let Some(&new) = remap.get(definition)
            {
                *definition = new;
            }
        }
        doc
    };
    let mut root = rewrite(&workspace.root, Uuid::new_v4());
    root.name = name.to_string();
    WorkspaceDocument {
        root,
        definitions: workspace
            .definitions
            .iter()
            .map(|(id, def)| (remap[id], Arc::new(rewrite(def, remap[id]))))
            .collect(),
    }
}
