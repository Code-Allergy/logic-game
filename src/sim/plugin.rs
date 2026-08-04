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
        apply_authoring_messages_system,
        drag_selected_system,
        draw_connections_system,
        draw_gates_system,
        draw_sim_background_system,
        drive_threadless_simulation,
        gate_selection_system,
        sync_sim_cache_system,
        sync_gate_components_to_model_system,
        rebuild_circuit_snapshot_from_ecs_system,
        rebuild_bridge_after_authoring_change_system,
        track_authoring_component_changes_system,
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

/// Installs the simulation runner, UUID bridge, and ECS state projection.
///
/// This plugin intentionally contains no rendering, picking, or egui systems,
/// so headless tools and tests can run the same simulation integration layer.
pub struct SimulationRuntimePlugin {
    pub state: GameState,
    pub runtime: SimRuntime,
}

/// Projects runner state into UUID-addressed Bevy components once per frame.
/// This is the sole scheduling boundary between runtime IDs and game ECS data.
pub struct SimulationProjectionPlugin {
    pub state: GameState,
}

/// Installs the Bevy presentation and interaction systems for a simulation.
/// It expects [`SimulationRuntimePlugin`] to have installed the authoritative
/// circuit, bridge, and cache resources first.
pub struct SimulationViewPlugin {
    pub state: GameState,
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
        app.add_plugins((
            SimulationRuntimePlugin {
                state: self.state.clone(),
                runtime: self.runtime,
            },
            SimulationProjectionPlugin {
                state: self.state.clone(),
            },
            SimulationViewPlugin {
                state: self.state.clone(),
            },
        ));
    }
}

impl Plugin for SimulationRuntimePlugin {
    fn build(&self, app: &mut App) {
        let state = self.state.clone();
        let game_circuit = fixtures::build_64bit_counter_subcircuits();
        let sim_bridge = SimBridge::from_circuit_with_runtime(&game_circuit, self.runtime);
        let active_circuit = crate::sim::ActiveCircuit::from_circuit(&game_circuit);

        app
            .insert_resource(game_circuit)
            .insert_resource(active_circuit)
            .insert_resource(sim_bridge)
            .init_resource::<crate::sim::WorkspaceRevision>()
            .init_resource::<SimulationConfig>()
            .init_resource::<TickRateMeter>()
            .init_resource::<ThreadlessStepper>()
            .add_systems(Startup, pause_bridge_system)
            .add_systems(
                OnEnter(state.clone()),
                (power_on_reset_system, resume_bridge_system),
            )
            .add_systems(OnExit(state.clone()), pause_bridge_system)
            .add_systems(PreUpdate, bridge_flush_system)
            .add_systems(
                Update,
                update_tickrate_system.run_if(in_state(state.clone())),
            );

        if self.runtime == SimRuntime::Threadless {
            app.add_systems(
                Update,
                drive_threadless_simulation.run_if(in_state(state.clone())),
            );
        }
    }
}

impl Plugin for SimulationProjectionPlugin {
    fn build(&self, app: &mut App) {
        let state = self.state.clone();

        app.init_resource::<SimCache>()
            .add_systems(
                OnEnter(state.clone()),
                (
                    configure_cache_trace_system,
                    rebuild_cache_index_system.after(configure_cache_trace_system),
                ),
            )
            .add_systems(
                PreUpdate,
                sync_sim_cache_system
                    .after(bridge_flush_system)
                    .run_if(in_state(state)),
            );
    }
}

impl Plugin for SimulationViewPlugin {
    fn build(&self, app: &mut App) {
        let state = self.state.clone();

        app.init_resource::<SelectedGate>()
            .insert_resource(SpatialGrid::new(100.0))
            .add_message::<crate::editor::LoadWorkspaceEvent>()
            .add_message::<crate::editor::SaveWorkspaceEvent>()
            .register_type::<crate::sim::WireCollection>()
            .register_type::<crate::sim::GateKindTag>()
            .register_type::<crate::sim::LogicDisplay>()
            .register_type::<crate::sim::SimNodeOutputs>()
            .register_type::<crate::sim::SimNodeInputs>()
            .register_type::<crate::sim::ActiveCircuit>()
            .register_type::<crate::sim::WorkspaceRevision>()
            .register_type::<crate::sim::CircuitMember>()
            .register_type::<crate::sim::CircuitLayer>()
            .register_type::<crate::sim::GateIdentity>()
            .register_type::<crate::sim::GateLabel>()
            .register_type::<crate::sim::GateType>()
            .register_type::<crate::sim::GatePorts>()
            .register_type::<crate::sim::GatePinState>()
            .register_type::<crate::sim::PersistentGateData>()
            .register_type::<crate::sim::ClockConfig>()
            .register_type::<crate::sim::DisplayConfig>()
            .register_type::<crate::sim::DisplayRadix>()
            .register_type::<crate::sim::SubCircuitInstance>()
            .register_type::<crate::sim::PortDirection>()
            .register_type::<crate::sim::InterfacePort>()
            .register_type::<crate::sim::WireIdentity>()
            .register_type::<crate::sim::WireEndpoints>()
            .register_type::<crate::sim::WireProperties>()
            .register_type::<crate::sim::WireState>()
            .add_message::<crate::sim::GateLabelChanged>()
            .add_message::<crate::sim::ClockConfigChanged>()
            .add_message::<crate::sim::DisplayConfigChanged>()
            .add_message::<crate::sim::GateMoved>()
            .add_message::<crate::sim::EntityLayerChanged>()
            .add_message::<crate::sim::AuthoringTopologyChanged>()
            .add_systems(
                OnEnter(state.clone()),
                (
                    reset_sim_camera_system,
                    draw_sim_background_system,
                    draw_gates_system,
                    draw_connections_system.after(draw_gates_system),
                    rebuild_spatial_grid_system.after(draw_gates_system),
                ),
            )
            .add_systems(OnExit(state.clone()), despawn_sim_entities)
            .add_systems(
                Update,
                (
                    apply_authoring_messages_system,
                    track_authoring_component_changes_system,
                    rebuild_circuit_snapshot_from_ecs_system,
                    rebuild_bridge_after_authoring_change_system,
                    workspace_io_system,
                )
                    .chain()
                    .run_if(in_state(state.clone())),
            )
            .add_systems(
                Update,
                (
                    sync_gate_components_to_model_system,
                    update_input_colours_system,
                    update_logic_displays_system,
                    gate_selection_system.before(drag_selected_system),
                    drag_selected_system,
                    update_connections_system,
                    update_spatial_grid_system,
                    update_inputs_on_left_click,
                    update_selected_gate,
                )
                    .run_if(in_state(state.clone())),
            )
            .add_systems(
                bevy_egui::EguiPrimaryContextPass,
                (simulation_config_ui_system, ui_for_selected_gate),
            )
            .add_systems(
                Update,
                save_on_exit_system.after(rebuild_circuit_snapshot_from_ecs_system),
            );
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

fn power_on_reset_system(mut bridge: ResMut<SimBridge>, game_circuit: Res<crate::sim::GameCircuit>) {
    power_on_reset(&mut bridge, &game_circuit);
}

// ─────────────────────────────────────────────────────────────────────────────
// On-enter / On-exit helpers
// ─────────────────────────────────────────────────────────────────────────────

fn resume_bridge_system(mut bridge: ResMut<SimBridge>) {
    bridge.resume();
}

fn configure_cache_trace_system(mut cache: ResMut<SimCache>) {
    if std::env::var("LOGI_SIM_TRACE").is_ok() {
        cache.set_trace_mapping(true);
    }
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
    cache.set_running(bridge.is_running());
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
