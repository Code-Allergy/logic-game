use bevy::prelude::*;
use bevy_egui::{EguiContexts, egui};
use serde::{Deserialize, Serialize};
use sim_core::{Circuit, GateId, GateType, LogicComponent};
use std::collections::HashMap;

use crate::{SAVE_FILE_NAME, misc::add_n_bit_adder};

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

#[derive(Resource, Debug)]
pub struct SimulationConfig {
    pub tick_rate: f64,
    pub unlimited_tick_rate: bool,
}

impl Default for SimulationConfig {
    fn default() -> Self {
        Self {
            tick_rate: 10.0, // 10 Hz by default
            unlimited_tick_rate: false,
        }
    }
}

#[derive(Serialize, Deserialize, Debug)]
pub struct SavedSimulation {
    pub circuit: Circuit,
    pub gate_positions: HashMap<GateId, GatePosition>,
}

impl Simulation {
    pub fn new() -> Self {
        let mut outer_circuit = Circuit::new();
        add_n_bit_adder(&mut outer_circuit, 4, 0);
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

// Systems related to simulation resources
pub fn simulation_config_ui_system(
    mut sim_config: ResMut<SimulationConfig>,
    mut contexts: EguiContexts,
) -> Result {
    egui::Window::new("Simulation Config").show(contexts.ctx_mut()?, |ui| {
        ui.label("Tick Rate (Hz)");
        // Slider range from 0.1 to 1000 Hz, preventing zero or negative values
        ui.add(egui::Slider::new(&mut sim_config.tick_rate, 0.1..=1000.0).text("Tick Rate"));

        // Add some helpful explanation
        ui.label("Higher values = faster simulation");
    });
    Ok(())
}

pub fn update_tickrate_system(sim_config: Res<SimulationConfig>, mut time: ResMut<Time<Fixed>>) {
    if sim_config.is_changed() {
        // Ensure we have a valid positive tick rate to avoid division by zero
        // and negative values
        let tick_rate = sim_config.tick_rate.max(0.001); // Minimum of 0.001, preventing division by zero
        time.set_timestep_seconds(1.0 / tick_rate);
    }
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
