//! Simulation subsystem for `gate_quest`.
//!
//! # Module layout
//!
//! - [`model`]  – Game-side data model (`GameCircuit`, `GameNode`, `GameWire`).
//! - [`bridge`] – `SimBridge`: thread lifecycle, ID mapping, event draining.
//! - [`systems`] – Bevy systems for rendering and user interaction.
//! - [`plugin`]  – `SimulationPlugin` that wires everything into Bevy.
//! - [`spatial_grid`] – Spatial acceleration for entity picking.

mod plugin;

pub mod bridge;
pub mod model;
pub mod spatial_grid;
pub mod systems;

pub use bridge::{SimBridge, SimStateCache};
pub use model::{GameCircuit, GameNode, GameNodeKind, GameWire, SavedSimulation};
pub use plugin::SimulationPlugin;

use bevy::prelude::*;
use uuid::Uuid;

// ─────────────────────────────────────────────────────────────────────────────
// Bevy components
// ─────────────────────────────────────────────────────────────────────────────

/// Bevy component that links a rendered entity to a [`GameNode`] by UUID.
#[derive(Component, Debug, Clone, Copy)]
pub struct GateComponent {
    pub id: Uuid,
    pub position: Vec2,
}

impl GateComponent {
    pub fn new(id: Uuid, position: Vec2) -> Self {
        Self { id, position }
    }
}

/// Bevy component representing a rendered wire between two nodes.
#[derive(Component, Debug, Clone, Copy)]
pub struct ConnectionLine {
    pub id: Uuid,
    pub source_node: Uuid,
    pub sink_node: Uuid,
}

/// Marker for gate entities that are currently selected for dragging.
#[derive(Component)]
pub struct Selected {
    pub offset: Vec2,
}

// ─────────────────────────────────────────────────────────────────────────────
// Resources
// ─────────────────────────────────────────────────────────────────────────────

/// Tracks which gate entity + UUID is currently selected in the editor.
#[derive(Resource, Default)]
pub struct SelectedGate(pub Option<(Entity, Uuid)>);

/// Simulation tick-rate configuration (ticks per second).
#[derive(Resource, Debug)]
pub struct SimulationConfig {
    pub tick_rate: f64,
    pub unlimited_tick_rate: bool,
}

impl Default for SimulationConfig {
    fn default() -> Self {
        Self {
            tick_rate: 10.0,
            unlimited_tick_rate: false,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Gate sprite colours
// ─────────────────────────────────────────────────────────────────────────────

/// Returns a base [`Sprite`] for a given node kind.
pub fn get_gate_sprite(kind: &GameNodeKind) -> Sprite {
    let color = match kind {
        GameNodeKind::And => Color::srgb(0.0, 0.6, 0.3),
        GameNodeKind::Or => Color::srgb(1.0, 1.0, 0.0),
        GameNodeKind::Not => Color::srgb(0.0, 0.1, 0.5),
        GameNodeKind::Nand => Color::srgb(0.6, 0.2, 0.0),
        GameNodeKind::Nor => Color::srgb(0.5, 0.5, 0.0),
        GameNodeKind::Xor => Color::srgb(0.0, 0.5, 0.8),
        GameNodeKind::Xnor => Color::srgb(0.2, 0.3, 0.8),
        GameNodeKind::Buffer => Color::srgb(0.4, 0.4, 0.4),
        GameNodeKind::Mux => Color::srgb(0.6, 0.0, 0.6),
        GameNodeKind::Demux => Color::srgb(0.8, 0.0, 0.8),
        GameNodeKind::TriStateBuffer => Color::srgb(0.3, 0.3, 0.6),
        GameNodeKind::Clock { .. } => Color::srgb(0.5, 0.1, 0.3),
        GameNodeKind::Input => Color::srgb(0.1, 0.1, 0.1),
        GameNodeKind::Output => Color::srgb(0.15, 0.15, 0.15),
        GameNodeKind::SubCircuit { .. } => Color::srgb(0.1, 0.5, 0.3),
    };
    Sprite {
        color,
        custom_size: Some(Vec2::new(50.0, 50.0)),
        ..Default::default()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Geometry helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Compute midpoint, rotation quaternion, and length for a connection line sprite.
#[inline]
pub fn calculate_connection_geometry(from: &Vec3, to: &Vec3) -> (Vec3, Quat, f32) {
    let mid = (*from + *to) / 2.0;
    let dir = (*to - *from).normalize_or_zero();
    let dist = (*to - *from).length();
    let rot = Quat::from_rotation_arc(Vec3::X, dir);
    (mid, rot, dist)
}
