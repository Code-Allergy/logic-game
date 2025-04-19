use bevy::{prelude::*, utils::HashMap};
use bevy_egui::{EguiContexts, egui};
use sim_core::{
    Circuit, CircuitTemplate, GateId, GateType, LogicComponent,
    circuits::builtin::{FullAdderTemplate, InputOutputCircuitTemplate},
};

use serde::{Deserialize, Serialize};

use crate::SAVE_FILE_NAME;

// Visual representation of the circuit component
#[derive(Component, Debug, Clone, Copy)]
pub struct GateComponent {
    pub id: GateId,
    pub position: Vec2,
}

impl GateComponent {
    pub fn new(id: GateId, position: Vec2) -> Self {
        GateComponent { id, position }
    }
}

/// Returns a sprite for the given logic component type
pub fn get_gate_sprite(lc: &LogicComponent) -> Sprite {
    let color = match lc.get_type() {
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

#[derive(Resource, Default)]
pub struct GatePositions(pub HashMap<GateId, GatePosition>);

#[deprecated]
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
        let circuit = InputOutputCircuitTemplate::template();
        // let mut circuit = Circuit::new();
        // add_n_bit_adder(&mut outer_circuit, 2, 0, 0);
        // // let input_id = outer_circuit.add_oscillator(10);
        let input_id = outer_circuit.add_input(false);
        let output_id = outer_circuit.add_output_pin(0);
        let target = circuit.id();

        outer_circuit.add_component(LogicComponent::Circuit(circuit));

        outer_circuit.add_connection(input_id, target, 0, 0);
        outer_circuit.add_connection(target, output_id, 0, 0);
        dbg!(&outer_circuit);
        Simulation {
            circuit: outer_circuit,
        }
    }

    // might be good to delegate to async task if we can split stuff up, that way we can simulate multiple circuits at once
    pub fn tick(&mut self) {
        self.circuit.tick().expect("Tick failed in main loop");
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
fn add_n_bit_adder(circuit: &mut Circuit, n: usize, input_offset: usize, output_offset: usize) {
    // Inputs: A0..An-1, B0..Bn-1, Cin
    let mut a = Vec::with_capacity(n);
    let mut b = Vec::with_capacity(n);
    for i in 0..n {
        a.push(circuit.add_input_pin(input_offset + i));
        b.push(circuit.add_input_pin(input_offset + n + i));
    }
    let cin = circuit.add_input_pin(input_offset + 2 * n);

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
        circuit.add_component(LogicComponent::Circuit(fa));
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

pub fn simulation_config_ui_system(
    mut sim_config: ResMut<SimulationConfig>,
    mut contexts: EguiContexts,
) {
    egui::Window::new("Simulation Config").show(contexts.ctx_mut(), |ui| {
        ui.label("Tick Rate (seconds per tick)");
        // Slider range from a small value to, say, 2 seconds per tick.
        ui.add(egui::Slider::new(&mut sim_config.tick_rate, 0.001..=2.0).text("Tick Rate"));
    });
}

pub fn update_tickrate_system(sim_config: Res<SimulationConfig>, mut time: ResMut<Time<Fixed>>) {
    if sim_config.is_changed() {
        time.set_timestep_seconds(sim_config.tick_rate);
    }
}

pub fn save_on_exit_system(
    mut exit_events: EventReader<AppExit>,
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
