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
//! - [`challenges`] – the build-it challenges on the demo road and their
//!   checks.
//! - [`rtl`] – RTL import: a Yosys JSON netlist becomes a workspace.
//! - [`netlist`] – a workspace as a `logi_sim` design, for simulating it
//!   without the app (tests, benchmarks).
//! - [`layout`] – automatic layered layout of a circuit.
//! - [`routing`] – orthogonal wire routing on the grid.
//! - [`bridge`] – `SimBridge`: maps the ECS onto a `logi_sim` v2 `Design`,
//!   sends revisions to the engine and reads probes back.
//! - [`chip`] – make chip: a selection becomes a subcircuit definition.
//! - [`multi_select`] – selecting several parts (Shift+click, Shift+drag).
//! - [`history`] – undo and redo of edits (workspace snapshots).
//! - [`view`] – instance views: opening a subcircuit shows its definition
//!   with live values probed from the running root simulation.
//! - [`visuals`] – change-driven gate and wire visuals, loop highlight.
//! - [`art`] – the Workshop art kit: theme-driven materials, cached tile,
//!   pin and glyph meshes, fonts, the dot-grid bench.
//! - [`screen`] – drawing a screen part: palettes, the text font, the
//!   rasterizer.
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
pub mod challenges;
pub mod chip;
pub mod demos;
pub mod document;
pub mod history;
pub mod kind;
pub mod layout;
pub mod multi_select;
pub mod netlist;
pub mod routing;
pub mod rtl;
pub mod screen;
pub mod shapes;
pub mod source;
pub mod spatial_grid;
pub mod systems;
pub mod view;
pub mod visuals;

pub use authoring::*;
pub use bridge::{CompileFailure, LoopNode, SimBridge, SimDiagnostics, SimRuntime};
pub use chip::{ChipPlan, MakeChip, plan_chip};
pub use demos::{Demo, DemoCatalog};
pub use document::{
    CircuitDocument, CircuitLibrary, DEFAULT_LAYER_UUID, DocNode, DocWire, Incoming, MergeReport,
    PinAddr, WorkspaceDocument,
};
pub use history::{HISTORY_LIMIT, HistoryCommand, UndoHistory};
pub use kind::{GateKind, MAX_SCREEN_SCALE, SCREEN_BEZEL, ScreenMode, ScreenPalette, TEXT_CELL};
pub use plugin::{SimulationPlugin, SimulationRuntimePlugin, SimulationViewPlugin};
pub use view::{
    CircuitView, CloseView, InstanceViewItem, OpenDefinitionView, OpenInstanceView, ViewFrame,
};
pub use visuals::{EntityWorkCounter, LoopHighlight, ON_TOP_Z, OnTop, SimulationVisualsPlugin};

use bevy::prelude::*;
use bevy::reflect::Reflect;

/// Default simulation rate in ticks per second.
pub const DEFAULT_TICK_RATE: f64 = 1000.0;

/// Text attached to output/display gates showing their value.
#[derive(Component, Debug, Clone, Copy, Reflect)]
#[reflect(Component)]
pub struct LogicDisplay;

/// Tracks which gate entity is currently selected in the editor.
#[derive(Resource, Default, Debug, Clone, Copy, PartialEq, Eq)]
pub struct SelectedGate(pub Option<Entity>);

/// Parts selected together (Shift+click, Shift+drag a box), for Make chip,
/// Delete and moving them as a group. Empty when only [`SelectedGate`] (the
/// part in the inspector) is selected; when not empty it contains the
/// [`SelectedGate`] too.
#[derive(Resource, Default, Debug, Clone, PartialEq, Eq)]
pub struct MultiSelection(Vec<Entity>);

impl MultiSelection {
    pub fn entities(&self) -> &[Entity] {
        &self.0
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn contains(&self, entity: Entity) -> bool {
        self.0.contains(&entity)
    }

    pub fn clear(&mut self) {
        self.0.clear();
    }

    /// Add `entity` (no duplicates).
    pub fn insert(&mut self, entity: Entity) {
        if !self.0.contains(&entity) {
            self.0.push(entity);
        }
    }

    /// Remove `entity`; returns whether it was selected.
    pub fn remove(&mut self, entity: Entity) -> bool {
        let before = self.0.len();
        self.0.retain(|e| *e != entity);
        self.0.len() != before
    }

    /// Keep only the entities for which `keep` holds.
    pub fn retain(&mut self, keep: impl FnMut(&Entity) -> bool) {
        self.0.retain(keep);
    }

    /// Shift+click on `entity` with `primary` in the inspector: start a
    /// group from the primary part if there is none, then add `entity`,
    /// or take it out if it was in. Returns the new primary part.
    pub fn toggle(&mut self, entity: Entity, primary: Option<Entity>) -> Option<Entity> {
        if self.0.is_empty()
            && let Some(p) = primary
        {
            self.0.push(p);
        }
        if self.remove(entity) {
            if primary == Some(entity) || primary.is_none() {
                return self.0.last().copied();
            }
            return primary;
        }
        self.0.push(entity);
        Some(entity)
    }

    /// Every selected part: the group, or the primary part alone.
    pub fn all(&self, primary: Option<Entity>) -> Vec<Entity> {
        if self.0.is_empty() {
            primary.into_iter().collect()
        } else {
            self.0.clone()
        }
    }
}

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
