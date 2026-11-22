use std::collections::HashMap;
use std::sync::Arc;

use bevy::ecs::entity_disabling::Disabled;
use bevy::ecs::system::SystemParam;
use bevy::prelude::*;
use bevy::window::PrimaryWindow;
use uuid::Uuid;
use bevy_egui::{EguiContexts, egui::{self, LayerId, Ui, UiBuilder}};

use crate::{
    GameState,
    sim::{
        ActiveCircuit, CircuitLibrary, CircuitView, CloseView, CreateGate, CreateWire,
        GateIdentity, GateKind, GateLabel, GateType, InstanceViewItem, OpenDefinitionView,
        RegisterDefinitions, WorkspaceDocument,
    },
};

use super::{EditorState, LoadWorkspaceEvent, SaveWorkspaceEvent};
use crate::editor::workspace::{load_saved_workspace_names, read_workspace};

fn gate_name(label: &GateLabel, ty: &GateType) -> String {
    if label.value.trim().is_empty() {
        ty.kind.display_name().to_string()
    } else {
        format!("{} ({})", label.value, ty.kind.display_name())
    }
}

/// The open view and the messages that change it, plus the root circuit's
/// gates (also while parked behind a subcircuit view).
#[derive(SystemParam)]
pub struct EditorViewParams<'w, 's> {
    view: Res<'w, CircuitView>,
    close: MessageWriter<'w, CloseView>,
    open_definition: MessageWriter<'w, OpenDefinitionView>,
    root_gates: Query<'w, 's, &'static GateType, (Without<InstanceViewItem>, Allow<Disabled>)>,
}

/// Every definition the root circuit uses, with how many instances of it
/// the running simulation has (counting nested ones), sorted by name.
fn definition_table<'a>(
    root_kinds: impl Iterator<Item = &'a GateKind>,
    library: &CircuitLibrary,
) -> Vec<(Uuid, String, usize)> {
    fn visit(def: Uuid, library: &CircuitLibrary, counts: &mut HashMap<Uuid, usize>, depth: usize) {
        if depth > 64 {
            return;
        }
        *counts.entry(def).or_default() += 1;
        if let Some(doc) = library.get(def) {
            for child in doc.instanced_definitions() {
                visit(child, library, counts, depth + 1);
            }
        }
    }
    let mut counts = HashMap::new();
    for kind in root_kinds {
        if let GateKind::SubCircuit { definition } = kind {
            visit(*definition, library, &mut counts, 0);
        }
    }
    let mut rows: Vec<(Uuid, String, usize)> = counts
        .into_iter()
        .map(|(id, n)| (id, library.get(id).map_or_else(|| "(missing)".into(), |d| d.name.clone()), n))
        .collect();
    rows.sort_by(|a, b| a.1.cmp(&b.1).then(a.0.cmp(&b.0)));
    rows
}

#[allow(clippy::too_many_arguments)]
pub fn editor_ui_system(
    mut contexts: EguiContexts,
    mut editor_state: ResMut<EditorState>,
    mut create_gate_events: MessageWriter<CreateGate>,
    mut create_wire_events: MessageWriter<CreateWire>,
    mut register_definitions: MessageWriter<RegisterDefinitions>,
    mut load_workspace_events: MessageWriter<LoadWorkspaceEvent>,
    mut save_workspace_events: MessageWriter<SaveWorkspaceEvent>,
    mut views: EditorViewParams,
    q_window: Query<&Window, With<PrimaryWindow>>,
    q_camera: Query<(&Camera, &GlobalTransform)>,
    button_input: Res<ButtonInput<MouseButton>>,
    game_state: Res<State<GameState>>,
    active: Res<ActiveCircuit>,
    library: Res<CircuitLibrary>,
    gates: Query<(&GateIdentity, &GateType, &GateLabel)>,
) -> Result<(), BevyError> {
    if game_state.get() != &GameState::Simulating {
        return Ok(());
    }

    let ctx = contexts.ctx_mut()?;
    let Ok(window) = q_window.single() else { return Ok(()) };
    let Ok((camera, camera_transform)) = q_camera.single() else { return Ok(()) };

    let mut root_ui = Ui::new(
        ctx.clone(),
        egui::Id::new("editor_root"),
        UiBuilder::new().layer_id(LayerId::background()).max_rect(ctx.viewport_rect()),
    );

    // Drop stale wire-builder picks (deleted gates).
    if editor_state.connection_source.is_some_and(|e| !gates.contains(e)) {
        editor_state.clear_connection();
    }
    if editor_state.connection_sink.is_some_and(|e| !gates.contains(e)) {
        editor_state.connection_sink = None;
    }
    if editor_state.current_workspace.is_none() {
        editor_state.current_workspace = Some(active.name.clone());
        editor_state.workspace_file_name = active.name.clone();
    }
    if editor_state.saved_workspaces.is_none() {
        editor_state.saved_workspaces = Some(load_saved_workspace_names().unwrap_or_default());
    }

    let mut select_kind: Option<(GateKind, Option<String>)> = None;
    let mut place_catalog: Option<usize> = None;
    let mut open_catalog: Option<usize> = None;
    let mut load_saved: Option<String> = None;
    let mut place_saved: Option<String> = None;
    let mut close_requested: Option<bool> = None;
    let mut open_definition: Option<Uuid> = None;
    let table = definition_table(views.root_gates.iter().map(|t| &t.kind), &library);

    egui::Panel::left("editor_panel").resizable(true).show_inside(&mut root_ui, |ui| {
        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.heading("Component Editor");
            ui.separator();

            if let Some(active_workspace) = &editor_state.current_workspace {
                ui.label(format!("Active workspace: {active_workspace}"));
            }
            ui.label(format!("View: {}", views.view.breadcrumb(&active.name)));
            if let Some(frame) = views.view.frames().last() {
                let instances = table.iter().find(|r| r.0 == frame.definition).map_or(0, |r| r.2);
                let note = if views.view.is_live() {
                    format!(
                        "Instance of \"{}\": values are live from the running simulation. Inputs \
                         are driven by the parent circuit. Edits change the shared definition and \
                         every instance of it ({instances} in this workspace).",
                        frame.name
                    )
                } else {
                    format!(
                        "Definition \"{}\" (not instanced here, so no live values). Edits change \
                         the shared definition.",
                        frame.name
                    )
                };
                ui.colored_label(egui::Color32::from_rgb(140, 200, 255), note);
                ui.horizontal(|ui| {
                    if ui.button("← Back to parent circuit").clicked() {
                        close_requested = Some(false);
                    }
                    if views.view.frames().len() > 1 && ui.button("⇤ Back to root").clicked() {
                        close_requested = Some(true);
                    }
                });
            }

            ui.horizontal(|ui| {
                ui.label("Workspace name");
                ui.text_edit_singleline(&mut editor_state.workspace_file_name);
            });
            if ui.button("Save Workspace").clicked() && !editor_state.workspace_file_name.trim().is_empty() {
                let name = editor_state.workspace_file_name.trim().to_string();
                editor_state.current_workspace = Some(name.clone());
                save_workspace_events.write(SaveWorkspaceEvent { name });
            }
            if let Some(status) = &editor_state.status {
                ui.colored_label(egui::Color32::from_rgb(255, 200, 90), status);
            }

            ui.separator();
            let label = if editor_state.connection_mode { "✓ Connection Mode" } else { "Connection Mode" };
            if ui.button(label).clicked() {
                editor_state.connection_mode = !editor_state.connection_mode;
                editor_state.selected_gate_type = None;
                editor_state.clear_connection();
            }

            ui.separator();
            ui.heading("Add Components");
            ui.horizontal(|ui| {
                ui.label("Width (bits)");
                ui.add(egui::DragValue::new(&mut editor_state.palette_width).range(1..=64));
            });
            for (group, kinds) in GateKind::palette(editor_state.palette_width) {
                ui.label(group);
                ui.horizontal_wrapped(|ui| {
                    for kind in kinds {
                        let selected = editor_state.selected_gate_type.as_ref() == Some(&kind);
                        if ui.selectable_label(selected, kind.display_name()).clicked() {
                            select_kind = Some((kind, None));
                        }
                    }
                });
            }

            ui.separator();
            ui.heading("Subcircuits");
            ui.label("Place as cell:");
            for (i, (name, _)) in editor_state.workspace_catalog.iter().enumerate() {
                if ui.button(format!("Place: {name}")).clicked() {
                    place_catalog = Some(i);
                }
            }

            ui.separator();
            ui.heading("Definitions");
            if table.is_empty() {
                ui.label("No subcircuits in this workspace.");
            }
            for (id, name, count) in &table {
                ui.horizontal(|ui| {
                    let short: String = id.to_string().chars().take(8).collect();
                    ui.label(format!("{name} ({short}…): {count} instance(s)"))
                        .on_hover_text(format!("Definition id {id}"));
                    if ui.small_button("Open").clicked() {
                        open_definition = Some(*id);
                    }
                });
            }

            ui.separator();
            ui.heading("Open Workspace");
            for (i, (name, _)) in editor_state.workspace_catalog.iter().enumerate() {
                if ui.button(format!("Open {name}")).clicked() {
                    open_catalog = Some(i);
                }
            }

            ui.separator();
            ui.horizontal(|ui| {
                ui.label("Saved workspace files:");
                if ui.small_button("Refresh").clicked() {
                    editor_state.saved_workspaces = None;
                }
            });
            match editor_state.saved_workspaces.as_deref() {
                Some([]) | None => {
                    ui.label("No saved workspace files yet.");
                }
                Some(saved) => {
                    for name in saved {
                        ui.horizontal(|ui| {
                            if ui.button(format!("Load {name}")).clicked() {
                                load_saved = Some(name.clone());
                            }
                            if ui.small_button("Place as cell").clicked() {
                                place_saved = Some(name.clone());
                            }
                        });
                    }
                }
            }

            ui.separator();
            if editor_state.connection_mode {
                let source = editor_state.connection_source.and_then(|e| gates.get(e).ok());
                let sink = editor_state.connection_sink.and_then(|e| gates.get(e).ok());
                match (source, sink) {
                    (Some((_, st, sl)), Some((_, kt, kl))) => {
                        ui.label(format!("Source: {}", gate_name(sl, st)));
                        ui.label(format!("Sink: {}", gate_name(kl, kt)));
                        ui.label("Choose pins in the Wire Builder.");
                    }
                    (Some((_, st, sl)), None) => {
                        ui.label(format!("Source: {}", gate_name(sl, st)));
                        ui.label("Select the sink gate in the world.");
                    }
                    _ => {
                        ui.label("Select the source gate first.");
                    }
                }
            } else {
                ui.label("Select a component or mode");
            }

            ui.separator();
            ui.label("Tips:");
            ui.label("• Right-click: drag gates");
            ui.label("• Left-click: select (properties window: toggle inputs, edit, delete)");
            ui.label("• Delete key: delete the selected gate");
            ui.label("• Connection Mode: click source then sink, choose pins, then connect");
        });
    });

    if let Some(to_root) = close_requested {
        views.close.write(CloseView { to_root });
        return Ok(());
    }
    if let Some(definition) = open_definition {
        views.open_definition.write(OpenDefinitionView { definition });
        return Ok(());
    }

    if let Some((kind, label)) = select_kind {
        editor_state.selected_gate_type = Some(kind);
        editor_state.selected_gate_label = label;
        editor_state.connection_mode = false;
        editor_state.clear_connection();
    }

    // Place a workspace as a cell: its root becomes a definition, keyed by
    // the root's id (definitions already in the library are kept).
    let mut place = |editor_state: &mut EditorState, name: String, ws: &WorkspaceDocument| {
        let mut definitions: HashMap<_, _> = ws.definitions.clone();
        definitions.insert(ws.root.id, Arc::new(ws.root.clone()));
        register_definitions.write(RegisterDefinitions { definitions });
        editor_state.selected_gate_type = Some(GateKind::SubCircuit { definition: ws.root.id });
        editor_state.selected_gate_label = Some(name);
        editor_state.connection_mode = false;
        editor_state.clear_connection();
    };
    if let Some(i) = place_catalog {
        let (name, ws) = editor_state.workspace_catalog[i].clone();
        place(&mut editor_state, name, &ws);
    }
    if let Some(name) = place_saved {
        match read_workspace(&name) {
            Ok(ws) => {
                editor_state.status = None;
                place(&mut editor_state, name, &ws);
            }
            Err(err) => {
                warn!("Cannot place workspace {name}: {err}");
                editor_state.status = Some(err.to_string());
            }
        }
    }

    let mut open = |editor_state: &mut EditorState, name: String, ws: Arc<WorkspaceDocument>| {
        editor_state.current_workspace = Some(name.clone());
        editor_state.workspace_file_name = name.clone();
        editor_state.selected_gate_type = None;
        editor_state.clear_connection();
        load_workspace_events.write(LoadWorkspaceEvent { name, workspace: ws });
    };

    if let Some(i) = open_catalog {
        let (name, ws) = editor_state.workspace_catalog[i].clone();
        editor_state.status = None;
        open(&mut editor_state, name, ws);
    }

    if let Some(name) = load_saved {
        // Reads the disk once, on the click.
        match read_workspace(&name) {
            Ok(ws) => {
                editor_state.status = None;
                open(&mut editor_state, name, Arc::new(ws));
            }
            Err(err) => {
                warn!("Cannot load workspace {name}: {err}");
                editor_state.status = Some(err.to_string());
            }
        }
    }

    if editor_state.connection_mode {
        let source = editor_state.connection_source.and_then(|e| gates.get(e).ok());
        let sink = editor_state.connection_sink.and_then(|e| gates.get(e).ok());
        egui::Window::new("Wire Builder").resizable(false).show(&root_ui, |ui| {
            ui.label("Build a wire between gates");
            let mut source_width = None;
            match source {
                Some((_, ty, label)) => {
                    ui.label(format!("Source: {}", gate_name(label, ty)));
                    let (_, outs) = library.pins(&ty.kind).unwrap_or_default();
                    let (_, names) = library.pin_names(&ty.kind);
                    source_width = draw_pin_selector(
                        ui,
                        "Source output pin",
                        &mut editor_state.connection_source_pin,
                        &outs,
                        &names,
                    );
                }
                None => {
                    ui.label("Source: not selected");
                }
            }
            ui.separator();
            let mut sink_width = None;
            match sink {
                Some((_, ty, label)) => {
                    ui.label(format!("Sink: {}", gate_name(label, ty)));
                    let (ins, _) = library.pins(&ty.kind).unwrap_or_default();
                    let (names, _) = library.pin_names(&ty.kind);
                    sink_width = draw_pin_selector(
                        ui,
                        "Sink input pin",
                        &mut editor_state.connection_sink_pin,
                        &ins,
                        &names,
                    );
                }
                None => {
                    ui.label("Sink: not selected");
                }
            }
            let widths_match = source_width.is_some() && source_width == sink_width;
            if source_width.is_some() && sink_width.is_some() && !widths_match {
                ui.colored_label(
                    egui::Color32::from_rgb(255, 200, 90),
                    format!(
                        "Pin widths differ ({} vs {} bits); use Split/Merge/Extend.",
                        source_width.unwrap_or(0),
                        sink_width.unwrap_or(0)
                    ),
                );
            }
            if ui.add_enabled(widths_match, egui::Button::new("Create Wire")).clicked()
                && let (Some((src, ..)), Some((snk, ..))) = (source, sink)
            {
                create_wire_events.write(CreateWire {
                    source_gate: src.uuid,
                    source_pin: editor_state.connection_source_pin,
                    sink_gate: snk.uuid,
                    sink_pin: editor_state.connection_sink_pin,
                });
                editor_state.clear_connection();
            }
            if ui.button("Clear Selection").clicked() {
                editor_state.clear_connection();
            }
        });
    }

    // World placement on left-click.
    if let Some(kind) = editor_state.selected_gate_type.clone()
        && button_input.just_pressed(MouseButton::Left)
        && !ctx.is_pointer_over_egui()
        && let Some(screen_pos) = window.cursor_position()
        && let Ok(ray) = camera.viewport_to_world(camera_transform, screen_pos)
    {
        let d = -ray.origin.z / ray.direction.z;
        let world_pos = (ray.origin + ray.direction * d).truncate();
        create_gate_events.write(CreateGate {
            kind,
            label: editor_state.selected_gate_label.clone(),
            position: world_pos,
        });
    }

    Ok(())
}

/// Pin picker; returns the selected pin's width.
fn draw_pin_selector(
    ui: &mut egui::Ui,
    label: &str,
    selected_pin: &mut u8,
    widths: &[u8],
    names: &[String],
) -> Option<u8> {
    if widths.is_empty() {
        ui.label(format!("{label}: (unavailable)"));
        *selected_pin = 0;
        return None;
    }
    let clamped = usize::from(*selected_pin).min(widths.len() - 1);
    *selected_pin = clamped as u8;
    let describe = |pin: usize| {
        let name = names.get(pin).map_or("", String::as_str);
        format!("pin {pin} {name} ({} bit)", widths[pin])
    };
    egui::ComboBox::from_label(label).selected_text(describe(clamped)).show_ui(ui, |ui| {
        for pin in 0..widths.len() {
            ui.selectable_value(selected_pin, pin as u8, describe(pin));
        }
    });
    widths.get(usize::from(*selected_pin)).copied()
}
