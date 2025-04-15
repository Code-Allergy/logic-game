use bevy::prelude::*;

use bevy_egui::EguiPlugin;
use bevy_egui::{EguiContexts, egui};
use sim_core::CircuitTemplate;
use sim_core::circuits::builtin::FullAdderTemplate;
use sim_core::{Circuit, GateId, LogicComponent};

mod systems;
use systems::*;

const SIMULATION_TICK_RATE: f64 = 0.1; // 1 s

pub const CONNECTION_Z_LEVEL: f32 = 10.0;

#[derive(Resource)]
pub struct Simulation {
    circuit: Circuit,
}

#[derive(Resource)]
pub struct GameConfig {}

#[derive(Resource)]
pub struct SimulationConfig {
    pub tick_rate: f64,
}

#[derive(Component)]
struct MainCamera;

#[derive(Component)]
struct GateComponent {
    id: GateId,
    position: Vec2,
}

#[derive(Component)]
pub struct ConnectionLine {
    from_gate: GateId,
    to_gate: GateId,
}

#[derive(Component)]
struct Selected {
    offset: Vec2,
}

impl Simulation {
    fn new() -> Self {
        let mut circuit = FullAdderTemplate::template();
        circuit.set_input(0, Some(true));
        circuit.set_input(1, Some(false));
        circuit.set_input(2, Some(false));
        Simulation { circuit }
    }

    // might be good to delegate to async task if we can split stuff up, that way we can simulate multiple circuits at once
    fn tick(&mut self) {
        self.circuit.tick().expect("Tick failed in main loop");
    }
}

fn simulation_config_ui_system(
    mut sim_config: ResMut<SimulationConfig>,
    mut contexts: EguiContexts,
) {
    egui::Window::new("Simulation Config").show(contexts.ctx_mut(), |ui| {
        ui.label("Tick Rate (seconds per tick)");
        // Slider range from a small value to, say, 2 seconds per tick.
        ui.add(egui::Slider::new(&mut sim_config.tick_rate, 0.001..=2.0).text("Tick Rate"));
    });
}

fn update_tickrate_system(sim_config: ResMut<SimulationConfig>, mut time: ResMut<Time<Fixed>>) {
    // Update the tick rate of the simulation
    time.set_timestep_seconds(sim_config.tick_rate);
}

fn main() {
    App::new()
        // Add Bevy's default plugins
        .add_plugins(DefaultPlugins)
        .add_plugins(EguiPlugin)
        // Insert our simulation as a resource
        .insert_resource(Simulation::new())
        .insert_resource(GameConfig {})
        .insert_resource(SimulationConfig {
            tick_rate: SIMULATION_TICK_RATE,
        })
        .insert_resource(SpatialGrid::new(100.0))
        .insert_resource(Time::<Fixed>::from_seconds(SIMULATION_TICK_RATE)) // Set the tick rate of the simulation
        // Startup and update systems
        .add_systems(
            Startup,
            (
                setup_system,
                draw_gates_system,
                draw_connections_system,
                rebuild_spatial_grid_system,
            )
                .chain(),
        )
        .add_systems(FixedUpdate, simulation_update_system)
        // Update systems
        .add_systems(
            Update,
            (
                update_input_colours_system,
                (gate_selection_system, drag_selected_system).chain(),
                update_connections_system,
                update_spatial_grid_system,
                update_inputs_on_left_click,
                simulation_config_ui_system,
                update_tickrate_system,
            ),
        )
        .run();
}
