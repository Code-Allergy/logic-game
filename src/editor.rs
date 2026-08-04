//! Logic-circuit editor UI plugin and systems.
//!
//! The editor lets the user place nodes, create wires, and toggle inputs.
//! All mutations go through [`GameCircuit`] (persistent model) and
//! [`SimBridge`] (simulator integration) together.

use std::path::Path;
use std::fs;
use std::io;

use bevy::prelude::*;
use bevy::window::PrimaryWindow;
use bevy_egui::{EguiContexts, egui};
use uuid::Uuid;

use crate::{
    sim::{
        calculate_connection_geometry, fixtures::fixture_workspace_catalog, get_gate_sprite, GameCircuit,
        GameNode, GameNodeKind, GameWire, GateComponent, SimBridge,
    },
    sim::spatial_grid::SpatialGrid,
    GameState, WORKSPACE_DIR,
    DEFAULT_WORKSPACE_EXT,
    temp::GateComponentSelectionEvent,
};

// ─────────────────────────────────────────────────────────────────────────────
// Editor resource
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Resource)]
pub struct EditorState {
    pub visible: bool,
    pub selected_gate_type: Option<GameNodeKind>,
    pub selected_gate_label: Option<String>,
    pub connection_mode: bool,
    pub current_workspace: Option<String>,
    pub workspace_file_name: String,
    pub workspace_catalog: Vec<(String, GameCircuit)>,
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
            selected_gate_label: None,
            connection_mode: false,
            current_workspace: None,
            workspace_file_name: "workspace".to_string(),
            workspace_catalog: Vec::new(),
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
    pub label: Option<String>,
}

#[derive(Message)]
pub struct CreateConnectionEvent {
    pub source_node: Uuid,
    pub source_pin: u8,
    pub sink_node: Uuid,
    pub sink_pin: u8,
}

#[derive(Message, Clone)]
pub struct LoadWorkspaceEvent {
    pub name: String,
    pub circuit: GameCircuit,
}

#[derive(Message)]
pub struct SaveWorkspaceEvent {
    pub name: String,
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
                    catalog_init_system,
                    editor_ui_system,
                    process_gate_placement,
                    handle_connection_creation,
                ),
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
    mut load_workspace_events: MessageWriter<LoadWorkspaceEvent>,
    mut save_workspace_events: MessageWriter<SaveWorkspaceEvent>,
    q_window: Query<&Window, With<PrimaryWindow>>,
    q_camera: Query<(&Camera, &GlobalTransform)>,
    button_input: Res<ButtonInput<MouseButton>>,
    game_state: Res<State<GameState>>,
    game_circuit: Res<GameCircuit>,
) -> Result<(), BevyError> {
    if game_state.get() != &GameState::Simulating {
        return Ok(());
    }

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

            if editor_state.current_workspace.is_none() {
                editor_state.current_workspace = Some(game_circuit.name.clone());
                editor_state.workspace_file_name = game_circuit.name.clone();
            }

            if let Some(active_workspace) = &editor_state.current_workspace {
                ui.label(format!("Active workspace: {active_workspace}"));
            } else {
                ui.label("Active workspace: (unsaved)");
            }

            ui.horizontal(|ui| {
                ui.label("Workspace name");
                ui.text_edit_singleline(&mut editor_state.workspace_file_name);
            });

            if ui
                .button("Save Workspace")
                .clicked()
                && !editor_state.workspace_file_name.trim().is_empty()
            {
                let name = editor_state.workspace_file_name.trim().to_string();
                editor_state.current_workspace = Some(name.clone());
                save_workspace_events.write(SaveWorkspaceEvent { name });
            }

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
                        editor_state.selected_gate_label = None;
                        editor_state.connection_mode = false;
                    }
                }
            });
            ui.horizontal(|ui| {
                for kind in [GameNodeKind::Xor, GameNodeKind::Nand, GameNodeKind::Nor] {
                    if ui.button(kind.display_name()).clicked() {
                        editor_state.selected_gate_type = Some(kind);
                        editor_state.selected_gate_label = None;
                        editor_state.connection_mode = false;
                    }
                }
            });
            ui.horizontal(|ui| {
                for kind in [GameNodeKind::Input, GameNodeKind::Output] {
                    if ui.button(kind.display_name()).clicked() {
                        editor_state.selected_gate_type = Some(kind);
                        editor_state.selected_gate_label = None;
                        editor_state.connection_mode = false;
                    }
                }
                if ui.button("CLOCK").clicked() {
                    editor_state.selected_gate_type = Some(GameNodeKind::Clock {
                        period_ticks: 10,
                        duty_ticks: 5,
                    });
                    editor_state.selected_gate_label = None;
                    editor_state.connection_mode = false;
                }
            });

            ui.separator();
            ui.heading("Subcircuits");
            ui.label("Place as cell:");
            let catalog = editor_state.workspace_catalog.clone();
            for (name, circuit) in &catalog {
                if ui.button(format!("Place: {name}")).clicked() {
                    editor_state.selected_gate_type = Some(GameNodeKind::SubCircuit {
                        circuit: Box::new(circuit.clone()),
                    });
                    editor_state.selected_gate_label = Some(name.clone());
                    editor_state.connection_mode = false;
                }
            }

            ui.separator();
            ui.heading("Open Workspace");
            for (name, circuit) in &catalog {
                if ui.button(format!("Open {name}")).clicked() {
                    editor_state.current_workspace = Some(name.clone());
                    editor_state.workspace_file_name = name.clone();
                    load_workspace_events.write(LoadWorkspaceEvent {
                        name: name.clone(),
                        circuit: circuit.clone(),
                    });
                }
            }

            match load_saved_workspace_names() {
                Ok(saved) if !saved.is_empty() => {
                    ui.separator();
                    ui.label("Saved workspace files:");
                    for name in saved {
                        if ui.button(format!("Load {name}")).clicked() {
                            if let Ok(circuit) = read_workspace_from_disk(&name) {
                                editor_state.current_workspace = Some(name.clone());
                                editor_state.workspace_file_name = name.clone();
                                load_workspace_events.write(LoadWorkspaceEvent { name, circuit });
                            }
                        }
                    }
                }
                Ok(_) => {
                    ui.label("No saved workspace files yet.");
                }
                Err(err) => {
                    ui.label(format!("Workspace files unavailable: {err}"));
                }
            }

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
    if let Some(kind) = editor_state.selected_gate_type.clone()
        && button_input.just_pressed(MouseButton::Left)
        && let Some(screen_pos) = window.cursor_position()
        && let Ok(ray) = camera.viewport_to_world(camera_transform, screen_pos)
    {
        let d = -ray.origin.z / ray.direction.z;
        let world_pos = (ray.origin + ray.direction * d).truncate();
        if !ctx.is_pointer_over_egui() {
            create_gate_events.write(CreateGateEvent {
                kind,
                label: editor_state.selected_gate_label.clone(),
                position: world_pos,
            });
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
    game_state: Res<State<GameState>>,
) {
    if game_state.get() != &GameState::Simulating {
        return;
    }

    for event in create_gate_events.read() {
        // 1. Add to frontend model.
        let mut node = GameNode::new(event.kind.clone(), event.position);
        if let Some(label) = event.label.clone() {
            node.label = Some(label);
        }
        let node_uuid = game_circuit.add_node(node.clone());

        // 2. Send to simulator (async – response captured via pending_nodes).
        bridge.add_node(&node);

        // 3. Spawn Bevy visual entity.
        let sprite = get_gate_sprite(&event.kind);
        let entity = commands
            .spawn((
                sprite,
                Transform::from_xyz(event.position.x, event.position.y, 0.0),
                Visibility::Visible,
                GateComponent::new(node_uuid, event.position),
            ))
            .id();

        grid.insert(entity, event.position);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Wire creation
// ─────────────────────────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
pub fn handle_connection_creation(
    mut commands: Commands,
    mut editor_state: ResMut<EditorState>,
    mut game_circuit: ResMut<GameCircuit>,
    mut bridge: ResMut<SimBridge>,
    mut create_connection_events: MessageWriter<CreateConnectionEvent>,
    mut selection_events: MessageReader<GateComponentSelectionEvent>,
    gate_query: Query<(&Transform, &GateComponent)>,
    game_state: Res<State<GameState>>,
) {
    if game_state.get() != &GameState::Simulating {
        return;
    }

    if !editor_state.connection_mode {
        return;
    }

    for event in selection_events.read() {
        let clicked_uuid = event.component.id;

        if editor_state.connection_source.is_none() {
            // First click: pick source.
            editor_state.connection_source = Some(clicked_uuid);
            editor_state.connection_source_pin = 0; // default output pin 0
        } else if let Some(src_uuid) = editor_state.connection_source
            && src_uuid != clicked_uuid
        {
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
            if let (Some(&from), Some(&to)) = (positions.get(&src_uuid), positions.get(&sink_uuid)) {
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
                    Visibility::Visible,
                    crate::sim::ConnectionLine {
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

fn catalog_init_system(mut editor_state: ResMut<EditorState>) {
    if editor_state.workspace_catalog.is_empty() {
        editor_state.workspace_catalog = fixture_workspace_catalog();
    }
}

fn load_saved_workspace_names() -> io::Result<Vec<String>> {
    let mut names = Vec::new();
    if !Path::new(WORKSPACE_DIR).exists() {
        return Ok(names);
    }

    for entry in fs::read_dir(WORKSPACE_DIR)? {
        let entry = entry?;
        let path = entry.path();
        if path
            .extension()
            .and_then(|ext| ext.to_str())
            == Some(DEFAULT_WORKSPACE_EXT.trim_start_matches('.'))
        {
            if let Some(stem) = path.file_stem().and_then(|name| name.to_str()) {
                names.push(stem.to_string());
            }
        }
    }
    names.sort_unstable();
    Ok(names)
}

fn read_workspace_from_disk(name: &str) -> io::Result<GameCircuit> {
    let path = workspace_path(name);
    let file = std::fs::File::open(path)?;
    let saved: crate::sim::SavedSimulation = serde_json::from_reader(file)?;
    if saved.version != crate::sim::SavedSimulation::CURRENT_VERSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unsupported save format version",
        ));
    }
    Ok(saved.circuit)
}

fn workspace_path(name: &str) -> String {
    format!(
        "{}/{}{}",
        WORKSPACE_DIR,
        sanitize_workspace_name(name),
        DEFAULT_WORKSPACE_EXT
    )
}

fn sanitize_workspace_name(name: &str) -> String {
    name.chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c => c,
        })
        .collect()
}
