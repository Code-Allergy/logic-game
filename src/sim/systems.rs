//! Bevy systems for the simulation editor view.
//!
//! All systems in this file operate exclusively on the game-side model
//! ([`GameCircuit`], [`GameNode`], [`GameWire`]) and the [`SimBridge`]
//! translation layer. They never import `logi-sim` types directly.

use bevy::{prelude::*, window::PrimaryWindow};
use bevy_egui::{EguiContexts, egui};

use crate::{
    SAVE_FILE_NAME,
    temp::GateComponentSelectionEvent,
};

use super::{
    ConnectionLine, GameCircuit, GameNodeKind, GateComponent, SavedSimulation, Selected,
    SelectedGate, SimBridge, SimulationConfig,
    calculate_connection_geometry,
    spatial_grid::SpatialGrid,
    get_gate_sprite,
};

// ─────────────────────────────────────────────────────────────────────────────
// Bridge flush (PreUpdate)
// ─────────────────────────────────────────────────────────────────────────────

/// Drain pending `AddNode`/`AddWire` responses from the worker thread so that
/// ID mappings are fully populated before any rendering system runs.
pub fn bridge_flush_system(mut bridge: ResMut<SimBridge>) {
    bridge.flush_pending();
}

// ─────────────────────────────────────────────────────────────────────────────
// Initialization
// ─────────────────────────────────────────────────────────────────────────────

/// Spawn Bevy sprite entities for every node in the current [`GameCircuit`].
pub fn draw_gates_system(game_circuit: Res<GameCircuit>, mut commands: Commands) {
    for node in game_circuit.nodes.values() {
        let sprite = get_gate_sprite(&node.kind);
        commands.spawn((
            sprite,
            Transform::from_xyz(node.position.x, node.position.y, 0.0),
            GateComponent::new(node.id, node.position),
        ));
    }
}

/// Spawn line-sprite entities for every wire in the current [`GameCircuit`].
pub fn draw_connections_system(
    game_circuit: Res<GameCircuit>,
    gate_query: Query<(&Transform, &GateComponent)>,
    mut commands: Commands,
) {
    let positions: std::collections::HashMap<_, _> = gate_query
        .iter()
        .map(|(t, gc)| (gc.id, t.translation))
        .collect();

    for wire in game_circuit.wires.values() {
        let (Some(&from), Some(&to)) = (
            positions.get(&wire.source_node),
            positions.get(&wire.sink_node),
        ) else {
            continue;
        };
        let (mid, rot, dist) = calculate_connection_geometry(&from, &to);
        commands.spawn((
            Sprite {
                color: Color::WHITE,
                custom_size: Some(Vec2::new(dist, 2.0)),
                ..Default::default()
            },
            Transform { translation: mid, rotation: rot, scale: Vec3::ONE },
            ConnectionLine {
                id: wire.id,
                source_node: wire.source_node,
                sink_node: wire.sink_node,
            },
        ));
    }
}

/// Rebuild the spatial grid from current gate positions.
pub fn rebuild_spatial_grid_system(
    mut grid: ResMut<SpatialGrid>,
    query: Query<(Entity, &Transform, &GateComponent)>,
) {
    grid.clear();
    for (entity, transform, _) in query.iter() {
        grid.insert(entity, transform.translation.truncate());
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Per-frame update systems
// ─────────────────────────────────────────────────────────────────────────────

/// Update worker tickrate when `SimulationConfig` changes.
pub fn update_tickrate_system(
    sim_config: Res<SimulationConfig>,
    bridge: Res<SimBridge>,
) {
    if sim_config.is_changed() {
        if sim_config.unlimited_tick_rate {
            bridge.set_tick_rate(None);
        } else {
            let tick_rate = sim_config.tick_rate.max(0.1);
            bridge.set_tick_rate(Some(tick_rate));
        }
    }
}

/// Recolour Input/Output gate sprites based on live wire state from [`SimBridge`].
pub fn update_input_colours_system(
    bridge: Res<SimBridge>,
    game_circuit: Res<GameCircuit>,
    mut query: Query<(&mut Sprite, &GateComponent)>,
) {
    for (mut sprite, gc) in query.iter_mut() {
        let Some(node) = game_circuit.nodes.get(&gc.id) else { continue };
        match &node.kind {
            GameNodeKind::Input => {
                // For an Input node read output pin 0 (its only pin).
                let on = bridge.node_output(gc.id, 0);
                sprite.color = if on {
                    Color::srgb(0.0, 1.0, 0.0)
                } else {
                    Color::srgb(0.8, 0.0, 0.0)
                };
            }
            GameNodeKind::Output => {
                // Output nodes have one input wire; read it.
                let on = bridge.node_input(gc.id, 0);
                sprite.color = if on {
                    Color::srgb(1.0, 1.0, 1.0)
                } else {
                    Color::srgb(0.1, 0.1, 0.1)
                };
            }
            _ => {}
        }
    }
}

/// Update connection-line transform when gates move and change color when state changes.
#[allow(clippy::type_complexity)]
pub fn update_connections_system(
    gate_query: Query<(&Transform, &GateComponent)>,
    mut line_query: Query<(&mut Transform, &mut Sprite, &ConnectionLine), Without<GateComponent>>,
    bridge: Res<SimBridge>,
) {
    let positions: std::collections::HashMap<_, _> = gate_query
        .iter()
        .map(|(t, gc)| (gc.id, t.translation))
        .collect();

    for (mut transform, mut sprite, line) in line_query.iter_mut() {
        let (Some(&from), Some(&to)) = (
            positions.get(&line.source_node),
            positions.get(&line.sink_node),
        ) else {
            continue;
        };
        let (mid, rot, dist) = calculate_connection_geometry(&from, &to);
        transform.translation = mid;
        transform.rotation = rot;
        sprite.custom_size = Some(Vec2::new(dist, 2.0));

        let on = bridge.wire_value(line.id);
        sprite.color = if on {
            Color::srgb(0.0, 1.0, 0.0)
        } else {
            Color::WHITE
        };
    }
}

/// Update the spatial grid when gate transforms change.
pub fn update_spatial_grid_system(
    mut grid: ResMut<SpatialGrid>,
    query: Query<(Entity, &Transform, &GateComponent), Changed<Transform>>,
) {
    for (entity, transform, gate) in query.iter() {
        grid.update(entity, gate.position, transform.translation.truncate());
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// User interaction
// ─────────────────────────────────────────────────────────────────────────────

/// Toggle an Input gate's value on left-click.
pub fn update_inputs_on_left_click(
    q_window: Query<&Window, With<PrimaryWindow>>,
    mouse_button_input: Res<ButtonInput<MouseButton>>,
    q_camera: Query<(&Camera, &GlobalTransform)>,
    query: Query<(Entity, &Transform, &GateComponent)>,
    mut select_writer: MessageWriter<GateComponentSelectionEvent>,
) {
    let Ok((camera, camera_transform)) = q_camera.single() else { return };
    let Ok(window) = q_window.single() else { return };

    if mouse_button_input.just_pressed(MouseButton::Left) {
        let Some(screen_pos) = window.cursor_position() else { return };
        let Ok(ray) = camera.viewport_to_world(camera_transform, screen_pos) else { return };
        let distance = -ray.origin.z / ray.direction.z;
        let world_pos = ray.origin + ray.direction * distance;

        for (entity, transform, gate) in query.iter() {
            let half = Vec2::splat(25.0);
            let local = transform
                .to_matrix()
                .inverse()
                .transform_point3(world_pos)
                .truncate();
            if local.x >= -half.x && local.x <= half.x && local.y >= -half.y && local.y <= half.y
            {
                select_writer.write(GateComponentSelectionEvent {
                    entity,
                    component: *gate,
                });
                break;
            }
        }
    }
}

/// Update [`SelectedGate`] resource when a selection event fires.
pub fn update_selected_gate(
    mut event_reader: MessageReader<GateComponentSelectionEvent>,
    mut selected_gate: ResMut<SelectedGate>,
) {
    for event in event_reader.read() {
        let entry = (event.entity, event.component.id);
        if selected_gate.0 != Some(entry) {
            selected_gate.0 = Some(entry);
            info!("Selected gate: {:?}", event.component.id);
        }
    }
}

/// Right-click to select and drag gates.
pub fn gate_selection_system(
    q_window: Query<&Window, With<PrimaryWindow>>,
    q_camera: Query<(&Camera, &GlobalTransform)>,
    grid: Res<SpatialGrid>,
    mouse_button_input: Res<ButtonInput<MouseButton>>,
    query: Query<(Entity, &Transform), With<GateComponent>>,
    mut commands: Commands,
    selected_query: Query<Entity, With<Selected>>,
) {
    if !mouse_button_input.just_pressed(MouseButton::Right) {
        return;
    }
    for e in selected_query.iter() {
        commands.entity(e).remove::<Selected>();
    }

    let Ok(window) = q_window.single() else { return };
    let Ok((camera, global_transform)) = q_camera.single() else { return };
    let Some(screen_pos) = window.cursor_position() else { return };
    let Ok(ray) = camera.viewport_to_world(global_transform, screen_pos) else { return };
    let distance = -ray.origin.z / ray.direction.z;
    let world_pos = ray.origin + ray.direction * distance;

    let nearby = grid.query_near(world_pos.truncate(), 50.0);
    for entity in nearby {
        if let Ok((ent, transform)) = query.get(entity) {
            let half = Vec2::splat(25.0);
            let local = transform
                .to_matrix()
                .inverse()
                .transform_point3(world_pos)
                .truncate();
            if local.x >= -half.x && local.x <= half.x && local.y >= -half.y && local.y <= half.y
            {
                let offset = transform.translation.truncate() - world_pos.truncate();
                commands.entity(ent).insert(Selected { offset });
                break;
            }
        }
    }
}

/// Move selected gate while right mouse button is held.
pub fn drag_selected_system(
    q_window: Query<&Window, With<PrimaryWindow>>,
    q_camera: Query<(&Camera, &GlobalTransform)>,
    mut query: Query<(&mut Transform, &Selected, &mut GateComponent)>,
    mouse_button_input: Res<ButtonInput<MouseButton>>,
    mut game_circuit: ResMut<GameCircuit>,
) {
    if !mouse_button_input.pressed(MouseButton::Right) {
        return;
    }
    let Ok((camera, camera_transform)) = q_camera.single() else { return };
    let Ok(window) = q_window.single() else { return };
    let Some(screen_pos) = window.cursor_position() else { return };
    let Ok(ray) = camera.viewport_to_world(camera_transform, screen_pos) else { return };
    let distance = -ray.origin.z / ray.direction.z;
    let world_pos = (ray.origin + ray.direction * distance).truncate();

    for (mut transform, selected, mut gate) in query.iter_mut() {
        let new_pos = world_pos + selected.offset;
        transform.translation.x = new_pos.x;
        transform.translation.y = new_pos.y;
        gate.position = new_pos;
        // Persist position back into the game model.
        if let Some(node) = game_circuit.nodes.get_mut(&gate.id) {
            node.position = new_pos;
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Egui panels
// ─────────────────────────────────────────────────────────────────────────────

pub fn simulation_config_ui_system(
    mut sim_config: ResMut<SimulationConfig>,
    mut contexts: EguiContexts,
    bridge: Res<SimBridge>,
) -> Result<(), BevyError> {
    egui::Window::new("Simulation Config").show(contexts.ctx_mut()?, |ui| {
        ui.label("Tick Rate (Hz)");

        ui.checkbox(&mut sim_config.unlimited_tick_rate, "Unlimited Tick Rate");

        ui.add_enabled(
            !sim_config.unlimited_tick_rate,
            egui::Slider::new(&mut sim_config.tick_rate, 0.1..=1_000_000.0).text("Tick Rate"),
        );

        ui.separator();
        ui.label(format!("Sim ticks: {}", bridge.tick_count()));
        ui.label(format!("Running: {}", bridge.is_running));
    });
    Ok(())
}

pub fn ui_for_selected_gate(
    mut contexts: EguiContexts,
    mut sel: ResMut<SelectedGate>,
    game_circuit: ResMut<GameCircuit>,
    bridge: Res<SimBridge>,
) -> Result<(), BevyError> {
    let ctx = contexts.ctx_mut()?;
    if let Some((_entity, node_id)) = sel.0 {
        // Clone what we need to avoid borrow conflicts in the closure.
        let node = game_circuit.nodes.get(&node_id).cloned();
        if let Some(node) = node {
            egui::Window::new("Gate Properties")
                .collapsible(false)
                .show(ctx, |ui| {
                    ui.label(format!("UUID: {}", node.id));
                    ui.label(format!("Kind: {}", node.kind.display_name()));

                    if let GameNodeKind::Input = &node.kind {
                        // Read current driven value via bridge.
                        let current_val = bridge.node_output(node.id, 0);
                        let mut value = current_val;
                        let resp = ui.checkbox(&mut value, "Driven Value");
                        if resp.changed() {
                            bridge.set_input(node.id, 0, value);
                        }
                    }

                    if let Some(label) = &node.label {
                        ui.label(format!("Label: {label}"));
                    }

                    if ui.button("Close").clicked() {
                        sel.0 = None;
                    }
                });
        } else {
            sel.0 = None;
        }
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Save / Load
// ─────────────────────────────────────────────────────────────────────────────

#[allow(dead_code)]
pub fn save_on_exit_system(
    mut exit_events: MessageReader<AppExit>,
    game_circuit: Res<GameCircuit>,
) {
    if exit_events.read().next().is_some() {
        if let Ok(file) = std::fs::File::create(SAVE_FILE_NAME) {
            let saved = SavedSimulation::new(game_circuit.clone());
            if let Err(e) = serde_json::to_writer_pretty(file, &saved) {
                eprintln!("Failed to save simulation: {e}");
            } else {
                info!("Saved simulation to {SAVE_FILE_NAME}");
            }
        }
    }
}
