use std::collections::HashMap;
use std::sync::Arc;

use bevy::ecs::entity_disabling::Disabled;
use bevy::ecs::system::SystemParam;
use bevy::prelude::*;
use bevy::window::PrimaryWindow;
use bevy_egui::{
    EguiContexts,
    egui::{self, LayerId, Ui, UiBuilder},
};
use uuid::Uuid;

use crate::{
    GameState,
    sim::{
        ActiveCircuit, CircuitLibrary, CircuitView, CloseView, CreateGate, CreateWire, Demo,
        GateIdentity, GateKind, GateLabel, GateType, InstanceViewItem, OpenDefinitionView,
        RegisterDefinitions, WorkspaceDocument, demos,
        rtl::{ImportOptions, import_yosys_json},
    },
};

use super::{EditorState, LoadWorkspaceEvent, SaveWorkspaceEvent};
use crate::editor::workspace::{load_saved_workspace_names, read_workspace, save_workspace};

fn gate_name(label: &GateLabel, ty: &GateType) -> String {
    if label.value.trim().is_empty() {
        ty.kind.display_name().to_string()
    } else {
        format!("{} ({})", label.value, ty.kind.display_name())
    }
}

type ChangedRootGate = (
    Changed<GateType>,
    Without<InstanceViewItem>,
    Allow<Disabled>,
);

/// The open view and the messages that change it, plus the root circuit's
/// gates (also while parked behind a subcircuit view).
#[derive(SystemParam)]
pub struct EditorViewParams<'w, 's> {
    view: Res<'w, CircuitView>,
    close: MessageWriter<'w, CloseView>,
    open_definition: MessageWriter<'w, OpenDefinitionView>,
    root_gates: Query<'w, 's, &'static GateType, (Without<InstanceViewItem>, Allow<Disabled>)>,
    changed_root_gates: Query<'w, 's, (), ChangedRootGate>,
    removed_gates: RemovedComponents<'w, 's, GateType>,
    /// The definition table, rebuilt only when the root's gates or the
    /// library change.
    table: Local<'s, Option<Vec<(Uuid, String, usize)>>>,
}

impl EditorViewParams<'_, '_> {
    fn definition_table(&mut self, library: &Res<CircuitLibrary>) -> Vec<(Uuid, String, usize)> {
        let removed = self.removed_gates.read().count() > 0;
        if self.table.is_none()
            || removed
            || library.is_changed()
            || !self.changed_root_gates.is_empty()
        {
            *self.table = Some(definition_table(
                self.root_gates.iter().map(|t| &t.kind),
                library,
            ));
        }
        self.table.clone().unwrap_or_default()
    }
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
        .map(|(id, n)| {
            (
                id,
                library
                    .get(id)
                    .map_or_else(|| "(missing)".into(), |d| d.name.clone()),
                n,
            )
        })
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
    let Ok(window) = q_window.single() else {
        return Ok(());
    };
    let Ok((camera, camera_transform)) = q_camera.single() else {
        return Ok(());
    };

    let mut root_ui = Ui::new(
        ctx.clone(),
        egui::Id::new("editor_root"),
        UiBuilder::new()
            .layer_id(LayerId::background())
            .max_rect(ctx.viewport_rect()),
    );

    // Drop stale wire-builder picks (deleted gates).
    if editor_state
        .connection_source
        .is_some_and(|e| !gates.contains(e))
    {
        editor_state.clear_connection();
    }
    if editor_state
        .connection_sink
        .is_some_and(|e| !gates.contains(e))
    {
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
    let catalog = demos::catalog();
    let open_demo = catalog.ok().and_then(|c| c.by_root(active.uuid));
    let mut place_catalog: Option<&Demo> = None;
    let mut open_catalog: Option<&Demo> = None;
    let mut copy_demo = false;
    let mut new_workspace = false;
    let mut import_rtl = false;
    let mut load_saved: Option<String> = None;
    let mut place_saved: Option<String> = None;
    let mut close_requested: Option<bool> = None;
    let mut open_definition: Option<Uuid> = None;
    let table = views.definition_table(&library);

    egui::Panel::left("editor_panel").resizable(true).show_inside(&mut root_ui, |ui| {
        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.heading("Component Editor");
            ui.separator();

            if let Some(active_workspace) = &editor_state.current_workspace {
                ui.label(format!("Active workspace: {active_workspace}"));
            }
            if active.read_only {
                if let Some(demo) = open_demo {
                    match (&demo.category, &catalog) {
                        (Some(category), Ok(catalog)) => ui.label(format!(
                            "Gallery, {}: {}",
                            catalog.category_title(category),
                            demo.title
                        )),
                        _ => ui.label(format!("Demo, level {}: {}", demo.level, demo.title)),
                    };
                    ui.label(&demo.description);
                }
                ui.colored_label(
                    egui::Color32::from_rgb(140, 200, 255),
                    "Read-only demo: toggle inputs and open subcircuits freely; copy it to edit.",
                );
                if ui.button("Copy to my workspace").clicked() {
                    copy_demo = true;
                }
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
            ui.horizontal(|ui| {
                if ui.add_enabled(!active.read_only, egui::Button::new("Save Workspace")).clicked()
                    && !editor_state.workspace_file_name.trim().is_empty()
                {
                    let name = editor_state.workspace_file_name.trim().to_string();
                    editor_state.current_workspace = Some(name.clone());
                    save_workspace_events.write(SaveWorkspaceEvent { name });
                }
                if ui.button("New empty workspace").clicked() {
                    new_workspace = true;
                }
            });
            // Import RTL… (native only: the web build has no file system).
            if cfg!(not(target_arch = "wasm32")) {
                ui.horizontal(|ui| {
                    ui.label("Import RTL…")
                        .on_hover_text("Path of a Yosys JSON netlist (write_json); see docs/rtl-import.md");
                    ui.text_edit_singleline(&mut editor_state.rtl_import_path);
                    if ui.button("Import").clicked() && !editor_state.rtl_import_path.trim().is_empty() {
                        import_rtl = true;
                    }
                });
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
            ui.label("Place a demo as a cell:");
            if let Ok(catalog) = catalog {
                ui.horizontal_wrapped(|ui| {
                    for demo in catalog.all() {
                        if ui.small_button(&demo.title).on_hover_text(&demo.description).clicked() {
                            place_catalog = Some(demo);
                        }
                    }
                });
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
            ui.heading("Demos");
            match catalog {
                Ok(catalog) => {
                    ui.label("From gates to a CPU: each level is built from the ones before it. Demos open read-only.");
                    let mut level = None;
                    for demo in &catalog.demos {
                        if level != Some(demo.level) {
                            level = Some(demo.level);
                            ui.strong(format!("Level {}: {}", demo.level, catalog.level_title(demo.level)));
                        }
                        let open = open_demo.is_some_and(|d| d.id == demo.id);
                        if ui.selectable_label(open, &demo.title).on_hover_text(&demo.description).clicked() {
                            open_catalog = Some(demo);
                        }
                        ui.small(&demo.description);
                    }
                    ui.separator();
                    ui.heading("Gallery");
                    ui.label("More circuits, from simple to complex, built from the demos and from each other. They open read-only too.");
                    let mut category = None;
                    for item in &catalog.gallery {
                        if category != item.category.as_deref() {
                            category = item.category.as_deref();
                            ui.strong(catalog.category_title(category.unwrap_or_default()));
                        }
                        let open = open_demo.is_some_and(|d| d.id == item.id);
                        if ui.selectable_label(open, &item.title).on_hover_text(&item.description).clicked() {
                            open_catalog = Some(item);
                        }
                        ui.small(&item.description);
                    }
                }
                Err(e) => {
                    ui.colored_label(egui::Color32::from_rgb(255, 120, 90), format!("Cannot load the demos: {e}"));
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
        views
            .open_definition
            .write(OpenDefinitionView { definition });
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
        editor_state.selected_gate_type = Some(GateKind::SubCircuit {
            definition: ws.root.id,
        });
        editor_state.selected_gate_label = Some(name);
        editor_state.connection_mode = false;
        editor_state.clear_connection();
    };
    if let Some(demo) = place_catalog {
        place(&mut editor_state, demo.title.clone(), &demo.workspace);
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

    let mut open = |editor_state: &mut EditorState,
                    name: String,
                    ws: Arc<WorkspaceDocument>,
                    read_only: bool| {
        editor_state.current_workspace = Some(name.clone());
        editor_state.workspace_file_name = name.clone();
        editor_state.selected_gate_type = None;
        editor_state.clear_connection();
        load_workspace_events.write(LoadWorkspaceEvent {
            name,
            workspace: ws,
            read_only,
        });
    };

    if let Some(demo) = open_catalog {
        editor_state.status = None;
        open(
            &mut editor_state,
            demo.title.clone(),
            Arc::clone(&demo.workspace),
            true,
        );
    }

    // An imported netlist opens as an unsaved, editable workspace.
    if import_rtl {
        let path = editor_state.rtl_import_path.trim().to_string();
        let result = std::fs::read_to_string(&path)
            .map_err(|e| e.to_string())
            .and_then(|text| {
                import_yosys_json(&text, &ImportOptions::default()).map_err(|e| e.to_string())
            });
        match result {
            Ok(import) => {
                editor_state.status = Some(if import.warnings.is_empty() {
                    format!("Imported {path}. Save the workspace to keep it.")
                } else {
                    format!("Imported {path}: {}", import.warnings.join("; "))
                });
                open(
                    &mut editor_state,
                    import.name,
                    Arc::new(import.workspace),
                    false,
                );
            }
            Err(err) => {
                warn!("Cannot import {path}: {err}");
                editor_state.status = Some(format!("Cannot import {path}: {err}"));
            }
        }
    }

    if new_workspace {
        editor_state.status = None;
        let root = crate::sim::CircuitDocument::new("workspace");
        open(
            &mut editor_state,
            "workspace".into(),
            Arc::new(WorkspaceDocument::new(root)),
            false,
        );
    }

    // An editable copy under fresh ids, saved right away so it is listed
    // with the saved workspaces.
    if copy_demo && let Some(demo) = open_demo {
        let name = format!("{} (copy)", demo.title);
        let copy = demos::editable_copy(&demo.workspace, &name);
        editor_state.status = Some(match save_workspace(&name, &copy) {
            Ok(()) => format!("Copied \"{}\" to your workspace \"{name}\".", demo.title),
            Err(err) => format!("Copied \"{}\" (not saved to disk: {err}).", demo.title),
        });
        editor_state.saved_workspaces = None;
        open(&mut editor_state, name, Arc::new(copy), false);
    }

    if let Some(name) = load_saved {
        // Reads the disk once, on the click.
        match read_workspace(&name) {
            Ok(ws) => {
                editor_state.status = None;
                open(&mut editor_state, name, Arc::new(ws), false);
            }
            Err(err) => {
                warn!("Cannot load workspace {name}: {err}");
                editor_state.status = Some(err.to_string());
            }
        }
    }

    if editor_state.connection_mode {
        let source = editor_state
            .connection_source
            .and_then(|e| gates.get(e).ok());
        let sink = editor_state.connection_sink.and_then(|e| gates.get(e).ok());
        egui::Window::new("Wire Builder")
            .resizable(false)
            .show(&root_ui, |ui| {
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
                if ui
                    .add_enabled(widths_match, egui::Button::new("Create Wire"))
                    .clicked()
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
    egui::ComboBox::from_label(label)
        .selected_text(describe(clamped))
        .show_ui(ui, |ui| {
            for pin in 0..widths.len() {
                ui.selectable_value(selected_pin, pin as u8, describe(pin));
            }
        });
    widths.get(usize::from(*selected_pin)).copied()
}
