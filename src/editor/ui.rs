use bevy::prelude::*;
use bevy::window::PrimaryWindow;
use bevy_egui::{EguiContexts, egui::{self, LayerId, Ui, UiBuilder}};

use crate::{
    GameState,
    sim::{GameCircuit, GameNode, GameNodeKind, WireKind},
};

use super::{
    CreateConnectionEvent, CreateGateEvent, EditorState, LoadWorkspaceEvent,
    OpenSubcircuitEvent, SaveWorkspaceEvent, CloseSubcircuitViewEvent, SubcircuitNavigationFrame,
};
use crate::editor::workspace::load_saved_workspace_names;
use crate::editor::workspace::read_workspace_from_disk;

pub fn editor_ui_system(
    mut contexts: EguiContexts,
    mut editor_state: ResMut<EditorState>,
    mut create_gate_events: MessageWriter<CreateGateEvent>,
    mut create_connection_events: MessageWriter<CreateConnectionEvent>,
    mut load_workspace_events: MessageWriter<LoadWorkspaceEvent>,
    mut save_workspace_events: MessageWriter<SaveWorkspaceEvent>,
    mut open_subcircuit_events: MessageReader<OpenSubcircuitEvent>,
    mut close_subcircuit_view_events: MessageReader<CloseSubcircuitViewEvent>,
    q_window: Query<&Window, With<PrimaryWindow>>,
    q_camera: Query<(&Camera, &GlobalTransform)>,
    button_input: Res<ButtonInput<MouseButton>>,
    game_state: Res<State<GameState>>,
    game_circuit: ResMut<GameCircuit>,
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

    // Build a root ui for panel.show(ui, ..) API used in this bevy_egui version.
    let mut root_ui = Ui::new(
        ctx.clone(),
        egui::Id::new("editor_root"),
        UiBuilder::new()
            .layer_id(LayerId::background())
            .max_rect(ctx.viewport_rect()),
    );

    let catalog = editor_state.workspace_catalog.clone();
    let nodes: Vec<GameNode> = game_circuit.nodes.values().cloned().collect();
    let source_node = editor_state
        .connection_source
        .and_then(|uuid| nodes.iter().find(|node| node.id == uuid).cloned());
    let sink_node = editor_state
        .connection_sink
        .and_then(|uuid| nodes.iter().find(|node| node.id == uuid).cloned());

    let select_gate_type = |editor_state: &mut EditorState, kind: GameNodeKind| {
        editor_state.selected_gate_type = Some(kind);
        editor_state.selected_gate_label = None;
        editor_state.connection_mode = false;
        editor_state.connection_source = None;
        editor_state.connection_sink = None;
    };

    let mut pending_load: Option<(String, GameCircuit)> = None;
    let mut close_requested = false;

    let pop_subcircuit_view = |editor_state: &mut EditorState,
                               game_circuit: &GameCircuit|
     -> Option<(String, GameCircuit)> {
        let Some(frame) = editor_state.subcircuit_navigation_stack.pop() else {
            warn!("CloseSubcircuitViewEvent ignored: already at root workspace");
            return None;
        };

        let mut restored_parent = frame.parent_circuit;
        if let Some(node) = restored_parent
            .nodes
            .get_mut(&frame.parent_subcircuit_node_id)
        {
            if let GameNodeKind::SubCircuit { circuit } = &mut node.kind {
                *circuit = Box::new(game_circuit.clone());
            } else {
                warn!("Cannot persist subcircuit edits: parent node kind changed");
            }
        }

        editor_state.current_workspace = frame.parent_workspace;
        editor_state.workspace_file_name = frame.parent_workspace_file_name;
        editor_state.workspace_breadcrumb = frame.parent_breadcrumb;
        let restore_name = editor_state
            .current_workspace
            .clone()
            .unwrap_or_else(|| restored_parent.name.clone());
        Some((restore_name, restored_parent))
    };

    for _ in close_subcircuit_view_events.read() {
        if pending_load.is_some() {
            continue;
        }
        pending_load = pop_subcircuit_view(&mut editor_state, &game_circuit);
    }

    for ev in open_subcircuit_events.read() {
        if pending_load.is_some() {
            continue;
        }

        let Some(node) = game_circuit.nodes.get(&ev.node_id).cloned() else {
            warn!("OpenSubcircuitEvent referenced missing node {}", ev.node_id);
            continue;
        };

        if let GameNodeKind::SubCircuit { circuit } = &node.kind {
            let child = circuit.as_ref().clone();
            let parent_workspace = editor_state.current_workspace.clone();
            let parent_workspace_file_name = editor_state.workspace_file_name.clone();
            let child_name = if child.name.trim().is_empty() {
                node.label.clone().unwrap_or_else(|| {
                    format!("{}-{}", node.kind.display_name(), &node.id.to_string()[..8])
                })
            } else {
                child.name.clone()
            };

            let current_workspace_breadcrumb = editor_state.workspace_breadcrumb.clone();
            editor_state
                .subcircuit_navigation_stack
                .push(SubcircuitNavigationFrame {
                    parent_subcircuit_node_id: node.id,
                    parent_circuit: game_circuit.clone(),
                    parent_workspace,
                    parent_workspace_file_name,
                    parent_breadcrumb: current_workspace_breadcrumb,
                });
            editor_state.current_workspace = Some(child_name.clone());
            editor_state.workspace_file_name = child_name.clone();
            editor_state.workspace_breadcrumb = if editor_state.workspace_breadcrumb.is_empty() {
                child_name.clone()
            } else {
                format!("{} / {}", editor_state.workspace_breadcrumb, child_name)
            };
            pending_load = Some((child_name, child));
        } else {
            warn!(
                "OpenSubcircuitEvent ignored for non-subcircuit node {}",
                node.id
            );
        }
    }

    if let Some((name, circuit)) = pending_load {
        load_workspace_events.write(LoadWorkspaceEvent { name, circuit });
        return Ok(());
    }

    // Ensure stale node selections do not persist after topology changes.
    if editor_state.connection_source.is_some() && source_node.is_none() {
        editor_state.connection_source = None;
        editor_state.connection_source_pin = 0;
    }
    if editor_state.connection_sink.is_some() && sink_node.is_none() {
        editor_state.connection_sink = None;
        editor_state.connection_sink_pin = 0;
    }

    let mut selected_place_gate = None::<(GameNodeKind, Option<String>)>;
    let mut open_workspace = None::<(String, GameCircuit)>;
    let mut open_saved_workspace = None::<(String, GameCircuit)>;

    egui::Panel::left("editor_panel")
        .resizable(true)
        .show_inside(&mut root_ui, |ui| {
            ui.heading("Component Editor");
            ui.separator();

            if editor_state.current_workspace.is_none() {
                editor_state.current_workspace = Some(game_circuit.name.clone());
                editor_state.workspace_file_name = game_circuit.name.clone();
            }

            if editor_state.workspace_breadcrumb.is_empty() {
                editor_state.workspace_breadcrumb = editor_state
                    .current_workspace
                    .clone()
                    .unwrap_or_else(|| "workspace".to_string());
            }

            if let Some(active_workspace) = &editor_state.current_workspace {
                ui.label(format!("Active workspace: {active_workspace}"));
                ui.label(format!("View: {}", editor_state.workspace_breadcrumb));
            } else {
                ui.label("Active workspace: (unsaved)");
                if !editor_state.workspace_breadcrumb.is_empty() {
                    ui.label(format!("View: {}", editor_state.workspace_breadcrumb));
                }
            }

            if !editor_state.subcircuit_navigation_stack.is_empty() {
                if ui.button("← Back to parent circuit").clicked() {
                    close_requested = true;
                }
            }

            ui.horizontal(|ui| {
                ui.label("Workspace name");
                ui.text_edit_singleline(&mut editor_state.workspace_file_name);
            });

            if ui.button("Save Workspace").clicked()
                && !editor_state.workspace_file_name.trim().is_empty()
            {
                let name = editor_state.workspace_file_name.trim().to_string();
                editor_state.current_workspace = Some(name.clone());
                save_workspace_events.write(SaveWorkspaceEvent { name });
            }

            ui.separator();
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
                    editor_state.connection_source = None;
                    editor_state.connection_sink = None;
                    editor_state.connection_source_pin = 0;
                    editor_state.connection_sink_pin = 0;
                } else {
                    editor_state.connection_source = None;
                    editor_state.connection_sink = None;
                    editor_state.connection_source_pin = 0;
                    editor_state.connection_sink_pin = 0;
                    editor_state.connection_wire_kind = WireKind::default();
                }
            }

            ui.separator();
            ui.heading("Add Components");

            ui.horizontal(|ui| {
                for kind in [GameNodeKind::And, GameNodeKind::Or, GameNodeKind::Not] {
                    if ui.button(kind.display_name()).clicked() {
                        select_gate_type(&mut editor_state, kind);
                    }
                }
            });
            ui.horizontal(|ui| {
                for kind in [
                    GameNodeKind::Xor,
                    GameNodeKind::Xnor,
                    GameNodeKind::Nand,
                    GameNodeKind::Nor,
                ] {
                    if ui.button(kind.display_name()).clicked() {
                        select_gate_type(&mut editor_state, kind);
                    }
                }
            });
            ui.horizontal(|ui| {
                for kind in [GameNodeKind::Input, GameNodeKind::Output] {
                    if ui.button(kind.display_name()).clicked() {
                        select_gate_type(&mut editor_state, kind);
                    }
                }
                if ui.button("CLOCK").clicked() {
                    select_gate_type(
                        &mut editor_state,
                        GameNodeKind::Clock {
                            period_ticks: 10,
                            duty_ticks: 5,
                        },
                    );
                }
            });
            ui.horizontal(|ui| {
                for kind in [GameNodeKind::Input8, GameNodeKind::Input64] {
                    if ui.button(kind.display_name()).clicked() {
                        select_gate_type(&mut editor_state, kind);
                    }
                }
                for kind in [GameNodeKind::Output8, GameNodeKind::Output64] {
                    if ui.button(kind.display_name()).clicked() {
                        select_gate_type(&mut editor_state, kind);
                    }
                }
            });
            ui.horizontal(|ui| {
                for kind in [GameNodeKind::Expand8, GameNodeKind::Expand64] {
                    if ui.button(kind.display_name()).clicked() {
                        select_gate_type(&mut editor_state, kind);
                    }
                }
                for kind in [GameNodeKind::ReduceOr8, GameNodeKind::ReduceOr64] {
                    if ui.button(kind.display_name()).clicked() {
                        select_gate_type(&mut editor_state, kind);
                    }
                }
            });
            ui.horizontal(|ui| {
                for kind in [GameNodeKind::Buffer, GameNodeKind::Mux, GameNodeKind::Demux] {
                    if ui.button(kind.display_name()).clicked() {
                        select_gate_type(&mut editor_state, kind);
                    }
                }
            });
            ui.horizontal(|ui| {
                for kind in [GameNodeKind::TriStateBuffer, GameNodeKind::Counter8] {
                    if ui.button(kind.display_name()).clicked() {
                        select_gate_type(&mut editor_state, kind);
                    }
                }
            });

            ui.separator();
            ui.heading("Subcircuits");
            ui.label("Place as cell:");
            for (name, circuit) in &catalog {
                if ui.button(format!("Place: {name}")).clicked() {
                    selected_place_gate = Some((
                        GameNodeKind::SubCircuit {
                            circuit: Box::new(circuit.clone()),
                        },
                        Some(name.clone()),
                    ));
                    editor_state.connection_mode = false;
                    editor_state.connection_source = None;
                    editor_state.connection_sink = None;
                }
            }

            ui.separator();
            ui.heading("Open Workspace");
            for (name, circuit) in &catalog {
                if ui.button(format!("Open {name}")).clicked() {
                    open_workspace = Some((name.clone(), circuit.clone()));
                }
            }

            match load_saved_workspace_names() {
                Ok(saved) if !saved.is_empty() => {
                    ui.separator();
                    ui.label("Saved workspace files:");
                    for name in saved {
                        if ui.button(format!("Load {name}")).clicked() {
                            if let Ok(circuit) = read_workspace_from_disk(&name) {
                                open_saved_workspace = Some((name.clone(), circuit));
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
            if editor_state.connection_mode {
                match (source_node.as_ref(), sink_node.as_ref()) {
                    (Some(source), Some(sink)) => {
                        ui.label(format!("Connection source: {}", friendly_node_name(source)));
                        ui.label(format!("Connection sink: {}", friendly_node_name(sink)));
                        if let Some(source_name) = source_node.as_ref().map(friendly_node_name) {
                            if let Some(sink_name) = sink_node.as_ref().map(friendly_node_name) {
                                ui.label(format!("Source: {source_name}"));
                                ui.label(format!("Sink: {sink_name}"));
                            }
                        }
                        ui.label("Pick gates in the world, then confirm in Wire Builder.");
                    }
                    (Some(source), None) => {
                        ui.label(format!("Connection source: {}", friendly_node_name(source)));
                        ui.label("Select sink gate in world.");
                    }
                    _ => {
                        ui.label("Select source gate first.");
                    }
                }
            } else {
                ui.label("Select a component or mode");
            }

            ui.separator();
            ui.label("Tips:");
            ui.label("• Right-click: drag gates");
            ui.label("• Left-click: select/toggle inputs");
            ui.label("• Connection Mode: click source then sink, choose pins, then connect");
        });

    if close_requested && pending_load.is_none() {
        pending_load = pop_subcircuit_view(&mut editor_state, &game_circuit);
    }

    if let Some((name, circuit)) = pending_load {
        load_workspace_events.write(LoadWorkspaceEvent { name, circuit });
        return Ok(());
    }

    if let Some((kind, label)) = selected_place_gate {
        editor_state.selected_gate_type = Some(kind);
        editor_state.selected_gate_label = label;
    }

    if let Some((name, circuit)) = open_workspace {
        editor_state.current_workspace = Some(name.clone());
        editor_state.workspace_file_name = name.clone();
        editor_state.workspace_breadcrumb = name.clone();
        editor_state.subcircuit_navigation_stack.clear();
        load_workspace_events.write(LoadWorkspaceEvent { name, circuit });
    }

    if let Some((name, circuit)) = open_saved_workspace {
        editor_state.current_workspace = Some(name.clone());
        editor_state.workspace_file_name = name.clone();
        editor_state.workspace_breadcrumb = name.clone();
        editor_state.subcircuit_navigation_stack.clear();
        load_workspace_events.write(LoadWorkspaceEvent { name, circuit });
    }

    if editor_state.connection_mode {
        let source_name = source_node.as_ref().map(friendly_node_name);
        let sink_name = sink_node.as_ref().map(friendly_node_name);
        let source_pin_count = source_node
            .as_ref()
            .map_or(0, |node| node.kind.output_count());
        let sink_pin_count = sink_node
            .as_ref()
            .map_or(0, |node| node.kind.input_count());
        let source_pin_width = source_node
            .as_ref()
            .and_then(|node| node.kind.output_pin_width(editor_state.connection_source_pin))
            .unwrap_or(0);
        let sink_pin_width = sink_node
            .as_ref()
            .and_then(|node| node.kind.input_pin_width(editor_state.connection_sink_pin))
            .unwrap_or(0);
        let connection_width = editor_state.connection_wire_kind.bit_width();

        egui::Window::new("Wire Builder")
            .resizable(false)
            .show(&mut root_ui, |ui| {
                ui.label("Build a wire between gates");
                if let Some(source_name) = source_name {
                    ui.label(format!("Source: {source_name}"));
                    draw_pin_selector(
                        ui,
                        "Source output pin",
                        &mut editor_state.connection_source_pin,
                        source_pin_count,
                    );
                } else {
                    ui.label("Source: not selected");
                }

                if let Some(sink_name) = sink_name {
                    ui.separator();
                    ui.label(format!("Sink: {sink_name}"));
                    draw_pin_selector(
                        ui,
                        "Sink input pin",
                        &mut editor_state.connection_sink_pin,
                        sink_pin_count,
                    );
                } else {
                    ui.label("Sink: not selected");
                }

                ui.separator();
                ui.label("Wire type");
                egui::ComboBox::from_label("Wire type")
                    .selected_text(editor_state.connection_wire_kind.display_name())
                    .show_ui(ui, |ui| {
                        ui.selectable_value(
                            &mut editor_state.connection_wire_kind,
                            WireKind::Scalar,
                            "Scalar (1-bit)",
                        );
                        ui.selectable_value(
                            &mut editor_state.connection_wire_kind,
                            WireKind::Vector { bits: 8 },
                            "Vector (8-bit)",
                        );
                        ui.selectable_value(
                            &mut editor_state.connection_wire_kind,
                            WireKind::Vector { bits: 64 },
                            "Vector (64-bit)",
                        );
                    });

                let selected = editor_state.connection_source.is_some()
                    && editor_state.connection_sink.is_some()
                    && source_pin_count > 0
                    && sink_pin_count > 0;
                if ui
                    .add_enabled(selected, egui::Button::new("Create Wire"))
                    .clicked()
                {
                    let event = CreateConnectionEvent {
                        source_node: editor_state.connection_source.unwrap(),
                        source_pin: editor_state.connection_source_pin,
                        sink_node: editor_state.connection_sink.unwrap(),
                        sink_pin: editor_state.connection_sink_pin,
                        wire_kind: editor_state.connection_wire_kind.clone(),
                    };
                    create_connection_events.write(event);
                    editor_state.connection_source = None;
                    editor_state.connection_sink = None;
                }

                if ui.button("Clear Selection").clicked() {
                    editor_state.connection_source = None;
                    editor_state.connection_sink = None;
                    editor_state.connection_source_pin = 0;
                    editor_state.connection_sink_pin = 0;
                }
                if !editor_state.connection_wire_kind.is_supported_by_sim() {
                    ui.label(
                        "Selected wire type is not yet supported by runtime; it will be stored as metadata.",
                    );
                }

                if !editor_state
                    .connection_wire_kind
                    .fits_between(source_pin_width, sink_pin_width)
                {
                    ui.label(format!(
                        "This wire ({connection_width}-bit) is wider than the selected pins."
                    ));
                }
            });
    }

    // World placement on left-click.
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

fn friendly_node_name(node: &GameNode) -> String {
    node.label.clone().unwrap_or_else(|| {
        format!(
            "{}-{}",
            node.kind.display_name(),
            &node.id.to_string()[..8.min(node.id.to_string().len())]
        )
    })
}

fn draw_pin_selector(ui: &mut egui::Ui, label: &str, selected_pin: &mut u8, pin_count: usize) {
    if pin_count == 0 {
        ui.label(format!("{label}: (unavailable)"));
        *selected_pin = 0;
        return;
    }
    let clamped = (*selected_pin as usize).min(pin_count.saturating_sub(1));
    *selected_pin = clamped as u8;
    egui::ComboBox::from_label(label)
        .selected_text(format!("pin {clamped}"))
        .show_ui(ui, |ui| {
            for pin in 0..pin_count {
                ui.selectable_value(selected_pin, pin as u8, format!("pin {pin}"));
            }
        });
}
