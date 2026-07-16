use bevy::{prelude::*, window::PrimaryWindow};
use bevy_egui::{EguiContexts, egui};
use sim_core::{GateId, GateType};

use crate::temp::{CONNECTION_Z_LEVEL, GateComponentSelectionEvent};

use super::{
    ConnectionLine, GateComponent, Selected, SelectedGate, Simulation, SimulationConfig,
    calculate_connection_geometry, find_entity_at_position, get_gate_sprite,
    spatial_grid::SpatialGrid,
};

// Initialization

// System to initialize or rebuild the grid
pub fn rebuild_spatial_grid_system(
    mut grid: ResMut<SpatialGrid>,
    query: Query<(Entity, &Transform, &GateComponent)>,
) {
    grid.clear();
    for (entity, transform, _) in query.iter() {
        grid.insert(entity, transform.translation.truncate());
    }
}
/// Initialize the simulation gate visual components
pub fn draw_gates_system(mut simulation: ResMut<Simulation>, mut commands: Commands) {
    for (_, gate) in simulation.circuit.components.iter_mut() {
        // let x = 50.0 + offset * 100.0; // Adjust the x position based on the index
        let x = 50.0;
        let y = 0.0;
        let sprite = get_gate_sprite(&gate.get_type());

        commands.spawn((
            sprite,
            Transform::from_xyz(x, y, 0.0),
            GateComponent::new(gate.id(), Vec2::new(x, y)),
        ));
    }
}

// This system runs at startup to create the initial connection lines
pub fn draw_connections_system(
    simulation: Res<Simulation>,
    gate_query: Query<(&Transform, &GateComponent)>,
    mut commands: Commands,
) {
    // Create a mapping of GateId to world position
    let mut gate_positions: std::collections::HashMap<GateId, Vec3> =
        std::collections::HashMap::new();

    // First pass: collect all gate positions
    for (transform, gate_component) in gate_query.iter() {
        gate_positions.insert(gate_component.id, transform.translation);
    }

    // Second pass: create connection lines for each connection in the circuit
    for connection in simulation.circuit.connections.iter() {
        if let (Some(from_pos), Some(to_pos)) = (
            gate_positions.get(&connection.source),
            gate_positions.get(&connection.target),
        ) {
            let (mid_point, rotation, distance) = calculate_connection_geometry(from_pos, to_pos);

            // Create a line entity
            commands.spawn((
                Sprite {
                    color: Color::WHITE,
                    custom_size: Some(Vec2::new(distance, 2.0)), // Line with thickness of 2 pixels
                    ..Default::default()
                },
                Transform {
                    translation: mid_point,
                    rotation,
                    scale: Vec3::ONE,
                },
                ConnectionLine {
                    from_gate: connection.source,
                    to_gate: connection.target,
                },
            ));
        }
    }
}

pub fn update_tickrate_system(sim_config: Res<SimulationConfig>, mut time: ResMut<Time<Fixed>>) {
    if sim_config.is_changed() {
        time.set_timestep_seconds(1.0 / sim_config.tick_rate);
    }
}

#[allow(clippy::type_complexity)]
pub fn update_connections_system(
    mut param_set: ParamSet<(
        Query<(&Transform, &GateComponent)>,
        Query<(&mut Transform, &mut Sprite, &ConnectionLine)>,
    )>,
) {
    // Create a mapping of GateId to world position
    let mut gate_positions: std::collections::HashMap<GateId, Vec3> =
        std::collections::HashMap::new();

    // Collect all gate positions using the first query in the param set
    for (transform, gate_component) in param_set.p0().iter() {
        gate_positions.insert(gate_component.id, transform.translation);
    }

    // Update each connection line using the second query in the param set
    for (mut transform, mut sprite, connection) in param_set.p1().iter_mut() {
        if let (Some(from_pos), Some(to_pos)) = (
            gate_positions.get(&connection.from_gate),
            gate_positions.get(&connection.to_gate),
        ) {
            // Calculate the mid point between the two gates
            let mid_point = (*from_pos + *to_pos) / 2.0;

            // Calculate the direction vector
            let direction = (*to_pos - *from_pos).normalize();

            // Calculate the distance between gates
            let distance = (*to_pos - *from_pos).length();

            // Update line position, rotation, and size
            transform.translation.x = mid_point.x;
            transform.translation.y = mid_point.y;
            transform.translation.z = CONNECTION_Z_LEVEL;
            transform.rotation = Quat::from_rotation_arc(Vec3::X, direction);
            sprite.custom_size = Some(Vec2::new(distance, 2.0));
        }
    }
}

pub fn update_inputs_on_left_click(
    q_window: Query<&Window, With<PrimaryWindow>>,
    mouse_button_input: Res<ButtonInput<MouseButton>>,
    q_camera: Query<(&Camera, &GlobalTransform)>,
    mut query: Query<(Entity, &Transform, &GateComponent)>,
    mut select_writer: MessageWriter<GateComponentSelectionEvent>,
) {
    // TODO 0.16 BEVY
    let (camera, camera_transform) = q_camera.single().unwrap();
    // TODO 0.16 BEVY
    let window = q_window.single().unwrap();

    // Handle selection if mouse is clicked
    if mouse_button_input.just_pressed(MouseButton::Left) {
        if let Some(screen_pos) = window.cursor_position() {
            // Then try to select a new entity
            if let Ok(ray) = camera.viewport_to_world(camera_transform, screen_pos) {
                let distance = -ray.origin.z / ray.direction.z;
                let world_pos = ray.origin + ray.direction * distance;

                for (entity, transform, gate_component) in query.iter_mut() {
                    let sprite_size = Vec2::new(50.0, 50.0);
                    let half_extents = sprite_size / 2.0;

                    let local_point = transform.to_matrix().inverse().transform_point3(world_pos);
                    let local_point_2d = Vec2::new(local_point.x, local_point.y);

                    if local_point_2d.x >= -half_extents.x
                        && local_point_2d.x <= half_extents.x
                        && local_point_2d.y >= -half_extents.y
                        && local_point_2d.y <= half_extents.y
                    {
                        select_writer.write(GateComponentSelectionEvent {
                            entity,
                            component: gate_component.clone(),
                        });
                        break; // Only select one entity
                    }
                }
            }
        }
    }
}

pub fn update_selected_gate(
    mut event_reader: MessageReader<GateComponentSelectionEvent>,
    mut selected_gate: ResMut<SelectedGate>,
) {
    // Check if the event is for a gate selection
    for event in event_reader.read() {
        // If the selected gate is different from the one in the event, update it
        if selected_gate.0 != Some((event.entity, event.component.id)) {
            selected_gate.0 = Some((event.entity, event.component.id));
            info!("Selected gate: {:?}", event.component.id);
        }
    }
}

pub fn ui_for_selected_gate(
    mut contexts: EguiContexts,
    mut sel: ResMut<SelectedGate>,
    mut simulation: ResMut<Simulation>,
) -> Result<(), BevyError> {
    let ctx = contexts.ctx_mut()?;

    if let Some((_entity, gate_id)) = sel.0 {
        // Grab the gate so you can show/edit its data
        if let Some(gate) = simulation.get_component_by_id_mut(&gate_id) {
            // if let Some(gate) = simulation.circuit.components.get_mut(&gate_id) {
            egui::Window::new("Gate Properties")
                .collapsible(false)
                .show(ctx, |ui| {
                    ui.label(format!("Gate ID: {:?}", gate_id));
                    ui.label(format!("Gate Type: {:?}", gate.get_type()));
                    // if it’s an input gate, show a toggle:
                    if let GateType::Input = gate.get_type() {
                        let mut value = gate.get_input(0).unwrap_or(false);
                        if ui.checkbox(&mut value, "Input Value").clicked() {
                            gate.set_input(0, Some(value));
                        }
                    }
                    // connections
                    ui.horizontal(|ui| {
                        ui.label("Connections:");
                        // ui.label(format!("{:?}", gate.get_connections()));
                    });
                    // ... other gate fields, sliders, etc.

                    if ui.button("Close").clicked() {
                        sel.0 = None;
                    }
                });
        } else {
            // gate went away?
            sel.0 = None;
        }
    }
    Ok(())
}

pub fn update_spatial_grid_system(
    mut grid: ResMut<SpatialGrid>,
    query: Query<(Entity, &Transform, &GateComponent), Changed<Transform>>,
) {
    for (entity, transform, gate) in query.iter() {
        grid.update(entity, gate.position, transform.translation.truncate());
    }
}

pub fn update_input_colours_system(
    simulation: Res<Simulation>,
    mut query: Query<(&mut Sprite, &GateComponent)>,
) {
    for (mut sprite, gate_component) in query.iter_mut() {
        // Get the gate from the simulation
        if let Some(component) = simulation.circuit.components.get(&gate_component.id) {
            // Check if the gate is an input
            if component.get_type() == GateType::Input {
                // Update the color based on the gate's input value
                let color = if component.get_input(0).unwrap_or(false) {
                    Color::srgb(0.0, 255.0, 0.0)
                } else {
                    Color::srgb(255.0, 0.0, 0.0)
                };
                sprite.color = color;
            } else if component.get_type() == GateType::Output {
                // Update the color based on the gate's output value
                let color = if component.get_output(0).unwrap_or(false) {
                    Color::srgb(255.0, 255.0, 255.0)
                } else {
                    Color::srgb(0.0, 0.0, 0.0)
                };
                sprite.color = color;
            }
        }
    }
}

pub fn gate_selection_system(
    q_window: Query<&Window, With<PrimaryWindow>>,
    q_camera: Query<(&Camera, &GlobalTransform)>,

    grid: Res<SpatialGrid>,
    mouse_button_input: Res<ButtonInput<MouseButton>>,
    query: Query<(Entity, &Transform), With<GateComponent>>,
    mut commands: Commands,
    selected_query: Query<Entity, With<Selected>>,
) {
    if mouse_button_input.just_pressed(MouseButton::Right) {
        // Remove any existing selections as right click was pressed
        for selected_entity in selected_query.iter() {
            commands.entity(selected_entity).remove::<Selected>();
        }

        // TODO 0.16
        if let Some(screen_pos) = q_window.single().unwrap().cursor_position() {
            let (camera, global_transform) = q_camera.single().unwrap();
            if let Ok(ray) = camera.viewport_to_world(global_transform, screen_pos) {
                let distance = -ray.origin.z / ray.direction.z;
                let world_pos = ray.origin + ray.direction * distance;

                // Check for entity under cursor
                if let Some((entity, offset)) =
                    find_entity_at_position(world_pos.truncate(), &grid, &query)
                {
                    commands.entity(entity).insert(Selected { offset });
                    info!("Selected gate entity: {:?}", entity);
                }
            }
        }
    }
}

pub fn drag_selected_system(
    q_window: Query<&Window, With<PrimaryWindow>>,
    q_camera: Query<(&Camera, &GlobalTransform)>,
    mut query: Query<(&mut Transform, &Selected, &mut GateComponent)>,
    mouse_button_input: Res<ButtonInput<MouseButton>>,
) {
    // Only process if left mouse is still held down
    if !mouse_button_input.pressed(MouseButton::Right) {
        return;
    }

    // TODO 0.16 BEVY
    let (camera, camera_transform) = q_camera.single().unwrap();
    // TODO 0.16 BEVY
    let window = q_window.single().unwrap();

    if let Some(screen_pos) = window.cursor_position() {
        if let Ok(ray) = camera.viewport_to_world(camera_transform, screen_pos) {
            let distance = -ray.origin.z / ray.direction.z;
            let world_pos = ray.origin + ray.direction * distance;

            // Update position of all selected entities
            for (mut transform, selected, mut gate) in query.iter_mut() {
                // Apply the offset to make dragging feel natural
                transform.translation.x = world_pos.x + selected.offset.x;
                gate.position.x = transform.translation.x;
                transform.translation.y = world_pos.y + selected.offset.y;
                gate.position.y = transform.translation.y;
            }
        }
    }
}

pub fn simulation_update_system(mut simulation: ResMut<Simulation>) {
    simulation.tick();
}
