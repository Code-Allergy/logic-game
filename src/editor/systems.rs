//! Editor systems that mutate game model and spawn entities.

use bevy::prelude::*;

use crate::sim::{
    calculate_connection_geometry, get_gate_sprite, GameCircuit, GameNodeKind, GameWire, GateComponent,
    SimBridge,
};
use crate::sim::{ConnectionLine, GameNode};
use crate::temp::GateComponentSelectionEvent;
use crate::sim::gate_display_name;

use super::{CreateConnectionEvent, CreateGateEvent, EditorState};

// ─────────────────────────────────────────────────────────────────────────────
// Gate placement
// ─────────────────────────────────────────────────────────────────────────────

pub fn process_gate_placement(
    mut commands: Commands,
    mut game_circuit: ResMut<GameCircuit>,
    mut bridge: ResMut<SimBridge>,
    mut grid: ResMut<crate::sim::spatial_grid::SpatialGrid>,
    mut create_gate_events: MessageReader<CreateGateEvent>,
    game_state: Res<State<crate::GameState>>,
) {
    if game_state.get() != &crate::GameState::Simulating {
        return;
    }

    for event in create_gate_events.read() {
        let mut label = event.label.clone();
        if is_input_or_output_node(&event.kind) && label.as_ref().is_none_or(|value| value.trim().is_empty())
        {
            label = Some(generate_required_port_label(&game_circuit, &event.kind));
        }

        let mut node = GameNode::new(event.kind.clone(), event.position);
        if let Some(label) = label {
            node.label = Some(label);
        }
        let node_uuid = game_circuit.add_node(node.clone());

        bridge.add_node(&node);

        let sprite = get_gate_sprite(&event.kind);
        let entity = commands
            .spawn((
                sprite,
                Transform::from_xyz(event.position.x, event.position.y, 0.0),
                Visibility::Visible,
                Name::new(gate_display_name(node_uuid, &node.label, &event.kind)),
                GateComponent::new(&node),
            ))
            .id();

        grid.insert(entity, event.position);
    }
}

fn generate_required_port_label(game_circuit: &GameCircuit, kind: &GameNodeKind) -> String {
    let base = match kind {
        GameNodeKind::Input => "Input",
        GameNodeKind::Input8 => "Input8",
        GameNodeKind::Input64 => "Input64",
        GameNodeKind::Output => "Output",
        GameNodeKind::Output8 => "Output8",
        GameNodeKind::Output64 => "Output64",
        _ => "IO",
    };
    let existing = game_circuit
        .nodes
        .values()
        .filter(|node| &node.kind == kind)
        .filter_map(|node| node.label.as_ref())
        .filter_map(|label| {
            label
                .strip_prefix(&format!("{base} "))
                .and_then(|suffix| suffix.parse::<usize>().ok())
        })
        .max()
        .unwrap_or(0);
    format!("{base} {}", existing + 1)
}

fn is_input_or_output_node(kind: &GameNodeKind) -> bool {
    matches!(
        kind,
        GameNodeKind::Input
            | GameNodeKind::Input8
            | GameNodeKind::Input64
            | GameNodeKind::Output
            | GameNodeKind::Output8
            | GameNodeKind::Output64
    )
}

// ─────────────────────────────────────────────────────────────────────────────
// Wire creation interactions
// ─────────────────────────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
pub fn handle_connection_creation(
    mut editor_state: ResMut<EditorState>,
    game_circuit: Res<GameCircuit>,
    mut selection_events: MessageReader<GateComponentSelectionEvent>,
    game_state: Res<State<crate::GameState>>,
) {
    if game_state.get() != &crate::GameState::Simulating {
        return;
    }
    if !editor_state.connection_mode {
        return;
    }

    for event in selection_events.read() {
        let clicked_uuid = event.component.id;

        match editor_state.connection_source {
            None => {
                let clicked_node = game_circuit.nodes.get(&clicked_uuid);
                if clicked_node
                    .map(|node| node.kind.output_count() > 0)
                    .unwrap_or(false)
                {
                    editor_state.connection_source = Some(clicked_uuid);
                    editor_state.connection_source_pin = 0;
                    editor_state.connection_sink = None;
                    editor_state.connection_sink_pin = 0;
                } else {
                    warn!(
                        "Cannot use node {} as connection source: no output pins",
                        clicked_uuid
                    );
                }
            }
            Some(src_uuid) => {
                if src_uuid == clicked_uuid {
                    editor_state.connection_source = None;
                    editor_state.connection_source_pin = 0;
                    editor_state.connection_sink = None;
                    editor_state.connection_sink_pin = 0;
                    continue;
                }

                let source_node = game_circuit.nodes.get(&src_uuid);
                let sink_has_input = game_circuit
                    .nodes
                    .get(&clicked_uuid)
                    .map(|node| node.kind.input_count() > 0)
                    .unwrap_or(false);
                if !sink_has_input {
                    warn!(
                        "Cannot use node {} as connection sink: no input pins",
                        clicked_uuid
                    );
                    continue;
                }

                if source_node
                    .map(|node| node.kind.output_count() > 0)
                    .unwrap_or(false)
                {
                    editor_state.connection_sink = Some(clicked_uuid);
                    editor_state.connection_sink_pin = 0;
                } else {
                    warn!("Cannot create connection: source {} has no output pins", src_uuid);
                    editor_state.connection_source = None;
                    editor_state.connection_source_pin = 0;
                    editor_state.connection_sink = None;
                    editor_state.connection_sink_pin = 0;
                }
            }
        }
    }
}

pub fn apply_connection_events_system(
    mut commands: Commands,
    mut game_circuit: ResMut<GameCircuit>,
    mut bridge: ResMut<SimBridge>,
    mut create_connection_events: MessageReader<CreateConnectionEvent>,
    gate_query: Query<(&Transform, &GateComponent)>,
    game_state: Res<State<crate::GameState>>,
) {
    if game_state.get() != &crate::GameState::Simulating {
        return;
    }

    for event in create_connection_events.read() {
        let source_node = game_circuit.nodes.get(&event.source_node);
        let sink_node = game_circuit.nodes.get(&event.sink_node);

        let source = match source_node {
            Some(node) => node,
            None => continue,
        };
        let sink = match sink_node {
            Some(node) => node,
            None => continue,
        };
        if event.source_node == event.sink_node {
            warn!("Ignoring self-loop wire for node {}", event.source_node);
            continue;
        }

        if source.kind.output_pin_width(event.source_pin).is_none() {
            warn!(
                "Invalid source pin {} for node {}",
                event.source_pin, source.id
            );
            continue;
        }
        if sink.kind.input_pin_width(event.sink_pin).is_none() {
            warn!("Invalid sink pin {} for node {}", event.sink_pin, sink.id);
            continue;
        }

        if usize::from(event.source_pin) >= source.kind.output_count()
            || usize::from(event.sink_pin) >= sink.kind.input_count()
        {
            warn!(
                "Pin out of range while creating wire {} -> {}",
                source.id, sink.id
            );
            continue;
        }

        let source_pin_width = source.kind.output_pin_width(event.source_pin).unwrap_or(0);
        let sink_pin_width = sink.kind.input_pin_width(event.sink_pin).unwrap_or(0);
        let is_supported = event.wire_kind.is_supported_by_sim();

        if is_supported && !event.wire_kind.fits_between(source_pin_width, sink_pin_width) {
            warn!(
                "Wire width {} does not fit selected pins on {} -> {}",
                event.wire_kind.bit_width(),
                source.id,
                sink.id
            );
            continue;
        }

        if game_circuit.wires.values().any(|wire| {
            wire.source_node == event.source_node
                && wire.sink_node == event.sink_node
                && wire.source_pin == event.source_pin
                && wire.sink_pin == event.sink_pin
        }) {
            warn!("Ignoring duplicate wire {} -> {}", source.id, sink.id);
            continue;
        }

        if !is_supported {
            warn!(
                "Skipping runtime wiring for unsupported kind {}. Saving as metadata only.",
                event.wire_kind.display_name()
            );
        }

        let wire = GameWire::with_kind(
            event.source_node,
            event.source_pin,
            event.sink_node,
            event.sink_pin,
            event.wire_kind.clone(),
        );
        game_circuit.add_wire(wire.clone());

        if is_supported && bridge.add_wire(&wire).is_none() {
            continue;
        }

        let positions: std::collections::HashMap<_, _> = gate_query
            .iter()
            .map(|(t, gc)| (gc.id, t.translation))
            .collect();
        if let (Some(&from), Some(&to)) = (
            positions.get(&event.source_node),
            positions.get(&event.sink_node),
        ) {
            let (mid, rot, dist) = calculate_connection_geometry(&from, &to);
            let line_color = if is_supported {
                Color::WHITE
            } else {
                Color::srgba(0.8, 0.6, 0.2, 1.0)
            };
            commands.spawn((
                Sprite {
                    color: line_color,
                    custom_size: Some(Vec2::new(dist, 2.0)),
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
                    source_node: event.source_node,
                    source_pin: event.source_pin,
                    sink_node: event.sink_node,
                    sink_pin: event.sink_pin,
                    bit_width: u8::try_from(event.wire_kind.bit_width()).unwrap_or(u8::MAX),
                    value: false,
                    wire_kind: event.wire_kind.clone(),
                },
                Name::new(format!("Wire {}", wire.id)),
            ));
        }
    }
}
