//! Bevy systems for the simulation editor view: visuals, picking, egui
//! panels, and workspace save.
//!
//! Visuals read the authoring components and the probe projections
//! ([`GateValues`], [`WireState`]); edits are sent as authoring messages.

use bevy::ecs::system::SystemParam;
use bevy::{prelude::*, window::PrimaryWindow};
use bevy_egui::{EguiContexts, egui};
use uuid::Uuid;

use crate::{
    MainCamera, UiPointerState, editor::workspace::save_workspace,
    temp::GateComponentSelectionEvent,
};

use super::{
    AuthoringFeedback, CircuitLibrary, CircuitReader, DeleteGate, DeleteWire, DisplayConfig,
    DisplayConfigChanged, DisplayRadix, GateIdentity, GateKind, GateLabel, GateLabelChanged,
    GateType, GateValues, InputDrive, SaveWorkspaceEvent, Selected, SelectedGate,
    SetGateKind, SetInputValue, SimBridge, SimDiagnostics, SimulationConfig, TickRateMeter,
    WireEndpoints, WireState, WorkspaceRevision, spatial_grid::SpatialGrid,
    view::{CircuitView, InstanceViewItem, OpenInstanceView},
};

#[derive(Component)]
pub struct SimBackground;

// ─────────────────────────────────────────────────────────────────────────────
// Background and camera
// ─────────────────────────────────────────────────────────────────────────────

/// Draw a background behind all simulation entities.
pub fn draw_sim_background_system(mut commands: Commands, query: Query<Entity, With<SimBackground>>) {
    if !query.is_empty() {
        return;
    }
    commands.spawn((
        SimBackground,
        Sprite { color: Color::srgba(0.050, 0.229, 0.793, 1.000), ..Default::default() },
        Transform {
            translation: Vec3::new(0.0, 0.0, -1000.0),
            scale: Vec3::new(8000.0, 6000.0, 1.0),
            ..Default::default()
        },
        Visibility::Visible,
    ));
}

/// Fit the camera to the circuit after a workspace load.
pub fn fit_camera_to_circuit_system(
    active: Res<crate::sim::ActiveCircuit>,
    view: Res<CircuitView>,
    windows: Query<&Window, With<PrimaryWindow>>,
    mut cameras: Query<(&mut Projection, &mut Transform), With<MainCamera>>,
    gates: Query<&Transform, (With<GateIdentity>, Without<MainCamera>)>,
) {
    if !active.is_changed() && !view.is_changed() {
        return;
    }
    let (Ok((mut projection, mut transform)), Ok(window)) = (cameras.single_mut(), windows.single())
    else {
        return;
    };
    let mut min = Vec2::splat(f32::INFINITY);
    let mut max = Vec2::splat(f32::NEG_INFINITY);
    for t in &gates {
        min = min.min(t.translation.truncate());
        max = max.max(t.translation.truncate());
    }
    if !min.x.is_finite() {
        return;
    }
    let center = (min + max) * 0.5;
    let size = (max - min).max(Vec2::ONE);
    let scale = ((size.x / window.width().max(1.0)).max(size.y / window.height().max(1.0)) * 1.3)
        .clamp(0.1, 10.0);
    if let Projection::Orthographic(ortho) = projection.as_mut() {
        ortho.scale = scale;
        ortho.near = -10_000.0;
        ortho.far = 10_000.0;
    }
    transform.translation = Vec3::new(center.x, center.y, 1000.0);
}

// ─────────────────────────────────────────────────────────────────────────────
// Spatial grid (gate visuals live in `super::visuals`)
// ─────────────────────────────────────────────────────────────────────────────

type MovedGate = (With<GateIdentity>, Changed<Transform>);

/// Keep the spatial grid in step with gate positions.
pub fn update_spatial_grid_system(
    mut grid: ResMut<SpatialGrid>,
    moved: Query<(Entity, &Transform), MovedGate>,
    mut removed: RemovedComponents<GateIdentity>,
) {
    for entity in removed.read() {
        grid.remove(entity);
    }
    for (entity, transform) in &moved {
        grid.update(entity, transform.translation.truncate());
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// User interaction
// ─────────────────────────────────────────────────────────────────────────────

fn cursor_world_pos(
    window: &Window,
    camera: &Camera,
    camera_transform: &GlobalTransform,
) -> Option<Vec2> {
    let screen_pos = window.cursor_position()?;
    let ray = camera.viewport_to_world(camera_transform, screen_pos).ok()?;
    let distance = -ray.origin.z / ray.direction.z;
    Some((ray.origin + ray.direction * distance).truncate())
}

fn hit(transform: &Transform, kind: &GateKind, world: Vec2) -> bool {
    let half = kind.size() * 0.5;
    let local = transform.to_matrix().inverse().transform_point3(world.extend(0.0)).truncate();
    local.x.abs() <= half.x && local.y.abs() <= half.y
}

/// Left-click selects a gate.
pub fn update_inputs_on_left_click(
    q_window: Query<&Window, With<PrimaryWindow>>,
    ui_pointer: Res<UiPointerState>,
    mouse_button_input: Res<ButtonInput<MouseButton>>,
    q_camera: Query<(&Camera, &GlobalTransform)>,
    query: Query<(Entity, &Transform, &GateType)>,
    mut select_writer: MessageWriter<GateComponentSelectionEvent>,
) {
    if ui_pointer.over_egui || !mouse_button_input.just_pressed(MouseButton::Left) {
        return;
    }
    let (Ok((camera, camera_transform)), Ok(window)) = (q_camera.single(), q_window.single()) else {
        return;
    };
    let Some(world) = cursor_world_pos(window, camera, camera_transform) else { return };
    if let Some((entity, ..)) = query.iter().find(|(_, t, ty)| hit(t, &ty.kind, world)) {
        select_writer.write(GateComponentSelectionEvent { entity });
    }
}

/// Update [`SelectedGate`] when a selection event fires.
pub fn update_selected_gate(
    mut event_reader: MessageReader<GateComponentSelectionEvent>,
    mut selected_gate: ResMut<SelectedGate>,
) {
    for event in event_reader.read() {
        selected_gate.0 = Some(event.entity);
    }
}

/// Right-click to select and drag gates.
#[allow(clippy::too_many_arguments)]
pub fn gate_selection_system(
    q_window: Query<&Window, With<PrimaryWindow>>,
    q_camera: Query<(&Camera, &GlobalTransform)>,
    grid: Res<SpatialGrid>,
    ui_pointer: Res<UiPointerState>,
    mouse_button_input: Res<ButtonInput<MouseButton>>,
    query: Query<(&Transform, &GateType), With<GateIdentity>>,
    mut commands: Commands,
    selected_query: Query<Entity, With<Selected>>,
) {
    if ui_pointer.over_egui || !mouse_button_input.just_pressed(MouseButton::Right) {
        return;
    }
    for e in &selected_query {
        commands.entity(e).remove::<Selected>();
    }
    let (Ok(window), Ok((camera, global_transform))) = (q_window.single(), q_camera.single()) else {
        return;
    };
    let Some(world) = cursor_world_pos(window, camera, global_transform) else { return };
    for entity in grid.query_near(world, 100.0) {
        if let Ok((transform, ty)) = query.get(entity)
            && hit(transform, &ty.kind, world)
        {
            let offset = transform.translation.truncate() - world;
            commands.entity(entity).insert(Selected { offset });
            break;
        }
    }
}

/// Move the selected gate while the right mouse button is held.
pub fn drag_selected_system(
    q_window: Query<&Window, With<PrimaryWindow>>,
    q_camera: Query<(&Camera, &GlobalTransform)>,
    ui_pointer: Res<UiPointerState>,
    mut query: Query<(&mut Transform, &Selected), With<GateIdentity>>,
    mouse_button_input: Res<ButtonInput<MouseButton>>,
) {
    if ui_pointer.over_egui || !mouse_button_input.pressed(MouseButton::Right) {
        return;
    }
    let (Ok((camera, camera_transform)), Ok(window)) = (q_camera.single(), q_window.single()) else {
        return;
    };
    let Some(world) = cursor_world_pos(window, camera, camera_transform) else { return };
    for (mut transform, selected) in &mut query {
        let new_pos = world + selected.offset;
        transform.translation.x = new_pos.x;
        transform.translation.y = new_pos.y;
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Egui panels
// ─────────────────────────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
pub fn simulation_config_ui_system(
    mut sim_config: ResMut<SimulationConfig>,
    mut tick_rate_meter: ResMut<TickRateMeter>,
    mut contexts: EguiContexts,
    mut bridge: ResMut<SimBridge>,
    diagnostics: Res<SimDiagnostics>,
    feedback: Res<AuthoringFeedback>,
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
        if ui.checkbox(&mut sim_config.unlimited_tick_rate, "Unlimited Tick Rate").changed() {
            tick_rate_meter.reset(tick_count);
        }
        ui.label(match tick_rate_meter.average_hz {
            Some(rate) => format!("Average tick rate: {}", format_si_hz(rate)),
            None => "Average tick rate: measuring…".to_owned(),
        });
        ui.add_enabled(
            !sim_config.unlimited_tick_rate,
            egui::Slider::new(&mut sim_config.tick_rate, 0.1..=10_000_000.0)
                .logarithmic(true)
                .text("Tick Rate"),
        );

        ui.separator();
        ui.label(format!("Sim ticks: {tick_count}"));
        ui.label(format!("Running: {}", bridge.is_running()));
        ui.horizontal(|ui| {
            let running = bridge.is_running();
            if ui.button(if running { "Pause" } else { "Play" }).clicked() {
                bridge.set_running(!running);
            }
            if ui.button("Step").clicked() {
                bridge.step(1);
            }
        });
        match diagnostics.running_revision {
            Some(rev) => ui.label(format!("Circuit revision: {rev}")),
            None => ui.label("Circuit: compiling…"),
        };
        if let Some(error) = &diagnostics.compile_error {
            ui.separator();
            ui.colored_label(
                egui::Color32::from_rgb(255, 120, 90),
                format!("Edit {} not applied; the previous circuit keeps running:", error.revision),
            );
            for message in &error.messages {
                ui.colored_label(egui::Color32::from_rgb(255, 120, 90), message);
            }
            if error.loops.iter().any(|l| !l.is_empty()) {
                ui.colored_label(
                    egui::Color32::from_rgb(255, 120, 90),
                    "The gates and wires on the loop are outlined in red.",
                );
            }
        }
        if let Some(message) = &feedback.message {
            ui.separator();
            ui.colored_label(egui::Color32::from_rgb(255, 200, 90), message);
        }
    });
    Ok(())
}

pub(crate) fn format_si_hz(rate: f64) -> String {
    const UNITS: [(&str, f64); 4] =
        [("GHz", 1_000_000_000.0), ("MHz", 1_000_000.0), ("kHz", 1_000.0), ("Hz", 1.0)];
    let (unit, scale) = UNITS.into_iter().find(|(_, scale)| rate >= *scale).unwrap_or(("Hz", 1.0));
    format!("{:.1} {unit}", rate / scale)
}

/// Message writers used by the gate properties window.
#[derive(SystemParam)]
pub struct GateEditWriters<'w> {
    labels: MessageWriter<'w, GateLabelChanged>,
    kinds: MessageWriter<'w, SetGateKind>,
    inputs: MessageWriter<'w, SetInputValue>,
    displays: MessageWriter<'w, DisplayConfigChanged>,
    delete_gate: MessageWriter<'w, DeleteGate>,
    delete_wire: MessageWriter<'w, DeleteWire>,
    open: MessageWriter<'w, OpenInstanceView>,
}

type SelectedGateQuery = (
    &'static GateIdentity,
    &'static GateType,
    &'static GateLabel,
    &'static GateValues,
    Option<&'static InputDrive>,
    Option<&'static DisplayConfig>,
    Has<InstanceViewItem>,
);

fn width_drag(ui: &mut egui::Ui, label: &str, value: &mut u8, range: std::ops::RangeInclusive<u8>) {
    ui.horizontal(|ui| {
        ui.label(label);
        ui.add(egui::DragValue::new(value).range(range));
    });
}

/// Edit the parameters of a kind. Returns the edited kind.
fn edit_kind_params(ui: &mut egui::Ui, kind: &GateKind) -> GateKind {
    let mut k = kind.clone();
    match &mut k {
        GateKind::Clock { period, duty, phase } => {
            ui.horizontal(|ui| {
                ui.label("Period");
                ui.add(egui::DragValue::new(period).range(1..=u32::MAX));
            });
            let max = *period;
            ui.horizontal(|ui| {
                ui.label("Duty");
                ui.add(egui::DragValue::new(duty).range(0..=max));
            });
            ui.horizontal(|ui| {
                ui.label("Phase");
                ui.add(egui::DragValue::new(phase));
            });
        }
        GateKind::Merge { widths } => {
            let mut parts = widths.len() as u8;
            let mut part_w = widths.first().copied().unwrap_or(1);
            width_drag(ui, "Parts", &mut parts, 1..=64);
            width_drag(ui, "Part width", &mut part_w, 1..=64);
            let uniform = widths.iter().all(|&w| w == part_w);
            if (usize::from(parts) != widths.len() || !uniform)
                && u32::from(parts) * u32::from(part_w) <= 64
            {
                *widths = vec![part_w; usize::from(parts)];
            }
            ui.label(format!("Output: {} bits", widths.iter().map(|&w| u32::from(w)).sum::<u32>()));
        }
        GateKind::SrLatch | GateKind::SubCircuit { .. } => {}
        _ => {
            if let Some(mut w) = k.width() {
                width_drag(ui, "Width", &mut w, 1..=64);
                if Some(w) != k.width() {
                    k = k.with_width(w);
                }
            }
            match &mut k {
                GateKind::Const { w, value } => {
                    ui.horizontal(|ui| {
                        ui.label("Value");
                        ui.add(egui::DragValue::new(value).hexadecimal(1, false, true));
                    });
                    *value &= logi_sim::netlist::mask(*w);
                }
                GateKind::Split { w, lo, len } => {
                    let w = *w;
                    width_drag(ui, "Low bit", lo, 0..=w - 1);
                    let max_len = w - *lo;
                    width_drag(ui, "Length", len, 1..=max_len);
                }
                GateKind::Extend { from, to, signed } => {
                    let to = *to;
                    width_drag(ui, "From width", from, 1..=to);
                    ui.checkbox(signed, "Sign extend");
                }
                GateKind::Ram { addr_bits, .. } | GateKind::Rom { addr_bits, .. } => {
                    width_drag(ui, "Address bits", addr_bits, 1..=logi_sim::netlist::MAX_ADDR_BITS);
                }
                _ => {}
            }
        }
    }
    k
}

/// The properties window of the selected gate: values, parameters, input
/// drive, wires, delete.
#[allow(clippy::too_many_arguments)]
pub fn ui_for_selected_gate(
    mut contexts: EguiContexts,
    mut sel: ResMut<SelectedGate>,
    library: Res<CircuitLibrary>,
    view: Res<CircuitView>,
    keys: Res<ButtonInput<KeyCode>>,
    gates: Query<SelectedGateQuery>,
    names: Query<(&GateIdentity, &GateLabel, &GateType)>,
    wires: Query<(Entity, &WireEndpoints, &WireState)>,
    mut writers: GateEditWriters,
    game_state: Res<State<crate::GameState>>,
) -> Result<(), BevyError> {
    if game_state.get() != &crate::GameState::Simulating {
        return Ok(());
    }
    let Some(entity) = sel.0 else { return Ok(()) };
    let Ok((identity, ty, label, values, drive, display, in_view)) = gates.get(entity) else {
        sel.0 = None;
        return Ok(());
    };
    let ctx = contexts.ctx_mut()?;

    if !ctx.egui_wants_keyboard_input()
        && (keys.just_pressed(KeyCode::Delete) || keys.just_pressed(KeyCode::Backspace))
    {
        writers.delete_gate.write(DeleteGate { entity });
        sel.0 = None;
        return Ok(());
    }

    let gate_name = |uuid: Uuid| {
        names
            .iter()
            .find(|(id, ..)| id.uuid == uuid)
            .map(|(_, l, t)| if l.value.is_empty() { t.kind.display_name().to_string() } else { l.value.clone() })
            .unwrap_or_else(|| "?".into())
    };
    let (in_names, out_names) = library.pin_names(&ty.kind);
    let mut close = false;

    egui::Window::new("Gate Properties").collapsible(false).show(ctx, |ui| {
        ui.label(format!("Kind: {}", ty.kind.display_name()));
        ui.label(format!("UUID: {}", identity.uuid));

        let mut label_text = label.value.clone();
        if ui.text_edit_singleline(&mut label_text).changed() {
            writers.labels.write(GateLabelChanged { gate_uuid: identity.uuid, label: label_text });
        }

        if in_view && ty.kind.is_input() {
            ui.separator();
            ui.label(if view.is_live() {
                "Driven by the parent circuit (live value below); change it there."
            } else {
                "Driven by the parent circuit."
            });
        } else if let (Some(drive), GateKind::Input { w }) = (drive, &ty.kind) {
            ui.separator();
            let mut value = drive.value;
            if *w == 1 {
                let mut on = value != 0;
                if ui.checkbox(&mut on, "Driven value").changed() {
                    value = u64::from(on);
                }
            } else {
                ui.horizontal(|ui| {
                    ui.label("Driven value");
                    ui.add(egui::DragValue::new(&mut value).hexadecimal(1, false, true));
                });
                if *w <= 16 {
                    ui.horizontal_wrapped(|ui| {
                        for bit in (0..*w).rev() {
                            let mut on = (value >> bit) & 1 != 0;
                            if ui.checkbox(&mut on, format!("{bit}")).changed() {
                                value ^= 1 << bit;
                            }
                        }
                    });
                }
            }
            if value != drive.value {
                writers.inputs.write(SetInputValue { entity, value });
            }
        }

        ui.separator();
        ui.label("Pins");
        for (i, v) in values.inputs.iter().enumerate() {
            let name = in_names.get(i).map_or("?", String::as_str);
            ui.label(format!("  in {i} ({name}) = {v:#x}"));
        }
        for (i, v) in values.outputs.iter().enumerate() {
            let name = out_names.get(i).map_or("?", String::as_str);
            ui.label(format!("  out {i} ({name}) = {v:#x}"));
        }

        ui.separator();
        let edited = edit_kind_params(ui, &ty.kind);
        if edited != ty.kind {
            writers.kinds.write(SetGateKind { entity, kind: edited });
        }

        if let Some(config) = display {
            let mut radix = config.radix;
            egui::ComboBox::from_label("Radix")
                .selected_text(format!("{radix:?}"))
                .show_ui(ui, |ui| {
                    for r in [DisplayRadix::Hexadecimal, DisplayRadix::Decimal, DisplayRadix::Binary] {
                        ui.selectable_value(&mut radix, r, format!("{r:?}"));
                    }
                });
            let mut leading = config.leading_zeroes;
            ui.checkbox(&mut leading, "Leading zeroes");
            if radix != config.radix || leading != config.leading_zeroes {
                writers.displays.write(DisplayConfigChanged {
                    gate_uuid: identity.uuid,
                    radix,
                    leading_zeroes: leading,
                });
            }
        }

        if let GateKind::SubCircuit { definition } = ty.kind {
            ui.separator();
            let name = library.get(definition).map_or("(missing definition)", |d| d.name.as_str());
            ui.label(format!("Subcircuit: {name}"));
            ui.label(format!("Definition id: {definition}"));
            if ui.button("Open instance (live values)").clicked() {
                writers.open.write(OpenInstanceView { entity });
            }
        }

        ui.separator();
        ui.label("Wires");
        for (wire, ep, state) in &wires {
            let text = if ep.sink_gate == identity.uuid {
                format!("in {} <- {} out {}", ep.sink_pin, gate_name(ep.source_gate), ep.source_pin)
            } else if ep.source_gate == identity.uuid {
                format!("out {} -> {} in {}", ep.source_pin, gate_name(ep.sink_gate), ep.sink_pin)
            } else {
                continue;
            };
            ui.horizontal(|ui| {
                let text = if state.connected { text } else { format!("{text} (not simulated)") };
                ui.label(text);
                if ui.small_button("Delete").clicked() {
                    writers.delete_wire.write(DeleteWire { entity: wire });
                }
            });
        }

        ui.separator();
        ui.horizontal(|ui| {
            if ui.button("Delete gate").clicked() {
                writers.delete_gate.write(DeleteGate { entity });
                close = true;
            }
            if ui.button("Close").clicked() {
                close = true;
            }
        });
    });
    if close {
        sel.0 = None;
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Save
// ─────────────────────────────────────────────────────────────────────────────

pub fn save_on_exit_system(mut exit_events: MessageReader<AppExit>, reader: CircuitReader) {
    if exit_events.read().next().is_none() {
        return;
    }
    let name = if reader.active.name.trim().is_empty() {
        "simulation".to_string()
    } else {
        reader.active.name.clone()
    };
    match save_workspace(&name, &reader.workspace()) {
        Ok(()) => info!("Saved simulation workspace: {name}"),
        Err(err) => error!("Failed to save simulation: {err}"),
    }
}

/// Write the open circuit to disk on [`SaveWorkspaceEvent`].
pub fn workspace_io_system(
    mut save_events: MessageReader<SaveWorkspaceEvent>,
    reader: CircuitReader,
    mut revision: ResMut<WorkspaceRevision>,
    mut feedback: ResMut<AuthoringFeedback>,
    editor_state: Option<ResMut<crate::editor::EditorState>>,
) {
    let mut saved = false;
    for ev in save_events.read() {
        match save_workspace(&ev.name, &reader.workspace()) {
            Ok(()) => {
                revision.mark_saved();
                feedback.message = Some(format!("Saved workspace \"{}\"", ev.name));
                saved = true;
            }
            Err(err) => feedback.message = Some(format!("Failed to save \"{}\": {err}", ev.name)),
        }
    }
    if saved && let Some(mut editor_state) = editor_state {
        editor_state.saved_workspaces = None;
    }
}
