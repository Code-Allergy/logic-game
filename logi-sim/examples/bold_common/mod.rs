#![allow(dead_code)] // each example uses part of the helpers
//! Shared helpers of the `bold_*` measurement examples
//! (`docs/bold-ideas.md`): load a workspace, compile it with every net
//! observable, and find nets by hierarchical name.
//!
//! Names are `<definition>/<definition>/.../<net label>`, the definition
//! names of the instance chain below the root (the root's own nets have no
//! prefix), exactly as `spec_keys` prints them: `cpu6502/addr`,
//! `ppu_ref/ppu_regs/mask`. A name is looked up by suffix, and must be
//! unique among the slots that have it.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use logi_sim::compile::{NodePath, Options, Program, compile};
use logi_sim::netlist::NodeKind;

#[path = "../workspace/mod.rs"]
pub mod workspace;

pub struct Loaded {
    pub ws: workspace::Loaded,
    pub program: Arc<Program>,
    /// Slot -> names (shortest first).
    pub names: HashMap<u32, Vec<String>>,
}

/// Load `folder/manifest.json` and compile it (debug: no dead-code
/// elimination, so authored nets stay readable).
pub fn load(folder: &str) -> Result<Loaded, String> {
    load_with(folder, true)
}

/// [`load`] with the default compile (`debug` false: dead nets are
/// dropped, as in the game) or with every authored net kept (`debug`).
pub fn load_with(folder: &str, debug: bool) -> Result<Loaded, String> {
    let ws = workspace::load(Path::new(folder))?;
    let o = Options {
        debug,
        ..Options::default()
    };
    let program = Arc::new(compile(&ws.design, &o).map_err(|e| format!("{e:?}"))?);
    let names = slot_names(&program, &ws);
    Ok(Loaded { ws, program, names })
}

/// Instance path (definition names below the root) of instance `inst`.
pub fn inst_path(p: &Program, w: &workspace::Loaded, inst: u32) -> String {
    let dm = &p.debug;
    let mut parts = Vec::new();
    let mut i = inst;
    while let Some((parent, _)) = dm.instances[i as usize].parent {
        parts.push(
            w.def_names
                .get(&dm.instances[i as usize].def)
                .cloned()
                .unwrap_or_default(),
        );
        i = parent;
    }
    parts.reverse();
    parts.join("/")
}

pub fn slot_names(p: &Program, w: &workspace::Loaded) -> HashMap<u32, Vec<String>> {
    let mut out: HashMap<u32, Vec<String>> = HashMap::new();
    for (inst, map) in p.debug.net_slots.iter().enumerate() {
        let def = p.debug.instances[inst].def;
        let path = inst_path(p, w, inst as u32);
        for (net, &slot) in map {
            let Some(name) = w.net_names.get(&(def, net)) else {
                continue;
            };
            let full = if path.is_empty() {
                name.clone()
            } else {
                format!("{path}/{name}")
            };
            out.entry(slot).or_default().push(full);
        }
    }
    for v in out.values_mut() {
        v.sort_by_key(|n| (n.len(), n.clone()));
        v.dedup();
    }
    out
}

impl Loaded {
    /// The slot named exactly `name` (or ending in `/name`).
    pub fn slot(&self, name: &str) -> Result<u32, String> {
        let suffix = format!("/{name}");
        let mut hits: Vec<u32> = self
            .names
            .iter()
            .filter(|(_, v)| v.iter().any(|n| n == name || n.ends_with(&suffix)))
            .map(|(&s, _)| s)
            .collect();
        hits.sort_unstable();
        match hits.len() {
            1 => Ok(hits[0]),
            0 => Err(format!("no net named {name}")),
            _ => Err(format!(
                "{name} is ambiguous: {}",
                hits.iter()
                    .map(|s| self.names[s][0].clone())
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        }
    }
}

/// First word of a node kind's `Debug` form (`Ram`, `Screen`, ...).
fn kind_name(k: &NodeKind) -> String {
    format!("{k:?}")
        .split(|c: char| !c.is_alphanumeric())
        .next()
        .unwrap_or("")
        .to_string()
}

impl Loaded {
    /// The part of the root a state element belongs to: the definition
    /// name of the root's child instance that contains it, or for a root
    /// level node its kind (a root `Ram` is told apart by what drives its
    /// data pin: an instance's definition name, `ram<-def`).
    pub fn group_of(&self, path: &NodePath) -> String {
        let design = &self.ws.design;
        let def = design.def(design.root).expect("root");
        let first = path.instances.first().copied().unwrap_or(path.node);
        match def.nodes.get(first) {
            Some(n) => match &n.kind {
                NodeKind::Instance(d) => self.ws.def_names.get(d).cloned().unwrap_or_default(),
                k => {
                    let name = kind_name(k);
                    let driver = n
                        .inputs
                        .get(2)
                        .copied()
                        .flatten()
                        .and_then(|net| def.nets.get(net))
                        .and_then(|net| def.nodes.get(net.driver.0))
                        .and_then(|d| match d.kind {
                            NodeKind::Instance(d) => self.ws.def_names.get(&d).cloned(),
                            _ => None,
                        });
                    match (name.as_str(), driver) {
                        ("Ram" | "Ram2", Some(d)) => format!("Ram<-{d}"),
                        _ => name,
                    }
                }
            },
            None => "?".into(),
        }
    }
}

/// The slot named `name` (exactly, or as a `/`-suffix) in `names`
/// (from [`slot_names`] of some compiled program).
pub fn find_slot(names: &HashMap<u32, Vec<String>>, name: &str) -> Option<u32> {
    let suffix = format!("/{name}");
    let mut hits: Vec<u32> = names
        .iter()
        .filter(|(_, v)| v.iter().any(|n| n == name || n.ends_with(&suffix)))
        .map(|(&s, _)| s)
        .collect();
    hits.sort_unstable();
    hits.first().copied()
}
