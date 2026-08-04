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
    Output,
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
    Clock { period_ticks: u32, duty_ticks: u32 },
    // Sub-circuits hold a complete nested `GameCircuit`.
    SubCircuit { circuit: Box<GameCircuit> },
}

impl GameNodeKind {
    /// Returns the number of input pins this node kind has.
    pub fn input_count(&self) -> usize {
        match self {
            GameNodeKind::Input => 0,
            GameNodeKind::Output => 1,
            GameNodeKind::And
            | GameNodeKind::Or
            | GameNodeKind::Xor
            | GameNodeKind::Nand
            | GameNodeKind::Nor
            | GameNodeKind::Xnor => 2,
            GameNodeKind::Not | GameNodeKind::Buffer => 1,
            GameNodeKind::Mux => 3,
            GameNodeKind::Demux | GameNodeKind::TriStateBuffer => 2,
            GameNodeKind::Clock { .. } => 0,
            GameNodeKind::SubCircuit { circuit } => circuit
                .nodes
                .values()
                .filter(|n| matches!(n.kind, GameNodeKind::Input))
                .count(),
        }
    }

    /// Returns the number of output pins this node kind has.
    pub fn output_count(&self) -> usize {
        match self {
            GameNodeKind::Input => 1,
            GameNodeKind::Output => 0,
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
            GameNodeKind::Clock { .. } => 1,
            GameNodeKind::SubCircuit { circuit } => circuit
                .nodes
                .values()
                .filter(|n| matches!(n.kind, GameNodeKind::Output))
                .count(),
        }
    }

    /// Convert to a `logi_sim` `NodeKind` for passing into the simulator.
    /// Returns `None` for `SubCircuit` (handled separately by the bridge).
    pub fn to_sim_kind(&self) -> Option<NodeKind> {
        match self {
            GameNodeKind::Input => Some(NodeKind::Input),
            GameNodeKind::Output => Some(NodeKind::Output),
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
            GameNodeKind::Output => "OUTPUT",
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
            GameNodeKind::Clock { .. } => "CLOCK",
            GameNodeKind::SubCircuit { .. } => "SUBCIRCUIT",
        }
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
}

impl GameWire {
    pub fn new(source_node: Uuid, source_pin: u8, sink_node: Uuid, sink_pin: u8) -> Self {
        Self {
            id: Uuid::new_v4(),
            source_node,
            source_pin,
            sink_node,
            sink_pin,
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
