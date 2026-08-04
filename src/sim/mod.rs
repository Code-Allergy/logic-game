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

#[cfg(test)]
mod tests;

pub mod bridge;
pub mod cache;
pub mod fixtures;
pub mod model;
pub mod service;
pub mod spatial_grid;
pub mod systems;

pub use bridge::SimBridge;
pub use cache::SimCache;
pub use model::{GameCircuit, GameNode, GameNodeKind, GameWire, SavedSimulation, WireKind};
pub use plugin::SimulationPlugin;
pub use service::{SimExecutionService, SimRuntime};

use bevy::prelude::*;
use logi_sim::engine::RunnerType;
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
#[derive(Component, Debug, Clone)]
pub struct ConnectionLine {
    pub id: Uuid,
    pub source_node: Uuid,
    pub sink_node: Uuid,
    pub wire_kind: WireKind,
}

/// Per-gate snapshot of all output pin states (bits 0..63).
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct SimNodeOutputs {
    pub bits: u64,
}

/// Per-gate snapshot of all input pin states (bits 0..63).
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct SimNodeInputs {
    pub bits: u64,
}

/// Per-wire snapshot of current logic value.
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct SimWireValue {
    pub value: bool,
}

/// Text attached to a multi-bit display node.
#[derive(Component, Debug, Clone, Copy)]
pub struct LogicDisplay {
    pub node_id: Uuid,
    pub bits: u8,
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
    pub current_runner: RunnerType,
}

impl Default for SimulationConfig {
    fn default() -> Self {
        Self {
            tick_rate: 10.0,
            unlimited_tick_rate: false,
            current_runner: RunnerType::default(),
        }
    }
}

/// Low-frequency throughput sampler for the config UI. Sampling the bridge's
/// atomic tick counter keeps measurement work off the simulation thread.
#[derive(Resource, Debug)]
pub struct TickRateMeter {
    last_tick_count: u64,
    sample_window_secs: f64,
    pub average_hz: Option<f64>,
}

impl Default for TickRateMeter {
    fn default() -> Self {
        Self {
            last_tick_count: 0,
            sample_window_secs: 0.0,
            average_hz: None,
        }
    }
}

#[derive(Resource, Default)]
pub struct ThreadlessStepper {
    pending_ticks: f64,
}

impl TickRateMeter {
    pub fn reset(&mut self, tick_count: u64) {
        self.last_tick_count = tick_count;
        self.sample_window_secs = 0.0;
        self.average_hz = None;
    }

    pub fn sample(&mut self, tick_count: u64, delta_secs: f64) {
        if delta_secs <= 0.0 {
            return;
        }
        self.sample_window_secs += delta_secs;
        if self.sample_window_secs < 0.250 {
            return;
        }

        let ticks = tick_count.saturating_sub(self.last_tick_count);
        let current_hz = ticks as f64 / self.sample_window_secs;
        self.average_hz = Some(match self.average_hz {
            Some(average) => average * 0.75 + current_hz * 0.25,
            None => current_hz,
        });
        self.last_tick_count = tick_count;
        self.sample_window_secs = 0.0;
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
        GameNodeKind::Counter8 => Color::srgb(0.1, 0.45, 0.65),
        GameNodeKind::Clock { .. } => Color::srgb(0.5, 0.1, 0.3),
        GameNodeKind::Input => Color::srgb(0.1, 0.1, 0.1),
        GameNodeKind::Input8 => Color::srgb(0.13, 0.24, 0.48),
        GameNodeKind::Input64 => Color::srgb(0.08, 0.19, 0.38),
        GameNodeKind::Output => Color::srgb(0.15, 0.15, 0.15),
        GameNodeKind::Output8 => Color::srgb(0.12, 0.35, 0.12),
        GameNodeKind::Output64 => Color::srgb(0.08, 0.26, 0.08),
        GameNodeKind::Expand8 => Color::srgb(0.35, 0.35, 0.0),
        GameNodeKind::Expand64 => Color::srgb(0.28, 0.28, 0.0),
        GameNodeKind::ReduceOr8 => Color::srgb(0.75, 0.45, 0.0),
        GameNodeKind::ReduceOr64 => Color::srgb(0.52, 0.25, 0.0),
        GameNodeKind::Display { .. } => Color::srgb(0.04, 0.09, 0.12),
        GameNodeKind::SubCircuit { .. } => Color::srgb(0.1, 0.5, 0.3),
    };
    let size = match kind {
        GameNodeKind::Display { bits } => {
            let hex_digits = usize::from(*bits).div_ceil(4);
            Vec2::new((hex_digits as f32 * 18.0 + 40.0).max(120.0), 70.0)
        }
        _ => Vec2::new(50.0, 50.0),
    };
    Sprite {
        color,
        custom_size: Some(size),
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
