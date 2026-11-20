//! Simulation subsystem for `gate_quest`.
//!
//! # Module layout
//!
//! - [`kind`] – [`GateKind`], the single editor node-kind enum.
//! - [`authoring`] – ECS authoring components (the source of truth) and the
//!   commands that edit them.
//! - [`document`] – portable circuit documents (demos, save format,
//!   subcircuit definitions) and the [`CircuitLibrary`].
//! - [`source`] – save format version 6, the hand-editable authoring form
//!   (names, arrays, bus wiring), and its expansion into documents.
//! - [`demos`] – the built-in demos, loaded from `assets/demos/`.
//! - [`rtl`] – RTL import: a Yosys JSON netlist becomes a workspace.
//! - [`layout`] – automatic layered layout of a circuit.
//! - [`bridge`] – `SimBridge`: maps the ECS onto a `logi_sim` v2 `Design`,
//!   sends revisions to the engine and reads probes back.
//! - [`view`] – instance views: opening a subcircuit shows its definition
//!   with live values probed from the running root simulation.
//! - [`visuals`] – change-driven gate and wire visuals, loop highlight.
//! - [`art`] – the Workshop art kit: theme-driven materials, cached tile,
//!   pin and glyph meshes, fonts, the dot-grid bench.
//! - [`shapes`] – triangle-list shapes (rounded rectangles, strokes).
//! - [`systems`] – Bevy systems for picking, dragging and egui panels.
//! - [`plugin`] – plugins that wire everything into Bevy.
//! - [`spatial_grid`] – spatial acceleration for entity picking.

mod plugin;

#[cfg(test)]
mod tests;

pub mod art;
pub mod authoring;
pub mod bridge;
pub mod demos;
pub mod document;
pub mod kind;
pub mod layout;
pub mod rtl;
pub mod shapes;
pub mod source;
pub mod spatial_grid;
pub mod systems;
pub mod view;
pub mod visuals;

pub use authoring::*;
pub use bridge::{CompileFailure, LoopNode, SimBridge, SimDiagnostics, SimRuntime};
pub use demos::{Demo, DemoCatalog};
pub use document::{
    CircuitDocument, CircuitLibrary, DEFAULT_LAYER_UUID, DocNode, DocWire, PinAddr,
    WorkspaceDocument,
};
pub use kind::GateKind;
pub use plugin::{SimulationPlugin, SimulationRuntimePlugin, SimulationViewPlugin};
pub use view::{
    CircuitView, CloseView, InstanceViewItem, OpenDefinitionView, OpenInstanceView, ViewFrame,
};
pub use visuals::{EntityWorkCounter, LoopHighlight, SimulationVisualsPlugin};

use bevy::prelude::*;
use bevy::reflect::Reflect;

/// Default simulation rate in ticks per second.
pub const DEFAULT_TICK_RATE: f64 = 1000.0;

/// Text attached to output/display gates showing their value.
#[derive(Component, Debug, Clone, Copy, Reflect)]
#[reflect(Component)]
pub struct LogicDisplay;

/// Tracks which gate entity is currently selected in the editor.
#[derive(Resource, Default)]
pub struct SelectedGate(pub Option<Entity>);

/// Simulation tick-rate configuration (ticks per second).
#[derive(Resource, Debug)]
pub struct SimulationConfig {
    pub tick_rate: f64,
    pub unlimited_tick_rate: bool,
}

impl Default for SimulationConfig {
    fn default() -> Self {
        Self {
            tick_rate: DEFAULT_TICK_RATE,
            unlimited_tick_rate: false,
        }
    }
}

/// Low-frequency throughput sampler for the config UI.
#[derive(Resource, Debug, Default)]
pub struct TickRateMeter {
    last_tick_count: u64,
    sample_window_secs: f64,
    pub average_hz: Option<f64>,
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

/// Compute midpoint, rotation quaternion, and length for a connection line sprite.
#[inline]
pub fn calculate_connection_geometry(from: &Vec3, to: &Vec3) -> (Vec3, Quat, f32) {
    let mid = (*from + *to) / 2.0;
    let dir = (*to - *from).normalize_or_zero();
    let dist = (*to - *from).length();
    let rot = Quat::from_rotation_arc(Vec3::X, dir);
    (mid, rot, dist)
}

/// Format a value for a display gate.
pub fn format_value(value: u64, width: u8, config: &DisplayConfig) -> String {
    let width = usize::from(width.max(1));
    match (config.radix, config.leading_zeroes) {
        (DisplayRadix::Hexadecimal, true) => {
            format!("0x{value:0digits$X}", digits = width.div_ceil(4))
        }
        (DisplayRadix::Hexadecimal, false) => format!("0x{value:X}"),
        (DisplayRadix::Binary, true) => format!("0b{value:0width$b}"),
        (DisplayRadix::Binary, false) => format!("0b{value:b}"),
        (DisplayRadix::Decimal, _) => format!("{value}"),
    }
}
