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
pub use plugin::{
    SimulationPlugin, SimulationProjectionPlugin, SimulationRuntimePlugin,
    SimulationViewPlugin,
};
pub use service::{SimExecutionService, SimRuntime};

use bevy::prelude::*;
use bevy::reflect::Reflect;
use logi_sim::engine::RunnerType;
use uuid::Uuid;

// ─────────────────────────────────────────────────────────────────────────────
// Bevy components
// ─────────────────────────────────────────────────────────────────────────────

/// Bevy component that links a rendered entity to a [`GameNode`] by UUID.
#[derive(Component, Debug, Clone, Reflect)]
#[reflect(Component)]
pub struct GateComponent {
    pub id: Uuid,
    pub label: String,
    pub kind: GateKindTag,
    pub input_count: u8,
    pub output_count: u8,
    pub input_bits: u64,
    pub output_bits: u64,
}

impl GateComponent {
    pub fn new(node: &GameNode) -> Self {
        let kind = GateKindTag::from_node_kind(&node.kind);
        Self {
            id: node.id,
            label: node
                .label
                .clone()
                .unwrap_or_else(|| node.id.to_string()[..8].to_string()),
            kind,
            input_count: node.kind.input_count().min(u8::MAX as usize) as u8,
            output_count: node.kind.output_count().min(u8::MAX as usize) as u8,
            input_bits: 0,
            output_bits: 0,
        }
    }

    pub fn display_name(&self) -> &'static str {
        self.kind.display_name()
    }
}

#[derive(Component, Debug, Clone, Copy, PartialEq, Eq, Hash, Reflect)]
pub enum GateKindTag {
    Input,
    Input8,
    Input64,
    Output,
    Output8,
    Output64,
    Display,
    Expand8,
    Expand64,
    ReduceOr8,
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
    Clock,
    SubCircuit,
}

impl GateKindTag {
    pub fn from_node_kind(kind: &GameNodeKind) -> Self {
        match kind {
            GameNodeKind::Input => Self::Input,
            GameNodeKind::Input8 => Self::Input8,
            GameNodeKind::Input64 => Self::Input64,
            GameNodeKind::Output => Self::Output,
            GameNodeKind::Output8 => Self::Output8,
            GameNodeKind::Output64 => Self::Output64,
            GameNodeKind::Display { .. } => Self::Display,
            GameNodeKind::Expand8 => Self::Expand8,
            GameNodeKind::Expand64 => Self::Expand64,
            GameNodeKind::ReduceOr8 => Self::ReduceOr8,
            GameNodeKind::ReduceOr64 => Self::ReduceOr64,
            GameNodeKind::And => Self::And,
            GameNodeKind::Or => Self::Or,
            GameNodeKind::Not => Self::Not,
            GameNodeKind::Xor => Self::Xor,
            GameNodeKind::Nand => Self::Nand,
            GameNodeKind::Nor => Self::Nor,
            GameNodeKind::Xnor => Self::Xnor,
            GameNodeKind::Buffer => Self::Buffer,
            GameNodeKind::Mux => Self::Mux,
            GameNodeKind::Demux => Self::Demux,
            GameNodeKind::TriStateBuffer => Self::TriStateBuffer,
            GameNodeKind::Counter8 => Self::Counter8,
            GameNodeKind::Clock { .. } => Self::Clock,
            GameNodeKind::SubCircuit { .. } => Self::SubCircuit,
        }
    }

    pub fn display_name(&self) -> &'static str {
        match self {
            Self::Input => "INPUT",
            Self::Input8 => "INPUT8",
            Self::Input64 => "INPUT64",
            Self::Output => "OUTPUT",
            Self::Output8 => "OUTPUT8",
            Self::Output64 => "OUTPUT64",
            Self::Display => "DISPLAY",
            Self::Expand8 => "1->8",
            Self::Expand64 => "1->64",
            Self::ReduceOr8 => "8->1",
            Self::ReduceOr64 => "64->1",
            Self::And => "AND",
            Self::Or => "OR",
            Self::Not => "NOT",
            Self::Xor => "XOR",
            Self::Nand => "NAND",
            Self::Nor => "NOR",
            Self::Xnor => "XNOR",
            Self::Buffer => "BUFFER",
            Self::Mux => "MUX",
            Self::Demux => "DEMUX",
            Self::TriStateBuffer => "TRI-STATE",
            Self::Counter8 => "COUNTER8",
            Self::Clock => "CLOCK",
            Self::SubCircuit => "SUBCIRCUIT",
        }
    }
}

/// Human-friendly label for inspector/overlay display.
pub fn gate_display_name(node_id: Uuid, label: &Option<String>, kind: &GameNodeKind) -> String {
    let fallback = format!("{}", &node_id.to_string()[..8]);
    match label {
        Some(text) if !text.is_empty() => format!("{} {}", kind.display_name(), text),
        _ => format!("{} {}", kind.display_name(), fallback),
    }
}

/// Bevy component representing a rendered wire between two nodes.
#[derive(Component, Debug, Clone, Reflect)]
#[reflect(Component)]
pub struct ConnectionLine {
    pub id: Uuid,
    pub source_node: Uuid,
    pub source_pin: u8,
    pub sink_node: Uuid,
    pub sink_pin: u8,
    pub bit_width: u8,
    pub value: bool,
    #[reflect(ignore)]
    pub wire_kind: WireKind,
}

/// Per-gate snapshot of all output pin states (bits 0..63).
#[derive(Component, Debug, Clone, Copy, Default, Reflect)]
#[reflect(Component)]
pub struct SimNodeOutputs {
    pub bits: u64,
}

/// Per-gate snapshot of all input pin states (bits 0..63).
#[derive(Component, Debug, Clone, Copy, Default, Reflect)]
#[reflect(Component)]
pub struct SimNodeInputs {
    pub bits: u64,
}

/// Text attached to a multi-bit display node.
#[derive(Component, Debug, Clone, Copy, Reflect)]
#[reflect(Component)]
pub struct LogicDisplay {
    pub node_id: Uuid,
    pub bits: u8,
}

/// Marker for gate entities that are currently selected for dragging.
#[derive(Component, Reflect)]
#[reflect(Component)]
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
