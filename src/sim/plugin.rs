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
    authoring::{
        apply_authoring_messages_system, apply_create_gate_system, apply_create_wire_system,
        apply_delete_system, apply_refresh_definitions_system, apply_register_definitions_system,
        apply_set_input_system, apply_set_kind_system, load_workspace_system,
        sync_gate_names_system, track_authoring_component_changes_system,
    },
    bridge::{
        collect_engine_events_system, drive_local_engine_system, project_probes_system,
        request_screens_system, sync_design_system, update_tickrate_system,
    },
    systems::{
        CanvasCursor, CanvasPointer, canvas_cursor_system, canvas_pointer_system,
        draw_sim_background_system, fit_camera_to_circuit_system, save_on_exit_system,
        simulation_config_ui_system, sync_bench_theme_system, ui_for_selected_gate,
        update_selected_gate, update_spatial_grid_system, workspace_io_system,
    },
    view::{
        CircuitView, CloseView, InstanceViewItem, OpenDefinitionView, OpenInstanceView,
        view_navigation_system, write_back_view_system,
    },
    visuals::SimulationVisualsPlugin,
    wiring::{PinPointer, pin_tooltip_system, wire_drag_visuals_system},
};
use crate::sim::{HistoryCommand, UndoHistory, history::edit_history_system};
use crate::sim::{
    LoadedRoot, MakeChip, MultiSelection, PendingChipUpdates, RefreshDefinitions,
    chip::apply_make_chip_system,
    multi_select::{
        BoxSelect, draw_box_select_system, move_group_system, multi_select_pointer_system,
        prune_multi_selection_system,
    },
};

use super::demos;
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
        Self {
            state,
            runtime: SimRuntime::platform_default(),
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
                runtime: self.runtime,
            },
            SimulationViewPlugin {
                state: self.state.clone(),
            },
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
            .init_resource::<UndoHistory>()
            .init_resource::<LoadedRoot>()
            .init_resource::<PendingChipUpdates>()
            .add_message::<HistoryCommand>()
            .add_message::<MakeChip>()
            .add_message::<RefreshDefinitions>()
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
                (
                    request_screens_system,
                    drive_local_engine_system,
                    collect_engine_events_system,
                    project_probes_system,
                )
                    .chain(),
            )
            .add_systems(
                Update,
                (
                    load_workspace_system,
                    view_navigation_system,
                    apply_register_definitions_system,
                    apply_refresh_definitions_system,
                    apply_create_gate_system,
                    apply_create_wire_system,
                    apply_delete_system,
                    apply_make_chip_system,
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
            // After every edit of the frame, including a view's write-back.
            .add_systems(Update, edit_history_system.after(write_back_view_system))
            .add_systems(PostUpdate, sync_design_system);
    }
}

impl Plugin for SimulationViewPlugin {
    fn build(&self, app: &mut App) {
        let state = self.state.clone();

        app.add_plugins(SimulationVisualsPlugin)
            .init_resource::<SelectedGate>()
            .init_resource::<MultiSelection>()
            .init_resource::<BoxSelect>()
            .init_resource::<CanvasCursor>()
            .init_resource::<CanvasPointer>()
            .init_resource::<PinPointer>()
            .add_message::<crate::camera::FitView>()
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
            .init_resource::<RunOnReturn>()
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
            .add_systems(
                OnExit(state.clone()),
                (
                    leave_bridge_system,
                    hide_circuit_system,
                    drop_canvas_pointer_system,
                ),
            )
            .add_systems(
                Update,
                (
                    fit_camera_to_circuit_system
                        .after(load_workspace_system)
                        .after(view_navigation_system),
                    crate::camera::track_canvas_zoom_system,
                    workspace_io_system,
                    sync_bench_theme_system,
                ),
            )
            .add_systems(
                Update,
                (
                    (canvas_cursor_system, canvas_pointer_system).chain(),
                    update_spatial_grid_system,
                    update_selected_gate,
                    (multi_select_pointer_system, prune_multi_selection_system)
                        .chain()
                        .after(update_selected_gate)
                        .after(canvas_pointer_system),
                    move_group_system.after(canvas_pointer_system),
                    draw_box_select_system,
                )
                    .run_if(in_state(state.clone())),
            )
            // Also outside the state: it hides the marks once the pointer
            // is dropped on leaving.
            .add_systems(
                Update,
                wire_drag_visuals_system.after(canvas_pointer_system),
            )
            .add_systems(
                bevy_egui::EguiPrimaryContextPass,
                (
                    simulation_config_ui_system,
                    ui_for_selected_gate,
                    pin_tooltip_system.run_if(in_state(state.clone())),
                ),
            )
            .add_systems(Last, save_on_exit_system.after(bevy::window::ExitSystems));
    }
}

/// Demo opened the first time the simulation state is entered.
pub const DEFAULT_DEMO: &str = "counter";

/// Open the default demo (read-only) the first time the simulation state is
/// entered, unless something else is being opened (a demo picked on the
/// demo map).
fn load_default_circuit_system(
    mut loaded: Local<bool>,
    gates: Query<(), With<GateIdentity>>,
    // Reading and writing the same message in one system needs a
    // `ParamSet`: Bevy refuses the two as conflicting parameters.
    mut messages: ParamSet<(
        MessageReader<LoadWorkspaceEvent>,
        MessageWriter<LoadWorkspaceEvent>,
    )>,
) {
    if *loaded || !gates.is_empty() {
        return;
    }
    *loaded = true;
    if !messages.p0().is_empty() {
        messages.p0().clear();
        return;
    }
    match demos::catalog() {
        Ok(catalog) => {
            if let Some(demo) = catalog.get(DEFAULT_DEMO) {
                messages.p1().write(LoadWorkspaceEvent {
                    name: demo.title.clone(),
                    workspace: Arc::clone(&demo.workspace),
                    read_only: true,
                    unsaved: false,
                });
            }
        }
        Err(e) => error!("Cannot load the built-in demos: {e}"),
    }
}

/// Whether the simulation was running when the simulation state was left
/// (for the demo map, the title menu), so coming back keeps a paused
/// circuit paused.
#[derive(Resource)]
struct RunOnReturn(bool);

impl Default for RunOnReturn {
    fn default() -> Self {
        Self(true)
    }
}

fn resume_bridge_system(mut bridge: ResMut<SimBridge>, run: Res<RunOnReturn>) {
    if run.0 {
        bridge.resume();
    }
}

fn leave_bridge_system(mut bridge: ResMut<SimBridge>, mut run: ResMut<RunOnReturn>) {
    run.0 = bridge.is_running();
    bridge.pause();
}

fn pause_bridge_system(mut bridge: ResMut<SimBridge>) {
    bridge.pause();
}

/// Gates and wires.
type CircuitItem = Or<(With<GateIdentity>, With<WireIdentity>)>;

/// The circuit stays in the ECS outside the simulation state; it is only
/// hidden.
/// Leaving the canvas drops any press in progress (a wire in hand, a part
/// being moved) and the hovered pin.
fn drop_canvas_pointer_system(mut pointer: ResMut<CanvasPointer>, mut pins: ResMut<PinPointer>) {
    pointer.press = None;
    pins.set_if_neq(PinPointer::default());
}

fn hide_circuit_system(mut items: Query<&mut Visibility, CircuitItem>) {
    for mut visibility in &mut items {
        *visibility = Visibility::Hidden;
    }
}

fn show_circuit_system(mut items: Query<&mut Visibility, CircuitItem>) {
    for mut visibility in &mut items {
        *visibility = Visibility::Visible;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> App {
        let mut app = App::new();
        app.add_message::<LoadWorkspaceEvent>()
            .add_systems(Update, load_default_circuit_system);
        app
    }

    fn loads(app: &App) -> Vec<String> {
        let messages = app.world().resource::<Messages<LoadWorkspaceEvent>>();
        messages
            .iter_current_update_messages()
            .map(|m| m.name.clone())
            .collect()
    }

    /// The system's parameters are valid (Bevy panics on its first run if
    /// they conflict), and with nothing pending it opens the default demo.
    #[test]
    fn default_circuit_loads_when_nothing_is_pending() {
        let mut app = app();
        app.update();
        let title = demos::catalog()
            .unwrap()
            .get(DEFAULT_DEMO)
            .unwrap()
            .title
            .clone();
        assert_eq!(loads(&app), vec![title]);
        app.update();
        assert!(loads(&app).is_empty(), "only the first time");
    }

    /// A demo picked on the map is already on its way: no default demo.
    #[test]
    fn a_pending_load_wins_over_the_default_circuit() {
        let mut app = app();
        let workspace = Arc::clone(&demos::catalog().unwrap().get("gates").unwrap().workspace);
        app.world_mut().write_message(LoadWorkspaceEvent {
            name: "picked".into(),
            workspace,
            read_only: true,
            unsaved: false,
        });
        app.update();
        // The picked load was written before this update; the system wrote
        // nothing of its own.
        assert!(loads(&app).is_empty(), "{:?}", loads(&app));
    }
}
