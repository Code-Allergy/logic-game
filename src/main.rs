use bevy::input::mouse::MouseButtonInput;
use bevy::prelude::*;
use bevy::window::PrimaryWindow;
use sim_core::circuits::*;
use sim_core::gates::*;

mod systems;
use systems::*;

const SIMULATION_TICK_RATE: f64 = 1.0; // 1 s

pub const CONNECTION_Z_LEVEL: f32 = 10.0;

#[derive(Resource)]
pub struct Simulation {
    circuit: Circuit,
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
        circuit.add_gate(Gate::new_input(GateId(0), Some(false)));
        circuit.add_gate(Gate::new_output(GateId(1)));
        circuit.add_gate(Gate::new_or(GateId(2)));
        circuit.add_gate(Gate::new_input(GateId(3), Some(false)));
        circuit.add_connection(GateId(0), GateId(2), 0);
        circuit.add_connection(GateId(3), GateId(2), 1);
        circuit.add_connection(GateId(2), GateId(1), 0);
        Simulation { circuit }
    }

    // might be good to delegate to async task if we can split stuff up, that way we can simulate multiple circuits at once
    fn tick(&mut self) {
        self.circuit.tick().expect("Tick failed in main loop");
    }
}

fn main() {
    App::new()
        // Add Bevy's default plugins
        .add_plugins(DefaultPlugins)
        // Insert our simulation as a resource
        .insert_resource(Simulation::new())
        // Register startup and update systems
        .add_systems(
            Startup,
            (setup_system, draw_gates_system, draw_connections_system).chain(),
        )
        .add_systems(FixedUpdate, simulation_update_system)
        .insert_resource(Time::<Fixed>::from_seconds(SIMULATION_TICK_RATE)) // Set the tick rate of the simulation
        // You can also add rendering systems for visualizing your circuit, if needed.
        .add_systems(
            Update,
            (
                interaction_system,
                drag_selected_system,
                update_connections_system,
            )
                .chain(),
        )
        .run();
}
