mod components;
mod plugin;

use bevy::{platform::collections::HashMap, prelude::*};
use bevy_egui::{EguiContexts, EguiPrimaryContextPass, egui};
use serde::{Deserialize, Serialize};
use sim_core::{Circuit, ComponentId, GateId, GateType, LogicComponent};
use spatial_grid::SpatialGrid;
use systems::{
    drag_selected_system, draw_connections_system, draw_gates_system, gate_selection_system,
    rebuild_spatial_grid_system, simulation_update_system, ui_for_selected_gate,
    update_connections_system, update_input_colours_system, update_inputs_on_left_click,
    update_selected_gate, update_spatial_grid_system, update_tickrate_system,
};

use crate::{
    GameState, SAVE_FILE_NAME, SIMULATION_TICK_RATE, misc::add_n_bit_adder,
    temp::GateComponentSelectionEvent,
    zoom_camera,
};

pub mod spatial_grid;
pub mod systems;
/// Visual representation of the circuit component
#[derive(Component, Debug, Clone)]
pub struct GateComponent {
    pub id: GateId,
    pub position: Vec2,
}
#[derive(Component)]
pub struct SimulationEntity;

impl GateComponent {
    pub fn new(id: ComponentId, position: Vec2) -> Self {
        GateComponent { id, position }
    }
}

/// Returns a sprite for the given logic component type
pub fn get_gate_sprite(gate_type: &GateType) -> Sprite {
    let color = match gate_type {
        GateType::And => Color::srgb(0.0, 0.6, 0.3),
        GateType::Or => Color::srgb(1.0, 1.0, 0.0),
        GateType::Not => Color::srgb(0.0, 0.1, 0.5),
        GateType::Input => Color::srgb(0.0, 0.0, 0.0),
        GateType::Output => Color::srgb(0.0, 0.0, 0.0),
        GateType::Circuit => Color::srgb(0.1, 0.5, 0.3),
        GateType::Oscillator => Color::srgb(0.5, 0.1, 0.3),
        _ => Color::WHITE,
    };

    Sprite {
        color,
        custom_size: Some(Vec2::new(50.0, 50.0)),
        ..Default::default()
    }
}

#[derive(Component)]
pub struct ConnectionLine {
    pub from_gate: GateId,
    pub to_gate: GateId,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy)]
pub struct GatePosition {
    pub x: f32,
    pub y: f32,
}

// #[derive(Resource, Default)]
// pub struct GatePositions(pub HashMap<GateId, GatePosition>);

// #[deprecated]
#[derive(Component)]
pub struct Selected {
    pub offset: Vec2,
}

#[derive(Resource, Default)]
pub struct SelectedGate(pub Option<(Entity, GateId)>);

#[derive(Resource)]
pub struct Simulation {
    pub circuit: Circuit,
}

#[derive(Resource)]
pub struct SimulationConfig {
    pub tick_rate: f64,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct SavedSimulation {
    pub circuit: Circuit,
    pub gate_positions: HashMap<GateId, GatePosition>,
}

impl Simulation {
    pub fn new() -> Self {
        let mut outer_circuit = Circuit::new();
        add_n_bit_adder(&mut outer_circuit, 8, 0);
        info!("Circuit has {} gates", outer_circuit.get_gate_count());

        Simulation {
            circuit: outer_circuit,
        }
    }

    // might be good to delegate to async task if we can split stuff up, that way we can simulate multiple circuits at once
    pub fn tick(&mut self) {
        self.circuit.tick().expect("Tick failed in main loop");
    }

    pub fn get_component_by_id_mut(&mut self, id: &ComponentId) -> Option<&mut LogicComponent> {
        self.circuit.components.get_mut(id)
    }
}

impl Default for Simulation {
    fn default() -> Self {
        // if std::path::Path::new(SAVE_FILE_NAME).exists() {
        //     Simulation::from_file(SAVE_FILE_NAME)
        // } else {
        //     Simulation::new()
        // }
        Simulation::new()
    }
}

pub fn simulation_config_ui_system(
    mut sim_config: ResMut<SimulationConfig>,
    mut contexts: EguiContexts,
) -> Result<(), BevyError> {
    egui::Window::new("Simulation Config").show(contexts.ctx_mut()?, |ui| {
        ui.label("Tick Rate (seconds per tick)");
        // Slider range from a small value to, say, 2 seconds per tick.
        ui.add(egui::Slider::new(&mut sim_config.tick_rate, 0.5..=1000.0).text("Tick Rate"));
    });
    Ok(())
}

#[allow(dead_code)]
pub fn save_on_exit_system(
    mut exit_events: MessageReader<AppExit>,
    simulation: Res<Simulation>,
    q_gate_components: Query<&GateComponent>,
) {
    if exit_events.read().next().is_some() {
        println!("Saving simulation...");
        if let Ok(file) = std::fs::File::create(SAVE_FILE_NAME) {
            let mut saved_simulation = SavedSimulation {
                circuit: simulation.circuit.clone(),
                gate_positions: HashMap::new(), // TODO: Save gate positions
            };

            for gate in q_gate_components.iter() {
                saved_simulation.gate_positions.insert(
                    gate.id,
                    GatePosition {
                        x: gate.position.x,
                        y: gate.position.y,
                    },
                );
            }

            if let Err(e) = serde_json::to_writer_pretty(file, &saved_simulation) {
                eprintln!("Failed to save simulation: {e}");
            }
        }
    }
}

#[inline]
fn calculate_connection_geometry(from_pos: &Vec3, to_pos: &Vec3) -> (Vec3, Quat, f32) {
    let mid_point = (from_pos + to_pos) / 2.0;
    let direction = (to_pos - from_pos).normalize_or_zero();
    let distance = (to_pos - from_pos).length();
    let rotation = Quat::from_rotation_arc(Vec3::X, direction);

    (mid_point, rotation, distance)
}

// new mouse system

// sim plugin
pub struct SimulationPlugin {
    pub state: GameState, // The state that this plugin will be active in
}
impl SimulationPlugin {
    pub fn new(state: GameState) -> Self {
        Self { state }
    }
}

fn setup_simulation_resources(mut commands: Commands) {
    // Create the simulation
    let simulation = Simulation::new();

    // Insert all simulation-specific resources
    commands.insert_resource(simulation);
    commands.insert_resource(SimulationConfig {
        tick_rate: SIMULATION_TICK_RATE,
    });
    commands.insert_resource(SelectedGate(None));
    commands.insert_resource(SpatialGrid::new(100.0));
    commands.insert_resource(ClearColor(Color::srgba(0.050, 0.229, 0.793, 1.000)));

    // commands.insert_resource(SimulationTimer {
    //     timer: Timer::from_seconds(SIMULATION_TICK_RATE as f32, TimerMode::Repeating),
    // });

    // // Add the simulation system to the custom schedule
    // commands.add_systems(SimulationSchedule, simulation_update_system);
}

fn cleanup_simulation_resources(
    mut commands: Commands,
    entities: Query<Entity, With<SimulationEntity>>,
) {
    // Remove all entities marked as simulation entities
    for entity in entities.iter() {
        commands.entity(entity).despawn();
    }

    // Remove all simulation-specific resources
    commands.remove_resource::<Simulation>();
    commands.remove_resource::<SimulationConfig>();
    commands.remove_resource::<SelectedGate>();
    commands.remove_resource::<SpatialGrid>();
    commands.remove_resource::<ClearColor>();
    // commands.remove_resource::<SimulationTimer>();
}

impl Plugin for SimulationPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<GateComponentSelectionEvent>()
            // Insert our simulation as a resource
            .init_resource::<Simulation>()
            .insert_resource(SimulationConfig {
                tick_rate: SIMULATION_TICK_RATE,
            })
            .init_resource::<SelectedGate>()
            // TIMESTEP
            .insert_resource(Time::<Fixed>::from_seconds(SIMULATION_TICK_RATE)) // Set the tick rate of the simulation
            .insert_resource(SpatialGrid::new(100.0))
            /* GAME STATE */
            .add_systems(
                OnEnter(self.state.clone()),
                (
                    setup_simulation_resources,
                    draw_gates_system.after(setup_simulation_resources),
                    draw_connections_system.after(draw_gates_system),
                    rebuild_spatial_grid_system.after(setup_simulation_resources),
                ),
            )
            .add_systems(
                EguiPrimaryContextPass,
                simulation_config_ui_system.run_if(in_state(self.state.clone())),
            )
            // Update systems
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
                    // mouse_selection_input,
                    // finalize_selection,
                    zoom_camera,
                    update_tickrate_system,
                    ui_for_selected_gate,
                )
                    .run_if(in_state(self.state.clone())),
            )
            .add_systems(
                FixedUpdate,
                (simulation_update_system).run_if(in_state(self.state.clone())),
            )
            .add_systems(OnExit(self.state.clone()), cleanup_simulation_resources);
    }
}

// Generic function to find entity under mouse cursor
pub fn find_entity_at_position(
    position: Vec2,
    grid: &SpatialGrid,
    query: &Query<(Entity, &Transform), With<GateComponent>>,
) -> Option<(Entity, Vec2)> {
    let nearby_entities = grid.query_near(position, 50.0); // Search radius based on gate size

    for entity in nearby_entities {
        if let Ok((entity, transform)) = query.get(entity) {
            let sprite_size = Vec2::new(50.0, 50.0);
            let half_extents = sprite_size / 2.0;

            let local_point = transform
                .to_matrix()
                .inverse()
                .transform_point3(position.extend(0.0));
            let local_point_2d = Vec2::new(local_point.x, local_point.y);

            if local_point_2d.x >= -half_extents.x
                && local_point_2d.x <= half_extents.x
                && local_point_2d.y >= -half_extents.y
                && local_point_2d.y <= half_extents.y
            {
                let offset = transform.translation.truncate() - position;
                return Some((entity, offset));
            }
        }
    }

    None
}
