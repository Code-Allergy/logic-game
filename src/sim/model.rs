//! Frontend domain model for the logic circuit editor.
//!
//! This module defines the **game-side** representation of a circuit, which is
//! completely decoupled from the logi-sim simulation engine. All IDs are stable
//! `Uuid` values, making this model fully serializable and safe to persist across
//! save/load cycles, circuit switching, and undo/redo.
//!
//! The [`SimBridge`](super::bridge::SimBridge) is responsible for mapping these
//! frontend UUIDs to the internal `NodeId`/`WireId` keys used by `logi-sim`.

use std::collections::HashMap;

use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use logi_sim::prelude::NodeKind;

const CIRCUIT_TEMPLATE_NAMESPACE: Uuid =
    Uuid::from_u128(0x6b2f8d8ff0a147dbbf4a2c7d9e3f6b5d);

fn circuit_template_signature_from_structure(signature: &str) -> Uuid {
    Uuid::new_v5(&CIRCUIT_TEMPLATE_NAMESPACE, signature.as_bytes())
}

// ─────────────────────────────────────────────────────────────────────────────
// Node kind serialization shim
//
// `NodeKind` comes from logi-sim and doesn't derive `Serialize`/`Deserialize`.
// We represent it with our own `GameNodeKind` that maps 1-to-1 and is fully
// owned by the game crate.
// ─────────────────────────────────────────────────────────────────────────────

/// Game-owned representation of a node's logical function.
///
/// Maps directly to `logi_sim::circuit::NodeKind`. Keeps the editor decoupled
/// from the simulator's internal types while retaining full serialization.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum GameNodeKind {
    Input,
    /// Multi-bit source with independent bit outputs.
    Input8,
    /// Multi-bit source with independent bit outputs.
    Input64,
    Output,
    /// Multi-bit sink expecting independent bit inputs.
    Output8,
    /// Multi-bit sink expecting independent bit inputs.
    Output64,
    /// A multi-bit output sink rendered as a single hexadecimal value.
    Display {
        bits: u8,
    },
    /// Expands a scalar signal to multiple identical outputs.
    Expand8,
    /// Expands a scalar signal to multiple identical outputs.
    Expand64,
    /// Reduces multiple inputs to one scalar via OR.
    ReduceOr8,
    /// Reduces multiple inputs to one scalar via OR.
    ReduceOr64,
    And,
    Or,
    Not,
    Xor,
    Nand,
    Nor,
    Xnor,
    Buffer,
    Mux,
    Demux,
    TriStateBuffer,
    Counter8,
    Clock {
        period_ticks: u32,
        duty_ticks: u32,
    },
    // Sub-circuits hold a complete nested `GameCircuit`.
    SubCircuit {
        circuit: Box<GameCircuit>,
    },
}

impl GameNodeKind {
    /// Returns the number of input pins this node kind has.
    pub fn input_count(&self) -> usize {
        match self {
            GameNodeKind::Input => 0,
            GameNodeKind::Input8 => 0,
            GameNodeKind::Input64 => 0,
            GameNodeKind::Output => 1,
            GameNodeKind::Output8 => 8,
            GameNodeKind::Output64 => 64,
            GameNodeKind::Display { bits } => usize::from(*bits),
            GameNodeKind::Expand8 => 1,
            GameNodeKind::Expand64 => 1,
            GameNodeKind::ReduceOr8 => 8,
            GameNodeKind::ReduceOr64 => 64,
            GameNodeKind::And
            | GameNodeKind::Or
            | GameNodeKind::Xor
            | GameNodeKind::Nand
            | GameNodeKind::Nor
            | GameNodeKind::Xnor => 2,
            GameNodeKind::Not | GameNodeKind::Buffer => 1,
            GameNodeKind::Mux => 3,
            GameNodeKind::Demux | GameNodeKind::TriStateBuffer => 2,
            GameNodeKind::Counter8 => 2,
            GameNodeKind::Clock { .. } => 0,
            GameNodeKind::SubCircuit { circuit } => circuit
                .nodes
                .values()
                .filter(|n| {
                    matches!(
                        n.kind,
                        GameNodeKind::Input | GameNodeKind::Input8 | GameNodeKind::Input64
                    )
                })
                .map(|n| n.kind.output_count())
                .sum(),
        }
    }

    /// Returns the number of output pins this node kind has.
    pub fn output_count(&self) -> usize {
        match self {
            GameNodeKind::Input => 1,
            GameNodeKind::Input8 => 8,
            GameNodeKind::Input64 => 64,
            GameNodeKind::Output => 0,
            GameNodeKind::Output8 => 0,
            GameNodeKind::Output64 => 0,
            GameNodeKind::Display { .. } => 0,
            GameNodeKind::Expand8 => 8,
            GameNodeKind::Expand64 => 64,
            GameNodeKind::ReduceOr8 => 1,
            GameNodeKind::ReduceOr64 => 1,
            GameNodeKind::And
            | GameNodeKind::Or
            | GameNodeKind::Xor
            | GameNodeKind::Nand
            | GameNodeKind::Nor
            | GameNodeKind::Xnor
            | GameNodeKind::Not
            | GameNodeKind::Buffer => 1,
            GameNodeKind::Mux => 1,
            GameNodeKind::Demux => 2,
            GameNodeKind::TriStateBuffer => 1,
            GameNodeKind::Counter8 => 8,
            GameNodeKind::Clock { .. } => 1,
            GameNodeKind::SubCircuit { circuit } => circuit
                .nodes
                .values()
                .filter(|n| {
                    matches!(
                        n.kind,
                        GameNodeKind::Output | GameNodeKind::Output8 | GameNodeKind::Output64
                    )
                })
                .map(|n| match n.kind {
                    GameNodeKind::Output => 1,
                    GameNodeKind::Output8 => 8,
                    GameNodeKind::Output64 => 64,
                    _ => 0,
                })
                .sum(),
        }
    }

    pub fn input_pin_width(&self, pin: u8) -> Option<u8> {
        (usize::from(pin) < self.input_count()).then_some(1)
    }

    pub fn output_pin_width(&self, pin: u8) -> Option<u8> {
        (usize::from(pin) < self.output_count()).then_some(1)
    }

    /// Convert to a `logi_sim` `NodeKind` for passing into the simulator.
    /// Returns `None` for `SubCircuit` (handled separately by the bridge).
    pub fn to_sim_kind(&self) -> Option<NodeKind> {
        match self {
            GameNodeKind::Input => Some(NodeKind::Input),
            GameNodeKind::Input8 => Some(NodeKind::BusInput { bits: 8 }),
            GameNodeKind::Input64 => Some(NodeKind::BusInput { bits: 64 }),
            GameNodeKind::Output => Some(NodeKind::Output),
            GameNodeKind::Output8 => Some(NodeKind::BusOutput { bits: 8 }),
            GameNodeKind::Output64 => Some(NodeKind::BusOutput { bits: 64 }),
            GameNodeKind::Display { bits } => Some(NodeKind::BusOutput { bits: *bits }),
            GameNodeKind::Expand8 => Some(NodeKind::BusExpand { bits: 8 }),
            GameNodeKind::Expand64 => Some(NodeKind::BusExpand { bits: 64 }),
            GameNodeKind::ReduceOr8 => Some(NodeKind::BusReduceOr { bits: 8 }),
            GameNodeKind::ReduceOr64 => Some(NodeKind::BusReduceOr { bits: 64 }),
            GameNodeKind::And => Some(NodeKind::And),
            GameNodeKind::Or => Some(NodeKind::Or),
            GameNodeKind::Not => Some(NodeKind::Not),
            GameNodeKind::Xor => Some(NodeKind::Xor),
            GameNodeKind::Nand => Some(NodeKind::Nand),
            GameNodeKind::Nor => Some(NodeKind::Nor),
            GameNodeKind::Xnor => Some(NodeKind::Xnor),
            GameNodeKind::Buffer => Some(NodeKind::Buffer),
            GameNodeKind::Mux => Some(NodeKind::Mux),
            GameNodeKind::Demux => Some(NodeKind::Demux),
            GameNodeKind::TriStateBuffer => Some(NodeKind::TriStateBuffer),
            GameNodeKind::Counter8 => Some(NodeKind::Counter8),
            GameNodeKind::Clock {
                period_ticks,
                duty_ticks,
            } => Some(NodeKind::Clock {
                duty_ticks: *duty_ticks,
                period_ticks: *period_ticks,

                phase: 0,
            }),
            GameNodeKind::SubCircuit { .. } => None,
        }
    }

    /// Display name shown in the editor UI.
    pub fn display_name(&self) -> &'static str {
        match self {
            GameNodeKind::Input => "INPUT",
            GameNodeKind::Input8 => "INPUT8",
            GameNodeKind::Input64 => "INPUT64",
            GameNodeKind::Output => "OUTPUT",
            GameNodeKind::Output8 => "OUTPUT8",
            GameNodeKind::Output64 => "OUTPUT64",
            GameNodeKind::Display { .. } => "DISPLAY",
            GameNodeKind::Expand8 => "1->8",
            GameNodeKind::Expand64 => "1->64",
            GameNodeKind::ReduceOr8 => "8->1",
            GameNodeKind::ReduceOr64 => "64->1",
            GameNodeKind::And => "AND",
            GameNodeKind::Or => "OR",
            GameNodeKind::Not => "NOT",
            GameNodeKind::Xor => "XOR",
            GameNodeKind::Nand => "NAND",
            GameNodeKind::Nor => "NOR",
            GameNodeKind::Xnor => "XNOR",
            GameNodeKind::Buffer => "BUFFER",
            GameNodeKind::Mux => "MUX",
            GameNodeKind::Demux => "DEMUX",
            GameNodeKind::TriStateBuffer => "TRI-STATE",
            GameNodeKind::Counter8 => "COUNTER8",
            GameNodeKind::Clock { .. } => "CLOCK",
            GameNodeKind::SubCircuit { .. } => "SUBCIRCUIT",
        }
    }

    fn structural_signature_internal(&self, visited: &mut HashMap<Uuid, String>) -> String {
        match self {
            GameNodeKind::SubCircuit { circuit } => circuit.structural_signature_internal(visited),
            GameNodeKind::Input => "Input".to_string(),
            GameNodeKind::Input8 => "Input8".to_string(),
            GameNodeKind::Input64 => "Input64".to_string(),
            GameNodeKind::Output => "Output".to_string(),
            GameNodeKind::Output8 => "Output8".to_string(),
            GameNodeKind::Output64 => "Output64".to_string(),
            GameNodeKind::Display { bits } => format!("Display({bits})"),
            GameNodeKind::Expand8 => "Expand8".to_string(),
            GameNodeKind::Expand64 => "Expand64".to_string(),
            GameNodeKind::ReduceOr8 => "ReduceOr8".to_string(),
            GameNodeKind::ReduceOr64 => "ReduceOr64".to_string(),
            GameNodeKind::And => "And".to_string(),
            GameNodeKind::Or => "Or".to_string(),
            GameNodeKind::Not => "Not".to_string(),
            GameNodeKind::Xor => "Xor".to_string(),
            GameNodeKind::Nand => "Nand".to_string(),
            GameNodeKind::Nor => "Nor".to_string(),
            GameNodeKind::Xnor => "Xnor".to_string(),
            GameNodeKind::Buffer => "Buffer".to_string(),
            GameNodeKind::Mux => "Mux".to_string(),
            GameNodeKind::Demux => "Demux".to_string(),
            GameNodeKind::TriStateBuffer => "TriStateBuffer".to_string(),
            GameNodeKind::Counter8 => "Counter8".to_string(),
            GameNodeKind::Clock {
                period_ticks,
                duty_ticks,
            } => format!("Clock({period_ticks}:{duty_ticks})"),
        }
    }

    pub fn structural_signature(&self) -> String {
        let mut visited = HashMap::new();
        self.structural_signature_internal(&mut visited)
    }

}

// ─────────────────────────────────────────────────────────────────────────────
// Core frontend models
// ─────────────────────────────────────────────────────────────────────────────

/// A single logic node in the frontend model.
///
/// Stores all persistent data the editor needs for display, interaction, and
/// serialization. The simulator counterpart is addressed via `SimBridge`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GameNode {
    /// Stable identifier. Survives save/load and circuit switches.
    pub id: Uuid,
    /// Logical function of this node.
    pub kind: GameNodeKind,
    /// World-space position in the editor canvas.
    pub position: Vec2,
    /// Optional human-readable label.
    pub label: Option<String>,
    /// Arbitrary key-value metadata (e.g. clock frequency, custom colors).
    pub custom_properties: HashMap<String, String>,
}

/// Logical width for a connection between two node pins.
///
/// Most circuits are still scalar today, but this enum is intentionally
/// forward-looking so we can add bus-level wiring (8-bit, 64-bit, etc.)
/// without changing the serialized game model.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", content = "bits")]
pub enum WireKind {
    Scalar,
    Vector {
        #[serde(default)]
        bits: u8,
    },
}

impl Default for WireKind {
    fn default() -> Self {
        Self::Scalar
    }
}

impl WireKind {
    pub fn bit_width(&self) -> u8 {
        match self {
            Self::Scalar => 1,
            Self::Vector { bits } => *bits,
        }
    }

    pub fn fits_between(&self, source_width: u8, sink_width: u8) -> bool {
        self.bit_width() <= source_width && self.bit_width() <= sink_width
    }

    pub fn display_name(&self) -> String {
        match self {
            Self::Scalar => "Scalar (1-bit)".to_string(),
            Self::Vector { bits } => format!("Vector ({bits}-bit)"),
        }
    }

    pub fn is_supported_by_sim(&self) -> bool {
        matches!(self, Self::Scalar | Self::Vector { .. })
    }
}

impl GameNode {
    pub fn new(kind: GameNodeKind, position: Vec2) -> Self {
        Self {
            id: Uuid::new_v4(),
            kind,
            position,
            label: None,
            custom_properties: HashMap::new(),
        }
    }
}

/// A directed wire connecting one node's output pin to another node's input pin.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GameWire {
    /// Stable identifier.
    pub id: Uuid,
    /// Source node UUID.
    pub source_node: Uuid,
    /// Output pin index on the source node.
    pub source_pin: u8,
    /// Sink node UUID.
    pub sink_node: Uuid,
    /// Input pin index on the sink node.
    pub sink_pin: u8,
    /// Logical width for this wire.
    #[serde(default)]
    pub kind: WireKind,
}

impl GameWire {
    pub fn new(source_node: Uuid, source_pin: u8, sink_node: Uuid, sink_pin: u8) -> Self {
        Self::with_kind(
            source_node,
            source_pin,
            sink_node,
            sink_pin,
            WireKind::default(),
        )
    }

    pub fn with_kind(
        source_node: Uuid,
        source_pin: u8,
        sink_node: Uuid,
        sink_pin: u8,
        kind: WireKind,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            source_node,
            source_pin,
            sink_node,
            sink_pin,
            kind,
        }
    }
}

/// The top-level frontend circuit: a collection of nodes and wires.
///
/// This is the authoritative source of truth for the editor and the only thing
/// that gets serialized to disk. `SimBridge` builds the `logi-sim` runtime from
/// this model on demand.
#[derive(Debug, Clone, Serialize, Deserialize, Default, Resource, PartialEq)]
pub struct GameCircuit {
    /// Circuit-level identifier, useful for sub-circuit references.
    pub id: Uuid,
    /// Human-readable circuit name.
    pub name: String,
    /// All nodes keyed by their UUID.
    pub nodes: HashMap<Uuid, GameNode>,
    /// All wires keyed by their UUID.
    pub wires: HashMap<Uuid, GameWire>,
}

impl GameCircuit {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: name.into(),
            nodes: HashMap::new(),
            wires: HashMap::new(),
        }
    }

    /// Insert a node and return its UUID.
    pub fn add_node(&mut self, node: GameNode) -> Uuid {
        let id = node.id;
        self.nodes.insert(id, node);
        id
    }

    /// Insert a wire and return its UUID.
    pub fn add_wire(&mut self, wire: GameWire) -> Uuid {
        let id = wire.id;
        self.wires.insert(id, wire);
        id
    }

    /// Reassign fresh UUIDs to every node/wire in this circuit and all
    /// nested subcircuits. This is used to materialize reusable subcircuit
    /// templates into unique world instances.
    pub fn remap_instance_ids(&mut self) {
        let old_nodes = std::mem::take(&mut self.nodes);
        let old_wires = std::mem::take(&mut self.wires);

        let mut node_id_map = HashMap::with_capacity(old_nodes.len());
        let mut new_nodes = HashMap::with_capacity(old_nodes.len());

        for (_, mut node) in old_nodes {
            if let GameNodeKind::SubCircuit { circuit } = &mut node.kind {
                circuit.remap_instance_ids();
                circuit.id = Uuid::new_v4();
            }

            let old_id = node.id;
            let new_id = Uuid::new_v4();
            node_id_map.insert(old_id, new_id);
            node.id = new_id;
            new_nodes.insert(new_id, node);
        }

        self.nodes = new_nodes;
        for (_, mut wire) in old_wires {
            let source_node = match node_id_map.get(&wire.source_node) {
                Some(id) => *id,
                None => continue,
            };
            let sink_node = match node_id_map.get(&wire.sink_node) {
                Some(id) => *id,
                None => continue,
            };

            wire.id = Uuid::new_v4();
            wire.source_node = source_node;
            wire.sink_node = sink_node;
            self.wires.insert(wire.id, wire);
        }

        self.id = Uuid::new_v4();
    }

    /// Build a deterministic signature for this circuit that ignores concrete IDs.
    /// Useful for detecting accidental duplication of identical circuit structures.
    pub fn structural_signature(&self) -> String {
        let mut visited = HashMap::new();
        self.structural_signature_internal(&mut visited)
    }

    /// Canonical template ID derived from structure-only content.
    ///
    /// This is stable across sessions and allows de-duplicating templates in
    /// workspace bundles when circuit structure is identical.
    pub fn template_uuid(&self) -> Uuid {
        circuit_template_signature_from_structure(&self.structural_signature())
    }

    fn structural_signature_internal(&self, visited: &mut HashMap<Uuid, String>) -> String {
        if let Some(sig) = visited.get(&self.id) {
            return sig.clone();
        }

        let mut node_signatures: Vec<String> = Vec::with_capacity(self.nodes.len());
        let mut node_kind_by_id: HashMap<Uuid, String> = HashMap::with_capacity(self.nodes.len());
        let mut node_signature_cache = HashMap::new();

        for node in self.nodes.values() {
            let kind_sig = match &node.kind {
                GameNodeKind::SubCircuit { circuit } => {
                    if let Some(sig) = visited.get(&circuit.id) {
                        format!("SubCircuit({sig})")
                    } else {
                        let sig = circuit.structural_signature_internal(visited);
                        format!("SubCircuit({sig})")
                    }
                }
                _ => node.kind.structural_signature_internal(&mut node_signature_cache),
            };

            let node_sig = format!("{kind_sig}|in:{}|out:{}", node.kind.input_count(), node.kind.output_count());
            node_kind_by_id.insert(node.id, node_sig.clone());
            node_signatures.push(node_sig);
        }

        node_signatures.sort_unstable();

        let mut wire_signatures: Vec<String> = Vec::with_capacity(self.wires.len());
        for wire in self.wires.values() {
            let source = node_kind_by_id
                .get(&wire.source_node)
                .cloned()
                .unwrap_or_else(|| "missing-node".to_string());
            let sink = node_kind_by_id
                .get(&wire.sink_node)
                .cloned()
                .unwrap_or_else(|| "missing-node".to_string());
            let kind_sig = wire.kind.display_name();
            wire_signatures.push(format!(
                "{source}:{}->{sink}:{}:{}",
                wire.source_pin, wire.sink_pin, kind_sig
            ));
        }

        wire_signatures.sort_unstable();
        let signature = format!(
            "nodes=[{}]; wires=[{}]",
            node_signatures.join("|"),
            wire_signatures.join("|")
        );

        visited.insert(self.id, signature.clone());
        signature
    }

    /// Remove a node and all wires connected to it.
    pub fn remove_node(&mut self, node_id: Uuid) {
        self.nodes.remove(&node_id);
        self.wires
            .retain(|_, w| w.source_node != node_id && w.sink_node != node_id);
    }

    /// Remove a wire by UUID.
    pub fn remove_wire(&mut self, wire_id: Uuid) {
        self.wires.remove(&wire_id);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Serialization root
// ─────────────────────────────────────────────────────────────────────────────

/// Top-level save file structure.
///
/// Everything needed to fully restore an editing session from disk.
#[derive(Debug, Serialize, Deserialize)]
pub struct SavedSimulation {
    /// Schema version, for forward-compatibility.
    pub version: u32,
    /// The circuit being edited.
    pub circuit: GameCircuit,
}

impl SavedSimulation {
    pub const CURRENT_VERSION: u32 = 1;

    pub fn new(circuit: GameCircuit) -> Self {
        Self {
            version: Self::CURRENT_VERSION,
            circuit,
        }
    }
}
