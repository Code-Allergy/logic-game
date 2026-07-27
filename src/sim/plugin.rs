//! Bevy plugin that registers all simulation resources and systems.

use bevy::prelude::*;

use crate::GameState;
use crate::sim::{
    SelectedGate, SimBridge, SimulationConfig,
    systems::{
        bridge_flush_system, drag_selected_system, draw_connections_system, draw_gates_system,
        gate_selection_system, rebuild_spatial_grid_system, save_on_exit_system,
        simulation_config_ui_system, ui_for_selected_gate, update_connections_system,
        update_input_colours_system, update_inputs_on_left_click, update_selected_gate,
        update_spatial_grid_system, update_tickrate_system,
    },
};

use super::fixtures::build_8bit_counter_subcircuit;
use super::spatial_grid::SpatialGrid;

pub struct SimulationPlugin {
    pub state: GameState,
}

impl SimulationPlugin {
    pub fn new(state: GameState) -> Self {
        Self { state }
    }
}

impl Plugin for SimulationPlugin {
    fn build(&self, app: &mut App) {
        let state = self.state.clone();

        // ── Resources inserted at startup so Bevy param validation never sees
        //    a missing resource, regardless of which GameState is active.
        //    (Bevy 0.19 validates Res<T> params *before* evaluating run_if.)
        let game_circuit = build_8bit_counter_subcircuit();
        let sim_bridge = SimBridge::from_circuit(&game_circuit);

        app
            // ── Core resources (always present) ─────────────────────────────
            .insert_resource(game_circuit)
            .insert_resource(sim_bridge)
            .init_resource::<SimulationConfig>()
            .init_resource::<SelectedGate>()
            .insert_resource(SpatialGrid::new(100.0))
            // ── On-enter: resume bridge + spawn visuals ──────────────────────
            .add_systems(
                OnEnter(state.clone()),
                (
                    resume_bridge_system,
                    draw_gates_system,
                    draw_connections_system.after(draw_gates_system),
                    rebuild_spatial_grid_system.after(draw_gates_system),
                ),
            )
            // ── On-exit: pause bridge + despawn visuals ──────────────────────
            .add_systems(
                OnExit(state.clone()),
                (pause_bridge_system, despawn_sim_entities),
            )
            // ── PreUpdate: drain bridge pending responses ────────────────────
            // No run_if here – bridge_flush_system is cheap and safe to call
            // in any state; it's a no-op when there are no pending items.
            .add_systems(PreUpdate, bridge_flush_system)
            // ── Update: interaction + visuals (state-gated) ──────────────────
            .add_systems(
                Update,
                (
                    update_input_colours_system,
                    gate_selection_system.before(drag_selected_system),
                    drag_selected_system,
                    update_connections_system,
                    update_spatial_grid_system,
                    update_inputs_on_left_click,
                    update_selected_gate,
                    update_tickrate_system,
                )
                    .run_if(in_state(state.clone())),
            )
            // ── Egui UI ──────────────────────────────────────────────────────
            // Must run in EguiPrimaryContextPass to access EguiContexts.
            // run_if is safe here because SimBridge now always exists (inserted
            // at plugin build time).
            .add_systems(
                bevy_egui::EguiPrimaryContextPass,
                (simulation_config_ui_system, ui_for_selected_gate).run_if(in_state(state.clone())),
            )
            // ── Save on exit ─────────────────────────────────────────────────
            // Runs independently of state so that it always fires when closing
            .add_systems(Update, save_on_exit_system);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// On-enter / On-exit helpers
// ─────────────────────────────────────────────────────────────────────────────

fn resume_bridge_system(mut bridge: ResMut<SimBridge>) {
    bridge.resume();
}

fn pause_bridge_system(mut bridge: ResMut<SimBridge>) {
    bridge.pause();
}

/// Despawn all gate / connection-line entities when leaving the sim state.
/// The `SimBridge` and `GameCircuit` resources are intentionally *kept* so
/// that Bevy param validation never sees a missing resource.
fn despawn_sim_entities(
    mut commands: Commands,
    gate_entities: Query<Entity, With<crate::sim::GateComponent>>,
    conn_entities: Query<Entity, With<crate::sim::ConnectionLine>>,
    mut grid: ResMut<SpatialGrid>,
) {
    for entity in gate_entities.iter().chain(conn_entities.iter()) {
        commands.entity(entity).despawn();
    }
    grid.clear();
}
