//! Portable circuit documents.
//!
//! A [`CircuitDocument`] is plain data used only at boundaries: built-in
//! examples, the save format, and subcircuit definitions in the
//! [`CircuitLibrary`]. While a circuit is open, the ECS authoring components
//! are its only source of truth; a document is produced from them on demand
//! (save, opening a subcircuit) and never kept in sync per frame.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use bevy::prelude::*;
use logi_sim::netlist::PinWidths;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::GateKind;

/// Default authoring layer.
pub const DEFAULT_LAYER_UUID: Uuid = Uuid::from_u128(1);

fn default_layer() -> Uuid {
    DEFAULT_LAYER_UUID
}

fn is_default_layer(layer: &Uuid) -> bool {
    *layer == DEFAULT_LAYER_UUID
}

fn is_zero(v: &i32) -> bool {
    *v == 0
}

/// One node of a circuit document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DocNode {
    pub id: Uuid,
    pub kind: GateKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub x: f32,
    pub y: f32,
    #[serde(default = "default_layer", skip_serializing_if = "is_default_layer")]
    pub layer: Uuid,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub layer_order: i32,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub properties: HashMap<String, String>,
}

impl DocNode {
    pub fn new(kind: GateKind, position: Vec2) -> Self {
        Self {
            id: Uuid::new_v4(),
            kind,
            label: None,
            x: position.x,
            y: position.y,
            layer: DEFAULT_LAYER_UUID,
            layer_order: 0,
            properties: HashMap::new(),
        }
    }

    pub fn labeled(kind: GateKind, position: Vec2, label: impl Into<String>) -> Self {
        let mut node = Self::new(kind, position);
        node.label = Some(label.into());
        node
    }

    pub fn position(&self) -> Vec2 {
        Vec2::new(self.x, self.y)
    }
}

/// One end of a wire: a node and a pin index on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PinAddr {
    pub node: Uuid,
    pub pin: u8,
}

/// A wire from an output pin to an input pin. Its width is the width of
/// the driving pin.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DocWire {
    pub id: Uuid,
    pub from: PinAddr,
    pub to: PinAddr,
    #[serde(default = "default_layer", skip_serializing_if = "is_default_layer")]
    pub layer: Uuid,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub layer_order: i32,
}

impl DocWire {
    pub fn new(from: Uuid, from_pin: u8, to: Uuid, to_pin: u8) -> Self {
        Self {
            id: Uuid::new_v4(),
            from: PinAddr {
                node: from,
                pin: from_pin,
            },
            to: PinAddr {
                node: to,
                pin: to_pin,
            },
            layer: DEFAULT_LAYER_UUID,
            layer_order: 0,
        }
    }
}

fn is_zero_u64(v: &u64) -> bool {
    *v == 0
}

/// A circuit: nodes and wires.
///
/// ## Identity
///
/// A circuit used as a subcircuit definition is identified by its `id`;
/// every instance refers to it by id. Two copies of the same definition
/// (the one in the open workspace, the one in a saved workspace) are told
/// apart by their content ([`Self::content_hash`], [`Self::same_content`])
/// and ordered by their `revision`: an edit counter that the editor bumps
/// each time it changes the definition's content (editing inside it,
/// re-saving the workspace it came from). A higher revision is a newer
/// version; the same revision with different content means both copies
/// were edited since they parted. See [`CircuitLibrary::merge`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CircuitDocument {
    pub id: Uuid,
    pub name: String,
    /// Edit counter of this circuit's content (0 when never counted, as
    /// in a file written before revisions existed).
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub revision: u64,
    pub nodes: Vec<DocNode>,
    pub wires: Vec<DocWire>,
}

/// 64-bit FNV-1a: a small, stable hash (the same on every platform and
/// build), for content hashes.
#[derive(Debug, Clone, Copy)]
struct Fnv(u64);

impl Fnv {
    fn new() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }
    fn bytes(&mut self, bytes: &[u8]) {
        for b in bytes {
            self.0 ^= u64::from(*b);
            self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    /// A length-prefixed field, so that adjacent fields cannot run into
    /// each other.
    fn field(&mut self, bytes: &[u8]) {
        self.bytes(&(bytes.len() as u64).to_le_bytes());
        self.bytes(bytes);
    }
}

impl CircuitDocument {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: name.into(),
            revision: 0,
            nodes: Vec::new(),
            wires: Vec::new(),
        }
    }

    /// A hash of the circuit's content: its id, name, nodes and wires, in
    /// any order (not the `revision`). Stable across runs and platforms.
    pub fn content_hash(&self) -> u64 {
        let mut h = Fnv::new();
        h.field(self.id.as_bytes());
        h.field(self.name.as_bytes());
        let mut nodes: Vec<&DocNode> = self.nodes.iter().collect();
        nodes.sort_by_key(|n| n.id);
        h.field(&(nodes.len() as u64).to_le_bytes());
        for n in nodes {
            h.field(n.id.as_bytes());
            // A kind has no maps, so its JSON is canonical.
            h.field(
                serde_json::to_string(&n.kind)
                    .unwrap_or_default()
                    .as_bytes(),
            );
            h.field(n.label.as_deref().unwrap_or("\u{0}").as_bytes());
            h.field(&n.x.to_bits().to_le_bytes());
            h.field(&n.y.to_bits().to_le_bytes());
            h.field(n.layer.as_bytes());
            h.field(&n.layer_order.to_le_bytes());
            let mut props: Vec<(&String, &String)> = n.properties.iter().collect();
            props.sort();
            h.field(&(props.len() as u64).to_le_bytes());
            for (k, v) in props {
                h.field(k.as_bytes());
                h.field(v.as_bytes());
            }
        }
        let mut wires: Vec<&DocWire> = self.wires.iter().collect();
        wires.sort_by_key(|w| w.id);
        h.field(&(wires.len() as u64).to_le_bytes());
        for w in wires {
            h.field(w.id.as_bytes());
            h.field(w.from.node.as_bytes());
            h.field(&[w.from.pin]);
            h.field(w.to.node.as_bytes());
            h.field(&[w.to.pin]);
            h.field(w.layer.as_bytes());
            h.field(&w.layer_order.to_le_bytes());
        }
        h.0
    }

    /// Equal up to node and wire order and the `revision`: the same
    /// definition content.
    pub fn same_content(&self, other: &CircuitDocument) -> bool {
        if self.id != other.id
            || self.name != other.name
            || self.nodes.len() != other.nodes.len()
            || self.wires.len() != other.wires.len()
        {
            return false;
        }
        let mut an: Vec<_> = self.nodes.iter().collect();
        let mut bn: Vec<_> = other.nodes.iter().collect();
        an.sort_by_key(|n| n.id);
        bn.sort_by_key(|n| n.id);
        let mut aw: Vec<_> = self.wires.iter().collect();
        let mut bw: Vec<_> = other.wires.iter().collect();
        aw.sort_by_key(|w| w.id);
        bw.sort_by_key(|w| w.id);
        an == bn && aw == bw
    }

    /// A short form of [`Self::content_hash`] for the player (hex).
    pub fn short_hash(&self) -> String {
        format!("{:08x}", self.content_hash() >> 32)
    }

    /// Add a node and return its id.
    pub fn add(&mut self, node: DocNode) -> Uuid {
        let id = node.id;
        self.nodes.push(node);
        id
    }

    pub fn connect(&mut self, from: Uuid, from_pin: u8, to: Uuid, to_pin: u8) -> Uuid {
        let wire = DocWire::new(from, from_pin, to, to_pin);
        let id = wire.id;
        self.wires.push(wire);
        id
    }

    pub fn node(&self, id: Uuid) -> Option<&DocNode> {
        self.nodes.iter().find(|n| n.id == id)
    }

    /// Port nodes `(inputs, outputs)` in port order: sorted by label, then
    /// id. Display nodes are not ports.
    pub fn interface(&self) -> (Vec<&DocNode>, Vec<&DocNode>) {
        let key = |n: &&DocNode| (n.label.clone().unwrap_or_default(), n.id);
        let mut inputs: Vec<&DocNode> = self
            .nodes
            .iter()
            .filter(|n| matches!(n.kind, GateKind::Input { .. }))
            .collect();
        let mut outputs: Vec<&DocNode> = self
            .nodes
            .iter()
            .filter(|n| matches!(n.kind, GateKind::Output { .. }))
            .collect();
        inputs.sort_by_key(key);
        outputs.sort_by_key(key);
        (inputs, outputs)
    }

    /// Ids of the definitions this circuit instances directly.
    pub fn instanced_definitions(&self) -> impl Iterator<Item = Uuid> + '_ {
        self.nodes.iter().filter_map(|n| match n.kind {
            GateKind::SubCircuit { definition } => Some(definition),
            _ => None,
        })
    }
}

/// The stored tests of a workspace's circuits, by circuit id: each test
/// is a JSON object with a `name` and one test kind
/// (`docs/save-format.md`, "Stored tests"; run by `crate::mcp::testing`).
pub type CircuitTests = BTreeMap<Uuid, Vec<serde_json::Value>>;

/// A workspace: the root circuit plus the definitions it (transitively)
/// instances, and the tests stored with them.
#[derive(Debug, Clone, PartialEq)]
pub struct WorkspaceDocument {
    pub root: CircuitDocument,
    pub definitions: HashMap<Uuid, Arc<CircuitDocument>>,
    /// Tests of the root and of definitions. An entry whose circuit is
    /// not in the workspace is dropped when it is saved.
    pub tests: CircuitTests,
}

impl WorkspaceDocument {
    pub fn new(root: CircuitDocument) -> Self {
        Self {
            root,
            definitions: HashMap::new(),
            tests: CircuitTests::new(),
        }
    }

    /// Register a definition and return its id.
    pub fn define(&mut self, def: CircuitDocument) -> Uuid {
        let id = def.id;
        self.definitions.insert(id, Arc::new(def));
        id
    }

    /// Register every definition of `other`, with its tests.
    pub fn absorb(&mut self, other: &WorkspaceDocument) {
        for (id, def) in &other.definitions {
            self.definitions.insert(*id, Arc::clone(def));
            if let Some(tests) = other.tests.get(id) {
                self.tests.insert(*id, tests.clone());
            }
        }
    }

    /// The tests of circuits that are in the workspace (the root and the
    /// definitions), without empty entries.
    pub fn kept_tests(&self) -> CircuitTests {
        self.tests
            .iter()
            .filter(|(id, tests)| {
                !tests.is_empty() && (**id == self.root.id || self.definitions.contains_key(id))
            })
            .map(|(id, tests)| (*id, tests.clone()))
            .collect()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Library
// ─────────────────────────────────────────────────────────────────────────────

/// How a definition offered to the library compares with the one it
/// already has under the same id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Incoming {
    /// The library has no definition with that id.
    New,
    /// Same content (whatever the revisions).
    Same,
    /// Different content and a higher revision: a newer version.
    Newer,
    /// Different content, same revision: both were edited since they
    /// parted.
    Diverged,
    /// Different content and a lower revision: an older version.
    Older,
}

impl Incoming {
    pub fn of(have: Option<&CircuitDocument>, incoming: &CircuitDocument) -> Self {
        let Some(have) = have else {
            return Self::New;
        };
        if have.same_content(incoming) {
            Self::Same
        } else if incoming.revision > have.revision {
            Self::Newer
        } else if incoming.revision == have.revision {
            Self::Diverged
        } else {
            Self::Older
        }
    }

    /// Different content under the same id.
    pub fn conflicts(self) -> bool {
        matches!(self, Self::Newer | Self::Diverged | Self::Older)
    }
}

/// What [`CircuitLibrary::merge`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MergeReport {
    /// Definitions added (their ids were new).
    pub added: Vec<Uuid>,
    /// Ids whose offered content differs from the library's, with how it
    /// compares. These were *not* replaced.
    pub conflicts: Vec<(Uuid, Incoming)>,
}

/// Subcircuit definitions available to the open circuit, by id. Instances
/// reference a definition; it is stored once.
///
/// Identity rules (see [`CircuitDocument`]): a merge adds definitions
/// whose ids are new and never replaces one with different content; it
/// reports those as conflicts, for the editor to ask the player
/// ([`super::RefreshDefinitions`]). Only [`Self::replace`] (an explicit
/// refresh, an edit inside a definition, undo) and loading a workspace
/// (which replaces the whole open circuit, so no instance of the earlier
/// version is left) change a definition in place.
#[derive(Resource, Debug, Default)]
pub struct CircuitLibrary {
    pub definitions: HashMap<Uuid, Arc<CircuitDocument>>,
}

impl CircuitLibrary {
    pub fn get(&self, id: Uuid) -> Option<&Arc<CircuitDocument>> {
        self.definitions.get(&id)
    }

    /// Add the definitions whose ids are new. Definitions with the same id
    /// and different content are left as they are and reported.
    pub fn merge(&mut self, defs: &HashMap<Uuid, Arc<CircuitDocument>>) -> MergeReport {
        let mut report = MergeReport::default();
        let mut ids: Vec<Uuid> = defs.keys().copied().collect();
        ids.sort();
        for id in ids {
            let def = &defs[&id];
            match Incoming::of(self.get(id).map(|d| &**d), def) {
                Incoming::New => {
                    self.definitions.insert(id, Arc::clone(def));
                    report.added.push(id);
                }
                Incoming::Same => {}
                other => report.conflicts.push((id, other)),
            }
        }
        report
    }

    /// Put `def` in the library, replacing any definition with its id.
    /// Callers that replace a definition instances use must renumber the
    /// wires to those instances (see [`super::view::PinRemap`]).
    pub fn replace(&mut self, def: Arc<CircuitDocument>) {
        self.definitions.insert(def.id, def);
    }

    /// Ids of the definitions that instance `def` directly.
    pub fn parents_of(&self, def: Uuid) -> Vec<Uuid> {
        let mut ids: Vec<Uuid> = self
            .definitions
            .iter()
            .filter(|(_, d)| d.instanced_definitions().any(|i| i == def))
            .map(|(id, _)| *id)
            .collect();
        ids.sort();
        ids
    }

    /// Pin widths `(inputs, outputs)` of a gate kind, resolving subcircuits.
    pub fn pins(&self, kind: &GateKind) -> Option<(PinWidths, PinWidths)> {
        match kind {
            GateKind::SubCircuit { definition } => {
                let def = self.get(*definition)?;
                let (ins, outs) = def.interface();
                let width = |n: &&DocNode| n.kind.width().unwrap_or(1);
                Some((
                    ins.iter().map(width).collect(),
                    outs.iter().map(width).collect(),
                ))
            }
            k => k.primitive_pins(),
        }
    }

    /// Human-readable pin names `(inputs, outputs)` for a gate kind.
    pub fn pin_names(&self, kind: &GateKind) -> (Vec<String>, Vec<String>) {
        if let GateKind::SubCircuit { definition } = kind {
            let Some(def) = self.get(*definition) else {
                return (Vec::new(), Vec::new());
            };
            let (ins, outs) = def.interface();
            let name = |n: &&DocNode| n.label.clone().unwrap_or_else(|| "?".into());
            return (
                ins.iter().map(name).collect(),
                outs.iter().map(name).collect(),
            );
        }
        let (ins, outs) = self.pins(kind).unwrap_or_default();
        let named = |names: &[&str], widths: &PinWidths| {
            widths
                .iter()
                .enumerate()
                .map(|(i, _)| {
                    names
                        .get(i)
                        .map_or_else(|| format!("in{i}"), |s| s.to_string())
                })
                .collect::<Vec<_>>()
        };
        let inputs = match kind {
            GateKind::Merge { .. } => (0..ins.len()).map(|i| format!("a{i}")).collect(),
            _ => named(kind.input_pin_names(), &ins),
        };
        (inputs, named(kind.output_pin_names(), &outs))
    }

    /// Whether definition `outer` instances `inner`, directly or through
    /// other definitions. A definition does not contain itself unless the
    /// library has a cycle.
    pub fn contains(&self, outer: Uuid, inner: Uuid) -> bool {
        let mut seen = HashSet::new();
        let mut stack: Vec<Uuid> = match self.get(outer) {
            Some(def) => def.instanced_definitions().collect(),
            None => return false,
        };
        while let Some(id) = stack.pop() {
            if id == inner {
                return true;
            }
            if seen.insert(id)
                && let Some(def) = self.get(id)
            {
                stack.extend(def.instanced_definitions());
            }
        }
        false
    }

    /// Why placing an instance of `placing` into the circuit `into` would
    /// make a circuit contain itself, or `None` when it is allowed.
    /// `into` is the id of the circuit being edited (the root circuit or a
    /// definition).
    /// `fallback_name` names `into` when it is not in the library (the
    /// root circuit).
    pub fn recursion_error(
        &self,
        placing: Uuid,
        into: Uuid,
        fallback_name: &str,
    ) -> Option<String> {
        let name = |id: Uuid| {
            self.get(id).map_or_else(
                || format!("\"{fallback_name}\""),
                |d| format!("\"{}\"", d.name),
            )
        };
        if placing == into {
            return Some(format!(
                "Cannot place {} inside itself: a circuit cannot contain an instance of itself.",
                name(placing)
            ));
        }
        if self.contains(placing, into) {
            return Some(format!(
                "Cannot place {} inside {}: {} already contains {}, so the circuit would contain \
                 itself.",
                name(placing),
                name(into),
                name(placing),
                name(into)
            ));
        }
        None
    }

    /// A definition that contains itself, if the library has a cycle.
    pub fn find_cycle(&self) -> Option<Uuid> {
        let mut ids: Vec<Uuid> = self.definitions.keys().copied().collect();
        ids.sort();
        ids.into_iter().find(|&id| self.contains(id, id))
    }

    /// Definitions reachable from `root` (excluding the root itself).
    pub fn reachable_from(&self, root: &CircuitDocument) -> HashMap<Uuid, Arc<CircuitDocument>> {
        let mut out = HashMap::new();
        let mut seen = HashSet::new();
        let mut stack: Vec<Uuid> = root.instanced_definitions().collect();
        while let Some(id) = stack.pop() {
            if !seen.insert(id) {
                continue;
            }
            if let Some(def) = self.get(id) {
                stack.extend(def.instanced_definitions());
                out.insert(id, Arc::clone(def));
            }
        }
        out
    }
}
