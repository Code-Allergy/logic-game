use bevy::input::mouse::MouseButtonInput;
use bevy::prelude::*;
use bevy::window::PrimaryWindow;
use sim_core::gates::{GateId, GateType};

use crate::{CONNECTION_Z_LEVEL, ConnectionLine, GateComponent, MainCamera, Selected, Simulation};

pub fn setup_system(mut commands: Commands) {
    // Set up your 2D camera (if you’re making a 2D frontend)
    commands.spawn((Camera2d::default(), MainCamera));
}

pub fn simulation_update_system(mut simulation: ResMut<Simulation>) {
    // Call the tick method on your simulation resource
    simulation.tick();
    println!("Simulation ticked!");
}

pub fn draw_gates_system(simulation: Res<Simulation>, mut commands: Commands) {
    // Iterate over the gates in the circuit. (You might need to make gates field public or provide a getter.)
    for gate in simulation.circuit.gates.iter() {
        // Procedurally position each gate based on its id (this is just a demo strategy).
        let x = (gate.id.0 as f32) * 120.0 - 300.0;
        let y = 0.0;

        // Choose a color based on the gate type.
        let color = match gate.gate_type {
            GateType::And => Color::srgb(0.0, 255.0, 0.0),
            GateType::Or => Color::srgb(255.0, 255.0, 0.0),
            GateType::Not => Color::srgb(255.0, 0.0, 0.0),
            GateType::Input => Color::srgb(0.0, 0.0, 255.0),
            GateType::Output => Color::srgb(255.0, 0.0, 255.0),
            // Default for any other type.
            _ => Color::WHITE,
        };

        commands.spawn((
            Sprite {
                color,
                custom_size: Some(Vec2::new(50.0, 50.0)),
                ..Default::default()
            },
            Transform::from_xyz(x, y, 0.0),
            GateComponent {
                id: gate.id,
                position: Vec2::new(x, y),
            },
        ));
    }
}

pub fn interaction_system(
    q_window: Query<&Window, With<PrimaryWindow>>,
    mouse_button_input: Res<ButtonInput<MouseButton>>,
    q_camera: Query<(&Camera, &GlobalTransform)>,
    mut query: Query<(Entity, &Transform, &GateComponent)>,
    mut commands: Commands,
    selected_query: Query<Entity, With<Selected>>,
) {
    let (camera, camera_transform) = q_camera.single();
    let window = q_window.single();

    // Handle deselection first - if mouse released, remove all selections
    if mouse_button_input.just_released(MouseButton::Left) {
        for entity in selected_query.iter() {
            commands.entity(entity).remove::<Selected>();
        }
        return;
    }

    // Handle selection if mouse is clicked
    if mouse_button_input.just_pressed(MouseButton::Left) {
        if let Some(screen_pos) = window.cursor_position() {
            // First remove any existing selections
            for entity in selected_query.iter() {
                commands.entity(entity).remove::<Selected>();
            }

            // Then try to select a new entity
            if let Ok(ray) = camera.viewport_to_world(camera_transform, screen_pos) {
                let distance = -ray.origin.z / ray.direction.z;
                let world_pos = ray.origin + ray.direction * distance;

                for (entity, transform, _) in query.iter_mut() {
                    let sprite_size = Vec2::new(50.0, 50.0);
                    let half_extents = sprite_size / 2.0;

                    let local_point = transform
                        .compute_matrix()
                        .inverse()
                        .transform_point3(world_pos);
                    let local_point_2d = Vec2::new(local_point.x, local_point.y);

                    if local_point_2d.x >= -half_extents.x
                        && local_point_2d.x <= half_extents.x
                        && local_point_2d.y >= -half_extents.y
                        && local_point_2d.y <= half_extents.y
                    {
                        // Calculate offset for smooth dragging
                        let offset = transform.translation.truncate() - world_pos.truncate();

                        // Add the Selected component
                        commands.entity(entity).insert(Selected { offset });
                        info!("Selected gate entity: {:?}", entity);
                        break; // Only select one entity
                    }
                }
            }
        }
    }
}

pub fn drag_selected_system(
    q_window: Query<&Window, With<PrimaryWindow>>,
    q_camera: Query<(&Camera, &GlobalTransform)>,
    mut query: Query<(&mut Transform, &Selected)>,
    mouse_button_input: Res<ButtonInput<MouseButton>>,
) {
    // Only process if left mouse is still held down
    if !mouse_button_input.pressed(MouseButton::Left) {
        return;
    }

    let (camera, camera_transform) = q_camera.single();
    let window = q_window.single();

    if let Some(screen_pos) = window.cursor_position() {
        if let Ok(ray) = camera.viewport_to_world(camera_transform, screen_pos) {
            let distance = -ray.origin.z / ray.direction.z;
            let world_pos = ray.origin + ray.direction * distance;

            // Update position of all selected entities
            for (mut transform, selected) in query.iter_mut() {
                // Apply the offset to make dragging feel natural
                transform.translation.x = world_pos.x + selected.offset.x;
                transform.translation.y = world_pos.y + selected.offset.y;
            }
        }
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
            gate_positions.get(&GateId(connection.source)),
            gate_positions.get(&GateId(connection.target)),
        ) {
            // Calculate the mid point between the two gates
            let mid_point = (*from_pos + *to_pos) / 2.0;

            // Calculate the direction vector
            let direction = (*to_pos - *from_pos).normalize();

            // Calculate the distance between gates
            let distance = (*to_pos - *from_pos).length();

            // Calculate rotation to align the line with the direction
            let rotation = Quat::from_rotation_arc(Vec3::X, direction);

            println!(
                "Drawing connection from {:?} to {:?}",
                connection.source, connection.target
            );
            // Create a line entity
            commands.spawn((
                Sprite {
                    color: Color::WHITE,
                    custom_size: Some(Vec2::new(distance, 8.0)), // Line with thickness of 2 pixels
                    ..Default::default()
                },
                Transform {
                    translation: mid_point,
                    rotation,
                    scale: Vec3::ONE,
                },
                ConnectionLine {
                    from_gate: GateId(connection.source),
                    to_gate: GateId(connection.target),
                },
            ));
        }
    }
}

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
