//! Bevy systems for the simulation editor view: visuals, picking, egui
//! panels, and workspace save.
//!
//! Visuals read the authoring components and the probe projections
//! ([`GateValues`], [`WireState`]); edits are sent as authoring messages.

use bevy::ecs::system::SystemParam;
use bevy::{prelude::*, window::PrimaryWindow};
use bevy_egui::{EguiContexts, egui};
use uuid::Uuid;

use crate::camera::{FIT_PADDING, FitView, fit_view, scale_for_wpp};
use crate::editor::parts::{kind_blurb, kind_title};
use crate::theme::{Family, Theme};
use crate::ui_style::{
    HudIcon, HudLayout, TOP_CARD_HEIGHT, caption, card, chip, chip_sized, chunky_button,
    chunky_icon_button, egui_color, format_live_value, heavy, rule, stroke,
};
use crate::{
    MainCamera, UiPointerState, editor::workspace::save_workspace,
    temp::GateComponentSelectionEvent,
};

use super::{
    AuthoringFeedback, CircuitLibrary, CircuitReader, DeleteGate, DeleteWire, DisplayConfig,
    DisplayConfigChanged, DisplayRadix, GateIdentity, GateKind, GateLabel, GateLabelChanged,
    GateType, GateValues, InputDrive, SaveWorkspaceEvent, Selected, SelectedGate, SetGateKind,
    SetInputValue, SimBridge, SimDiagnostics, SimulationConfig, TickRateMeter, WireEndpoints,
    WireState, WorkspaceRevision, art,
    spatial_grid::SpatialGrid,
    view::{CircuitView, InstanceViewItem, OpenInstanceView},
};

/// The bench: one quad with the dot-grid texture repeated across it.
#[derive(Component)]
pub struct SimBackground {
    image: Handle<Image>,
}

// ─────────────────────────────────────────────────────────────────────────────
// Background and camera
// ─────────────────────────────────────────────────────────────────────────────

/// Draw the bench (the dot grid every 20 units) behind all simulation
/// entities: a single mesh whose material repeats a small dot texture, so
/// the grid costs one draw at any size.
pub fn draw_sim_background_system(
    mut commands: Commands,
    query: Query<(), With<SimBackground>>,
    theme: Res<Theme>,
    meshes: Option<ResMut<Assets<Mesh>>>,
    materials: Option<ResMut<Assets<ColorMaterial>>>,
    images: Option<ResMut<Assets<Image>>>,
) {
    if !query.is_empty() {
        return;
    }
    let (Some(mut meshes), Some(mut materials), Some(mut images)) = (meshes, materials, images)
    else {
        return;
    };
    let image = images.add(art::bench_image(&theme.palette));
    let material = materials.add(art::bench_material(image.clone()));
    commands.spawn((
        SimBackground { image },
        Mesh2d(meshes.add(Rectangle::new(art::BENCH_SIZE, art::BENCH_SIZE))),
        MeshMaterial2d(material),
        Transform::from_xyz(0.0, 0.0, -1000.0),
        Visibility::Visible,
    ));
}

/// Repaint the bench texture when the theme changes.
pub fn sync_bench_theme_system(
    theme: Res<Theme>,
    benches: Query<&SimBackground>,
    images: Option<ResMut<Assets<Image>>>,
) {
    if !theme.is_changed() {
        return;
    }
    let Some(mut images) = images else { return };
    for bench in &benches {
        if let Some(mut image) = images.get_mut(&bench.image) {
            image.data = Some(art::bench_texels(&theme.palette));
        }
    }
}

type FitGate = (With<GateIdentity>, Without<MainCamera>);

/// Fit the camera to the circuit after a workspace load, a view change
/// or a [`FitView`] request: the circuit is centred in the canvas area
/// the HUD cards leave free ([`HudLayout::free_area`]).
pub fn fit_camera_to_circuit_system(
    active: Res<crate::sim::ActiveCircuit>,
    view: Res<CircuitView>,
    mut requests: MessageReader<FitView>,
    windows: Query<&Window, With<PrimaryWindow>>,
    mut cameras: Query<(&mut Projection, &mut Transform), With<MainCamera>>,
    gates: Query<(&Transform, &GateType), FitGate>,
) {
    let requested = requests.read().count() > 0;
    if !active.is_changed() && !view.is_changed() && !requested {
        return;
    }
    let (Ok((mut projection, mut transform)), Ok(window)) =
        (cameras.single_mut(), windows.single())
    else {
        return;
    };
    let Some(content) = gates
        .iter()
        .map(|(t, ty)| Rect::from_center_size(t.translation.truncate(), ty.kind.size()))
        .reduce(|a, b| a.union(b))
    else {
        return;
    };
    let screen = window.size();
    let free = HudLayout::new(screen).free_area();
    let (centre, wpp) = fit_view(content, screen, free, FIT_PADDING);
    if let Projection::Orthographic(ortho) = projection.as_mut() {
        ortho.scale = scale_for_wpp(ortho, screen.y, wpp);
        ortho.near = -10_000.0;
        ortho.far = 10_000.0;
    }
    transform.translation = Vec3::new(centre.x, centre.y, 1000.0);
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
    let ray = camera
        .viewport_to_world(camera_transform, screen_pos)
        .ok()?;
    let distance = -ray.origin.z / ray.direction.z;
    Some((ray.origin + ray.direction * distance).truncate())
}

fn hit(transform: &Transform, kind: &GateKind, world: Vec2) -> bool {
    let half = kind.size() * 0.5;
    let local = transform
        .to_matrix()
        .inverse()
        .transform_point3(world.extend(0.0))
        .truncate();
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
    let (Ok((camera, camera_transform)), Ok(window)) = (q_camera.single(), q_window.single())
    else {
        return;
    };
    let Some(world) = cursor_world_pos(window, camera, camera_transform) else {
        return;
    };
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
    active: Res<crate::sim::ActiveCircuit>,
) {
    // Gates of a read-only demo cannot be dragged.
    if ui_pointer.over_egui
        || active.read_only
        || !mouse_button_input.just_pressed(MouseButton::Right)
    {
        return;
    }
    for e in &selected_query {
        commands.entity(e).remove::<Selected>();
    }
    let (Ok(window), Ok((camera, global_transform))) = (q_window.single(), q_camera.single())
    else {
        return;
    };
    let Some(world) = cursor_world_pos(window, camera, global_transform) else {
        return;
    };
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
    let (Ok((camera, camera_transform)), Ok(window)) = (q_camera.single(), q_window.single())
    else {
        return;
    };
    let Some(world) = cursor_world_pos(window, camera, camera_transform) else {
        return;
    };
    for (mut transform, selected) in &mut query {
        let new_pos = world + selected.offset;
        transform.translation.x = new_pos.x;
        transform.translation.y = new_pos.y;
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Egui panels
// ─────────────────────────────────────────────────────────────────────────────

/// The speeds offered as chips on the transport card (ticks per second).
pub const SPEED_CHIPS: [(&str, f64); 3] = [("1 Hz", 1.0), ("60 Hz", 60.0), ("1 kHz", 1000.0)];

/// A tick count with thin groups of three digits (`48 213`).
pub fn format_tick(tick: u64) -> String {
    let digits = tick.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push('\u{2009}');
        }
        out.push(c);
    }
    out
}

/// The transport card, top centre: run, pause, step, reset (also `R`),
/// speed chips (and a `⋯` popover with the exact rate), and the tick.
#[allow(clippy::too_many_arguments)]
pub fn simulation_config_ui_system(
    mut sim_config: ResMut<SimulationConfig>,
    mut tick_rate_meter: ResMut<TickRateMeter>,
    mut contexts: EguiContexts,
    mut bridge: ResMut<SimBridge>,
    diagnostics: Res<SimDiagnostics>,
    time: Res<Time>,
    game_state: Res<State<crate::GameState>>,
    theme: Res<Theme>,
    keys: Res<ButtonInput<KeyCode>>,
    mut more_open: Local<bool>,
) -> Result<(), BevyError> {
    if game_state.get() != &crate::GameState::Simulating {
        return Ok(());
    }

    let tick_count = bridge.tick_count();
    tick_rate_meter.sample(tick_count, time.delta_secs_f64());

    let palette = theme.palette;
    let ctx = contexts.ctx_mut()?;
    if !crate::ui_style::workshop_fonts_ready(ctx) {
        return Ok(());
    }
    let size = ctx.content_rect().size();
    let layout = HudLayout::new(Vec2::new(size.x, size.y));
    let rate = match tick_rate_meter.average_hz {
        Some(rate) => format_si_hz(rate),
        None => "measuring…".to_owned(),
    };
    let mut speed: Option<Option<f64>> = None;
    let mut reset = keys.just_pressed(KeyCode::KeyR) && !ctx.egui_wants_keyboard_input();
    egui::Area::new(egui::Id::new("hud_transport"))
        .anchor(egui::Align2::CENTER_TOP, [0.0, layout.margin])
        .order(egui::Order::Middle)
        .show(ctx, |ui| {
            card(&palette)
                .inner_margin(egui::Margin::symmetric(10, 0))
                .show(ui, |ui| {
                    ui.set_height(TOP_CARD_HEIGHT - 5.0);
                    ui.horizontal_centered(|ui| {
                        ui.spacing_mut().item_spacing.x = 10.0;
                        let running = bridge.is_running();
                        let run = if running { "▶ Running" } else { "▶ Run" };
                        if chunky_button(ui, &palette, run, running).clicked() && !running {
                            bridge.set_running(true);
                        }
                        if chunky_icon_button(ui, &palette, HudIcon::Pause, false)
                            .on_hover_text("Pause")
                            .clicked()
                            && running
                        {
                            bridge.set_running(false);
                        }
                        if chunky_button(ui, &palette, "Step", false)
                            .on_hover_text("One tick")
                            .clicked()
                        {
                            bridge.step(1);
                        }
                        if chunky_button(ui, &palette, "Reset", false)
                            .on_hover_text(
                                "Back to tick 0: registers and memories to their \
                                 initial values, inputs kept (R)",
                            )
                            .clicked()
                        {
                            reset = true;
                        }
                        rule(ui, &palette, 30.0);
                        ui.spacing_mut().item_spacing.x = 6.0;
                        for (label, hz) in SPEED_CHIPS {
                            let on = !sim_config.unlimited_tick_rate
                                && (sim_config.tick_rate - hz).abs() < 1e-9;
                            if chip(ui, &palette, label, on).clicked() {
                                speed = Some(Some(hz));
                            }
                        }
                        if chip(ui, &palette, "Max", sim_config.unlimited_tick_rate)
                            .on_hover_text("As fast as the machine goes")
                            .clicked()
                        {
                            speed = Some(None);
                        }
                        let custom = !sim_config.unlimited_tick_rate
                            && !SPEED_CHIPS
                                .iter()
                                .any(|(_, hz)| (sim_config.tick_rate - hz).abs() < 1e-9);
                        if chip(ui, &palette, "…", custom || *more_open)
                            .on_hover_text("Exact rate")
                            .clicked()
                        {
                            *more_open = !*more_open;
                        }
                        ui.spacing_mut().item_spacing.x = 10.0;
                        rule(ui, &palette, 30.0);
                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing.y = 0.0;
                            ui.add_space(4.0);
                            ui.label(caption(&palette, "tick"));
                            ui.label(
                                egui::RichText::new(format_tick(tick_count))
                                    .monospace()
                                    .size(16.0),
                            )
                            .on_hover_text(format!("Measured rate: {rate}"));
                        });
                        ui.add_space(4.0);
                    });
                });
        });
    if reset {
        bridge.reset();
        tick_rate_meter.reset(0);
    }
    if let Some(choice) = speed {
        match choice {
            Some(hz) => {
                sim_config.unlimited_tick_rate = false;
                sim_config.tick_rate = hz;
            }
            None => sim_config.unlimited_tick_rate = true,
        }
        tick_rate_meter.reset(tick_count);
    }
    if *more_open {
        egui::Area::new(egui::Id::new("hud_transport_more"))
            .anchor(egui::Align2::CENTER_TOP, [0.0, layout.top_band()])
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                card(&palette).show(ui, |ui| {
                    if ui
                        .checkbox(&mut sim_config.unlimited_tick_rate, "Unlimited tick rate")
                        .changed()
                    {
                        tick_rate_meter.reset(tick_count);
                    }
                    ui.add_enabled(
                        !sim_config.unlimited_tick_rate,
                        egui::Slider::new(&mut sim_config.tick_rate, 0.1..=10_000_000.0)
                            .logarithmic(true)
                            .text("Tick rate (Hz)"),
                    );
                    ui.label(format!("Measured: {rate}"));
                    match diagnostics.running_revision {
                        Some(rev) => ui.label(format!("Circuit revision: {rev}")),
                        None => ui.label("Circuit: compiling…"),
                    };
                });
            });
    }
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
        GateKind::Clock {
            period,
            duty,
            phase,
        } => {
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
            ui.label(format!(
                "Output: {} bits",
                widths.iter().map(|&w| u32::from(w)).sum::<u32>()
            ));
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
                    width_drag(
                        ui,
                        "Address bits",
                        addr_bits,
                        1..=logi_sim::netlist::MAX_ADDR_BITS,
                    );
                }
                _ => {}
            }
        }
    }
    k
}

/// Kinds whose only parameter is their width (the inspector's width
/// chips); every other kind also gets [`edit_kind_params`].
fn width_only(kind: &GateKind) -> bool {
    use GateKind as G;
    matches!(
        kind,
        G::Input { .. }
            | G::Output { .. }
            | G::Display { .. }
            | G::Not { .. }
            | G::And { .. }
            | G::Or { .. }
            | G::Xor { .. }
            | G::Nand { .. }
            | G::Nor { .. }
            | G::Xnor { .. }
            | G::Mux { .. }
            | G::Switch { .. }
            | G::Add { .. }
            | G::Eq { .. }
            | G::LtU { .. }
            | G::ReduceOr { .. }
            | G::ReduceAnd { .. }
            | G::ReduceXor { .. }
            | G::Delay { .. }
            | G::Register { .. }
            | G::DLatch { .. }
            | G::SrLatch
            | G::SubCircuit { .. }
    )
}

/// Widths offered as chips in the inspector.
pub const WIDTH_CHIPS: [u8; 5] = [1, 4, 8, 16, 32];

/// A gate's pins in the order the inspector lists their live values:
/// outputs, then inputs with the buses before the 1-bit controls (so a
/// register reads Q, D, EN). Each is `(is_output, pin, name, width)`.
pub fn live_value_rows(
    in_widths: &[u8],
    out_widths: &[u8],
    in_names: &[String],
    out_names: &[String],
) -> Vec<(bool, usize, String, u8)> {
    let name = |names: &[String], i: usize, fallback: &str| {
        names
            .get(i)
            .filter(|n| !n.is_empty())
            .cloned()
            .unwrap_or_else(|| format!("{fallback}{i}"))
            .to_uppercase()
    };
    let outs = out_widths
        .iter()
        .enumerate()
        .map(|(i, w)| (true, i, name(out_names, i, "out"), *w));
    let mut ins: Vec<_> = in_widths
        .iter()
        .enumerate()
        .map(|(i, w)| (false, i, name(in_names, i, "in"), *w))
        .collect();
    let any_bus = ins.iter().any(|r| r.3 > 1);
    if any_bus {
        ins.sort_by_key(|r| r.3 <= 1);
    }
    outs.chain(ins).collect()
}

/// The inspector card of the selected part, on the right: what it is,
/// its label, width and parameters, live values, input drive, readout
/// settings, actions (watch, open inside, delete) and wires.
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
    theme: Res<Theme>,
    active: Res<crate::sim::ActiveCircuit>,
    editor: Option<ResMut<crate::editor::EditorState>>,
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
    if !crate::ui_style::workshop_fonts_ready(ctx) {
        return Ok(());
    }

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
            .map(|(_, l, t)| {
                if l.value.is_empty() {
                    t.kind.display_name().to_string()
                } else {
                    l.value.clone()
                }
            })
            .unwrap_or_else(|| "?".into())
    };
    let palette = theme.palette;
    let c = egui_color;
    let (in_names, out_names) = library.pin_names(&ty.kind);
    let (in_widths, out_widths) = library.pins(&ty.kind).unwrap_or_default();
    let family = art::family(&ty.kind);
    let editable = !active.read_only;
    let watching = editor
        .as_ref()
        .is_some_and(|e| e.watched.contains(&identity.uuid));
    let mut close = false;
    let mut toggle_watch = false;

    let size = ctx.content_rect().size();
    let layout = HudLayout::new(Vec2::new(size.x, size.y));
    let column = layout.inspector_rect();
    let frame = card(&palette).inner_margin(egui::Margin::same(16));
    let inner_w = column.width() - frame.total_margin().sum().x;
    let max_h = column.height() - frame.total_margin().sum().y;
    egui::Area::new(egui::Id::new("hud_inspector"))
        .anchor(
            egui::Align2::RIGHT_TOP,
            [-layout.margin, layout.top_band()],
        )
        .order(egui::Order::Middle)
        .show(ctx, |ui| {
            frame.show(ui, |ui| {
                ui.set_width(inner_w);
                egui::ScrollArea::vertical()
                    .max_height(max_h)
                    .min_scrolled_height(max_h)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 10.0;
                        // Header: family tile, title and blurb, close.
                        ui.horizontal(|ui| {
                            let (icon, _) =
                                ui.allocate_exact_size(egui::vec2(44.0, 44.0), egui::Sense::hover());
                            let tint = palette.family_fill(family);
                            ui.painter().rect(
                                icon,
                                egui::CornerRadius::same(11),
                                c(tint),
                                stroke(2.5, c(palette.ink)),
                                egui::StrokeKind::Inside,
                            );
                            let swatch = match family {
                                Family::State => palette.state,
                                Family::Io => palette.io,
                                Family::Logic => palette.ink,
                            };
                            ui.painter().rect_filled(
                                egui::Rect::from_center_size(icon.center(), egui::vec2(18.0, 16.0)),
                                egui::CornerRadius::same(3),
                                c(swatch),
                            );
                            ui.vertical(|ui| {
                                ui.spacing_mut().item_spacing.y = 0.0;
                                ui.add_space(2.0);
                                ui.label(
                                    egui::RichText::new(kind_title(&ty.kind)).font(heavy(18.0)),
                                )
                                .on_hover_text(format!("Id {}", identity.uuid));
                                ui.label(
                                    egui::RichText::new(kind_blurb(&ty.kind))
                                        .size(12.0)
                                        .color(c(palette.text_secondary)),
                                );
                            });
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                                if ui.small_button("×").on_hover_text("Close").clicked() {
                                    close = true;
                                }
                            });
                        });

                        // Label.
                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing.y = 4.0;
                            ui.label(caption(&palette, "Label"));
                            let mut label_text = label.value.clone();
                            let edit = egui::TextEdit::singleline(&mut label_text)
                                .font(egui::TextStyle::Monospace)
                                .interactive(editable)
                                .desired_width(f32::INFINITY);
                            if ui.add(edit).changed() {
                                writers.labels.write(GateLabelChanged {
                                    gate_uuid: identity.uuid,
                                    label: label_text,
                                });
                            }
                        });

                        // Width chips.
                        let width_kind = !matches!(
                            ty.kind,
                            GateKind::Merge { .. } | GateKind::SrLatch | GateKind::SubCircuit { .. }
                        );
                        if let (Some(w), true) = (ty.kind.width(), width_kind) {
                            ui.vertical(|ui| {
                                ui.spacing_mut().item_spacing.y = 4.0;
                                ui.label(caption(&palette, "Width"));
                                ui.scope(|ui| {
                                    ui.horizontal(|ui| {
                                        ui.spacing_mut().item_spacing.x = 6.0;
                                        let n = WIDTH_CHIPS.len() as f32;
                                        let chip_w = (ui.available_width() - 6.0 * (n - 1.0)) / n;
                                        for chip_width in WIDTH_CHIPS {
                                            if chip_sized(
                                                ui,
                                                &palette,
                                                chip_width.to_string(),
                                                w == chip_width,
                                                Some(chip_w),
                                            )
                                            .clicked()
                                                && w != chip_width
                                            {
                                                writers.kinds.write(SetGateKind {
                                                    entity,
                                                    kind: ty.kind.with_width(chip_width),
                                                });
                                            }
                                        }
                                    });
                                });
                            });
                        }

                        // The kind's other parameters (the width has its chips).
                        if !width_only(&ty.kind) {
                            let edited = edit_kind_params(ui, &ty.kind);
                            if edited != ty.kind {
                                writers.kinds.write(SetGateKind {
                                    entity,
                                    kind: edited,
                                });
                            }
                        }

                        // Input drive.
                        if in_view && ty.kind.is_input() {
                            ui.colored_label(
                                c(palette.io_text),
                                if view.is_live() {
                                    "Driven by the parent circuit (live value below); change it there."
                                } else {
                                    "Driven by the parent circuit."
                                },
                            );
                        } else if let (Some(drive), GateKind::Input { w }) = (drive, &ty.kind) {
                            ui.vertical(|ui| {
                                ui.spacing_mut().item_spacing.y = 4.0;
                                ui.label(caption(&palette, "Set by hand"));
                                let mut value = drive.value;
                                if *w == 1 {
                                    ui.horizontal(|ui| {
                                        if chip(ui, &palette, "off", value == 0).clicked() {
                                            value = 0;
                                        }
                                        if chip(ui, &palette, "on", value != 0).clicked() {
                                            value = 1;
                                        }
                                    });
                                } else {
                                    ui.add(egui::DragValue::new(&mut value).hexadecimal(1, false, true));
                                    if *w <= 16 {
                                        ui.horizontal_wrapped(|ui| {
                                            ui.spacing_mut().item_spacing.x = 4.0;
                                            for bit in (0..*w).rev() {
                                                let on = (value >> bit) & 1 != 0;
                                                if chip(ui, &palette, format!("{bit}"), on).clicked() {
                                                    value ^= 1 << bit;
                                                }
                                            }
                                        });
                                    }
                                }
                                if value != drive.value {
                                    writers.inputs.write(SetInputValue { entity, value });
                                }
                            });
                        }

                        // Live values.
                        let rows = live_value_rows(&in_widths, &out_widths, &in_names, &out_names);
                        if !rows.is_empty() {
                            let well = match family {
                                Family::State => palette.state_tint,
                                Family::Io => palette.io_tint,
                                Family::Logic => palette.hover,
                            };
                            egui::Frame::new()
                                .fill(c(well))
                                .stroke(stroke(2.0, c(palette.ink)))
                                .corner_radius(egui::CornerRadius::same(11))
                                .inner_margin(egui::Margin::symmetric(14, 10))
                                .show(ui, |ui| {
                                    ui.set_width(ui.available_width());
                                    egui::Grid::new("inspector_values")
                                        .num_columns(2)
                                        .spacing([14.0, 4.0])
                                        .show(ui, |ui| {
                                            for (is_out, pin, name, width) in &rows {
                                                let value = if *is_out {
                                                    values.outputs.get(*pin)
                                                } else {
                                                    values.inputs.get(*pin)
                                                }
                                                .copied()
                                                .unwrap_or(0);
                                                let colour = match (*is_out, *width > 1, value != 0) {
                                                    (true, ..) => palette.text,
                                                    (false, true, _) => palette.bus_text,
                                                    (false, false, true) => palette.on_text,
                                                    (false, false, false) => palette.text_faint,
                                                };
                                                ui.label(
                                                    egui::RichText::new(name)
                                                        .monospace()
                                                        .color(c(palette.text_secondary)),
                                                );
                                                ui.label(
                                                    egui::RichText::new(format_live_value(*width, value))
                                                        .monospace()
                                                        .color(c(colour)),
                                                );
                                                ui.end_row();
                                            }
                                        });
                                });
                        }

                        // Readout settings.
                        if let Some(config) = display {
                            ui.scope(|ui| {
                                let mut radix = config.radix;
                                ui.horizontal(|ui| {
                                    ui.spacing_mut().item_spacing.x = 6.0;
                                    for (r, name) in [
                                        (DisplayRadix::Hexadecimal, "hex"),
                                        (DisplayRadix::Decimal, "dec"),
                                        (DisplayRadix::Binary, "bin"),
                                    ] {
                                        if chip(ui, &palette, name, radix == r).clicked() {
                                            radix = r;
                                        }
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
                            });
                        }

                        if let GateKind::SubCircuit { definition } = ty.kind {
                            let name = library
                                .get(definition)
                                .map_or("(missing definition)", |d| d.name.as_str());
                            ui.label(egui::RichText::new(name).font(heavy(14.0)))
                                .on_hover_text(format!("Definition id {definition}"));
                        }

                        // Actions.
                        ui.horizontal(|ui| {
                            if editor.is_some()
                                && chunky_button(
                                    ui,
                                    &palette,
                                    if watching { "Watching" } else { "Watch" },
                                    watching,
                                )
                                .on_hover_text("List its value in the Signals card")
                                .clicked()
                            {
                                toggle_watch = true;
                            }
                            if matches!(ty.kind, GateKind::SubCircuit { .. })
                                && chunky_button(ui, &palette, "Open inside", false)
                                    .on_hover_text("Open this instance, with live values")
                                    .clicked()
                            {
                                writers.open.write(OpenInstanceView { entity });
                            }
                            if editable
                                && chunky_button(ui, &palette, "Delete", false)
                                    .on_hover_text("Delete this part (Del)")
                                    .clicked()
                            {
                                writers.delete_gate.write(DeleteGate { entity });
                                close = true;
                            }
                        });

                        // Wires.
                        let attached: Vec<_> = wires
                            .iter()
                            .filter_map(|(wire, ep, state)| {
                                let text = if ep.sink_gate == identity.uuid {
                                    format!(
                                        "in {} ← {} out {}",
                                        ep.sink_pin,
                                        gate_name(ep.source_gate),
                                        ep.source_pin
                                    )
                                } else if ep.source_gate == identity.uuid {
                                    format!(
                                        "out {} → {} in {}",
                                        ep.source_pin,
                                        gate_name(ep.sink_gate),
                                        ep.sink_pin
                                    )
                                } else {
                                    return None;
                                };
                                Some((wire, text, state.connected))
                            })
                            .collect();
                        egui::CollapsingHeader::new(
                            egui::RichText::new(format!("Wires ({})", attached.len())).font(heavy(13.0)),
                        )
                        .id_salt("inspector_wires")
                        .show(ui, |ui| {
                            for (wire, text, connected) in attached {
                                ui.horizontal(|ui| {
                                    let text = if connected {
                                        text
                                    } else {
                                        format!("{text} (not simulated)")
                                    };
                                    ui.label(egui::RichText::new(text).monospace().size(12.0));
                                    if editable && ui.small_button("Delete").clicked() {
                                        writers.delete_wire.write(DeleteWire { entity: wire });
                                    }
                                });
                            }
                        });
                    });
            });
        });
    if toggle_watch && let Some(mut editor) = editor {
        editor.toggle_watch(identity.uuid);
    }
    if close {
        sel.0 = None;
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Save
// ─────────────────────────────────────────────────────────────────────────────

pub fn save_on_exit_system(mut exit_events: MessageReader<AppExit>, reader: CircuitReader) {
    // A read-only demo is not the player's work; it is never saved.
    if exit_events.read().next().is_none() || reader.active.read_only {
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
        if reader.active.read_only {
            feedback.refuse(crate::sim::READ_ONLY_MESSAGE.into());
            continue;
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ticks_group_by_thousands() {
        assert_eq!(format_tick(0), "0");
        assert_eq!(format_tick(999), "999");
        assert_eq!(format_tick(48_213), "48\u{2009}213");
        assert_eq!(format_tick(1_234_567), "1\u{2009}234\u{2009}567");
    }

    #[test]
    fn live_values_list_outputs_then_buses_then_controls() {
        let names = |n: &[&str]| n.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        // A register: inputs en (1 bit), d (8 bits); output q.
        let rows = live_value_rows(&[1, 8], &[8], &names(&["en", "d"]), &names(&["q"]));
        let order: Vec<&str> = rows.iter().map(|r| r.2.as_str()).collect();
        assert_eq!(order, ["Q", "D", "EN"]);
        assert_eq!(rows[1], (false, 1, "D".to_string(), 8));
        // All 1-bit: pin order is kept; missing names get numbered.
        let rows = live_value_rows(&[1, 1], &[1], &names(&["a"]), &[]);
        let order: Vec<&str> = rows.iter().map(|r| r.2.as_str()).collect();
        assert_eq!(order, ["OUT0", "A", "IN1"]);
    }
}
