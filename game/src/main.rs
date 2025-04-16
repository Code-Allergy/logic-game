use bevy::prelude::*;

use bevy_egui::EguiPlugin;
use bevy_egui::egui::output;
use bevy_egui::{EguiContexts, egui};
use sim_core::CircuitTemplate;
use sim_core::circuits::builtin::{FullAdderTemplate, MultiplexerTemplate};
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

fn add_n_bit_adder(circuit: &mut Circuit, n: usize, input_offset: usize, output_offset: usize) {
    // Inputs: A0..An-1, B0..Bn-1, Cin
    let mut a = Vec::with_capacity(n);
    let mut b = Vec::with_capacity(n);
    for i in 0..n {
        a.push(circuit.add_input_pin(Some(false), input_offset + i));
        b.push(circuit.add_input_pin(Some(false), input_offset + n + i));
    }
    let cin = circuit.add_input_pin(Some(false), input_offset + 2 * n);

    // Outputs: S0..Sn-1, Cout
    let mut s = Vec::with_capacity(n);
    for i in 0..n {
        s.push(circuit.add_output_pin(output_offset + i));
    }
    let cout = circuit.add_output_pin(output_offset + n);

    // Add full adders
    let mut fa_ids = Vec::with_capacity(n);
    for _ in 0..n {
        let fa = FullAdderTemplate::template();
        let fa_id = fa.id();
        circuit.add_component(Box::new(fa));
        fa_ids.push(fa_id);
    }

    // Wire up the first adder
    circuit.add_connection(a[0], fa_ids[0], 0, 0); // A0 -> FA0.A
    circuit.add_connection(b[0], fa_ids[0], 1, 0); // B0 -> FA0.B
    circuit.add_connection(cin, fa_ids[0], 2, 0); // Cin -> FA0.Cin
    circuit.add_connection(fa_ids[0], s[0], 0, 0); // FA0.Sum -> S0

    // Wire up the rest
    for i in 1..n {
        circuit.add_connection(a[i], fa_ids[i], 0, 0); // Ai -> FAi.A
        circuit.add_connection(b[i], fa_ids[i], 1, 0); // Bi -> FAi.B
        circuit.add_connection(fa_ids[i - 1], fa_ids[i], 2, 1); // FA(i-1).Cout -> FAi.Cin
        circuit.add_connection(fa_ids[i], s[i], 0, 0); // FAi.Sum -> Si
    }

    // Final carry out
    circuit.add_connection(fa_ids[n - 1], cout, 0, 1); // FA(n-1).Cout -> Cout
}

impl Simulation {
    fn new() -> Self {
        let mut outer_circuit = Circuit::new();
        add_n_bit_adder(&mut outer_circuit, 8, 0, 0);
        Simulation {
            circuit: outer_circuit,
        }
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
