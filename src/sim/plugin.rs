//! Bevy plugins that register the simulation resources and systems.

use std::sync::Arc;

use bevy::prelude::*;

use crate::GameState;
use crate::sim::{
    ActiveCircuit, AuthoringFeedback, CircuitLibrary, CreateGate, CreateWire, DeleteGate,
    DeleteWire, DisplayConfigChanged, EntityLayerChanged, GateIdentity, GateLabelChanged,
    GateMoved, LoadWorkspaceEvent, RegisterDefinitions, SaveWorkspaceEvent, SelectedGate,
    SetGateKind, SetInputValue, SimBridge, SimDiagnostics, SimRuntime, SimulationConfig,
    TickRateMeter, WireIdentity, WorkspaceRevision,
    view::{
        CircuitView, CloseView, InstanceViewItem, OpenDefinitionView, OpenInstanceView,
        view_navigation_system, write_back_view_system,
    },
    authoring::{
        apply_authoring_messages_system, apply_create_gate_system, apply_create_wire_system,
        apply_delete_system, apply_register_definitions_system, apply_set_input_system,
        apply_set_kind_system, load_workspace_system, sync_gate_names_system,
        track_authoring_component_changes_system,
    },
    bridge::{
        collect_engine_events_system, drive_local_engine_system, project_probes_system,
        sync_design_system, update_tickrate_system,
    },
    systems::{
        drag_selected_system, draw_sim_background_system, fit_camera_to_circuit_system,
        gate_selection_system, save_on_exit_system, simulation_config_ui_system,
        ui_for_selected_gate, update_inputs_on_left_click, update_selected_gate,
        update_spatial_grid_system, workspace_io_system,
    },
    visuals::SimulationVisualsPlugin,
};

use super::fixtures;
use super::spatial_grid::SpatialGrid;

/// Everything: runtime plus view, entering `state` loads the default example.
pub struct SimulationPlugin {
    pub state: GameState,
    pub runtime: SimRuntime,
}

/// The simulation runtime: ECS authoring commands, the bridge to the v2
/// engine, probe readback. No rendering, picking or egui, so headless tools
/// and tests run the same integration layer.
pub struct SimulationRuntimePlugin {
    pub runtime: SimRuntime,
}

/// Presentation and interaction (includes [`SimulationVisualsPlugin`]).
/// Expects [`SimulationRuntimePlugin`].
pub struct SimulationViewPlugin {
    pub state: GameState,
}

impl SimulationPlugin {
    pub fn new(state: GameState) -> Self {
        Self { state, runtime: SimRuntime::platform_default() }
    }

    pub fn with_runtime(state: GameState, runtime: SimRuntime) -> Self {
        Self { state, runtime }
    }
}

impl Plugin for SimulationPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins((
            SimulationRuntimePlugin { runtime: self.runtime },
            SimulationViewPlugin { state: self.state.clone() },
        ));
    }
}

impl Plugin for SimulationRuntimePlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(SimBridge::new(self.runtime))
            .init_resource::<SimDiagnostics>()
            .init_resource::<CircuitLibrary>()
            .init_resource::<CircuitView>()
            .init_resource::<ActiveCircuit>()
            .init_resource::<WorkspaceRevision>()
            .init_resource::<AuthoringFeedback>()
            .init_resource::<SimulationConfig>()
            .init_resource::<TickRateMeter>()
            .add_message::<CreateGate>()
            .add_message::<CreateWire>()
            .add_message::<DeleteGate>()
            .add_message::<DeleteWire>()
            .add_message::<SetInputValue>()
            .add_message::<SetGateKind>()
            .add_message::<RegisterDefinitions>()
            .add_message::<GateLabelChanged>()
            .add_message::<DisplayConfigChanged>()
            .add_message::<GateMoved>()
            .add_message::<EntityLayerChanged>()
            .add_message::<LoadWorkspaceEvent>()
            .add_message::<SaveWorkspaceEvent>()
            .add_message::<OpenInstanceView>()
            .add_message::<OpenDefinitionView>()
            .add_message::<CloseView>()
            .add_systems(
                PreUpdate,
                (drive_local_engine_system, collect_engine_events_system, project_probes_system)
                    .chain(),
            )
            .add_systems(
                Update,
                (
                    load_workspace_system,
                    view_navigation_system,
                    apply_register_definitions_system,
                    apply_create_gate_system,
                    apply_create_wire_system,
                    apply_delete_system,
                    apply_set_kind_system,
                    apply_set_input_system,
                    apply_authoring_messages_system,
                    sync_gate_names_system,
                    track_authoring_component_changes_system,
                    write_back_view_system,
                    update_tickrate_system,
                )
                    .chain(),
            )
            .add_systems(PostUpdate, sync_design_system);
    }
}

impl Plugin for SimulationViewPlugin {
    fn build(&self, app: &mut App) {
        let state = self.state.clone();

        app.add_plugins(SimulationVisualsPlugin)
            .init_resource::<SelectedGate>()
            .insert_resource(SpatialGrid::new(100.0))
            .register_type::<crate::sim::LogicDisplay>()
            .register_type::<crate::sim::GateKind>()
            .register_type::<crate::sim::ActiveCircuit>()
            .register_type::<crate::sim::WorkspaceRevision>()
            .register_type::<crate::sim::CircuitMember>()
            .register_type::<crate::sim::CircuitLayer>()
            .register_type::<crate::sim::GateIdentity>()
            .register_type::<crate::sim::GateLabel>()
            .register_type::<crate::sim::GateType>()
            .register_type::<crate::sim::GateValues>()
            .register_type::<crate::sim::GateProbes>()
            .register_type::<crate::sim::InputDrive>()
            .register_type::<crate::sim::PersistentGateData>()
            .register_type::<crate::sim::DisplayConfig>()
            .register_type::<crate::sim::DisplayRadix>()
            .register_type::<crate::sim::WireIdentity>()
            .register_type::<crate::sim::WireEndpoints>()
            .register_type::<crate::sim::WireState>()
            .register_type::<InstanceViewItem>()
            .add_systems(Startup, pause_bridge_system)
            .add_systems(
                OnEnter(state.clone()),
                (
                    load_default_circuit_system,
                    show_circuit_system,
                    resume_bridge_system,
                    draw_sim_background_system,
                ),
            )
            .add_systems(OnExit(state.clone()), (pause_bridge_system, hide_circuit_system))
            .add_systems(
                Update,
                (
                    fit_camera_to_circuit_system
                        .after(load_workspace_system)
                        .after(view_navigation_system),
                    workspace_io_system,
                ),
            )
            .add_systems(
                Update,
                (
                    gate_selection_system.before(drag_selected_system),
                    drag_selected_system,
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
            .add_systems(Update, save_on_exit_system);
    }
}

/// Load the default example the first time the simulation state is entered.
fn load_default_circuit_system(
    mut loaded: Local<bool>,
    gates: Query<(), With<GateIdentity>>,
    mut load: MessageWriter<LoadWorkspaceEvent>,
) {
    if *loaded || !gates.is_empty() {
        return;
    }
    *loaded = true;
    let workspace = fixtures::build_64bit_counter_subcircuits();
    load.write(LoadWorkspaceEvent {
        name: workspace.root.name.clone(),
        workspace: Arc::new(workspace),
    });
}

fn resume_bridge_system(mut bridge: ResMut<SimBridge>) {
    bridge.resume();
}

fn pause_bridge_system(mut bridge: ResMut<SimBridge>) {
    bridge.pause();
}

/// Gates and wires.
type CircuitItem = Or<(With<GateIdentity>, With<WireIdentity>)>;

/// The circuit stays in the ECS outside the simulation state; it is only
/// hidden.
fn hide_circuit_system(
    mut items: Query<&mut Visibility, CircuitItem>,
) {
    for mut visibility in &mut items {
        *visibility = Visibility::Hidden;
    }
}

fn show_circuit_system(
    mut items: Query<&mut Visibility, CircuitItem>,
) {
    for mut visibility in &mut items {
        *visibility = Visibility::Visible;
    }
}
