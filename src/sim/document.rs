//! Portable circuit documents.
//!
//! A [`CircuitDocument`] is plain data used only at boundaries: built-in
//! examples, the save format, and subcircuit definitions in the
//! [`CircuitLibrary`]. While a circuit is open, the ECS authoring components
//! are its only source of truth; a document is produced from them on demand
//! (save, opening a subcircuit) and never kept in sync per frame.

use std::collections::{HashMap, HashSet};
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

/// A circuit: nodes and wires.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CircuitDocument {
    pub id: Uuid,
    pub name: String,
    pub nodes: Vec<DocNode>,
    pub wires: Vec<DocWire>,
}

impl CircuitDocument {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: name.into(),
            nodes: Vec::new(),
            wires: Vec::new(),
        }
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

/// A workspace: the root circuit plus the definitions it (transitively)
/// instances.
#[derive(Debug, Clone, PartialEq)]
pub struct WorkspaceDocument {
    pub root: CircuitDocument,
    pub definitions: HashMap<Uuid, Arc<CircuitDocument>>,
}

impl WorkspaceDocument {
    pub fn new(root: CircuitDocument) -> Self {
        Self {
            root,
            definitions: HashMap::new(),
        }
    }

    /// Register a definition and return its id.
    pub fn define(&mut self, def: CircuitDocument) -> Uuid {
        let id = def.id;
        self.definitions.insert(id, Arc::new(def));
        id
    }

    /// Register every definition of `other`.
    pub fn absorb(&mut self, other: &WorkspaceDocument) {
        for (id, def) in &other.definitions {
            self.definitions.insert(*id, Arc::clone(def));
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Library
// ─────────────────────────────────────────────────────────────────────────────

/// Subcircuit definitions available to the open circuit, by id. Instances
/// reference a definition; it is stored once.
#[derive(Resource, Debug, Default)]
pub struct CircuitLibrary {
    pub definitions: HashMap<Uuid, Arc<CircuitDocument>>,
}

impl CircuitLibrary {
    pub fn get(&self, id: Uuid) -> Option<&Arc<CircuitDocument>> {
        self.definitions.get(&id)
    }

    /// Insert or replace definitions. Returns whether anything changed.
    pub fn merge(&mut self, defs: &HashMap<Uuid, Arc<CircuitDocument>>) -> bool {
        let mut changed = false;
        for (id, def) in defs {
            match self.definitions.get(id) {
                Some(existing) if Arc::ptr_eq(existing, def) || **existing == **def => {}
                _ => {
                    self.definitions.insert(*id, Arc::clone(def));
                    changed = true;
                }
            }
        }
        changed
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
