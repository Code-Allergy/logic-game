//! Bevy systems for the simulation editor view.
//!
//! All systems in this file operate exclusively on the game-side model
//! ([`GameCircuit`], [`GameNode`], [`GameWire`]) and the [`SimBridge`]
//! translation layer. They never import `logi-sim` types directly.

use bevy::{prelude::*, window::PrimaryWindow};
use bevy_egui::{EguiContexts, egui};

use crate::{SAVE_FILE_NAME, temp::GateComponentSelectionEvent};

use super::{
    ConnectionLine, GameCircuit, GameNodeKind, GateComponent, LogicDisplay, SavedSimulation,
    Selected, SelectedGate, SimBridge, SimCache, SimulationConfig, TickRateMeter,
    calculate_connection_geometry, get_gate_sprite, spatial_grid::SpatialGrid,
};

// ─────────────────────────────────────────────────────────────────────────────
// Bridge flush (PreUpdate)
// ─────────────────────────────────────────────────────────────────────────────

/// Drain pending `AddNode`/`AddWire` responses, rebuild the cache reverse index
/// when the circuit changes, and apply all queued [`SimEvent`]s to [`SimCache`].
pub fn bridge_flush_system(
    mut bridge: ResMut<SimBridge>,
    mut cache: ResMut<SimCache>,
    mut sim_config: ResMut<SimulationConfig>,
    game_circuit: Res<GameCircuit>,
) {
    bridge.flush_pending();

    // Rebuild the wire→pin reverse index whenever the circuit is mutated.
    // `is_changed()` is true on the first frame (Added implies Changed), so the
    // index is always populated before any events arrive.
    if game_circuit.is_changed() {
        cache.rebuild_index(&game_circuit, &bridge);
    }

    // Snapshot all wire states in one RwLock::read() — O(N wires), independent
    // of how many simulation ticks ran this frame.
    cache.update_from_snapshot(&bridge);

    // Drain events for lifecycle signals (StatusChanged only; wire events are
    // handled by the snapshot above).
    let events = bridge.drain_events();
    cache.apply_events(&events, &mut sim_config);
}

// ─────────────────────────────────────────────────────────────────────────────
// Initialization
// ─────────────────────────────────────────────────────────────────────────────

/// Spawn Bevy sprite entities for every node in the current [`GameCircuit`].
pub fn draw_gates_system(game_circuit: Res<GameCircuit>, mut commands: Commands) {
    for node in game_circuit.nodes.values() {
        let sprite = get_gate_sprite(&node.kind);
        let mut entity = commands.spawn((
            sprite,
            Transform::from_xyz(node.position.x, node.position.y, 0.0),
            GateComponent::new(node.id, node.position),
        ));
        if let GameNodeKind::Display { bits } = node.kind {
            let digits = usize::from(bits).div_ceil(4);
            entity.with_children(|parent| {
                parent.spawn((
                    Text2d::new(format!("0x{:0digits$X}", 0)),
                    TextFont::from_font_size(22.0),
                    TextColor(Color::srgb(0.3, 1.0, 0.5)),
                    Transform::from_xyz(0.0, 0.0, 1.0),
                    LogicDisplay {
                        node_id: node.id,
                        bits,
                    },
                ));
            });
        }
    }
}

/// Format multi-bit display inputs as a fixed-width hexadecimal value.
pub fn update_logic_displays_system(
    cache: Res<SimCache>,
    mut displays: Query<(&mut Text2d, &LogicDisplay)>,
) {
    for (mut text, display) in displays.iter_mut() {
        let mut value = 0_u64;
        for bit in 0..display.bits.min(64) {
            if cache
                .node_inputs
                .get(&(display.node_id, bit))
                .copied()
                .unwrap_or(false)
            {
                value |= 1_u64 << bit;
            }
        }
        let digits = usize::from(display.bits).div_ceil(4);
        text.0 = format!("0x{value:0digits$X}");
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
            Transform {
                translation: mid,
                rotation: rot,
                scale: Vec3::ONE,
            },
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
pub fn update_tickrate_system(sim_config: Res<SimulationConfig>, bridge: Res<SimBridge>) {
    if sim_config.is_changed() {
        if sim_config.unlimited_tick_rate {
            bridge.set_tick_rate(None);
        } else {
            let tick_rate = sim_config.tick_rate.max(0.1);
            bridge.set_tick_rate(Some(tick_rate));
        }
    }
}

/// Recolour Input/Output gate sprites based on cached wire state.
pub fn update_input_colours_system(
    cache: Res<SimCache>,
    game_circuit: Res<GameCircuit>,
    mut query: Query<(&mut Sprite, &GateComponent)>,
) {
    for (mut sprite, gc) in query.iter_mut() {
        let Some(node) = game_circuit.nodes.get(&gc.id) else {
            continue;
        };
        match &node.kind {
            GameNodeKind::Input => {
                // Input nodes have one output pin (pin 0).
                let on = cache
                    .node_outputs
                    .get(&(gc.id, 0))
                    .copied()
                    .unwrap_or(false);
                sprite.color = if on {
                    Color::srgb(0.0, 1.0, 0.0)
                } else {
                    Color::srgb(0.8, 0.0, 0.0)
                };
            }
            GameNodeKind::Output => {
                // Output nodes have one input wire (pin 0).
                let on = cache.node_inputs.get(&(gc.id, 0)).copied().unwrap_or(false);
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

/// Update connection-line transform when gates move and change color from cached wire state.
#[allow(clippy::type_complexity)]
pub fn update_connections_system(
    gate_query: Query<(&Transform, &GateComponent)>,
    mut line_query: Query<(&mut Transform, &mut Sprite, &ConnectionLine), Without<GateComponent>>,
    cache: Res<SimCache>,
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

        let on = cache.wire_values.get(&line.id).copied().unwrap_or(false);
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
    let Ok((camera, camera_transform)) = q_camera.single() else {
        return;
    };
    let Ok(window) = q_window.single() else {
        return;
    };

    if mouse_button_input.just_pressed(MouseButton::Left) {
        let Some(screen_pos) = window.cursor_position() else {
            return;
        };
        let Ok(ray) = camera.viewport_to_world(camera_transform, screen_pos) else {
            return;
        };
        let distance = -ray.origin.z / ray.direction.z;
        let world_pos = ray.origin + ray.direction * distance;

        for (entity, transform, gate) in query.iter() {
            let half = Vec2::splat(25.0);
            let local = transform
                .to_matrix()
                .inverse()
                .transform_point3(world_pos)
                .truncate();
            if local.x >= -half.x && local.x <= half.x && local.y >= -half.y && local.y <= half.y {
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

    let Ok(window) = q_window.single() else {
        return;
    };
    let Ok((camera, global_transform)) = q_camera.single() else {
        return;
    };
    let Some(screen_pos) = window.cursor_position() else {
        return;
    };
    let Ok(ray) = camera.viewport_to_world(global_transform, screen_pos) else {
        return;
    };
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
            if local.x >= -half.x && local.x <= half.x && local.y >= -half.y && local.y <= half.y {
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
    let Ok((camera, camera_transform)) = q_camera.single() else {
        return;
    };
    let Ok(window) = q_window.single() else {
        return;
    };
    let Some(screen_pos) = window.cursor_position() else {
        return;
    };
    let Ok(ray) = camera.viewport_to_world(camera_transform, screen_pos) else {
        return;
    };
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
    mut tick_rate_meter: ResMut<TickRateMeter>,
    mut contexts: EguiContexts,
    mut bridge: ResMut<SimBridge>,
    cache: Res<SimCache>,
) -> Result<(), BevyError> {
    let tick_count = bridge.tick_count();
    tick_rate_meter.sample(tick_count);

    egui::Window::new("Simulation Config").show(contexts.ctx_mut()?, |ui| {
        ui.label("Tick Rate (Hz)");

        if ui
            .checkbox(&mut sim_config.unlimited_tick_rate, "Unlimited Tick Rate")
            .changed()
        {
            tick_rate_meter.reset(tick_count);
        }

        if sim_config.unlimited_tick_rate {
            ui.label(match tick_rate_meter.average_hz {
                Some(rate) => format!("Average tick rate: {}", format_si_hz(rate)),
                None => "Average tick rate: measuring…".to_owned(),
            });
        }

        ui.add_enabled(
            !sim_config.unlimited_tick_rate,
            egui::Slider::new(&mut sim_config.tick_rate, 0.1..=10_000_000.0).text("Tick Rate"),
        );

        ui.separator();

        let mut selected_runner = sim_config.current_runner;
        egui::ComboBox::from_label("Active Runner")
            .selected_text(selected_runner.to_string())
            .show_ui(ui, |ui| {
                for &runner_type in logi_sim::engine::RunnerType::ALL {
                    ui.selectable_value(&mut selected_runner, runner_type, runner_type.to_string());
                }
            });

        if selected_runner != sim_config.current_runner {
            bridge.set_runner_type(selected_runner);
        }

        ui.separator();
        ui.label(format!("Sim ticks: {tick_count}"));
        ui.label(format!("Running: {}", cache.is_running));
        ui.horizontal(|ui| {
            if ui
                .button(if cache.is_running { "Pause" } else { "Play" })
                .clicked()
            {
                if cache.is_running {
                    bridge.pause();
                } else {
                    bridge.resume();
                }
            }
            if ui.button("Step").clicked() {
                bridge.step();
            }
        });
    });
    Ok(())
}

pub(crate) fn format_si_hz(rate: f64) -> String {
    const UNITS: [(&str, f64); 4] = [
        ("GHz", 1_000_000_000.0),
        ("MHz", 1_000_000.0),
        ("kHz", 1_000.0),
        ("Hz", 1.0),
    ];
    let (unit, scale) = UNITS
        .into_iter()
        .find(|(_, scale)| rate >= *scale)
        .unwrap_or(("Hz", 1.0));
    format!("{:.1} {unit}", rate / scale)
}

pub fn ui_for_selected_gate(
    mut contexts: EguiContexts,
    mut sel: ResMut<SelectedGate>,
    game_circuit: ResMut<GameCircuit>,
    bridge: Res<SimBridge>,
    cache: Res<SimCache>,
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
                        // Read current driven value from cache.
                        let current_val = cache
                            .node_outputs
                            .get(&(node.id, 0))
                            .copied()
                            .unwrap_or(false);
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
