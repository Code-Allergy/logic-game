//! Logic-circuit editor UI plugin and systems.
//!
//! The editor lets the user place nodes, create wires, and toggle inputs.
//! All mutations go through [`GameCircuit`] (persistent model) and
//! [`SimBridge`] (simulator integration) together.

use bevy::prelude::*;
use bevy::window::PrimaryWindow;
use bevy_egui::{EguiContexts, egui};
use uuid::Uuid;

use crate::{
    GameState,
    sim::{
        ConnectionLine, GameCircuit, GameNode, GameNodeKind, GameWire, GateComponent, SimBridge,
        calculate_connection_geometry, get_gate_sprite, spatial_grid::SpatialGrid,
    },
    temp::GateComponentSelectionEvent,
};

// ─────────────────────────────────────────────────────────────────────────────
// Editor resource
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Resource)]
pub struct EditorState {
    pub visible: bool,
    pub selected_gate_type: Option<GameNodeKind>,
    pub connection_mode: bool,
    /// First gate picked in connection mode.
    pub connection_source: Option<Uuid>,
    /// Output pin on the source gate.
    pub connection_source_pin: u8,
}

impl Default for EditorState {
    fn default() -> Self {
        EditorState {
            visible: true,
            selected_gate_type: None,
            connection_mode: false,
            connection_source: None,
            connection_source_pin: 0,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Messages
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Message)]
pub struct CreateGateEvent {
    pub kind: GameNodeKind,
    pub position: Vec2,
}

#[derive(Message)]
pub struct CreateConnectionEvent {
    pub source_node: Uuid,
    pub source_pin: u8,
    pub sink_node: Uuid,
    pub sink_pin: u8,
}

// ─────────────────────────────────────────────────────────────────────────────
// Plugin
// ─────────────────────────────────────────────────────────────────────────────

pub struct EditorPlugin;

impl Plugin for EditorPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<EditorState>()
            .add_message::<CreateGateEvent>()
            .add_message::<CreateConnectionEvent>()
            .add_systems(
                bevy_egui::EguiPrimaryContextPass,
                (
                    editor_ui_system,
                    process_gate_placement,
                    handle_connection_creation,
                )
                    .run_if(in_state(GameState::Simulating)),
            );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Editor UI
// ─────────────────────────────────────────────────────────────────────────────

pub fn editor_ui_system(
    mut contexts: EguiContexts,
    mut editor_state: ResMut<EditorState>,
    mut create_gate_events: MessageWriter<CreateGateEvent>,
    q_window: Query<&Window, With<PrimaryWindow>>,
    q_camera: Query<(&Camera, &GlobalTransform)>,
    button_input: Res<ButtonInput<MouseButton>>,
) -> Result<(), BevyError> {
    let ctx = contexts.ctx_mut()?;
    let Ok(window) = q_window.single() else {
        return Ok(());
    };
    let Ok((camera, camera_transform)) = q_camera.single() else {
        return Ok(());
    };

    egui::SidePanel::left("editor_panel")
        .resizable(true)
        .show(ctx, |ui| {
            ui.heading("Component Editor");
            ui.separator();

            // Connection mode toggle
            if ui
                .button(if editor_state.connection_mode {
                    "✓ Connection Mode"
                } else {
                    "Connection Mode"
                })
                .clicked()
            {
                editor_state.connection_mode = !editor_state.connection_mode;
                if editor_state.connection_mode {
                    editor_state.selected_gate_type = None;
                } else {
                    editor_state.connection_source = None;
                }
            }

            ui.separator();
            ui.heading("Add Components");

            // Basic gate buttons
            ui.horizontal(|ui| {
                for kind in [GameNodeKind::And, GameNodeKind::Or, GameNodeKind::Not] {
                    if ui.button(kind.display_name()).clicked() {
                        editor_state.selected_gate_type = Some(kind);
                        editor_state.connection_mode = false;
                    }
                }
            });
            ui.horizontal(|ui| {
                for kind in [GameNodeKind::Xor, GameNodeKind::Nand, GameNodeKind::Nor] {
                    if ui.button(kind.display_name()).clicked() {
                        editor_state.selected_gate_type = Some(kind);
                        editor_state.connection_mode = false;
                    }
                }
            });
            ui.horizontal(|ui| {
                for kind in [GameNodeKind::Input, GameNodeKind::Output] {
                    if ui.button(kind.display_name()).clicked() {
                        editor_state.selected_gate_type = Some(kind);
                        editor_state.connection_mode = false;
                    }
                }
                if ui.button("CLOCK").clicked() {
                    editor_state.selected_gate_type = Some(GameNodeKind::Clock {
                        period_ticks: 10,
                        duty_ticks: 5,
                    });
                    editor_state.connection_mode = false;
                }
            });

            ui.separator();

            // Status
            if let Some(ref kind) = editor_state.selected_gate_type {
                ui.label(format!("Selected: {}", kind.display_name()));
                ui.label("Left-click in world to place");
            } else if editor_state.connection_mode {
                match editor_state.connection_source {
                    Some(src) => {
                        ui.label(format!("From: {src}"));
                        ui.label("Click destination gate");
                    }
                    None => {
                        ui.label("Connection Mode: pick source gate");
                    }
                }
            } else {
                ui.label("Select a component or mode");
            }

            ui.separator();
            ui.label("Tips:");
            ui.label("• Right-click: drag gates");
            ui.label("• Left-click: select/toggle inputs");
            ui.label("• Connection Mode: wire output → input");
        });

    // World placement on left-click
    if let Some(kind) = editor_state.selected_gate_type.clone() {
        if button_input.just_pressed(MouseButton::Left) {
            if let Some(screen_pos) = window.cursor_position() {
                if let Ok(ray) = camera.viewport_to_world(camera_transform, screen_pos) {
                    let d = -ray.origin.z / ray.direction.z;
                    let world_pos = (ray.origin + ray.direction * d).truncate();
                    if !ctx.is_pointer_over_egui() {
                        create_gate_events.write(CreateGateEvent {
                            kind,
                            position: world_pos,
                        });
                    }
                }
            }
        }
    }

    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Gate placement
// ─────────────────────────────────────────────────────────────────────────────

pub fn process_gate_placement(
    mut commands: Commands,
    mut game_circuit: ResMut<GameCircuit>,
    mut bridge: ResMut<SimBridge>,
    mut grid: ResMut<SpatialGrid>,
    mut create_gate_events: MessageReader<CreateGateEvent>,
) {
    for event in create_gate_events.read() {
        // 1. Add to frontend model.
        let node = GameNode::new(event.kind.clone(), event.position);
        let node_uuid = game_circuit.add_node(node.clone());

        // 2. Send to simulator (async – response captured via pending_nodes).
        bridge.add_node(&node);

        // 3. Spawn Bevy visual entity.
        let sprite = get_gate_sprite(&event.kind);
        let entity = commands
            .spawn((
                sprite,
                Transform::from_xyz(event.position.x, event.position.y, 0.0),
                GateComponent::new(node_uuid, event.position),
            ))
            .id();

        grid.insert(entity, event.position);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Wire creation
// ─────────────────────────────────────────────────────────────────────────────

pub fn handle_connection_creation(
    mut commands: Commands,
    mut editor_state: ResMut<EditorState>,
    mut game_circuit: ResMut<GameCircuit>,
    mut bridge: ResMut<SimBridge>,
    mut create_connection_events: MessageWriter<CreateConnectionEvent>,
    mut selection_events: MessageReader<GateComponentSelectionEvent>,
    gate_query: Query<(&Transform, &GateComponent)>,
) {
    if !editor_state.connection_mode {
        return;
    }

    for event in selection_events.read() {
        let clicked_uuid = event.component.id;

        if editor_state.connection_source.is_none() {
            // First click: pick source.
            editor_state.connection_source = Some(clicked_uuid);
            editor_state.connection_source_pin = 0; // default output pin 0
        } else if let Some(src_uuid) = editor_state.connection_source {
            if src_uuid != clicked_uuid {
                let sink_uuid = clicked_uuid;
                let src_pin = editor_state.connection_source_pin;
                let sink_pin = 0u8; // default input pin 0

                // 1. Add wire to frontend model.
                let wire = GameWire::new(src_uuid, src_pin, sink_uuid, sink_pin);
                game_circuit.add_wire(wire.clone());

                // 2. Send to simulator (async).
                bridge.add_wire(&wire);

                // 3. Broadcast event for other systems (e.g. spawn line sprite).
                create_connection_events.write(CreateConnectionEvent {
                    source_node: src_uuid,
                    source_pin: src_pin,
                    sink_node: sink_uuid,
                    sink_pin,
                });

                // 4. Spawn line sprite immediately.
                let positions: std::collections::HashMap<_, _> = gate_query
                    .iter()
                    .map(|(t, gc)| (gc.id, t.translation))
                    .collect();
                if let (Some(&from), Some(&to)) =
                    (positions.get(&src_uuid), positions.get(&sink_uuid))
                {
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
                            source_node: src_uuid,
                            sink_node: sink_uuid,
                        },
                    ));
                }

                editor_state.connection_source = None;
            }
        }
    }
}
