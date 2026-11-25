//! Bevy systems for the simulation editor view: visuals, picking, egui
//! panels, and workspace save.
//!
//! Visuals read the authoring components and the probe projections
//! ([`GateValues`], [`WireState`]); edits are sent as authoring messages.

use bevy::ecs::system::SystemParam;
use bevy::{prelude::*, window::PrimaryWindow};
use bevy_egui::{EguiContexts, egui};
use uuid::Uuid;

use crate::camera::{FIT_PADDING, FitView, fit_view_around, scale_for_wpp};
use crate::editor::parts::{kind_blurb, kind_title};
use crate::editor::workspace::{OnConflict, WEB_SAVE_UNAVAILABLE, WorkspaceStore, exit_save_plan};
use crate::theme::{Family, Theme};
use crate::ui_style::{
    HudIcon, HudLayout, caption, card, chip, chip_sized, chunky_button, chunky_icon_button,
    egui_color, format_live_value, heavy, rule, stroke,
};
use crate::{MainCamera, UiPointerState, temp::GateComponentSelectionEvent};

use super::{
    ActiveCircuit, AuthoringFeedback, CircuitLibrary, CircuitMember, CircuitReader, CreateWire,
    DeleteGate, DeleteWire, DisplayConfig, DisplayConfigChanged, DisplayRadix, GateIdentity,
    GateKind, GateLabel, GateLabelChanged, GateType, GateValues, InputDrive, SaveWorkspaceEvent,
    Selected, SelectedGate, SetGateKind, SetInputValue, SimBridge, SimDiagnostics,
    SimulationConfig, TickRateMeter, WireEndpoints, WireState, WorkspaceRevision, art,
    art::PinLayout,
    spatial_grid::SpatialGrid,
    view::{CircuitView, InstanceViewItem, OpenInstanceView},
    visuals::TileSpec,
    wiring::{self, PinPointer, PinRef},
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
    settings: Option<Res<crate::settings::Settings>>,
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
    let bounds = |input_only: bool| {
        gates
            .iter()
            .filter(|(_, ty)| !input_only || ty.kind.is_input())
            .map(|(t, ty)| Rect::from_center_size(t.translation.truncate(), ty.kind.size()))
            .reduce(|a, b| a.union(b))
    };
    let Some(content) = bounds(false) else {
        return;
    };
    let screen = window.size();
    let scale = crate::ui_style::ui_scale_for(settings.as_deref(), screen);
    let free = HudLayout::scaled(screen, scale).free_area_in_window(scale);
    let (centre, wpp) = fit_view_around(content, bounds(true), screen, free, FIT_PADDING);
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

/// A left click on the canvas: on a gate, that gate (to select); on empty
/// canvas, nothing, and the selection is cleared.
pub fn canvas_click(hit: Option<Entity>, selected: &mut SelectedGate) -> Option<Entity> {
    if hit.is_none() && selected.0.is_some() {
        selected.0 = None;
    }
    hit
}

/// The cursor over the canvas this frame: window position (logical
/// pixels, y down) and world position, or `None` outside the window.
/// [`canvas_cursor_system`] fills it in; the pointer systems only read it.
#[derive(Resource, Debug, Default, Clone, Copy)]
pub struct CanvasCursor {
    pub screen: Option<Vec2>,
    pub world: Option<Vec2>,
    /// World units per screen pixel (the zoom), for pin reach.
    pub world_per_pixel: f32,
}

/// Track the cursor in [`CanvasCursor`] (left as it is without a window
/// or camera, as in headless tests).
pub fn canvas_cursor_system(
    q_window: Query<&Window, With<PrimaryWindow>>,
    q_camera: Query<(&Camera, &GlobalTransform)>,
    mut cursor: ResMut<CanvasCursor>,
) {
    let (Ok(window), Ok((camera, camera_transform))) = (q_window.single(), q_camera.single())
    else {
        return;
    };
    let screen = window.cursor_position();
    let world = cursor_world_pos(window, camera, camera_transform);
    // Orthographic: one pixel across is the same world length anywhere.
    let world_per_pixel = match (
        camera.viewport_to_world_2d(camera_transform, Vec2::ZERO),
        camera.viewport_to_world_2d(camera_transform, Vec2::X),
    ) {
        (Ok(a), Ok(b)) => a.distance(b),
        _ => cursor.world_per_pixel,
    };
    *cursor = CanvasCursor {
        screen,
        world,
        world_per_pixel,
    };
}

/// Pixels the pointer must travel with the left button held on a part
/// before the press becomes a move instead of a click.
pub const DRAG_THRESHOLD: f32 = 5.0;

/// A press on the canvas being followed until its release.
#[derive(Debug, Clone, Copy)]
pub struct CanvasPress {
    pub button: MouseButton,
    /// The part under the pointer when pressed.
    pub gate: Option<Entity>,
    /// Window position of the press.
    pub start: Vec2,
    /// The part's position minus the pointer's world position at the press.
    pub offset: Vec2,
    /// The part follows the pointer (a right press on a part at once, a
    /// left press once it moved [`DRAG_THRESHOLD`]).
    pub dragging: bool,
    /// The pointer travelled past [`DRAG_THRESHOLD`].
    pub moved: bool,
    /// The pin under a left press that may start a wire (in an editable
    /// circuit with no part armed).
    pub pin: Option<PinRef>,
    /// A wire is being dragged out of [`pin`](Self::pin) (it moved past
    /// [`DRAG_THRESHOLD`]).
    pub wire: bool,
    /// `Esc` cancelled the wire: the release does nothing.
    pub cancelled: bool,
}

/// The press on the canvas, if a mouse button went down on it.
#[derive(Resource, Debug, Default)]
pub struct CanvasPointer {
    pub press: Option<CanvasPress>,
}

impl CanvasPointer {
    /// A part is being dragged.
    pub fn dragging(&self) -> bool {
        self.press.is_some_and(|p| p.dragging)
    }

    /// A wire is being dragged out of a pin (or was, until `Esc`, with the
    /// button still down).
    pub fn wiring(&self) -> bool {
        self.press.is_some_and(|p| p.wire)
    }
}

/// The value a click on an input part sets: a 1-bit input toggles. A
/// wider input is only selected (its value is set in the inspector, where
/// each bit has a chip), so a click never changes a bus by surprise.
pub fn input_click_value(kind: &GateKind, value: u64) -> Option<u64> {
    match kind {
        GateKind::Input { w: 1 } => Some(value ^ 1),
        _ => None,
    }
}

#[derive(SystemParam)]
pub struct CanvasPointerWriters<'w> {
    select: MessageWriter<'w, GateComponentSelectionEvent>,
    inputs: MessageWriter<'w, SetInputValue>,
    create_wire: MessageWriter<'w, CreateWire>,
    feedback: ResMut<'w, AuthoringFeedback>,
}

/// What dragging a wire out of a pin reads besides the parts.
#[derive(SystemParam)]
pub struct WireDragParams<'w, 's> {
    pins: ResMut<'w, PinPointer>,
    keys: Option<Res<'w, ButtonInput<KeyCode>>>,
    library: Res<'w, CircuitLibrary>,
    wires: Query<'w, 's, &'static WireEndpoints>,
}

type CanvasGate = (
    Entity,
    &'static mut Transform,
    &'static GateType,
    Option<&'static InputDrive>,
    Has<InstanceViewItem>,
    (
        &'static GateIdentity,
        Option<&'static GateLabel>,
        Option<&'static PinLayout>,
        Option<&'static TileSpec>,
    ),
);

/// Mouse buttons on the canvas (not over the HUD):
///
/// - a left press on a part selects it (on empty canvas, clears the
///   selection); released without moving, it toggles a 1-bit input (also
///   in a read-only demo: inputs may change there), see
///   [`input_click_value`];
/// - a left drag on a part, past [`DRAG_THRESHOLD`], moves it, and a right
///   drag picks it up at once; it moves on the snap grid
///   ([`SNAP`](super::layout::SNAP)), point to point;
/// - a left drag that starts on a pin ([`wiring::pin_under`]) drags out a
///   wire instead; released on a pin it makes the wire, or says why not
///   ([`wiring::drop_outcome`]); `Esc` drops it. [`PinPointer`] tracks the
///   hovered pin and the wire in hand for drawing;
/// - a `Shift`+left press is a multi-selection gesture
///   ([`super::multi_select`]) and is not handled here, also on a pin
///   (Shift+click on a part's pin adds the part to the selection).
///
/// Nothing moves and no wire is made in a read-only demo, and with a part
/// armed or the wire tool on a left click only picks (no move, no toggle;
/// the wire tool still drags wires). The moved part keeps [`Selected`]
/// (drawn on top) until the next drag.
#[allow(clippy::too_many_arguments)]
pub fn canvas_pointer_system(
    ui_pointer: Res<UiPointerState>,
    buttons: Res<ButtonInput<MouseButton>>,
    cursor: Res<CanvasCursor>,
    active: Res<crate::sim::ActiveCircuit>,
    editor: Option<Res<crate::editor::EditorState>>,
    mut pointer: ResMut<CanvasPointer>,
    mut selected: ResMut<SelectedGate>,
    mut gates: Query<CanvasGate, With<GateIdentity>>,
    dragged: Query<Entity, With<Selected>>,
    mut commands: Commands,
    mut writers: CanvasPointerWriters,
    mut wiring: WireDragParams,
) {
    let shift = wiring
        .keys
        .as_ref()
        .is_some_and(|k| super::multi_select::shift_held(k));
    let tool_in_hand = editor
        .as_ref()
        .is_some_and(|e| e.connection_mode || e.selected_gate_type.is_some());
    let part_armed = editor
        .as_ref()
        .is_some_and(|e| e.selected_gate_type.is_some());
    let start_drag = |commands: &mut Commands, gate: Entity, offset: Vec2| {
        for e in &dragged {
            commands.entity(e).remove::<Selected>();
        }
        commands.entity(gate).insert(Selected { offset });
    };
    let pin_at = |gates: &Query<CanvasGate, With<GateIdentity>>, world: Vec2| {
        let pins = gates
            .iter()
            .flat_map(|(entity, t, .., (id, _, layout, spec))| {
                layout
                    .zip(spec)
                    .into_iter()
                    .flat_map(move |(layout, spec)| {
                        wiring::gate_pins(entity, id.uuid, t.translation.truncate(), layout, spec)
                    })
            });
        wiring::pin_under(pins, world, cursor.world_per_pixel)
    };

    // A new press.
    for button in [MouseButton::Left, MouseButton::Right] {
        if !buttons.just_pressed(button) || pointer.press.is_some() || ui_pointer.over_egui {
            continue;
        }
        // Shift+left: a click adds to or takes from the group, a drag on
        // empty canvas draws a box (`multi_select_pointer_system`).
        if button == MouseButton::Left && shift {
            continue;
        }
        let (Some(screen), Some(world)) = (cursor.screen, cursor.world) else {
            continue;
        };
        let target = gates
            .iter()
            .find(|(_, t, ty, ..)| hit(t, &ty.kind, world))
            .map(|(entity, t, ..)| (entity, t.translation.truncate() - world));
        // A pin wins over the tile it sits on.
        let pin = (button == MouseButton::Left && !active.read_only && !part_armed)
            .then(|| pin_at(&gates, world))
            .flatten();
        let mut press = CanvasPress {
            button,
            gate: pin.map(|p| p.gate).or(target.map(|(e, _)| e)),
            start: screen,
            offset: target.map_or(Vec2::ZERO, |(_, o)| o),
            dragging: false,
            moved: false,
            pin,
            wire: false,
            cancelled: false,
        };
        if button == MouseButton::Left {
            let mut next = *selected;
            if let Some(entity) = canvas_click(press.gate, &mut next) {
                writers.select.write(GateComponentSelectionEvent { entity });
            }
            selected.set_if_neq(next);
        } else if let Some(gate) = press.gate
            && !active.read_only
        {
            start_drag(&mut commands, gate, press.offset);
            press.dragging = true;
        }
        pointer.press = Some(press);
    }

    // The pin under the pointer (none while a part moves or is armed).
    let hover = match (cursor.world, pointer.press) {
        (Some(world), press)
            if !ui_pointer.over_egui && !part_armed && !press.is_some_and(|p| p.dragging) =>
        {
            pin_at(&gates, world)
        }
        _ => None,
    };

    let Some(mut press) = pointer.press else {
        wiring.pins.set_if_neq(PinPointer { hover, drag: None });
        return;
    };
    // Movement.
    if let Some(screen) = cursor.screen
        && screen.distance(press.start) > DRAG_THRESHOLD
    {
        press.moved = true;
    }
    let held = buttons.pressed(press.button);
    if held && press.moved && !press.wire && !press.cancelled && press.pin.is_some() {
        press.wire = true;
    }
    if press.wire
        && !press.cancelled
        && wiring
            .keys
            .as_ref()
            .is_some_and(|k| k.just_pressed(KeyCode::Escape))
        && !ui_pointer.wants_keyboard
    {
        press.cancelled = true;
    }
    let drag = match (press.pin, cursor.world) {
        (Some(from), Some(world)) if press.wire && !press.cancelled && held => {
            Some(wiring::WireDrag {
                from,
                pointer: world,
            })
        }
        _ => None,
    };
    wiring.pins.set_if_neq(PinPointer { hover, drag });

    if held
        && press.moved
        && !press.dragging
        && !press.wire
        && press.button == MouseButton::Left
        && !active.read_only
        && !tool_in_hand
        && let Some(gate) = press.gate
    {
        start_drag(&mut commands, gate, press.offset);
        press.dragging = true;
    }
    if press.dragging
        && let Some(gate) = press.gate
        && let Ok((_, mut transform, ..)) = gates.get_mut(gate)
    {
        // The part jumps from point to point of the snap grid as it
        // follows the pointer; a right press without a move leaves it.
        let at = transform.translation.truncate();
        let to = match cursor.world {
            Some(world) if held && press.moved => super::layout::snap_to_grid(world + press.offset),
            _ => at,
        };
        if at != to {
            transform.translation.x = to.x;
            transform.translation.y = to.y;
        }
    }
    if held {
        pointer.press = Some(press);
        return;
    }
    pointer.press = None;
    // Released: a wire dropped on a pin, or ...
    if press.wire {
        if !press.cancelled
            && let Some(from) = press.pin
        {
            let target = cursor.world.and_then(|world| pin_at(&gates, world));
            let name = |pin: &wiring::PinRef| match gates.get(pin.gate) {
                Ok((_, _, ty, .., (_, label, _, _))) => {
                    wiring::pin_title(&wiring.library, &ty.kind, label, pin)
                }
                Err(_) => "a pin".into(),
            };
            match wiring::drop_outcome(&from, target.as_ref(), name) {
                wiring::DropOutcome::Connect(wire) => {
                    let exists = wiring.wires.iter().any(|ep| {
                        (ep.source_gate, ep.source_pin, ep.sink_gate, ep.sink_pin)
                            == (
                                wire.source_gate,
                                wire.source_pin,
                                wire.sink_gate,
                                wire.sink_pin,
                            )
                    });
                    if !exists {
                        writers.create_wire.write(wire);
                    }
                }
                wiring::DropOutcome::Refuse(message) => writers.feedback.refuse(message),
                wiring::DropOutcome::Cancel => {}
            }
        }
        return;
    }
    // ... a click on a 1-bit input toggles it.
    if press.button == MouseButton::Left
        && !press.moved
        && !tool_in_hand
        && let Some(gate) = press.gate
        && let Ok((entity, _, ty, Some(drive), false, _)) = gates.get(gate)
        && let Some(value) = input_click_value(&ty.kind, drive.value)
    {
        writers.inputs.write(SetInputValue { entity, value });
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
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push('\u{2009}');
        }
        out.push(c);
    }
    out
}

/// What the transport card shows.
#[derive(Debug, Clone, PartialEq)]
pub struct TransportState {
    pub running: bool,
    pub tick: u64,
    pub unlimited: bool,
    pub tick_rate: f64,
    pub more_open: bool,
}

/// What was clicked on the transport card this frame.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TransportClicks {
    pub run: bool,
    pub pause: bool,
    pub step: bool,
    pub reset: bool,
    /// A speed chip: `Some(hz)`, or `None` for Max.
    pub speed: Option<Option<f64>>,
    /// The `…` chip (the exact-rate popover).
    pub more: bool,
}

/// The widest tick label the card shows whole (`9 999 999`): past it
/// the count is shortened ([`tick_label`]), so the card never grows.
const TICK_LABEL_MAX_CHARS: usize = 9;

/// The tick count as the transport card shows it: [`format_tick`] up to
/// `9 999 999`, then three decimals of millions, billions, ... (`12.346 M`)
/// so it keeps its width. The tooltip has the whole count.
pub fn tick_label(tick: u64) -> String {
    let full = format_tick(tick);
    if full.chars().count() <= TICK_LABEL_MAX_CHARS {
        return full;
    }
    let mut value = tick as f64 / 1e6;
    for unit in ["M", "G", "T", "P"] {
        // Below 999.9995, so `{:.3}` does not round up to 1000.000.
        if value < 999.9995 {
            return format!("{value:.3} {unit}");
        }
        value /= 1e3;
    }
    format!("{value:.3} E")
}

/// The transport card's contents, `layout.transport` exactly: run,
/// pause, step, reset, the speed chips and the tick. In the compact
/// layout Run is an icon button (the label moves to its tooltip).
pub fn transport_card(
    ui: &mut egui::Ui,
    palette: &crate::theme::Palette,
    state: &TransportState,
    measured_rate: &str,
    layout: &HudLayout,
) -> TransportClicks {
    let mut clicked = TransportClicks::default();
    let frame = card(palette).inner_margin(egui::Margin::symmetric(10, 0));
    let margin = frame.total_margin().sum();
    let inner = layout.transport - Vec2::new(margin.x, margin.y);
    frame.show(ui, |ui| {
        ui.set_width(inner.x);
        ui.set_height(inner.y);
        ui.horizontal_centered(|ui| {
            // The compact card is narrower: closer items, Run as an icon,
            // a smaller tick.
            let (gap, chip_gap, tick_size) = if layout.compact {
                (6.0, 5.0, 14.0)
            } else {
                (10.0, 6.0, 16.0)
            };
            ui.spacing_mut().item_spacing.x = gap;
            let running = state.running;
            let run = if layout.compact {
                chunky_button(ui, palette, "▶", running).on_hover_text(if running {
                    "Running (Space pauses)"
                } else {
                    "Run (Space)"
                })
            } else {
                chunky_button(
                    ui,
                    palette,
                    if running { "▶ Running" } else { "▶ Run" },
                    running,
                )
                .on_hover_text("Run (Space)")
            };
            clicked.run = run.clicked();
            clicked.pause = chunky_icon_button(ui, palette, HudIcon::Pause, false)
                .on_hover_text("Pause (Space)")
                .clicked();
            clicked.step = chunky_button(ui, palette, "Step", false)
                .on_hover_text("One tick")
                .clicked();
            clicked.reset = chunky_button(ui, palette, "Reset", false)
                .on_hover_text(
                    "Back to tick 0: registers and memories to their \
                     initial values, inputs kept (R)",
                )
                .clicked();
            rule(ui, palette, 30.0);
            ui.spacing_mut().item_spacing.x = chip_gap;
            for (label, hz) in SPEED_CHIPS {
                let on = !state.unlimited && (state.tick_rate - hz).abs() < 1e-9;
                if chip(ui, palette, label, on).clicked() {
                    clicked.speed = Some(Some(hz));
                }
            }
            if chip(ui, palette, "Max", state.unlimited)
                .on_hover_text("As fast as the machine goes")
                .clicked()
            {
                clicked.speed = Some(None);
            }
            let custom = !state.unlimited
                && !SPEED_CHIPS
                    .iter()
                    .any(|(_, hz)| (state.tick_rate - hz).abs() < 1e-9);
            clicked.more = chip(ui, palette, "…", custom || state.more_open)
                .on_hover_text("Exact rate")
                .clicked();
            ui.spacing_mut().item_spacing.x = gap;
            rule(ui, palette, 30.0);
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                ui.add_space(4.0);
                ui.label(caption(palette, "tick"));
                ui.label(
                    egui::RichText::new(tick_label(state.tick))
                        .monospace()
                        .size(tick_size),
                )
                .on_hover_text(format!(
                    "{} ticks. Measured rate: {measured_rate}",
                    format_tick(state.tick)
                ));
            });
        });
    });
    clicked
}

/// The exact-rate popover under the transport card, at most
/// [`crate::ui_style::TRANSPORT_MORE_SIZE`]. Returns whether the
/// unlimited switch changed.
pub fn transport_more_card(
    ui: &mut egui::Ui,
    palette: &crate::theme::Palette,
    config: &mut SimulationConfig,
    measured_rate: &str,
    running_revision: Option<u64>,
) -> bool {
    let frame = card(palette);
    let inner = crate::ui_style::TRANSPORT_MORE_SIZE.x - frame.total_margin().sum().x;
    frame
        .show(ui, |ui| {
            ui.set_width(inner);
            let changed = ui
                .checkbox(&mut config.unlimited_tick_rate, "Unlimited tick rate")
                .changed();
            ui.label("Tick rate (Hz)");
            // The slider and its value box share the row.
            ui.spacing_mut().slider_width = inner - 100.0;
            ui.add_enabled(
                !config.unlimited_tick_rate,
                egui::Slider::new(&mut config.tick_rate, 0.1..=10_000_000.0).logarithmic(true),
            );
            ui.label(format!("Measured: {measured_rate}"));
            match running_revision {
                Some(rev) => ui.label(format!("Circuit revision: {rev}")),
                None => ui.label("Circuit: compiling…"),
            };
            changed
        })
        .inner
}

/// The transport card, top centre: run, pause, step, reset (also `R`),
/// speed chips (and a `⋯` popover with the exact rate), and the tick.
/// `Space` runs or pauses (not while a text field has the keyboard).
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
    // `Space` runs or pauses; not while a widget has the keyboard focus
    // (Space would also press a focused button).
    let typing = ctx.egui_wants_keyboard_input();
    let focused = ctx.memory(|m| m.focused().is_some());
    if keys.just_pressed(KeyCode::Space) && !typing && !focused {
        let running = bridge.is_running();
        bridge.set_running(!running);
    }
    let state = TransportState {
        running: bridge.is_running(),
        tick: tick_count,
        unlimited: sim_config.unlimited_tick_rate,
        tick_rate: sim_config.tick_rate,
        more_open: *more_open,
    };
    let mut clicked = TransportClicks::default();
    let rect = layout.transport_rect();
    egui::Area::new(egui::Id::new("hud_transport"))
        .fixed_pos(egui::pos2(rect.min.x, rect.min.y))
        .order(egui::Order::Middle)
        .show(ctx, |ui| {
            clicked = transport_card(ui, &palette, &state, &rate, &layout);
        });
    if clicked.run && !state.running {
        bridge.set_running(true);
    }
    if clicked.pause && state.running {
        bridge.set_running(false);
    }
    if clicked.step {
        bridge.step(1);
    }
    let reset = clicked.reset || (keys.just_pressed(KeyCode::KeyR) && !typing);
    if clicked.more {
        *more_open = !*more_open;
    }
    let speed = clicked.speed;
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
        let rect = layout.transport_more_rect();
        egui::Area::new(egui::Id::new("hud_transport_more"))
            .fixed_pos(egui::pos2(rect.min.x, rect.min.y))
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                let revision = diagnostics.running_revision;
                let config = &mut *sim_config;
                if transport_more_card(ui, &palette, config, &rate, revision) {
                    tick_rate_meter.reset(tick_count);
                }
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

/// The screen settings: mode, resolution, colours and pixel size.
fn edit_screen_params(ui: &mut egui::Ui, palette: &crate::theme::Palette, kind: &mut GateKind) {
    let GateKind::Screen {
        mode,
        width,
        height,
        palette: colours,
        scale,
    } = kind
    else {
        return;
    };
    let section = |ui: &mut egui::Ui, title: &str, body: &mut dyn FnMut(&mut egui::Ui)| {
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 4.0;
            ui.label(caption(palette, title));
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
                body(ui);
            });
        });
    };
    section(ui, "Mode", &mut |ui| {
        for m in super::ScreenMode::ALL {
            if chip(ui, palette, m.name(), *mode == m).clicked() && *mode != m {
                *mode = m;
                *width = m.fit(*width);
                *height = m.fit(*height);
            }
        }
    });
    let text = *mode == super::ScreenMode::Text;
    let sides = mode.sides();
    for (title, side) in [
        (if text { "Columns" } else { "Width" }, &mut *width),
        (if text { "Rows" } else { "Height" }, &mut *height),
    ] {
        section(ui, title, &mut |ui| {
            for &s in sides {
                if chip(ui, palette, s.to_string(), *side == s).clicked() {
                    *side = s;
                }
            }
        });
    }
    if text {
        ui.label(
            egui::RichText::new("8-bit character codes (ASCII); bit 7 inverts the cell.")
                .size(13.0)
                .color(egui_color(palette.text_secondary)),
        );
    } else {
        section(ui, "Colours", &mut |ui| {
            for p in super::ScreenPalette::ALL {
                let label = format!("{} ({} bit)", p.name(), p.bits());
                if chip(ui, palette, label, *colours == p).clicked() {
                    *colours = p;
                }
            }
        });
    }
    ui.horizontal(|ui| {
        ui.label("Pixel size");
        ui.add(egui::DragValue::new(scale).range(1..=super::MAX_SCREEN_SCALE));
    });
    let hint = match mode {
        super::ScreenMode::Pixel => "Each tick that draw is on paints colour at (x, y).".into(),
        super::ScreenMode::Scanline => {
            "Each tick paints one pixel and moves right; hsync starts the next line, vsync the \
             top."
                .into()
        }
        _ => format!("Cell (x, y) is at address y × {width} + x."),
    };
    ui.label(
        egui::RichText::new(hint)
            .size(13.0)
            .color(egui_color(palette.text_secondary)),
    );
}

/// Edit the parameters of a kind. Returns the edited kind.
fn edit_kind_params(
    ui: &mut egui::Ui,
    palette: &crate::theme::Palette,
    kind: &GateKind,
) -> GateKind {
    let mut k = kind.clone();
    match &mut k {
        GateKind::Screen { .. } => edit_screen_params(ui, palette, &mut k),
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
        // The width has its chips and exact value above.
        _ => match &mut k {
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
            GateKind::Ram2 {
                addr_bits,
                reads,
                mask,
                init,
                ..
            } => {
                width_drag(
                    ui,
                    "Address bits",
                    addr_bits,
                    1..=logi_sim::netlist::MAX_ADDR_BITS,
                );
                width_drag(
                    ui,
                    "Read ports",
                    reads,
                    1..=logi_sim::netlist::MAX_READ_PORTS,
                );
                ui.checkbox(mask, "Per-bit write enable");
                // Initial contents past the (new) address space are cut.
                init.truncate(1usize << *addr_bits);
                if !init.is_empty() {
                    ui.label(format!("Initial contents: {} words", init.len()));
                }
            }
            _ => {}
        },
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
    multi: Option<Res<crate::sim::MultiSelection>>,
) -> Result<(), BevyError> {
    // Several parts selected: the selection card stands in for the
    // inspector (`crate::editor::chips`).
    if game_state.get() != &crate::GameState::Simulating || multi.is_some_and(|m| m.len() >= 2) {
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
        // A read-only demo refuses the delete (with a toast); the part
        // stays, and so does the selection.
        writers.delete_gate.write(DeleteGate { entity });
        if !active.read_only {
            sel.0 = None;
        }
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
                                        .size(13.0)
                                        .color(c(palette.text_secondary)),
                                );
                            });
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                                if ui.small_button("×").on_hover_text("Close (Esc)").clicked() {
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

                        // Width chips, and the exact width beside the caption.
                        let width_kind = !matches!(
                            ty.kind,
                            GateKind::Merge { .. } | GateKind::SrLatch | GateKind::SubCircuit { .. }
                        );
                        if let (Some(w), true) = (ty.kind.width(), width_kind) {
                            ui.vertical(|ui| {
                                ui.spacing_mut().item_spacing.y = 4.0;
                                ui.horizontal(|ui| {
                                    ui.label(caption(&palette, "Width"));
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            let mut exact = w;
                                            ui.add_enabled(
                                                editable,
                                                egui::DragValue::new(&mut exact)
                                                    .range(1..=64)
                                                    .suffix(" bit"),
                                            )
                                            .on_hover_text("Any width from 1 to 64");
                                            if exact != w {
                                                writers.kinds.write(SetGateKind {
                                                    entity,
                                                    kind: ty.kind.with_width(exact),
                                                });
                                            }
                                        },
                                    );
                                });
                                ui.add_enabled_ui(editable, |ui| {
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
                            let edited = ui
                                .add_enabled_ui(editable, |ui| edit_kind_params(ui, &palette, &ty.kind))
                                .inner;
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
                                    ui.label(egui::RichText::new(text).monospace().size(13.0));
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

/// Save the open workspace when the app exits, if that would otherwise
/// lose work (see [`exit_save_plan`]).
///
/// Runs in [`Last`] in [`bevy::app::OnAppExitSystems`], which Bevy orders
/// after [`bevy::window::ExitSystems`] and before winit despawns the
/// windows: closing the window sends [`AppExit`] from `Last` (bevy_window's
/// exit condition) and the title menu's Exit sends it in `Update`; the
/// runner stops after the frame in which it was sent, so this is the last
/// system that sees it, with the circuit still in the world.
pub fn save_on_exit_system(
    mut exit_events: MessageReader<AppExit>,
    reader: CircuitReader,
    revision: Res<WorkspaceRevision>,
) {
    if exit_events.read().next().is_none() {
        return;
    }
    let store = WorkspaceStore::player();
    let active = &reader.active;
    let workspace = reader.workspace();
    let empty = workspace.root.nodes.is_empty();
    let Some(plan) = exit_save_plan(
        store.as_ref(),
        &active.name,
        active.uuid,
        active.read_only,
        revision.is_dirty(),
        empty,
    ) else {
        return;
    };
    let Some(store) = store else { return };
    match store.save_open(&plan.name, &plan.name, workspace, OnConflict::Rename) {
        Ok(saved) => info!(
            "Saved workspace \"{}\" on exit in {}",
            saved.name,
            store.dir().display()
        ),
        Err(err) => error!("{err}"),
    }
}

/// Write the open circuit to disk on [`SaveWorkspaceEvent`]. Saving under
/// another name is a Save As: the saved copy gets a fresh root id and
/// becomes the open workspace (the old file is left as it was). A name
/// that holds a different workspace is refused.
#[allow(clippy::too_many_arguments)]
pub fn workspace_io_system(
    mut save_events: MessageReader<SaveWorkspaceEvent>,
    mut circuit: ParamSet<(
        CircuitReader,
        ResMut<ActiveCircuit>,
        Option<ResMut<super::WorkspaceTests>>,
    )>,
    mut members: Query<&mut CircuitMember, Allow<bevy::ecs::entity_disabling::Disabled>>,
    mut revision: ResMut<WorkspaceRevision>,
    mut feedback: ResMut<AuthoringFeedback>,
    save_outcome: Option<ResMut<super::SaveOutcome>>,
    editor_state: Option<ResMut<crate::editor::EditorState>>,
) {
    let Some(name) = save_events.read().last().map(|ev| ev.name.clone()) else {
        return;
    };
    let reader = circuit.p0();
    let previous = reader.active.name.clone();
    let old_root = reader.active.uuid;
    let result = if reader.active.read_only {
        Err(crate::sim::READ_ONLY_MESSAGE.to_string())
    } else {
        match WorkspaceStore::player() {
            Some(store) => {
                store.save_open(&name, &previous, reader.workspace(), OnConflict::Refuse)
            }
            None => Err(WEB_SAVE_UNAVAILABLE.to_string()),
        }
    };
    let outcome = match result {
        Ok(saved) => {
            revision.mark_saved();
            if saved.root != old_root || saved.name != previous {
                // The open workspace is now the saved one. No change
                // detection: the camera stays where it is.
                let mut active = circuit.p1();
                let active = active.bypass_change_detection();
                active.uuid = saved.root;
                active.name = saved.name.clone();
                // The root's stored tests follow it to its new id (the
                // reader holds them too, hence the set).
                if let Some(mut tests) = circuit.p2()
                    && let Some(root_tests) = tests.0.remove(&old_root)
                {
                    tests.0.insert(saved.root, root_tests);
                }
                for mut member in &mut members {
                    if member.circuit_uuid == old_root {
                        member.circuit_uuid = saved.root;
                    }
                }
            }
            let message = saved.message(&previous);
            feedback.message = Some(message.clone());
            Ok(message)
        }
        Err(err) => {
            feedback.refuse(err.clone());
            Err(err)
        }
    };
    if let Some(mut save_outcome) = save_outcome {
        save_outcome.0 = Some(outcome.clone());
    }
    if let Some(mut editor_state) = editor_state {
        if outcome.is_ok() {
            editor_state.saved_workspaces = None;
        }
        editor_state.last_save = Some(outcome);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_click_on_empty_canvas_clears_the_selection() {
        let mut world = World::new();
        let (a, b) = (world.spawn_empty().id(), world.spawn_empty().id());
        let mut selected = SelectedGate(Some(a));
        assert_eq!(canvas_click(Some(b), &mut selected), Some(b));
        assert_eq!(
            selected.0,
            Some(a),
            "a click on a gate selects via the event"
        );
        assert_eq!(canvas_click(None, &mut selected), None);
        assert_eq!(selected.0, None, "empty canvas deselects");
        assert_eq!(canvas_click(None, &mut selected), None);
        assert_eq!(selected.0, None);
    }

    #[test]
    fn tick_labels_keep_their_width() {
        assert_eq!(tick_label(48_213), "48\u{2009}213");
        assert_eq!(tick_label(9_999_999), format_tick(9_999_999));
        assert_eq!(tick_label(12_345_678), "12.346 M");
        assert_eq!(tick_label(999_999_999), "1.000 G");
        assert_eq!(tick_label(1_234_567_890), "1.235 G");
        assert_eq!(tick_label(4_321_000_000_000), "4.321 T");
        assert_eq!(tick_label(u64::MAX), "18.447 E");
        for tick in [
            0,
            9_999_999,
            10_000_000,
            999_999_999,
            999_999_999_999_999,
            u64::MAX,
        ] {
            assert!(
                tick_label(tick).chars().count() <= TICK_LABEL_MAX_CHARS,
                "{tick}"
            );
        }
    }

    /// The transport card is exactly as wide as the layout reserves, for
    /// every label it shows (Run or Running, the widest tick), in both
    /// layouts and themes.
    #[test]
    fn transport_card_is_its_layout_size() {
        for theme in [Theme::day(), Theme::night()] {
            let ctx = crate::ui_style::test_ui::ctx(&theme);
            for screen in [Vec2::new(1280.0, 720.0), Vec2::new(1920.0, 1080.0)] {
                let layout = HudLayout::new(screen);
                for running in [false, true] {
                    for tick in [0, 9_999_999, 999_499_999, u64::MAX] {
                        let state = TransportState {
                            running,
                            tick,
                            unlimited: false,
                            tick_rate: 12_345.0,
                            more_open: true,
                        };
                        let rect = crate::ui_style::test_ui::measure(&ctx, |ui| {
                            transport_card(ui, &theme.palette, &state, "12 kHz", &layout);
                        });
                        let size = Vec2::new(rect.width(), rect.height());
                        assert!(
                            (size - layout.transport).abs().max_element() < 0.5,
                            "{size} for {:?} at {screen} (running {running}, tick {tick})",
                            layout.transport
                        );
                    }
                }
            }
        }
    }

    /// The exact-rate popover keeps to the size the layout reserves.
    #[test]
    fn transport_more_card_fits_its_room() {
        let theme = Theme::day();
        let ctx = crate::ui_style::test_ui::ctx(&theme);
        for tick_rate in [0.1, 12_345.678, 10_000_000.0] {
            let mut config = SimulationConfig {
                tick_rate,
                ..Default::default()
            };
            let rect = crate::ui_style::test_ui::measure(&ctx, |ui| {
                transport_more_card(ui, &theme.palette, &mut config, "9.99 MHz", Some(12_345));
            });
            let max = crate::ui_style::TRANSPORT_MORE_SIZE;
            assert!(
                rect.width() <= max.x + 0.5 && rect.height() <= max.y,
                "{:?} at {tick_rate} Hz",
                rect.size()
            );
        }
    }

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
