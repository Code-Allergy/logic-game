use bevy::input::mouse::MouseButtonInput;
use bevy::prelude::*;
use bevy::window::PrimaryWindow;
use bevy_egui::{egui, EguiContexts};
use bevy_egui::{EguiContext, EguiPlugin};
use sim_core::circuits::*;
use sim_core::gates::*;

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
        let mut circuit = Circuit::new();

        // Create input gates for data and select
        let d0_gate = Gate::new_input(GateId(0), Some(false)); // Data input D0
        let d1_gate = Gate::new_input(GateId(1), Some(false)); // Data input D1
        let s_gate = Gate::new_input(GateId(2), Some(false)); // Select input S

        // Create a NOT gate to invert S
        let not_gate = Gate::new_not(GateId(3));

        // Create two AND gates:
        // one for (NOT S AND D0) and one for (S AND D1)
        let and_gate_left = Gate::new_and(GateId(4)); // (NOT S) AND D0
        let and_gate_right = Gate::new_and(GateId(5)); // S AND D1

        // Create an OR gate to combine the AND outputs
        let or_gate = Gate::new_or(GateId(6));

        // Create an output gate to observe the final output.
        let output_gate = Gate::new_output(GateId(7));

        // Add all the gates to the circuit
        circuit.add_gate(d0_gate);
        circuit.add_gate(d1_gate);
        circuit.add_gate(s_gate);
        circuit.add_gate(not_gate);
        circuit.add_gate(and_gate_left);
        circuit.add_gate(and_gate_right);
        circuit.add_gate(or_gate);
        circuit.add_gate(output_gate);

        // Build the multiplexer connections:
        //
        // Invert the select signal:
        circuit.add_connection(GateId(2), GateId(3), 0); // S -> NOT

        // Left AND gate for (NOT S AND D0)
        circuit.add_connection(GateId(0), GateId(4), 0); // D0 -> left AND input 0
        circuit.add_connection(GateId(3), GateId(4), 1); // NOT S -> left AND input 1

        // Right AND gate for (S AND D1)
        circuit.add_connection(GateId(2), GateId(5), 0); // S -> right AND input 0
        circuit.add_connection(GateId(1), GateId(5), 1); // D1 -> right AND input 1

        // OR gate combines both AND outputs:
        circuit.add_connection(GateId(4), GateId(6), 0); // left AND -> OR input 0
        circuit.add_connection(GateId(5), GateId(6), 1); // right AND -> OR input 1

        // Connect OR output to the final output gate
        circuit.add_connection(GateId(6), GateId(7), 0);
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

fn update_tickrate_system(mut sim_config: ResMut<SimulationConfig>, mut time: ResMut<Time<Fixed>>) {
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
