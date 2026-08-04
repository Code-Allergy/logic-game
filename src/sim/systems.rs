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
    ActiveCircuit, AuthoringTopologyChanged, CircuitLayer, CircuitMember, ClockConfig,
    ClockConfigChanged, ConnectionLine, DisplayConfig, DisplayConfigChanged, EntityLayerChanged,
    GateAuthoringBundle, GateLabelChanged, GateMoved,
    GateComponent, GateIdentity, GateLabel, GatePinState, GateType, GameCircuit, GameNode,
    GameNodeKind, GateKindTag, LogicDisplay, PersistentGateData, SubCircuitInstance,
    WireAuthoringBundle, WireCollection, WireEndpoints, WireIdentity, WireProperties, WireState,
    WorkspaceRevision,
    configuration_components, node_kind_from_components,
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

/// Apply validated authoring commands to focused ECS components. Configuration
/// changes emit a topology notification consumed by the bridge rebuild path.
#[allow(clippy::too_many_arguments)]
pub fn apply_authoring_messages_system(
    mut labels: MessageReader<GateLabelChanged>,
    mut clocks: MessageReader<ClockConfigChanged>,
    mut displays: MessageReader<DisplayConfigChanged>,
    mut moves: MessageReader<GateMoved>,
    mut layers: MessageReader<EntityLayerChanged>,
    mut topology: MessageWriter<AuthoringTopologyChanged>,
    mut revision: ResMut<WorkspaceRevision>,
    mut gate_labels: Query<(&GateIdentity, &mut GateLabel, &GateType, &mut Name)>,
    mut clock_configs: Query<(&GateIdentity, &mut ClockConfig)>,
    mut display_configs: Query<(&GateIdentity, &mut DisplayConfig)>,
    mut gate_transforms: Query<(&GateIdentity, &mut Transform)>,
    mut gate_layers: Query<(&GateIdentity, &mut CircuitLayer)>,
    mut wire_layers: Query<(&WireIdentity, &mut CircuitLayer), Without<GateIdentity>>,
) {
    for command in labels.read() {
        if command.label.trim().is_empty() { continue; }
        for (identity, mut label, gate_type, mut name) in &mut gate_labels {
            if identity.uuid != command.gate_uuid { continue; }
            label.value = command.label.trim().to_owned();
            name.set(format!("{} {}", gate_type.kind.display_name(), label.value));
            revision.mark_dirty();
        }
    }
    for command in clocks.read() {
        if command.period_ticks == 0 || command.duty_ticks == 0 || command.duty_ticks > command.period_ticks { continue; }
        for (identity, mut config) in &mut clock_configs {
            if identity.uuid != command.gate_uuid { continue; }
            config.period_ticks = command.period_ticks;
            config.duty_ticks = command.duty_ticks;
            revision.mark_dirty();
            topology.write(AuthoringTopologyChanged);
        }
    }
    for command in displays.read() {
        if !(1..=64).contains(&command.bits) { continue; }
        for (identity, mut config) in &mut display_configs {
            if identity.uuid != command.gate_uuid { continue; }
            config.bits = command.bits;
            config.radix = command.radix;
            config.leading_zeroes = command.leading_zeroes;
            revision.mark_dirty();
            topology.write(AuthoringTopologyChanged);
        }
    }
    for command in moves.read() {
        for (identity, mut transform) in &mut gate_transforms {
            if identity.uuid != command.gate_uuid { continue; }
            transform.translation.x = command.position.x;
            transform.translation.y = command.position.y;
            revision.mark_dirty();
        }
    }
    for command in layers.read() {
        for (identity, mut layer) in &mut gate_layers {
            if identity.uuid == command.entity_uuid {
                layer.layer_uuid = command.layer_uuid;
                layer.order = command.order;
                revision.mark_dirty();
            }
        }
        for (identity, mut layer) in &mut wire_layers {
            if identity.uuid == command.entity_uuid {
                layer.layer_uuid = command.layer_uuid;
                layer.order = command.order;
                revision.mark_dirty();
            }
        }
    }
}

/// Recompile the simulator from the ECS-derived snapshot after a validated
/// topology/configuration command.
pub fn rebuild_bridge_after_authoring_change_system(
    mut changes: MessageReader<AuthoringTopologyChanged>,
    snapshot: Res<GameCircuit>,
    mut bridge: ResMut<SimBridge>,
    mut cache: ResMut<SimCache>,
) {
    if changes.read().next().is_none() { return; }
    bridge.load_circuit(&snapshot);
    cache.clear();
    cache.rebuild_index(&snapshot, &bridge);
}

/// Capture direct inspector/plugin edits that bypass command messages.
pub fn track_authoring_component_changes_system(
    mut revision: ResMut<WorkspaceRevision>,
    transforms: Query<Ref<Transform>, (With<GateIdentity>, Changed<Transform>)>,
    labels: Query<Ref<GateLabel>, Changed<GateLabel>>,
    clocks: Query<Ref<ClockConfig>, Changed<ClockConfig>>,
    displays: Query<Ref<DisplayConfig>, Changed<DisplayConfig>>,
    layers: Query<Ref<CircuitLayer>, Changed<CircuitLayer>>,
    mut topology: MessageWriter<AuthoringTopologyChanged>,
) {
    let moved = transforms.iter().any(|value| !value.is_added());
    let relabeled = labels.iter().any(|value| !value.is_added());
    let clock_changed = clocks.iter().any(|value| !value.is_added());
    let display_changed = displays.iter().any(|value| !value.is_added());
    let layer_changed = layers.iter().any(|value| !value.is_added());
    if moved || relabeled || clock_changed || display_changed || layer_changed {
        revision.mark_dirty();
    }
    if clock_changed || display_changed {
        topology.write(AuthoringTopologyChanged);
    }
}

/// Copy the simulation snapshot from [`SimCache`] into ECS components so game
/// rendering systems can consume plain component data only.
pub fn sync_sim_cache_system(
    cache: Res<SimCache>,
    mut gate_query: Query<(&GateIdentity, &mut GatePinState, Option<&mut GateComponent>), Without<ConnectionLine>>,
    mut wire_query: Query<(&mut ConnectionLine, &mut WireState), Without<GateComponent>>,
) {
    for (identity, mut state, legacy) in gate_query.iter_mut() {
        let input_bits = cache.node_input_bits(identity.uuid);
        let output_bits = cache.node_output_bits(identity.uuid);
        if state.input_bits != input_bits || state.output_bits != output_bits {
            state.input_bits = input_bits;
            state.output_bits = output_bits;
        }
        if let Some(mut legacy) = legacy {
            legacy.input_bits = input_bits;
            legacy.output_bits = output_bits;
        }
    }

    for (mut line, mut state) in wire_query.iter_mut() {
        let bits = cache.wire_bits(line.id);
        let value = bits != 0;
        if line.value != value {
            line.value = value;
        }
        state.value = bits;
    }
}

/// Persist inspector/editor changes from the ECS gate record into the saved
/// circuit model and project its presentation fields onto standard Bevy data.
pub fn sync_gate_components_to_model_system(
    mut gates: Query<
        (&GateType, &GateLabel, &mut Name),
        Or<(Changed<GateType>, Changed<GateLabel>)>,
    >,
) {
    for (gate_type, label, mut name) in gates.iter_mut() {
        name.set(format!("{} {}", gate_type.kind.display_name(), label.value));
    }
}

/// Rebuild the portable bridge/serialization snapshot from the authoring ECS.
/// The snapshot is derived data; editor systems must not treat it as authority.
#[allow(clippy::type_complexity)]
pub fn rebuild_circuit_snapshot_from_ecs_system(
    active: Res<ActiveCircuit>,
    revision: Res<WorkspaceRevision>,
    mut last_revision: Local<Option<u64>>,
    mut snapshot: ResMut<GameCircuit>,
    gates: Query<(
        &GateIdentity,
        &GateLabel,
        &GateType,
        &Transform,
        &PersistentGateData,
        &CircuitMember,
        &CircuitLayer,
        Option<&ClockConfig>,
        Option<&DisplayConfig>,
        Option<&SubCircuitInstance>,
    )>,
    wires: Query<(&WireIdentity, &WireEndpoints, &WireProperties, &CircuitMember, &CircuitLayer)>,
) {
    if *last_revision == Some(revision.current) && !active.is_changed() {
        return;
    }
    let mut nodes = HashMap::new();
    for (identity, label, gate_type, transform, persistent, member, layer, clock, display, subcircuit) in &gates {
        if member.circuit_uuid != active.uuid { continue; }
        let mut custom_properties = persistent.custom_properties.clone();
        custom_properties.insert("gatequest.layer_uuid".to_owned(), layer.layer_uuid.to_string());
        custom_properties.insert("gatequest.layer_order".to_owned(), layer.order.to_string());
        let node = GameNode {
            id: identity.uuid,
            kind: node_kind_from_components(gate_type, clock, display, subcircuit),
            position: transform.translation.truncate(),
            label: (!label.value.trim().is_empty()).then(|| label.value.clone()),
            custom_properties,
        };
        nodes.insert(node.id, node);
    }

    let mut game_wires = HashMap::new();
    for (identity, endpoints, properties, member, layer) in &wires {
        if member.circuit_uuid != active.uuid { continue; }
        let wire = super::GameWire {
            id: identity.uuid,
            source_node: endpoints.source_gate,
            source_pin: endpoints.source_pin,
            sink_node: endpoints.sink_gate,
            sink_pin: endpoints.sink_pin,
            kind: properties.kind.clone(),
            layer_uuid: layer.layer_uuid,
            layer_order: layer.order,
        };
        game_wires.insert(wire.id, wire);
    }

    snapshot.id = active.uuid;
    snapshot.name.clone_from(&active.name);
    snapshot.nodes = nodes;
    snapshot.wires = game_wires;
    *last_revision = Some(revision.current);
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
            GateAuthoringBundle::new(node, game_circuit.id),
        ));
        let (clock, display, subcircuit) = configuration_components(node);
        if let Some(config) = clock { entity.insert(config); }
        if let Some(config) = display { entity.insert(config); }
        if let Some(config) = subcircuit { entity.insert(config); }

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
    gate_inputs: Query<&GatePinState>,
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
    let wire_root = commands
        .spawn((
            Name::new("Wires"),
            WireCollection,
            Transform::default(),
            Visibility::Visible,
        ))
        .id();
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
            ChildOf(wire_root),
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
            WireAuthoringBundle::new(wire, game_circuit.id),
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
    mut query: Query<(&mut Sprite, &GateType, &GatePinState)>,
) {
    for (mut sprite, gate_type, state) in query.iter_mut() {
        match gate_type.kind {
            GateKindTag::Input => {
                // Input nodes have one output pin (pin 0).
                let on = (state.output_bits & 1) != 0;
                sprite.color = if on {
                    Color::srgb(0.0, 1.0, 0.0)
                } else {
                    Color::srgb(0.8, 0.0, 0.0)
                };
            }
            GateKindTag::Input8 => {
                let mut on = true;
                for pin in 0..8 {
                    on &= ((state.output_bits >> pin) & 1) != 0;
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
                    on &= ((state.output_bits >> pin) & 1) != 0;
                }
                sprite.color = if on {
                    Color::srgb(0.0, 1.0, 0.0)
                } else {
                    Color::srgb(0.8, 0.0, 0.0)
                };
            }
            GateKindTag::Output => {
                // Output nodes have one input pin (pin 0).
                let on = (state.input_bits & 1) != 0;
                sprite.color = if on {
                    Color::srgb(1.0, 1.0, 1.0)
                } else {
                    Color::srgb(0.1, 0.1, 0.1)
                };
            }
            GateKindTag::Output8 => {
                let mut on = false;
                for pin in 0..8 {
                    on |= ((state.input_bits >> pin) & 1) != 0;
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
                    on |= ((state.input_bits >> pin) & 1) != 0;
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
    game_circuit: Res<GameCircuit>,
    bridge: Res<SimBridge>,
    sim_gate_query: Query<&GateComponent>,
    mut label_changes: MessageWriter<GateLabelChanged>,
    mut clock_changes: MessageWriter<ClockConfigChanged>,
    mut display_changes: MessageWriter<DisplayConfigChanged>,
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

        if needs_save {
            if let Some(label) = edited_node.label.clone() {
                label_changes.write(GateLabelChanged { gate_uuid: edited_node.id, label });
            }
            match edited_node.kind {
                GameNodeKind::Clock { period_ticks, duty_ticks } => {
                    clock_changes.write(ClockConfigChanged {
                        gate_uuid: edited_node.id,
                        period_ticks,
                        duty_ticks,
                    });
                }
                GameNodeKind::Display { bits } => {
                    display_changes.write(DisplayConfigChanged {
                        gate_uuid: edited_node.id,
                        bits,
                        radix: super::DisplayRadix::Hexadecimal,
                        leading_zeroes: true,
                    });
                }
                _ => {}
            }
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
    mut active_circuit: ResMut<ActiveCircuit>,
    mut revision: ResMut<WorkspaceRevision>,
    mut bridge: ResMut<SimBridge>,
    mut cache: ResMut<SimCache>,
    mut grid: ResMut<SpatialGrid>,
    mut load_events: MessageReader<LoadWorkspaceEvent>,
    mut save_events: MessageReader<SaveWorkspaceEvent>,
    gate_entities: Query<Entity, With<GateComponent>>,
    connection_entities: Query<(Entity, Option<&ChildOf>), With<ConnectionLine>>,
    wire_roots: Query<Entity, With<WireCollection>>,
    background_entities: Query<Entity, With<SimBackground>>,
) {
    let mut has_event = false;
    for ev in save_events.read() {
        has_event = true;
        if let Err(err) = save_workspace_to_disk(&ev.name, &game_circuit) {
            eprintln!("Failed to save workspace {}: {err}", ev.name);
        } else {
            revision.mark_saved();
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
        *active_circuit = ActiveCircuit::from_circuit(&next_circuit);
        *game_circuit = next_circuit;
        revision.current = 0;
        revision.saved = 0;

        // Clear simulation cache and replace the running graph.
        cache.clear();
        grid.clear();
        bridge.load_circuit(&game_circuit);
        apply_power_on_reset(&mut bridge, &game_circuit);
        cache.rebuild_index(&game_circuit, &bridge);

        // Wipe all old gate/connection visuals so we can rebuild cleanly.
        let mut entities_to_despawn: Vec<Entity> = gate_entities
            .iter()
            .chain(
                connection_entities
                    .iter()
                    .filter_map(|(entity, parent)| parent.is_none().then_some(entity)),
            )
            .chain(wire_roots.iter())
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
                GateAuthoringBundle::new(node, game_circuit.id),
            ));
            let (clock, display, subcircuit) = configuration_components(node);
            if let Some(config) = clock { entity.insert(config); }
            if let Some(config) = display { entity.insert(config); }
            if let Some(config) = subcircuit { entity.insert(config); }
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

        let wire_root = commands
            .spawn((
                Name::new("Wires"),
                WireCollection,
                Transform::default(),
                Visibility::Visible,
            ))
            .id();
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
                ChildOf(wire_root),
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
                WireAuthoringBundle::new(wire, game_circuit.id),
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
