use bevy::{prelude::*, utils::HashMap, window::PrimaryWindow};
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

pub fn simulation_config_ui_system(
    mut sim_config: ResMut<SimulationConfig>,
    mut contexts: EguiContexts,
) {
    egui::Window::new("Simulation Config").show(contexts.ctx_mut(), |ui| {
        ui.label("Tick Rate (seconds per tick)");
        // Slider range from a small value to, say, 2 seconds per tick.
        ui.add(egui::Slider::new(&mut sim_config.tick_rate, 0.5..=1000.0).text("Tick Rate"));
    });
}

pub fn update_tickrate_system(sim_config: Res<SimulationConfig>, mut time: ResMut<Time<Fixed>>) {
    if sim_config.is_changed() {
        time.set_timestep_seconds(1.0 / sim_config.tick_rate);
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

// new mouse system

#[derive(Resource, Default)]
pub struct SelectionState {
    pub drag_start: Option<Vec2>,
    pub drag_current: Option<Vec2>,
}
pub fn mouse_selection_input(
    mouse_input: Res<ButtonInput<MouseButton>>,
    mut cursor_moved: EventReader<CursorMoved>,
    mut sel: ResMut<SelectionState>,
) {
    // 1) Update the latest cursor position every frame
    let mut latest_pos = None;
    for ev in cursor_moved.read() {
        latest_pos = Some(ev.position);
    }
    let pos = if let Some(p) = latest_pos { p } else { return };

    // 2) On right‑button press, record drag start
    if mouse_input.just_pressed(MouseButton::Left) {
        info!("Right mouse button pressed at {:?}", pos);
        sel.drag_start = Some(pos);
        sel.drag_current = Some(pos);
    }
    // 3) While held, update drag_current
    else if mouse_input.pressed(MouseButton::Left) {
        info!("Right mouse button held at {:?}", pos);
        if sel.drag_start.is_some() {
            sel.drag_current = Some(pos);
        }
    }
    // 4) On release, leave detection to a separate system
    else if mouse_input.just_released(MouseButton::Left) {
        info!("Right mouse button released at {:?}", pos);
        // keep sel.drag_start/current for finalize_selection()
    }
}

pub fn finalize_selection(
    mut sel: ResMut<SelectionState>,
    camera_q: Query<(&Camera, &GlobalTransform)>,
    windows: Query<&Window, With<PrimaryWindow>>,
    // mut select_writer: EventWriter<ComponentsSelected>,
    // mut unselect_writer: EventWriter<ComponentsUnselected>,
    query: Query<(Entity, &GlobalTransform), With<GateComponent>>,
) {
    // Only act on release
    if sel.drag_start.is_none() || sel.drag_current.is_none() {
        return;
    }

    let (camera, cam_tf) = camera_q.single();
    let window = windows.single();

    // Build screen‑space rectangle
    let a = sel.drag_start.unwrap();
    let b = sel.drag_current.unwrap();
    let min = Vec2::new(a.x.min(b.x), a.y.min(b.y));
    let max = Vec2::new(a.x.max(b.x), a.y.max(b.y));

    // Gather entities inside the rectangle
    let mut selected = Vec::new();
    for (ent, tf) in query.iter() {
        // project world → viewport coords
        if let Ok(screen_pos) = camera.world_to_viewport(cam_tf, tf.translation()) {
            if screen_pos.x >= min.x
                && screen_pos.x <= max.x
                && screen_pos.y >= min.y
                && screen_pos.y <= max.y
            {
                selected.push(ent);
            }
        }
    }
    dbg!(&selected);

    // // Emit your selection events
    // select_writer.send(ComponentsSelected {
    //     entities: selected.clone(),
    // });
    // // Optionally compute and send unselected if you track previous selection...
    // unselect_writer.send(ComponentsUnselected { entities: vec![] });

    // Clear the drag state
    sel.drag_start = None;
    sel.drag_current = None;
}

// pub struct SelectionPlugin;

// impl Plugin for SelectionPlugin {
//     fn build(&self, app: &mut App) {
//         app //.add_event::<ComponentsSelected>()
//             //.add_event::<ComponentsUnselected>()
//             .init_resource::<>()
//             .add_systems(Update)
//             .add_systems(Update);
//     }
// }
