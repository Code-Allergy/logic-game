//! Bevy plugin that registers all simulation resources and systems.

use bevy::prelude::*;
use bevy::window::PrimaryWindow;

use crate::MainCamera;
use crate::GameState;
use crate::sim::{
    SelectedGate, SimBridge, SimCache, SimRuntime, SimulationConfig, ThreadlessStepper,
    TickRateMeter,
    systems::{
        bridge_flush_system,
        drag_selected_system,
        draw_connections_system,
        draw_gates_system,
        draw_sim_background_system,
        drive_threadless_simulation,
        gate_selection_system,
        rebuild_spatial_grid_system,
        save_on_exit_system,
        workspace_io_system,
        simulation_config_ui_system,
        ui_for_selected_gate,
        update_connections_system,
        update_input_colours_system,
        update_inputs_on_left_click,
        update_logic_displays_system,
        update_selected_gate,
        update_spatial_grid_system,
        update_tickrate_system,
    },
};

use super::fixtures;
use super::spatial_grid::SpatialGrid;

pub struct SimulationPlugin {
    pub state: GameState,
    pub runtime: SimRuntime,
}

impl SimulationPlugin {
    pub fn new(state: GameState) -> Self {
        Self {
            state,
            #[cfg(not(target_arch = "wasm32"))]
            runtime: SimRuntime::Threaded,
            #[cfg(target_arch = "wasm32")]
            runtime: SimRuntime::Threadless,
        }
    }

    pub fn with_runtime(state: GameState, runtime: SimRuntime) -> Self {
        Self { state, runtime }
    }
}

impl Plugin for SimulationPlugin {
    fn build(&self, app: &mut App) {
        let state = self.state.clone();
        let runtime = self.runtime;

        // ── Resources inserted at startup so Bevy param validation never sees
        //    a missing resource, regardless of which GameState is active.
        //    (Bevy 0.19 validates Res<T> params *before* evaluating run_if.)
        let game_circuit = fixtures::build_64bit_counter_subcircuits();
        let mut sim_bridge = SimBridge::from_circuit_with_runtime(&game_circuit, runtime);
        power_on_reset(&mut sim_bridge, &game_circuit);

        app
            // ── Core resources (always present) ─────────────────────────────────────────
            .insert_resource(game_circuit)
            .insert_resource(sim_bridge)
            .init_resource::<SimulationConfig>()
            .init_resource::<TickRateMeter>()
            .init_resource::<ThreadlessStepper>()
            .init_resource::<SelectedGate>()
            .init_resource::<SimCache>()
            .insert_resource(SpatialGrid::new(100.0))
            .add_message::<crate::editor::LoadWorkspaceEvent>()
            .add_message::<crate::editor::SaveWorkspaceEvent>()
            // ── On-enter: resume bridge + spawn visuals + prime cache index ───────────────
            .add_systems(
                OnEnter(state.clone()),
                (
                    reset_sim_camera_system,
                    resume_bridge_system,
                    draw_sim_background_system,
                    draw_gates_system,
                    draw_connections_system.after(draw_gates_system),
                    rebuild_spatial_grid_system.after(draw_gates_system),
                    rebuild_cache_index_system.after(draw_gates_system),
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
                    workspace_io_system,
                    update_input_colours_system,
                    update_logic_displays_system,
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
            // State filtering happens inside the systems because run_if in
            // this pass can be brittle on Bevy 0.19.
            .add_systems(
                bevy_egui::EguiPrimaryContextPass,
                (simulation_config_ui_system, ui_for_selected_gate),
            )
            // ── Save on exit ─────────────────────────────────────────────────
            // Runs independently of state so that it always fires when closing
            .add_systems(Update, save_on_exit_system);

        if runtime == SimRuntime::Threadless {
            app.add_systems(
                Update,
                drive_threadless_simulation.run_if(in_state(state.clone())),
            );
        }
    }
}

/// Hold the fixture's asynchronous reset high long enough for every nested
/// latch to settle, then release it before normal clocked execution begins.
fn power_on_reset(bridge: &mut SimBridge, circuit: &crate::sim::GameCircuit) {
    let Some(reset) = circuit.nodes.values().find(|node| {
        matches!(
            node.kind,
            crate::sim::GameNodeKind::Input
                | crate::sim::GameNodeKind::Input8
                | crate::sim::GameNodeKind::Input64
        )
            && node.label.as_deref() == Some("Reset")
    }) else {
        return;
    };

    // Commands are consumed FIFO by the worker. Step is honored while paused,
    // so no wall-clock delay or startup timing race is required here.
    bridge.pause();
    bridge.set_input(reset.id, 0, true);
    let settled_at = bridge.tick_count().saturating_add(64);
    for _ in 0..64 {
        bridge.step();
    }
    // Do not let the first rendered frame observe the latch network before
    // reset propagation has completed. This reads one atomic in userspace.
    while bridge.tick_count() < settled_at {
        std::hint::spin_loop();
    }
    bridge.set_input(reset.id, 0, false);
    bridge.resume();
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

/// Prime the [`SimCache`] reverse index immediately after entering the sim state.
///
/// `bridge_flush_system` also rebuilds on `is_changed()`, but running it here
/// in `OnEnter` guarantees the index is ready before the first `Update` frame.
fn rebuild_cache_index_system(
    bridge: Res<SimBridge>,
    game_circuit: Res<crate::sim::GameCircuit>,
    mut cache: ResMut<SimCache>,
) {
    cache.rebuild_index(&game_circuit, &bridge);
    // Seed is_running from the bridge's synchronously-maintained flag so the
    // Pause/Play button is correct on the very first frame, before any
    // StatusChanged event arrives from the worker.
    cache.is_running = bridge.is_running();
}

/// Despawn all gate / connection-line entities when leaving the sim state.
/// The `SimBridge` and `GameCircuit` resources are intentionally *kept* so
/// that Bevy param validation never sees a missing resource.
fn despawn_sim_entities(
    mut commands: Commands,
    gate_entities: Query<Entity, With<crate::sim::GateComponent>>,
    conn_entities: Query<Entity, With<crate::sim::ConnectionLine>>,
    bg_entities: Query<Entity, With<crate::sim::systems::SimBackground>>,
    mut grid: ResMut<SpatialGrid>,
    mut cache: ResMut<SimCache>,
) {
    for entity in gate_entities
        .iter()
        .chain(conn_entities.iter())
        .chain(bg_entities.iter())
    {
        commands.entity(entity).despawn();
    }
    grid.clear();
    cache.clear();
}

fn reset_sim_camera_system(
    windows: Query<&Window, With<PrimaryWindow>>,
    mut cameras: Query<(&mut Projection, &mut Transform), With<MainCamera>>,
    game_circuit: Res<crate::sim::GameCircuit>,
) {
    let (mut projection, mut transform) = match cameras.single_mut() {
        Ok(query) => query,
        Err(_) => return,
    };
    let window = match windows.single() {
        Ok(window) => window,
        Err(_) => return,
    };

    let mut min_x = f32::INFINITY;
    let mut max_x = f32::NEG_INFINITY;
    let mut min_y = f32::INFINITY;
    let mut max_y = f32::NEG_INFINITY;
    for node in game_circuit.nodes.values() {
        min_x = min_x.min(node.position.x);
        max_x = max_x.max(node.position.x);
        min_y = min_y.min(node.position.y);
        max_y = max_y.max(node.position.y);
    }
    if !min_x.is_finite() || min_x == f32::INFINITY {
        return;
    }

    let center_x = (min_x + max_x) * 0.5;
    let center_y = (min_y + max_y) * 0.5;
    let width = (max_x - min_x).max(1.0);
    let height = (max_y - min_y).max(1.0);
    let padding = 1.3_f32;
    let window_width = window.width().max(1.0);
    let window_height = window.height().max(1.0);
    let mut fit_scale = (width / window_width).max(height / window_height) * padding;
    fit_scale = fit_scale.clamp(0.1, 10.0);

    if let Projection::Orthographic(ortho) = projection.as_mut() {
        ortho.scale = fit_scale;
        ortho.near = -10_000.0;
        ortho.far = 10_000.0;
    }
    transform.translation = Vec3::new(center_x, center_y, 1000.0);
}
