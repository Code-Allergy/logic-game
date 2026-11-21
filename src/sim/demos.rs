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
//!
//! Beside the curriculum, `assets/gallery/` holds the gallery: more
//! circuits, simple to complex, grouped in categories instead of levels.
//! Its `manifest.json` has the same shape with `categories` and `items`;
//! an item may use any curriculum demo and any gallery item listed before
//! it (`"<id>/<circuit>"` references share one namespace, so gallery ids
//! differ from demo ids). Its ids derive from `gallery/<id>/<circuit>`.

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, OnceLock};

use serde::Deserialize;
use uuid::Uuid;

use super::source::{self, Externals, SourceFile};
use super::{CircuitDocument, CircuitLibrary, GateKind, WorkspaceDocument};

/// Folder of the demo files, relative to the working directory.
pub const DEMO_DIR: &str = "assets/demos";
/// Folder of the gallery files, relative to the working directory.
pub const GALLERY_DIR: &str = "assets/gallery";
pub const MANIFEST_FILE: &str = "manifest.json";
/// Format tag of the demos' `manifest.json`.
pub const MANIFEST_FORMAT: &str = "gatequest-demos";
/// Format tag of the gallery's `manifest.json`.
pub const GALLERY_FORMAT: &str = "gatequest-gallery";
pub const MANIFEST_VERSION: u32 = 1;

macro_rules! embed {
    ($dir:literal: $($file:literal),* $(,)?) => {
        &[$(($file, include_str!(concat!("../../assets/", $dir, "/", $file)))),*]
    };
}

/// Every file of `assets/demos/`, embedded (a test checks that this list
/// matches the folder).
pub const EMBEDDED: &[(&str, &str)] = embed!(
    "demos":
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

/// Every file of `assets/gallery/`, embedded (checked like [`EMBEDDED`]).
pub const GALLERY_EMBEDDED: &[(&str, &str)] = embed!(
    "gallery":
    "manifest.json",
    "majority.json",
    "comparator.json",
    "magnitude.json",
    "parity.json",
    "gray_code.json",
    "priority_encoder.json",
    "seven_segment.json",
    "bcd_to_binary.json",
    "barrel_shifter.json",
    "cla_adder.json",
    "multiplier.json",
    "edge_detector.json",
    "shift_register.json",
    "johnson_counter.json",
    "lfsr.json",
    "clock_divider.json",
    "updown_counter.json",
    "bcd_counter.json",
    "debouncer.json",
    "pwm.json",
    "sequence_detector.json",
    "traffic_light.json",
    "stopwatch.json",
    "stack.json",
    "uart_tx.json",
    "reaction_timer.json",
    "cpu_fibonacci.json",
    "screen_plotter.json",
    "screen_xor.json",
    "screen_bars.json",
    "screen_text.json",
);

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestFile {
    format: String,
    version: u32,
    levels: Vec<Level>,
    demos: Vec<ManifestDemo>,
}

/// A level of the pyramid: a stop on the demo map.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Level {
    pub level: u32,
    pub title: String,
    /// One or two sentences about the level (the demo map's level panel).
    /// Optional: manifests without it still load.
    #[serde(default)]
    pub description: String,
    /// A short name for the stop on the demo map. Optional: defaults to
    /// the title up to a colon (see [`Level::short_title`]).
    #[serde(default)]
    pub short: Option<String>,
}

impl Level {
    /// The stop's name on the demo map: `short`, else the title up to a
    /// colon (`"Choosing: decoders and multiplexers"` reads `"Choosing"`).
    pub fn short_title(&self) -> &str {
        match &self.short {
            Some(short) if !short.trim().is_empty() => short.trim(),
            _ => self.title.split(':').next().unwrap_or_default().trim(),
        }
    }
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

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GalleryFile {
    format: String,
    version: u32,
    categories: Vec<Category>,
    items: Vec<GalleryItem>,
}

/// A group of the gallery.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Category {
    pub id: String,
    pub title: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GalleryItem {
    id: String,
    category: String,
    title: String,
    description: String,
    #[serde(default)]
    uses: Vec<String>,
}

/// One loaded demo or gallery item.
#[derive(Debug, Clone)]
pub struct Demo {
    pub id: String,
    /// Its curriculum level; 0 for a gallery item.
    pub level: u32,
    /// Its gallery category (`None` for a curriculum demo).
    pub category: Option<String>,
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

/// The demos in manifest (pyramid) order, then the gallery.
#[derive(Debug, Default)]
pub struct DemoCatalog {
    pub levels: Vec<Level>,
    pub demos: Vec<Demo>,
    pub categories: Vec<Category>,
    /// The gallery items in manifest order (grouped by category).
    pub gallery: Vec<Demo>,
    /// `"demo/circuit"` -> definition id, for every circuit of every demo
    /// and gallery item.
    names: HashMap<String, Uuid>,
    library: CircuitLibrary,
}

/// The key that derives a demo's ids.
pub fn demo_key(id: &str) -> String {
    format!("demos/{id}")
}

/// The key that derives a gallery item's ids.
pub fn gallery_key(id: &str) -> String {
    format!("gallery/{id}")
}

fn embedded_file(files: &[(&str, &str)], file: &str) -> Result<String, String> {
    files
        .iter()
        .find(|(name, _)| *name == file)
        .map(|(_, text)| (*text).to_string())
        .ok_or_else(|| format!("{file} is not embedded"))
}

impl DemoCatalog {
    /// Load from the embedded files.
    pub fn embedded() -> Result<Self, String> {
        Self::load(
            |file| embedded_file(EMBEDDED, file),
            |file| embedded_file(GALLERY_EMBEDDED, file),
        )
    }

    /// Load from the demo and gallery folders (native only).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn from_dir(demos: &std::path::Path, gallery: &std::path::Path) -> Result<Self, String> {
        let reader = |dir: &std::path::Path| {
            let dir = dir.to_path_buf();
            move |file: &str| {
                std::fs::read_to_string(dir.join(file))
                    .map_err(|e| format!("{}: {e}", dir.join(file).display()))
            }
        };
        Self::load(reader(demos), reader(gallery))
    }

    /// Load with `read` giving the text of a demo file by name, and
    /// `read_gallery` that of a gallery file.
    pub fn load(
        read: impl Fn(&str) -> Result<String, String>,
        read_gallery: impl Fn(&str) -> Result<String, String>,
    ) -> Result<Self, String> {
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
                category: None,
                title: entry.title,
                description: entry.description,
                uses: entry.uses,
                source,
                workspace: Arc::new(expansion.workspace),
                circuits: expansion.circuits,
            });
        }
        catalog.load_gallery(read_gallery)?;
        Ok(catalog)
    }

    /// Load the gallery on top of the curriculum.
    fn load_gallery(
        &mut self,
        read: impl Fn(&str) -> Result<String, String>,
    ) -> Result<(), String> {
        let place = |e: String| format!("gallery {MANIFEST_FILE}: {e}");
        let manifest: GalleryFile =
            serde_json::from_str(&read(MANIFEST_FILE)?).map_err(|e| place(e.to_string()))?;
        if manifest.format != GALLERY_FORMAT || manifest.version != MANIFEST_VERSION {
            return Err(place(format!(
                "expected format \"{GALLERY_FORMAT}\" version {MANIFEST_VERSION}"
            )));
        }
        self.categories = manifest.categories;
        let mut last_category = 0;
        for entry in manifest.items {
            let file = format!("{}.json", entry.id);
            let place = |e: String| format!("gallery item \"{}\" ({file}): {e}", entry.id);
            if self.get(&entry.id).is_some() {
                return Err(place(
                    "the id is taken by another demo or gallery item".into(),
                ));
            }
            let Some(category) = self.categories.iter().position(|c| c.id == entry.category) else {
                return Err(place(format!(
                    "category \"{}\" is not in \"categories\"",
                    entry.category
                )));
            };
            if category < last_category {
                return Err(place(
                    "items of a category must be listed together, in category order".into(),
                ));
            }
            last_category = category;

            let source = source::parse(&read(&file)?).map_err(|e| place(e.to_string()))?;
            // `uses` lists exactly what the file instances: curriculum demos
            // and gallery items listed before it.
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
                if self.get(demo).is_none() {
                    return Err(place(format!(
                        "\"{r}\": \"{demo}\" is not a demo or a gallery item listed before it"
                    )));
                }
            }

            let expansion = source::expand(&source, &gallery_key(&entry.id), &self.externals())
                .map_err(|e| place(e.to_string()))?;
            for (name, doc) in &expansion.circuits {
                self.names.insert(format!("{}/{name}", entry.id), doc.id);
                self.library.definitions.insert(doc.id, Arc::clone(doc));
            }
            self.gallery.push(Demo {
                id: entry.id,
                level: 0,
                category: Some(entry.category),
                title: entry.title,
                description: entry.description,
                uses: entry.uses,
                source,
                workspace: Arc::new(expansion.workspace),
                circuits: expansion.circuits,
            });
        }
        Ok(())
    }

    /// Every demo, then every gallery item.
    pub fn all(&self) -> impl Iterator<Item = &Demo> {
        self.demos.iter().chain(&self.gallery)
    }

    /// The demo or gallery item `id`.
    pub fn get(&self, id: &str) -> Option<&Demo> {
        self.all().find(|d| d.id == id)
    }

    /// The demo or gallery item whose root circuit has this id.
    pub fn by_root(&self, root: Uuid) -> Option<&Demo> {
        self.all().find(|d| d.workspace.root.id == root)
    }

    pub fn category_title(&self, id: &str) -> &str {
        self.categories
            .iter()
            .find(|c| c.id == id)
            .map_or("", |c| c.title.as_str())
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

/// The demo catalog, loaded once: on native from [`DEMO_DIR`] and
/// [`GALLERY_DIR`] when they have a manifest (falling back to the embedded
/// files on error), otherwise from the embedded files.
pub fn catalog() -> Result<&'static DemoCatalog, &'static str> {
    static CATALOG: OnceLock<Result<DemoCatalog, String>> = OnceLock::new();
    CATALOG
        .get_or_init(|| {
            #[cfg(not(target_arch = "wasm32"))]
            {
                let (dir, gallery) = (
                    std::path::Path::new(DEMO_DIR),
                    std::path::Path::new(GALLERY_DIR),
                );
                if dir.join(MANIFEST_FILE).exists() && gallery.join(MANIFEST_FILE).exists() {
                    match DemoCatalog::from_dir(dir, gallery) {
                        Ok(catalog) => return Ok(catalog),
                        Err(e) => bevy::log::warn!(
                            "Cannot load the demos in {DEMO_DIR} and {GALLERY_DIR}: {e}; \
                             using the built-in copies"
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
