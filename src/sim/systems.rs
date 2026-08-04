//! Bevy systems for the simulation editor view.
//!
//! All systems in this file operate exclusively on the game-side model
//! ([`GameCircuit`], [`GameNode`], [`GameWire`]) and the [`SimBridge`]
//! translation layer. They never import `logi-sim` types directly.

use bevy::{prelude::*, window::PrimaryWindow};
use bevy_egui::{EguiContexts, egui};
use std::collections::HashMap;
use uuid::Uuid;

use crate::{
    editor::{LoadWorkspaceEvent, OpenSubcircuitEvent, SaveWorkspaceEvent},
    temp::GateComponentSelectionEvent,
    editor::workspace::save_workspace_to_disk,
    UiPointerState,
};

use super::{
    ConnectionLine, GameCircuit, GameNode, GameNodeKind, GateComponent, GateKindTag, LogicDisplay,
    gate_display_name,
    WireKind,
    Selected, SelectedGate, SimBridge, SimCache, SimulationConfig, ThreadlessStepper,
    TickRateMeter, calculate_connection_geometry, get_gate_sprite, spatial_grid::SpatialGrid,
};

#[derive(Component)]
pub struct SimBackground;

const UNLIMITED_THREADLESS_BATCH: u64 = 4_096;
const MAX_THREADLESS_TICKS_PER_FRAME: u64 = 4_096;

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
    cache.sync_with_bridge(&bridge, &mut sim_config);
}

/// Copy the simulation snapshot from [`SimCache`] into ECS components so game
/// rendering systems can consume plain component data only.
pub fn sync_sim_cache_system(
    cache: Res<SimCache>,
    mut gate_query: Query<&mut GateComponent, Without<ConnectionLine>>,
    mut wire_query: Query<&mut ConnectionLine, Without<GateComponent>>,
) {
    for mut gc in gate_query.iter_mut() {
        let input_bits = cache.node_input_bits(gc.id);
        let output_bits = cache.node_output_bits(gc.id);
        if gc.input_bits != input_bits || gc.output_bits != output_bits {
            gc.input_bits = input_bits;
            gc.output_bits = output_bits;
        }
    }

    for mut line in wire_query.iter_mut() {
        let value = cache.wire_value(line.id);
        if line.value != value {
            line.value = value;
        }
    }
}

/// Persist inspector/editor changes from the ECS gate record into the saved
/// circuit model and project its presentation fields onto standard Bevy data.
pub fn sync_gate_components_to_model_system(
    mut game_circuit: ResMut<GameCircuit>,
    mut gates: Query<
        (&GateComponent, &Transform, &mut Name),
        Or<(Changed<GateComponent>, Changed<Transform>)>,
    >,
) {
    for (gate, transform, mut name) in gates.iter_mut() {
        name.set(format!("{} {}", gate.display_name(), gate.label));

        if let Some(node) = game_circuit.nodes.get_mut(&gate.id) {
            node.label = Some(gate.label.clone());
            node.position = transform.translation.truncate();
        }
    }
}

/// Drive threadless simulation mode by stepping the bridge directly from Bevy's
/// main loop. This keeps wasm and other single-thread targets responsive while
/// preserving the same externally-visible bridge APIs.
pub fn drive_threadless_simulation(
    bridge: ResMut<SimBridge>,
    time: Res<Time>,
    mut stepper: ResMut<ThreadlessStepper>,
    sim_config: Res<SimulationConfig>,
) {
    if !bridge.is_running() {
        return;
    }

    if sim_config.unlimited_tick_rate {
        bridge.step_many(UNLIMITED_THREADLESS_BATCH);
        return;
    }

    let tick_rate = sim_config.tick_rate.max(0.1);
    stepper.pending_ticks += time.delta_secs_f64() * tick_rate;
    let mut due_ticks = stepper.pending_ticks.floor() as u64;
    if due_ticks > MAX_THREADLESS_TICKS_PER_FRAME {
        due_ticks = MAX_THREADLESS_TICKS_PER_FRAME;
    }
    if due_ticks > 0 {
        bridge.step_many(due_ticks);
        stepper.pending_ticks -= due_ticks as f64;
    }
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
            Name::new(gate_display_name(node.id, &node.label, &node.kind)),
            Transform::from_xyz(node.position.x, node.position.y, 0.0),
            Visibility::Visible,
            GateComponent::new(node),
        ));

        let logic_bits = match node.kind {
            GameNodeKind::Display { bits } => bits,
            GameNodeKind::Output => 1,
            GameNodeKind::Output8 => 8,
            GameNodeKind::Output64 => 64,
            _ => 0,
        };

        if logic_bits > 0 {
            let digits = usize::from(logic_bits).div_ceil(4);
            entity.with_children(|parent| {
                parent.spawn((
                    Text2d::new(format!("0x{:0digits$X}", 0)),
                    TextFont::from_font_size(22.0),
                    TextColor(Color::srgb(0.3, 1.0, 0.5)),
                    Transform::from_xyz(0.0, 0.0, 1.0),
                    LogicDisplay {
                        node_id: node.id,
                        bits: logic_bits,
                    },
                ));
            });
        }
    }
}

/// Draw a fallback background behind all simulation entities so the circuit is
/// visible even if the scene is otherwise misconfigured at startup.
pub fn draw_sim_background_system(mut commands: Commands, query: Query<Entity, With<SimBackground>>) {
    if !query.is_empty() {
        return;
    }
    commands.spawn((
        SimBackground,
        Sprite {
            color: Color::srgba(0.050, 0.229, 0.793, 1.000),
            ..Default::default()
        },
        Transform {
            translation: Vec3::new(0.0, 0.0, -1000.0),
            scale: Vec3::new(8000.0, 6000.0, 1.0),
            ..Default::default()
        },
        Visibility::Visible,
    ));
}

/// Format multi-bit display inputs as a fixed-width hexadecimal value.
pub fn update_logic_displays_system(
    mut displays: Query<(&mut Text2d, &LogicDisplay, &ChildOf)>,
    gate_inputs: Query<&GateComponent>,
) {
    for (mut text, display, child_of) in displays.iter_mut() {
        let Ok(inputs) = gate_inputs.get(child_of.parent()) else {
            continue;
        };
        let mut value = 0_u64;
        for bit in 0..display.bits.min(64) {
            if (inputs.input_bits >> bit) & 1 != 0 {
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
        let (color, thickness) = match wire.kind {
            WireKind::Scalar => (Color::WHITE, 2.0),
            WireKind::Vector { .. } => (Color::srgb(0.25, 0.7, 1.0), 4.0),
        };
        commands.spawn((
            Sprite {
                color,
                custom_size: Some(Vec2::new(dist, thickness)),
                ..Default::default()
            },
            Transform {
                translation: mid,
                rotation: rot,
                scale: Vec3::ONE,
            },
            Visibility::Visible,
            Name::new(format!("Wire {}", wire.id)),
            ConnectionLine {
                id: wire.id,
                source_node: wire.source_node,
                source_pin: wire.source_pin,
                sink_node: wire.sink_node,
                sink_pin: wire.sink_pin,
                bit_width: u8::try_from(wire.kind.bit_width()).unwrap_or(u8::MAX),
                value: false,
                wire_kind: wire.kind.clone(),
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
    mut query: Query<(&mut Sprite, &GateComponent)>,
) {
    for (mut sprite, gc) in query.iter_mut() {
        match gc.kind {
            GateKindTag::Input => {
                // Input nodes have one output pin (pin 0).
                let on = (gc.output_bits & 1) != 0;
                sprite.color = if on {
                    Color::srgb(0.0, 1.0, 0.0)
                } else {
                    Color::srgb(0.8, 0.0, 0.0)
                };
            }
            GateKindTag::Input8 => {
                let mut on = true;
                for pin in 0..8 {
                    on &= ((gc.output_bits >> pin) & 1) != 0;
                }
                sprite.color = if on {
                    Color::srgb(0.0, 1.0, 0.0)
                } else {
                    Color::srgb(0.8, 0.0, 0.0)
                };
            }
            GateKindTag::Input64 => {
                let mut on = true;
                for pin in 0..64 {
                    on &= ((gc.output_bits >> pin) & 1) != 0;
                }
                sprite.color = if on {
                    Color::srgb(0.0, 1.0, 0.0)
                } else {
                    Color::srgb(0.8, 0.0, 0.0)
                };
            }
            GateKindTag::Output => {
                // Output nodes have one input pin (pin 0).
                let on = (gc.input_bits & 1) != 0;
                sprite.color = if on {
                    Color::srgb(1.0, 1.0, 1.0)
                } else {
                    Color::srgb(0.1, 0.1, 0.1)
                };
            }
            GateKindTag::Output8 => {
                let mut on = false;
                for pin in 0..8 {
                    on |= ((gc.input_bits >> pin) & 1) != 0;
                }
                sprite.color = if on {
                    Color::srgb(1.0, 1.0, 1.0)
                } else {
                    Color::srgb(0.1, 0.1, 0.1)
                };
            }
            GateKindTag::Output64 => {
                let mut on = false;
                for pin in 0..64 {
                    on |= ((gc.input_bits >> pin) & 1) != 0;
                }
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
pub fn update_connections_system(
    game_circuit: Res<GameCircuit>,
    gate_query: Query<(&Transform, &GateComponent)>,
    mut line_query: Query<
        (&mut Transform, &mut Sprite, &ConnectionLine),
        Without<GateComponent>,
    >,
) {
    type GatePositions = HashMap<uuid::Uuid, Vec3>;
    let positions: GatePositions = gate_query
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
        let (base_color, thickness) = match line.wire_kind {
            WireKind::Scalar => (Color::WHITE, 2.0),
            WireKind::Vector { .. } => (Color::srgb(0.25, 0.7, 1.0), 4.0),
        };
        sprite.custom_size = Some(Vec2::new(dist, thickness));

        let on = line.value;
        let line_is_supported = game_circuit
            .wires
            .get(&line.id)
            .is_some_and(|wire| wire.kind.is_supported_by_sim());

        sprite.color = if on {
            Color::srgb(0.0, 1.0, 0.0)
        } else if line_is_supported {
            base_color
        } else {
            Color::srgba(0.8, 0.6, 0.2, 1.0)
        };
    }
}

/// Update the spatial grid when gate transforms change.
pub fn update_spatial_grid_system(
    mut grid: ResMut<SpatialGrid>,
    query: Query<(Entity, &Transform), (With<GateComponent>, Changed<Transform>)>,
) {
    for (entity, transform) in query.iter() {
        grid.update(entity, transform.translation.truncate());
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// User interaction
// ─────────────────────────────────────────────────────────────────────────────

/// Toggle an Input gate's value on left-click.
pub fn update_inputs_on_left_click(
    q_window: Query<&Window, With<PrimaryWindow>>,
    editor_state: Option<Res<crate::editor::EditorState>>,
    ui_pointer: Res<UiPointerState>,
    mouse_button_input: Res<ButtonInput<MouseButton>>,
    q_camera: Query<(&Camera, &GlobalTransform)>,
    query: Query<(Entity, &Transform, &GateComponent)>,
    mut select_writer: MessageWriter<GateComponentSelectionEvent>,
) {
    let is_connection_mode = editor_state
        .as_ref()
        .is_some_and(|state| state.connection_mode);

    if ui_pointer.over_egui {
        return;
    }
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
                        component: gate.clone(),
                    });

                if !is_connection_mode {
                    return;
                }
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
    ui_pointer: Res<UiPointerState>,
    mouse_button_input: Res<ButtonInput<MouseButton>>,
    query: Query<(Entity, &Transform), With<GateComponent>>,
    mut commands: Commands,
    selected_query: Query<Entity, With<Selected>>,
) {
    if ui_pointer.over_egui {
        return;
    }
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
    ui_pointer: Res<UiPointerState>,
    mut query: Query<(&mut Transform, &Selected, &GateComponent)>,
    mouse_button_input: Res<ButtonInput<MouseButton>>,
) {
    if ui_pointer.over_egui {
        return;
    }
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

    for (mut transform, selected, _gate) in query.iter_mut() {
        let new_pos = world_pos + selected.offset;
        transform.translation.x = new_pos.x;
        transform.translation.y = new_pos.y;
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
    time: Res<Time>,
    game_state: Res<State<crate::GameState>>,
) -> Result<(), BevyError> {
    if game_state.get() != &crate::GameState::Simulating {
        return Ok(());
    }

    let tick_count = bridge.tick_count();
    tick_rate_meter.sample(tick_count, time.delta_secs_f64());

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
            sim_config.current_runner = selected_runner;
            bridge.set_runner_type(selected_runner);
        }

        ui.separator();
        ui.label(format!("Sim ticks: {tick_count}"));
        ui.label(format!("Running: {}", cache.is_running()));
        ui.horizontal(|ui| {
            if ui
                .button(if cache.is_running() { "Pause" } else { "Play" })
                .clicked()
            {
                if cache.is_running() {
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
    mut game_circuit: ResMut<GameCircuit>,
    bridge: Res<SimBridge>,
    sim_gate_query: Query<&GateComponent>,
    mut load_workspace_events: MessageWriter<LoadWorkspaceEvent>,
    mut open_subcircuit_events: MessageWriter<OpenSubcircuitEvent>,
    game_state: Res<State<crate::GameState>>,
) -> Result<(), BevyError> {
    if game_state.get() != &crate::GameState::Simulating {
        return Ok(());
    }

    let ctx = contexts.ctx_mut()?;
    let selected_gate = sel.0.and_then(|(entity, _)| sim_gate_query.get(entity).ok());

    if let Some((_, node_id)) = sel.0 {
        let Some(mut edited_node) = game_circuit.nodes.get(&node_id).cloned() else {
            sel.0 = None;
            return Ok(());
        };

        let mut needs_save = false;
        let mut needs_sim_reload = false;
    let mut input_toggles = Vec::<(u8, bool)>::new();
        let mut open_subcircuit = false;

        egui::Window::new("Gate Properties")
            .collapsible(false)
            .show(ctx, |ui| {
                ui.label(format!("UUID: {}", edited_node.id));
                ui.label(format!("Kind: {}", edited_node.kind.display_name()));

                match edited_node.kind {
                    GameNodeKind::Input => {
                        let current_val = selected_gate
                            .as_ref()
                            .map_or(false, |outputs| (outputs.output_bits & 1) != 0);
                        let mut value = current_val;
                        if ui.checkbox(&mut value, "Driven Value").changed() {
                            input_toggles.push((0, value));
                        }
                    }
                    GameNodeKind::Input8 => {
                        for bit in 0..8_u8 {
                            let mut value = selected_gate.as_ref().map_or(false, |outputs| {
                                ((outputs.output_bits >> bit) & 1) != 0
                            });
                            if ui.checkbox(&mut value, format!("Bit {bit}")).changed() {
                                input_toggles.push((bit, value));
                            }
                        }
                    }
                    GameNodeKind::Input64 => {
                        for bit in 0..64_u8 {
                            let mut value = selected_gate.as_ref().map_or(false, |outputs| {
                                ((outputs.output_bits >> bit) & 1) != 0
                            });
                            if ui.checkbox(&mut value, format!("Bit {bit}")).changed() {
                                input_toggles.push((bit, value));
                            }
                        }
                    }
                    _ => {}
                }

                let mut label_text = edited_node.label.clone().unwrap_or_default();
                if ui.text_edit_singleline(&mut label_text).changed() {
                    let trimmed = label_text.trim();
                    edited_node.label = if trimmed.is_empty() {
                        None
                    } else {
                        Some(trimmed.to_string())
                    };
                    needs_save = true;
                }

                match &mut edited_node.kind {
                    GameNodeKind::Clock {
                        period_ticks,
                        duty_ticks,
                    } => {
                        let mut period = *period_ticks;
                        let mut duty = *duty_ticks;

                        ui.horizontal(|ui| {
                            ui.label("Clock period (ticks)");
                            if ui
                                .add(egui::DragValue::new(&mut period).range(1_u32..=u32::MAX))
                                .changed()
                            {
                                needs_save = true;
                                needs_sim_reload = true;
                            }
                        });
                        if ui
                            .add(
                                egui::DragValue::new(&mut duty)
                                    .range(1_u32..=period.max(1_u32)),
                            )
                            .changed()
                        {
                            needs_save = true;
                            needs_sim_reload = true;
                        }
                        if period == 0 {
                            period = 1;
                            needs_save = true;
                            needs_sim_reload = true;
                        }

                        if *period_ticks != period {
                            needs_save = true;
                            needs_sim_reload = true;
                        }

                        if duty == 0 || duty > period {
                            duty = period;
                            needs_save = true;
                            needs_sim_reload = true;
                        }

                        *period_ticks = period;
                        *duty_ticks = duty;
                    }
                    GameNodeKind::Display { bits } => {
                        let mut display_bits = *bits;
                        ui.label("Display width (hex digits / 4 bits)");
                        if ui
                            .add(egui::DragValue::new(&mut display_bits).range(1_u8..=64_u8))
                            .changed()
                        {
                            let next_bits = display_bits.max(1);
                            if *bits != next_bits {
                                *bits = next_bits;
                                needs_save = true;
                                needs_sim_reload = true;
                            }
                        }
                    }
                    GameNodeKind::SubCircuit { circuit } => {
                        ui.separator();
                        ui.label(format!("Subcircuit: {}", circuit.name));
                        ui.label("Pin mapping is positional: in/output index maps to pin number.");
                        let input_pins = subcircuit_interface_pins(circuit, true);
                        let output_pins = subcircuit_interface_pins(circuit, false);

                        ui.separator();
                        ui.label(format!("Inputs: {}", input_pins.len()));
                        for (index, name) in input_pins.iter().enumerate() {
                            ui.label(format!("  in {index}: {name}"));
                        }
                        ui.separator();
                        ui.label(format!("Outputs: {}", output_pins.len()));
                        for (index, name) in output_pins
                            .iter()
                            .enumerate()
                        {
                            ui.label(format!("  out {index}: {name}"));
                        }
                        if ui.button("Open in new view").clicked() {
                            open_subcircuit = true;
                        }
                    }
                    _ => {}
                }

                if ui.button("Close").clicked() {
                    sel.0 = None;
                }
            });

        if let Some(node) = game_circuit.nodes.get_mut(&edited_node.id) {
            if needs_save {
                node.kind = edited_node.kind;
                node.label = edited_node.label;
            }
        }

        if needs_sim_reload {
            let circuit = game_circuit.clone();
            load_workspace_events.write(LoadWorkspaceEvent {
                name: circuit.name.clone(),
                circuit,
            });
        }

        for (pin, value) in input_toggles {
            bridge.set_input(edited_node.id, pin, value);
        }

        if open_subcircuit {
            open_subcircuit_events.write(OpenSubcircuitEvent {
                node_id: edited_node.id,
            });
        }
    } else {
        sel.0 = None;
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Save / Load
// ─────────────────────────────────────────────────────────────────────────────

pub fn save_on_exit_system(
    mut exit_events: MessageReader<AppExit>,
    game_circuit: Res<GameCircuit>,
) {
    if exit_events.read().next().is_some() {
        let name = if game_circuit.name.trim().is_empty() {
            "simulation"
        } else {
            game_circuit.name.as_str()
        };
        if let Err(err) = save_workspace_to_disk(name, &game_circuit) {
            eprintln!("Failed to save simulation: {err}");
        } else {
            info!("Saved simulation workspace: {name}");
        }
    }
}

pub fn workspace_io_system(
    mut commands: Commands,
    mut game_circuit: ResMut<GameCircuit>,
    mut bridge: ResMut<SimBridge>,
    mut cache: ResMut<SimCache>,
    mut grid: ResMut<SpatialGrid>,
    mut load_events: MessageReader<LoadWorkspaceEvent>,
    mut save_events: MessageReader<SaveWorkspaceEvent>,
    gate_entities: Query<Entity, With<GateComponent>>,
    connection_entities: Query<Entity, With<ConnectionLine>>,
    background_entities: Query<Entity, With<SimBackground>>,
) {
    let mut has_event = false;
    for ev in save_events.read() {
        has_event = true;
        if let Err(err) = save_workspace_to_disk(&ev.name, &game_circuit) {
            eprintln!("Failed to save workspace {}: {err}", ev.name);
        }
    }

    let mut load_circuit: Option<GameCircuit> = None;
    for ev in load_events.read() {
        has_event = true;
        load_circuit = Some(ev.circuit.clone());
    }
    if !has_event {
        return;
    }

    // Load requests replace the active circuit and rebuild simulation visuals.
    if let Some(next_circuit) = load_circuit {
        *game_circuit = next_circuit;

        // Clear simulation cache and replace the running graph.
        cache.clear();
        grid.clear();
        bridge.load_circuit(&game_circuit);
        apply_power_on_reset(&mut bridge, &game_circuit);
        cache.rebuild_index(&game_circuit, &bridge);

        // Wipe all old gate/connection visuals so we can rebuild cleanly.
        let mut entities_to_despawn: Vec<Entity> = gate_entities
            .iter()
            .chain(connection_entities.iter())
            .chain(background_entities.iter())
            .collect();
        entities_to_despawn.sort_unstable_by_key(|entity| entity.index());
        entities_to_despawn.dedup();
        for entity in entities_to_despawn {
            commands.entity(entity).despawn();
        }

        // Spawn fresh visuals for the new circuit.
        let mut positions: HashMap<Uuid, Vec3> = HashMap::new();
        for node in game_circuit.nodes.values() {
            positions.insert(
                node.id,
                Vec3::new(node.position.x, node.position.y, 0.0),
            );

            let sprite = get_gate_sprite(&node.kind);
            let mut entity = commands.spawn((
                sprite,
                Name::new(gate_display_name(node.id, &node.label, &node.kind)),
                Transform::from_xyz(node.position.x, node.position.y, 0.0),
                Visibility::Visible,
                GateComponent::new(node),
            ));
            let logic_bits = match node.kind {
                GameNodeKind::Display { bits } => bits,
                GameNodeKind::Output => 1,
                GameNodeKind::Output8 => 8,
                GameNodeKind::Output64 => 64,
                _ => 0,
            };
            if logic_bits > 0 {
                let digits = usize::from(logic_bits).div_ceil(4);
                entity.with_children(|parent| {
                    parent.spawn((
                        Text2d::new(format!("0x{:0digits$X}", 0)),
                        TextFont::from_font_size(22.0),
                        TextColor(Color::srgb(0.3, 1.0, 0.5)),
                        Transform::from_xyz(0.0, 0.0, 1.0),
                        LogicDisplay {
                            node_id: node.id,
                            bits: logic_bits,
                        },
                    ));
                });
            }
            let entity = entity.id();
            grid.insert(entity, node.position);
        }

        for wire in game_circuit.wires.values() {
            let (Some(&from), Some(&to)) = (
                positions.get(&wire.source_node),
                positions.get(&wire.sink_node),
            ) else {
                continue;
            };
            let (mid, rot, dist) = calculate_connection_geometry(&from, &to);
            let (color, thickness) = match wire.kind {
                WireKind::Scalar => (Color::WHITE, 2.0),
                WireKind::Vector { .. } => (Color::srgb(0.25, 0.7, 1.0), 4.0),
            };
            commands.spawn((
                Sprite {
                    color,
                    custom_size: Some(Vec2::new(dist, thickness)),
                    ..Default::default()
                },
                Transform {
                    translation: mid,
                    rotation: rot,
                    scale: Vec3::ONE,
                },
                Visibility::Visible,
                ConnectionLine {
                    id: wire.id,
                    source_node: wire.source_node,
                    source_pin: wire.source_pin,
                    sink_node: wire.sink_node,
                    sink_pin: wire.sink_pin,
                    bit_width: u8::try_from(wire.kind.bit_width()).unwrap_or(u8::MAX),
                    value: false,
                    wire_kind: wire.kind.clone(),
                },
                Name::new(format!("Wire {}", wire.id)),
            ));
        }

        // Re-create the background behind the circuit.
        commands.spawn((
            SimBackground,
            Sprite {
                color: Color::srgba(0.050, 0.229, 0.793, 1.000),
                ..Default::default()
            },
            Transform {
                translation: Vec3::new(0.0, 0.0, -1000.0),
                scale: Vec3::new(8000.0, 6000.0, 1.0),
                ..Default::default()
            },
            Visibility::Visible,
        ));
    }
}

fn subcircuit_interface_pins(circuit: &GameCircuit, input: bool) -> Vec<String> {
    let pins: Vec<&GameNode> = subcircuit_interface_nodes(circuit, input);
    let mut names: Vec<String> = Vec::with_capacity(pins.len());
    for pin in pins {
        names.push(interface_node_label(pin));
    }
    names
}

fn subcircuit_interface_nodes<'a>(circuit: &'a GameCircuit, input: bool) -> Vec<&'a GameNode> {
    let mut pins: Vec<&GameNode> = circuit
        .nodes
        .values()
        .filter(|node| {
            if input {
                matches!(
                    node.kind,
                    GameNodeKind::Input | GameNodeKind::Input8 | GameNodeKind::Input64
                )
            } else {
                matches!(
                    node.kind,
                    GameNodeKind::Output | GameNodeKind::Output8 | GameNodeKind::Output64
                )
            }
        })
        .collect();

    pins.sort_by(|a, b| {
        let a_label = a.label.as_deref().unwrap_or("");
        let b_label = b.label.as_deref().unwrap_or("");
        a_label.cmp(b_label).then_with(|| a.id.cmp(&b.id))
    });
    pins
}

fn interface_node_label(node: &GameNode) -> String {
    node.label
        .clone()
        .unwrap_or_else(|| format!("{}-{}", node.kind.display_name(), &node.id.to_string()[..8]))
}

fn apply_power_on_reset(bridge: &mut SimBridge, game_circuit: &GameCircuit) {
    let Some(reset_node) = game_circuit.nodes.values().find(|node| {
        matches!(node.kind, GameNodeKind::Input | GameNodeKind::Input8 | GameNodeKind::Input64)
            && node.label.as_deref() == Some("Reset")
    }) else {
        return;
    };
    let reset_bits = reset_node.kind.output_count().min(usize::from(u8::MAX));

    bridge.pause();
    for pin in 0..reset_bits {
        bridge.set_input(reset_node.id, pin as u8, true);
    }
    let settled_at = bridge.tick_count().saturating_add(64);
    for _ in 0..64 {
        bridge.step();
    }
    while bridge.tick_count() < settled_at {
        std::hint::spin_loop();
    }
    for pin in 0..reset_bits {
        bridge.set_input(reset_node.id, pin as u8, false);
    }
    bridge.resume();
}
