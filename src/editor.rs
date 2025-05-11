use bevy::prelude::*;
use bevy::window::PrimaryWindow;
use bevy_egui::{EguiContexts, egui};
use sim_core::{GateId, GateType};

use crate::{
    GameState,
    sim::{GateComponent, Simulation, get_gate_sprite},
    spatial_grid::SpatialGrid,
    temp::GateComponentSelectionEvent,
};

/// Resource to track the editor's state
#[derive(Resource)]
pub struct EditorState {
    /// Whether the editor panel is visible
    pub visible: bool,
    /// The currently selected gate type for placing
    pub selected_gate_type: Option<GateType>,
    /// Whether we're in connection creation mode
    pub connection_mode: bool,
    /// The first gate selected for connection (source)
    pub connection_source: Option<GateId>,
    /// The gate placement queue - gates waiting to be placed
    pub placement_queue: Vec<(GateType, Vec2)>,
}

impl Default for EditorState {
    fn default() -> Self {
        EditorState {
            visible: true,
            selected_gate_type: None,
            connection_mode: false,
            connection_source: None,
            placement_queue: Vec::new(),
        }
    }
}

/// Event for creating a new gate
#[derive(Event)]
pub struct CreateGateEvent {
    pub gate_type: GateType,
    pub position: Vec2,
}

/// Event for creating a new connection
#[derive(Event)]
pub struct CreateConnectionEvent {
    pub source_gate: GateId,
    pub target_gate: GateId,
}

/// Plugin for the editor functionality
pub struct EditorPlugin;

impl Plugin for EditorPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<EditorState>()
            .add_event::<CreateGateEvent>()
            .add_event::<CreateConnectionEvent>()
            .add_systems(
                Update,
                (
                    editor_ui_system,
                    process_gate_placement,
                    handle_connection_creation,
                ).run_if(in_state(GameState::Simulating)),
            );
    }
}

/// System to show the editor UI panel
pub fn editor_ui_system(
    mut contexts: EguiContexts,
    mut editor_state: ResMut<EditorState>,
    mut create_gate_events: EventWriter<CreateGateEvent>,
    q_window: Query<&Window, With<PrimaryWindow>>,
    q_camera: Query<(&Camera, &GlobalTransform)>,
    button_input: Res<ButtonInput<MouseButton>>,
) {
    let ctx = contexts.ctx_mut();

    // Editor panel
    egui::SidePanel::left("editor_panel")
        .resizable(true)
        .show(ctx, |ui| {
            ui.heading("Component Editor");
            ui.separator();

            // Create a toggle button for connection mode
            if ui.button(if editor_state.connection_mode { "✓ Connection Mode" } else { "Connection Mode" })
                .clicked() {
                editor_state.connection_mode = !editor_state.connection_mode;
                if editor_state.connection_mode {
                    editor_state.selected_gate_type = None;
                } else {
                    editor_state.connection_source = None;
                }
            }

            ui.separator();
            ui.heading("Add Components");

            // Gate type buttons
            ui.horizontal(|ui| {
                if ui.button("AND").clicked() {
                    editor_state.selected_gate_type = Some(GateType::And);
                    editor_state.connection_mode = false;
                }
                if ui.button("OR").clicked() {
                    editor_state.selected_gate_type = Some(GateType::Or);
                    editor_state.connection_mode = false;
                }
                if ui.button("NOT").clicked() {
                    editor_state.selected_gate_type = Some(GateType::Not);
                    editor_state.connection_mode = false;
                }
            });

            ui.horizontal(|ui| {
                if ui.button("INPUT").clicked() {
                    editor_state.selected_gate_type = Some(GateType::Input);
                    editor_state.connection_mode = false;
                }
                if ui.button("OUTPUT").clicked() {
                    editor_state.selected_gate_type = Some(GateType::Output);
                    editor_state.connection_mode = false;
                }
                if ui.button("OSCILLATOR").clicked() {
                    editor_state.selected_gate_type = Some(GateType::Oscillator);
                    editor_state.connection_mode = false;
                }
            });

            // Show status of current selection/mode
            ui.separator();
            if let Some(gate_type) = &editor_state.selected_gate_type {
                ui.label(format!("Selected: {:?}", gate_type));
                ui.label("Click in the world to place the component");
            } else if editor_state.connection_mode {
                if let Some(source) = editor_state.connection_source {
                    ui.label(format!("Connection from: {:?}", source));
                    ui.label("Select destination gate to complete connection");
                } else {
                    ui.label("Connection Mode: Select first gate");
                }
            } else {
                ui.label("Select a component type or mode");
            }

            ui.separator();
            ui.label("Tips:");
            ui.label("- Right-click to select and drag components");
            ui.label("- Left-click to toggle inputs and outputs");
            ui.label("- Connect outputs to inputs with Connection Mode");
        });

    // Handle mouse clicks for gate placement
    if let Some(gate_type) = editor_state.selected_gate_type {
        if button_input.just_pressed(MouseButton::Left) {
            if let Some(screen_pos) = q_window.single().cursor_position() {
                let (camera, camera_transform) = q_camera.single();
                if let Ok(ray) = camera.viewport_to_world(camera_transform, screen_pos) {
                    let distance = -ray.origin.z / ray.direction.z;
                    let world_pos = ray.origin + ray.direction * distance;
                    
                    // Only place gates if we're clicking in the world, not on UI
                    if !ctx.is_pointer_over_area() {
                        create_gate_events.send(CreateGateEvent {
                            gate_type,
                            position: world_pos.truncate(),
                        });
                    }
                }
            }
        }
    }
}

/// System to process gate placement
pub fn process_gate_placement(
    mut commands: Commands,
    mut simulation: ResMut<Simulation>,
    mut grid: ResMut<SpatialGrid>,
    mut create_gate_events: EventReader<CreateGateEvent>,
) {
    for event in create_gate_events.read() {
        // Add the gate to the simulation
        let gate_id = match event.gate_type {
            GateType::And => simulation.circuit.add_and(),
            GateType::Or => simulation.circuit.add_or(),
            GateType::Not => simulation.circuit.add_not(),
            GateType::Input => simulation.circuit.add_input(false),
            GateType::Output => simulation.circuit.add_output(),
            GateType::Oscillator => simulation.circuit.add_oscillator(10),
            _ => continue, // Skip unsupported gate types
        };

        // Get the component from simulation to create its visual representation
        if let Some(component) = simulation.circuit.components.get(&gate_id) {
            let sprite = get_gate_sprite(component);
            let position = event.position;
            
            // Spawn the visual entity for the gate
            let entity = commands.spawn((
                sprite,
                Transform::from_xyz(position.x, position.y, 0.0),
                GateComponent::new(gate_id, position),
            )).id();
            
            // Add to spatial grid for selection
            grid.insert(entity, position);
        }
    }
}

/// System to handle connection creation
pub fn handle_connection_creation(
    mut editor_state: ResMut<EditorState>,
    mut simulation: ResMut<Simulation>,
    mut create_connection_events: EventWriter<CreateConnectionEvent>,
    mut selection_events: EventReader<GateComponentSelectionEvent>,
) {
    // Only process in connection mode
    if !editor_state.connection_mode {
        return;
    }

    for event in selection_events.read() {
        if editor_state.connection_source.is_none() {
            // Set the first gate as source
            editor_state.connection_source = Some(event.component.id);
        } else if let Some(source_id) = editor_state.connection_source {
            if source_id != event.component.id {
                // Create connection from source to target
                simulation.circuit.add_connection(
                    source_id,
                    event.component.id,
                    0, // Default to first input
                    0, // Default to first output
                );
                
                // Send event for other systems
                create_connection_events.send(CreateConnectionEvent {
                    source_gate: source_id,
                    target_gate: event.component.id,
                });
                
                // Reset connection state
                editor_state.connection_source = None;
            }
        }
    }
}
