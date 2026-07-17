use crate::{
    GameState, SIMULATION_TICK_RATE,
    sim::systems::{
        draw_connections_system, draw_gates_system, simulation_update_system, ui_for_selected_gate,
        update_connections_system, update_input_colours_system, update_selected_gate,
    },
};
use bevy::prelude::*;

use crate::sim::components::SimulationConfig;

use super::{
    SelectedGate, Simulation, save_on_exit_system, simulation_config_ui_system,
    update_tickrate_system,
};

/// Core Simulation Plugin
///
/// This plugin handles the core simulation logic including:
/// - Managing the simulation state
/// - Processing simulation ticks
/// - Updating visual representations of gates and connections
/// - Handling user interaction with the simulation
pub struct SimulationPlugin;

impl Plugin for SimulationPlugin {
    fn build(&self, app: &mut App) {
        app
            // Resources
            .init_resource::<Simulation>()
            .init_resource::<SimulationConfig>()
            .init_resource::<SelectedGate>()
            // Fixed timestep for simulation updates
            .insert_resource(Time::<Fixed>::from_seconds(SIMULATION_TICK_RATE))
            // Setup systems
            .add_systems(
                OnEnter(GameState::Simulating),
                (
                    draw_gates_system,
                    draw_connections_system.after(draw_gates_system),
                ),
            )
            // Simulation tick system
            .add_systems(
                FixedUpdate,
                simulation_update_system.run_if(in_state(GameState::Simulating)),
            )
            // Update visualization systems
            .add_systems(
                Update,
                (
                    update_input_colours_system,
                    update_connections_system,
                    update_selected_gate,
                    ui_for_selected_gate,
                    simulation_config_ui_system,
                    update_tickrate_system,
                )
                    .run_if(in_state(GameState::Simulating)),
            )
            // Save on exit
            .add_systems(Last, save_on_exit_system);
    }
}
