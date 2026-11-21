//! The floating HUD cards around the canvas, other than the file card and
//! Library drawer (in `ui`) and the transport and inspector (in
//! [`crate::sim::systems`]):
//!
//! - the parts bar (bottom centre): family tabs, number-key slots, the
//!   wire tool; click a slot to arm it and click the canvas, or drag it
//!   onto the canvas;
//! - the minimap (bottom left) and the zoom row;
//! - the build status pill (top right);
//! - the Signals card (bottom right): live values of the watched gates
//!   and of the open circuit's ports and registers;
//! - toasts for authoring feedback and workspace status;
//! - the editor's keys: `1`–`9`, `Esc`, `W`, `F`.
//!
//! Every card is placed by [`HudLayout`].

use std::sync::Arc;

use bevy::prelude::*;
use bevy::window::PrimaryWindow;
use bevy_egui::{
    EguiContexts,
    egui::{self, Align2, Color32, CornerRadius, FontId, Order, Pos2, Sense, StrokeKind, Ui},
};
use uuid::Uuid;

use crate::camera::{FitView, MAX_WPP, MIN_WPP, scale_for_wpp, visible_world, world_per_pixel};
use crate::sim::{
    ActiveCircuit, AuthoringFeedback, CircuitLibrary, CreateGate, Demo, GateIdentity, GateKind,
    GateLabel, GateType, GateValues, RegisterDefinitions, SelectedGate, SimDiagnostics,
    WorkspaceDocument, art, demos,
};
use crate::theme::{Family, Palette, Theme};
use crate::ui_style::{
    HudLayout, SLOT_GAP, TOOL_GAP, caption, card, chip, chunky_button, chunky_button_with,
    egui_color, egui_rect, format_live_value, heavy, stroke,
};
use crate::{GameState, MainCamera};

use super::EditorState;
use super::parts::{
    KEYED_SLOTS, PartTab, SLOT_KEYS, kind_blurb, kind_title, slot_for_key, slot_key_label,
    tab_kinds,
};
use super::workspace::read_workspace;

/// The HUD layout for the egui screen.
pub fn hud_layout(ctx: &egui::Context) -> HudLayout {
    let size = ctx.content_rect().size();
    HudLayout::new(Vec2::new(size.x, size.y))
}

/// A card whose outer edge is exactly `rect`; contents get the rest.
pub fn fixed_card<R>(
    ctx: &egui::Context,
    id: &str,
    rect: Rect,
    palette: &Palette,
    margin: i8,
    add_contents: impl FnOnce(&mut Ui) -> R,
) -> egui::InnerResponse<R> {
    let frame = card(palette).inner_margin(egui::Margin::same(margin));
    let inset = frame.total_margin().sum();
    let inner = egui::vec2(rect.width(), rect.height()) - inset;
    egui::Area::new(egui::Id::new(id))
        .fixed_pos(egui::pos2(rect.min.x, rect.min.y))
        .order(Order::Middle)
        .show(ctx, |ui| {
            frame
                .show(ui, |ui| {
                    ui.set_min_size(inner);
                    ui.set_max_size(inner);
                    add_contents(ui)
                })
                .inner
        })
}

// ─────────────────────────────────────────────────────────────────────────────
// Minimap
// ─────────────────────────────────────────────────────────────────────────────

/// Maps world positions (y up) into a minimap rectangle (screen, y down)
/// at one scale on both axes, the world bounds centred in the map.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MinimapTransform {
    world_centre: Vec2,
    map_centre: Vec2,
    /// Map pixels per world unit.
    scale: f32,
}

impl MinimapTransform {
    pub fn new(world: Rect, map: Rect) -> Self {
        let size = world.size().max(Vec2::ONE);
        let scale = (map.width() / size.x).min(map.height() / size.y);
        Self {
            world_centre: world.center(),
            map_centre: map.center(),
            scale,
        }
    }

    pub fn scale(&self) -> f32 {
        self.scale
    }

    pub fn to_map(&self, world: Vec2) -> Vec2 {
        let d = (world - self.world_centre) * self.scale;
        self.map_centre + Vec2::new(d.x, -d.y)
    }

    pub fn to_world(&self, map: Vec2) -> Vec2 {
        let d = (map - self.map_centre) / self.scale;
        self.world_centre + Vec2::new(d.x, -d.y)
    }

    /// A world rectangle in map space.
    pub fn rect_to_map(&self, world: Rect) -> Rect {
        Rect::from_corners(self.to_map(world.min), self.to_map(world.max))
    }
}

/// The world bounds the minimap shows: the circuit with a margin.
pub fn minimap_world(content: Rect) -> Rect {
    let pad = content.size().max_element() * 0.12 + 60.0;
    content.inflate(pad)
}

/// The world rectangle of the circuit's parts, if it has any.
fn content_bounds<'a>(parts: impl Iterator<Item = (&'a Transform, &'a GateType)>) -> Option<Rect> {
    parts
        .map(|(t, ty)| Rect::from_center_size(t.translation.truncate(), ty.kind.size()))
        .reduce(|a, b| a.union(b))
}

fn family_colour(palette: &Palette, kind: &GateKind) -> Color32 {
    egui_color(match art::family(kind) {
        Family::Logic => palette.ink,
        Family::State => palette.state,
        Family::Io => palette.io,
    })
}

type MinimapGate = (With<GateIdentity>, Without<MainCamera>);

/// The minimap card and the zoom row.
#[allow(clippy::too_many_arguments)]
pub fn minimap_system(
    mut contexts: EguiContexts,
    theme: Res<Theme>,
    windows: Query<&Window, With<PrimaryWindow>>,
    mut cameras: Query<(&mut Projection, &mut Transform), With<MainCamera>>,
    gates: Query<(&Transform, &GateType), MinimapGate>,
    mut fit: MessageWriter<FitView>,
    game_state: Res<State<GameState>>,
) -> Result {
    if game_state.get() != &GameState::Simulating {
        return Ok(());
    }
    let ctx = contexts.ctx_mut()?;
    if !crate::ui_style::workshop_fonts_ready(ctx) {
        return Ok(());
    }
    let (Ok(window), Ok((mut projection, mut camera))) = (windows.single(), cameras.single_mut())
    else {
        return Ok(());
    };
    let Projection::Orthographic(ortho) = projection.as_mut() else {
        return Ok(());
    };
    let palette = theme.palette;
    let layout = hud_layout(ctx);
    let screen = window.size();
    let wpp = world_per_pixel(ortho, screen.y);
    let centre = camera.translation.truncate();
    let content = content_bounds(gates.iter());

    let mut jump = None;
    fixed_card(
        ctx,
        "hud_minimap",
        layout.minimap_rect(),
        &palette,
        10,
        |ui| {
            ui.label(caption(&palette, "Map"));
            let (rect, response) =
                ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
            let painter = ui.painter_at(rect);
            painter.rect_filled(rect, CornerRadius::same(9), egui_color(palette.well));
            let map = Rect::new(rect.min.x, rect.min.y, rect.max.x, rect.max.y).inflate(-6.0);
            let Some(content) = content else {
                painter.text(
                    rect.center(),
                    Align2::CENTER_CENTER,
                    "empty",
                    FontId::proportional(12.0),
                    egui_color(palette.text_faint),
                );
                return;
            };
            let t = MinimapTransform::new(minimap_world(content), map);
            for (transform, ty) in &gates {
                let r = t.rect_to_map(Rect::from_center_size(
                    transform.translation.truncate(),
                    ty.kind.size(),
                ));
                let r = egui_rect(r).expand(0.5);
                let r =
                    egui::Rect::from_center_size(r.center(), r.size().max(egui::vec2(3.0, 3.0)));
                painter.rect_filled(r, CornerRadius::same(2), family_colour(&palette, &ty.kind));
            }
            let view = egui_rect(t.rect_to_map(visible_world(screen, centre, wpp)));
            painter.rect_stroke(
                view.intersect(rect.shrink(1.5)),
                CornerRadius::same(5),
                stroke(2.5, egui_color(palette.on)),
                StrokeKind::Inside,
            );
            if (response.clicked() || response.dragged())
                && let Some(pos) = response.interact_pointer_pos()
            {
                jump = Some(t.to_world(Vec2::new(pos.x, pos.y)));
            }
            response
                .on_hover_cursor(egui::CursorIcon::PointingHand)
                .on_hover_text("Click or drag to move the view");
        },
    );
    if let Some(world) = jump {
        camera.translation.x = world.x;
        camera.translation.y = world.y;
    }

    // − 100% +
    let zoom = layout.zoom_rect();
    let mut new_wpp = None;
    egui::Area::new(egui::Id::new("hud_zoom"))
        .fixed_pos(egui::pos2(zoom.min.x, zoom.min.y))
        .order(Order::Middle)
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                if chunky_button(ui, &palette, "−", false)
                    .on_hover_text("Zoom out (mouse wheel)")
                    .clicked()
                {
                    new_wpp = Some(wpp * 1.25);
                }
                let percent = format!("{:.0}%", 100.0 / wpp);
                let text = egui::WidgetText::from(percent).into_galley(
                    ui,
                    Some(egui::TextWrapMode::Extend),
                    f32::INFINITY,
                    egui::TextStyle::Button,
                );
                if chunky_button_with(ui, &palette, 72.0, false, |painter, face, colour| {
                    painter.galley(face.center() - text.size() * 0.5, text, colour);
                })
                .on_hover_text("Fit the circuit (F)")
                .clicked()
                {
                    fit.write(FitView);
                }
                if chunky_button(ui, &palette, "+", false)
                    .on_hover_text("Zoom in (mouse wheel)")
                    .clicked()
                {
                    new_wpp = Some(wpp / 1.25);
                }
            });
        });
    if let Some(w) = new_wpp {
        ortho.scale = scale_for_wpp(ortho, screen.y, w.clamp(MIN_WPP, MAX_WPP));
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Parts bar
// ─────────────────────────────────────────────────────────────────────────────

/// What a slot of the parts bar places.
#[derive(Clone)]
enum SlotItem {
    Kind(GateKind),
    /// A saved workspace, placed as a cell.
    Saved(String),
    /// A demo or gallery item, placed as a cell.
    Demo(&'static Demo),
}

impl SlotItem {
    fn hover(&self) -> String {
        match self {
            SlotItem::Kind(kind) => format!("{}: {}", kind_title(kind), kind_blurb(kind)),
            SlotItem::Saved(name) => format!("{name}: your saved workspace, as a chip"),
            SlotItem::Demo(demo) => format!("{}: {}", demo.title, demo.description),
        }
    }

    fn is_armed(&self, state: &EditorState) -> bool {
        match (self, &state.selected_gate_type) {
            (SlotItem::Kind(k), Some(armed)) => k == armed,
            (SlotItem::Demo(d), Some(GateKind::SubCircuit { definition })) => {
                *definition == d.workspace.root.id
            }
            (SlotItem::Saved(name), Some(GateKind::SubCircuit { .. })) => {
                state.selected_gate_label.as_deref() == Some(name.as_str())
            }
            _ => false,
        }
    }
}

/// The slots of a tab: its primitives, or saved workspaces, then demos,
/// then gallery items.
fn slot_items(tab: PartTab, state: &EditorState) -> Vec<SlotItem> {
    if tab != PartTab::Chips {
        return tab_kinds(tab, state.palette_width)
            .into_iter()
            .map(SlotItem::Kind)
            .collect();
    }
    let saved = state.saved_workspaces.iter().flatten().cloned();
    let demos = demos::catalog()
        .map(|c| c.demos.iter().chain(&c.gallery))
        .ok()
        .into_iter()
        .flatten();
    saved
        .map(SlotItem::Saved)
        .chain(demos.map(SlotItem::Demo))
        .collect()
}

/// Draw a gate glyph (AND, OR, XOR, NOT and their inversions) centred
/// at `c`, in a 34 x 26 box. Returns false for kinds without a glyph.
fn paint_gate_glyph(painter: &egui::Painter, c: Pos2, kind: &GateKind, ink: Color32) -> bool {
    use GateKind as G;
    let (base, bubble) = match kind {
        G::And { .. } => ("and", false),
        G::Nand { .. } => ("and", true),
        G::Or { .. } => ("or", false),
        G::Nor { .. } => ("or", true),
        G::Xor { .. } => ("xor", false),
        G::Xnor { .. } => ("xor", true),
        G::Not { .. } => ("not", true),
        _ => return false,
    };
    let origin = c - egui::vec2(17.0, 13.0);
    let p = |x: f32, y: f32| origin + egui::vec2(x, y);
    let quad = |a: Pos2, b: Pos2, d: Pos2, out: &mut Vec<Pos2>| {
        for i in 1..=10 {
            let t = i as f32 / 10.0;
            let u = 1.0 - t;
            out.push(Pos2::new(
                u * u * a.x + 2.0 * u * t * b.x + t * t * d.x,
                u * u * a.y + 2.0 * u * t * b.y + t * t * d.y,
            ));
        }
    };
    let s = stroke(2.5, ink);
    let shift = if bubble && base != "not" { -2.0 } else { 0.0 };
    let q = |x: f32, y: f32| p(x + shift, y);
    let mut points = Vec::new();
    let tip = match base {
        "and" => {
            points.push(q(5.0, 3.0));
            points.push(q(17.0, 3.0));
            for i in 1..=12 {
                let a = -std::f32::consts::FRAC_PI_2 + std::f32::consts::PI * i as f32 / 12.0;
                points.push(q(17.0 + 10.0 * a.cos(), 13.0 + 10.0 * a.sin()));
            }
            points.push(q(5.0, 23.0));
            27.0
        }
        "or" | "xor" => {
            let dx = if base == "xor" { 4.0 } else { 0.0 };
            let r = |x: f32, y: f32| q(x + dx, y);
            points.push(r(4.0, 3.0));
            quad(r(4.0, 3.0), r(20.0, 3.0), r(28.0, 13.0), &mut points);
            quad(r(28.0, 13.0), r(20.0, 23.0), r(4.0, 23.0), &mut points);
            quad(r(4.0, 23.0), r(11.0, 13.0), r(4.0, 3.0), &mut points);
            if base == "xor" {
                let mut back = vec![q(1.0, 3.0)];
                quad(q(1.0, 3.0), q(8.0, 13.0), q(1.0, 23.0), &mut back);
                painter.add(egui::Shape::line(back, s));
            }
            28.0 + dx
        }
        _ => {
            points.extend([q(5.0, 3.0), q(23.0, 13.0), q(5.0, 23.0)]);
            23.0
        }
    };
    painter.add(egui::Shape::closed_line(points, s));
    if bubble {
        painter.circle_stroke(q(tip + 3.5, 13.0), 3.5, s);
    }
    true
}

/// One parts-bar slot: a chunky tile with its key number, a glyph or
/// short name, lifting on hover and marigold while armed.
fn part_slot(
    ui: &mut Ui,
    palette: &Palette,
    side: f32,
    key: Option<String>,
    item: &SlotItem,
    armed: bool,
) -> egui::Response {
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(side, side + 3.0), Sense::click_and_drag());
    if !ui.is_rect_visible(rect) {
        return response;
    }
    let painter = ui.painter();
    let ink = egui_color(palette.ink);
    let lift = if response.hovered() && !response.is_pointer_button_down_on() {
        2.0
    } else {
        0.0
    };
    let face = egui::Rect::from_min_size(rect.min - egui::vec2(0.0, lift), egui::vec2(side, side));
    let radius = CornerRadius::same(12);
    painter.rect_filled(face.translate(egui::vec2(0.0, 3.0 + lift)), radius, ink);
    let fill = match item {
        _ if armed => palette.on,
        SlotItem::Kind(kind) => match art::family(kind) {
            Family::Logic if kind_has_glyph(kind) => palette.raised,
            Family::Logic => palette.hover,
            Family::State => palette.state_tint,
            Family::Io => palette.io_tint,
        },
        _ => palette.state_tint,
    };
    painter.rect(
        face,
        radius,
        egui_color(fill),
        stroke(2.5, ink),
        StrokeKind::Inside,
    );
    let text_colour = egui_color(if armed { palette.on_ink } else { palette.text });
    if let Some(key) = key {
        painter.text(
            face.min + egui::vec2(7.0, 4.0),
            Align2::LEFT_TOP,
            key,
            FontId::monospace(10.0),
            egui_color(if armed {
                palette.on_ink
            } else {
                palette.text_faint
            }),
        );
    }
    match item {
        SlotItem::Kind(kind) => {
            let c = face.center() + egui::vec2(0.0, 3.0);
            if !paint_gate_glyph(painter, c, kind, text_colour) {
                // The inspector's name for the kind, as large as fits.
                let (galley, _) =
                    slot_label(ui.ctx(), kind_title(kind), face.width() - SLOT_TEXT_PAD);
                // Centred rows are laid out about x = 0: centre the rect.
                let pos = c - galley.rect.center().to_vec2();
                painter.galley(pos, galley, text_colour);
            }
        }
        SlotItem::Saved(name) => chip_name(painter, face, name, text_colour),
        SlotItem::Demo(demo) => chip_name(painter, face, &demo.title, text_colour),
    }
    response
}

/// Room left and right of a slot's text, together.
const SLOT_TEXT_PAD: f32 = 8.0;

/// Font sizes a slot's text tries, largest first.
const SLOT_TEXT_SIZES: [f32; 6] = [13.0, 12.0, 11.0, 10.0, 9.0, 8.0];

/// `text` laid out centred for a slot face with `width` of room: at the
/// largest of [`SLOT_TEXT_SIZES`] where no word is wider than the room and
/// it takes at most two lines. Returns the galley and whether it fits (at
/// the smallest size otherwise).
fn slot_label(ctx: &egui::Context, text: &str, width: f32) -> (Arc<egui::Galley>, bool) {
    let layout = |size: f32, wrap: f32| {
        let mut job =
            egui::text::LayoutJob::simple(text.to_owned(), heavy(size), Color32::PLACEHOLDER, wrap);
        job.halign = egui::Align::Center;
        ctx.fonts_mut(|f| f.layout_job(job))
    };
    let mut last = None;
    for size in SLOT_TEXT_SIZES {
        let words_fit = text
            .split_whitespace()
            .all(|w| layout_word(ctx, w, size) <= width);
        let galley = layout(size, width);
        let fits = words_fit && galley.rows.len() <= 2;
        if fits {
            return (galley, true);
        }
        last = Some(galley);
    }
    (last.expect("at least one size"), false)
}

fn layout_word(ctx: &egui::Context, word: &str, size: f32) -> f32 {
    ctx.fonts_mut(|f| f.layout_no_wrap(word.to_owned(), heavy(size), Color32::PLACEHOLDER))
        .size()
        .x
}

fn kind_has_glyph(kind: &GateKind) -> bool {
    use GateKind as G;
    matches!(
        kind,
        G::And { .. }
            | G::Nand { .. }
            | G::Or { .. }
            | G::Nor { .. }
            | G::Xor { .. }
            | G::Xnor { .. }
            | G::Not { .. }
    )
}

/// A chip's name, wrapped small inside its slot.
fn chip_name(painter: &egui::Painter, face: egui::Rect, name: &str, colour: Color32) {
    // Below the key number, centred in the rest of the face.
    let area = egui::Rect::from_min_max(
        face.min + egui::vec2(3.0, 15.0),
        face.max - egui::vec2(3.0, 3.0),
    );
    let galley = painter.layout(name.to_string(), heavy(9.5), colour, area.width() - 2.0);
    let pos = egui::pos2(
        area.center().x - galley.size().x * 0.5,
        (area.center().y - galley.size().y * 0.5).max(area.min.y),
    );
    painter.with_clip_rect(area).galley(pos, galley, colour);
}

/// Register a workspace's root as a definition and return the cell kind.
fn cell_kind(
    register: &mut MessageWriter<RegisterDefinitions>,
    ws: &WorkspaceDocument,
) -> GateKind {
    let (registration, kind) = RegisterDefinitions::chip(ws);
    register.write(registration);
    kind
}

/// The kind and label a slot places (registering a chip's definition).
fn resolve_slot(
    item: &SlotItem,
    register: &mut MessageWriter<RegisterDefinitions>,
    status: &mut Option<String>,
) -> Option<(GateKind, Option<String>)> {
    match item {
        SlotItem::Kind(kind) => Some((kind.clone(), None)),
        SlotItem::Demo(demo) => Some((
            cell_kind(register, &demo.workspace),
            Some(demo.title.clone()),
        )),
        SlotItem::Saved(name) => match read_workspace(name) {
            Ok(ws) => Some((cell_kind(register, &ws), Some(name.clone()))),
            Err(err) => {
                warn!("Cannot place workspace {name}: {err}");
                *status = Some(err.to_string());
                None
            }
        },
    }
}

/// The parts bar: family tabs, the palette width, nine numbered slots
/// (keys `1`–`9`) and the wire tool. Click a slot to arm it (then click
/// the canvas), or drag it onto the canvas.
#[allow(clippy::too_many_arguments)]
pub fn parts_bar_system(
    mut contexts: EguiContexts,
    mut state: ResMut<EditorState>,
    theme: Res<Theme>,
    keys: Res<ButtonInput<KeyCode>>,
    mut register: MessageWriter<RegisterDefinitions>,
    mut create: MessageWriter<CreateGate>,
    q_camera: Query<(&Camera, &GlobalTransform), With<MainCamera>>,
    active: Res<ActiveCircuit>,
    game_state: Res<State<GameState>>,
) -> Result {
    if game_state.get() != &GameState::Simulating {
        state.dragging_part = None;
        return Ok(());
    }
    let ctx = contexts.ctx_mut()?;
    if !crate::ui_style::workshop_fonts_ready(ctx) {
        return Ok(());
    }
    let palette = theme.palette;
    let layout = hud_layout(ctx);
    let items = slot_items(state.part_tab, &state);

    let mut activate: Option<usize> = None;
    let mut drag: Option<usize> = None;
    let mut tab = state.part_tab;
    let mut width = state.palette_width;
    let mut toggle_wire = false;
    fixed_card(ctx, "hud_parts", layout.parts_rect(), &palette, 12, |ui| {
        ui.spacing_mut().item_spacing = egui::vec2(6.0, 10.0);
        ui.horizontal(|ui| {
            for t in PartTab::ALL {
                if chip(ui, &palette, t.title(), t == tab).clicked() {
                    tab = t;
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add(
                    egui::DragValue::new(&mut width)
                        .range(1..=64)
                        .suffix(" bit"),
                )
                .on_hover_text("Width of the parts you place");
                if !layout.compact {
                    ui.label(
                        egui::RichText::new(if active.read_only {
                            "read-only demo"
                        } else {
                            "drag, or press a number"
                        })
                        .size(12.0)
                        .color(egui_color(palette.text_secondary)),
                    );
                }
            });
        });
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = SLOT_GAP;
            let slots = layout.slot;
            let row = (slots + SLOT_GAP) * KEYED_SLOTS as f32 - SLOT_GAP;
            let mut slots_row = |ui: &mut Ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = SLOT_GAP;
                    for (i, item) in items.iter().enumerate() {
                        let response = part_slot(
                            ui,
                            &palette,
                            slots,
                            slot_key_label(i),
                            item,
                            item.is_armed(&state),
                        )
                        .on_hover_text(item.hover());
                        if response.drag_started() {
                            drag = Some(i);
                        } else if response.clicked() {
                            activate = Some(i);
                        }
                    }
                    if items.is_empty() {
                        ui.label("No chips yet: save a workspace to use it here.");
                    }
                });
            };
            if items.len() > KEYED_SLOTS {
                egui::ScrollArea::horizontal()
                    .id_salt("parts_slots")
                    .max_width(row)
                    .auto_shrink([false, true])
                    .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
                    .show(ui, |ui| slots_row(ui));
            } else {
                ui.scope(|ui| {
                    ui.set_width(row);
                    slots_row(ui);
                });
            }
            ui.add_space(TOOL_GAP - SLOT_GAP);
            let (rect, response) =
                ui.allocate_exact_size(egui::vec2(slots, slots + 3.0), Sense::click());
            let on = state.connection_mode;
            paint_tool_slot(ui, &palette, rect, &response, on);
            if response
                .on_hover_text("Wire tool (W): click a source part, then a sink")
                .clicked()
            {
                toggle_wire = true;
            }
        });
    });

    if tab != state.part_tab {
        state.part_tab = tab;
    }
    if width != state.palette_width {
        state.palette_width = width;
    }
    if toggle_wire {
        state.connection_mode = !state.connection_mode;
        state.selected_gate_type = None;
        state.clear_connection();
    }
    // Number keys pick a slot of the open tab.
    if !ctx.egui_wants_keyboard_input() {
        for key in SLOT_KEYS {
            if keys.just_pressed(key)
                && let Some(slot) = slot_for_key(key)
                && slot < items.len()
            {
                activate = Some(slot);
            }
        }
    }
    let mut status = state.status.take();
    if let Some(i) = activate {
        let item = &items[i];
        if item.is_armed(&state) {
            state.selected_gate_type = None;
        } else if let Some((kind, label)) = resolve_slot(item, &mut register, &mut status) {
            state.arm(kind, label);
        }
    }
    if let Some(i) = drag
        && let Some(pick) = resolve_slot(&items[i], &mut register, &mut status)
    {
        state.dragging_part = Some(pick);
    }
    state.status = status;

    // Drag and drop onto the canvas.
    if let Some((kind, label)) = state.dragging_part.clone() {
        let (pointer, released) =
            ctx.input(|i| (i.pointer.latest_pos(), !i.pointer.primary_down()));
        if let Some(pos) = pointer {
            egui::Area::new(egui::Id::new("hud_drag_ghost"))
                .fixed_pos(pos + egui::vec2(12.0, 12.0))
                .order(Order::Tooltip)
                .interactable(false)
                .show(ctx, |ui| {
                    card(&palette).show(ui, |ui| {
                        ui.label(
                            egui::RichText::new(label.as_deref().unwrap_or(kind_title(&kind)))
                                .font(heavy(14.0)),
                        );
                    });
                });
        }
        if released {
            state.dragging_part = None;
            if let Some(pos) = pointer
                && ctx.layer_id_at(pos).is_none()
                && let Ok((camera, transform)) = q_camera.single()
                && let Ok(world) = camera.viewport_to_world_2d(transform, Vec2::new(pos.x, pos.y))
            {
                create.write(CreateGate {
                    kind,
                    label,
                    position: world,
                });
            }
        }
    }
    Ok(())
}

/// The wire tool slot: a cased wire between two pins.
fn paint_tool_slot(
    ui: &Ui,
    palette: &Palette,
    rect: egui::Rect,
    response: &egui::Response,
    on: bool,
) {
    let painter = ui.painter();
    let ink = egui_color(palette.ink);
    let side = rect.width();
    let lift = if response.hovered() { 2.0 } else { 0.0 };
    let face = egui::Rect::from_min_size(rect.min - egui::vec2(0.0, lift), egui::vec2(side, side));
    let radius = CornerRadius::same(12);
    painter.rect_filled(face.translate(egui::vec2(0.0, 3.0 + lift)), radius, ink);
    let fill = if on { palette.on } else { palette.raised };
    painter.rect(
        face,
        radius,
        egui_color(fill),
        stroke(2.5, ink),
        StrokeKind::Inside,
    );
    painter.text(
        face.min + egui::vec2(7.0, 4.0),
        Align2::LEFT_TOP,
        "W",
        FontId::monospace(10.0),
        egui_color(if on {
            palette.on_ink
        } else {
            palette.text_faint
        }),
    );
    let c = face.center() + egui::vec2(0.0, 2.0);
    let a = c + egui::vec2(-side * 0.26, side * 0.12);
    let b = c + egui::vec2(side * 0.26, -side * 0.12);
    let mid = egui::pos2(c.x, a.y);
    let mid2 = egui::pos2(c.x, b.y);
    let path = vec![a, mid, mid2, b];
    painter.add(egui::Shape::line(path.clone(), stroke(8.0, ink)));
    painter.add(egui::Shape::line(
        path,
        stroke(4.0, egui_color(palette.bus)),
    ));
    for p in [a, b] {
        painter.circle(p, 4.0, egui_color(palette.tile), stroke(2.0, ink));
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Build status pill
// ─────────────────────────────────────────────────────────────────────────────

/// Top right: whether the circuit built, its part count, and loop or
/// build errors (click for the details, which also open by themselves
/// when a new error appears).
pub fn build_pill_system(
    mut contexts: EguiContexts,
    theme: Res<Theme>,
    diagnostics: Res<SimDiagnostics>,
    gates: Query<(), With<GateIdentity>>,
    game_state: Res<State<GameState>>,
    mut details_open: Local<bool>,
    mut seen_error: Local<Option<u64>>,
) -> Result {
    if game_state.get() != &GameState::Simulating {
        return Ok(());
    }
    let ctx = contexts.ctx_mut()?;
    if !crate::ui_style::workshop_fonts_ready(ctx) {
        return Ok(());
    }
    let palette = theme.palette;
    let layout = hud_layout(ctx);
    let parts = gates.iter().count();
    let error = diagnostics.compile_error.as_ref();
    let error_rev = error.map(|e| e.revision);
    if error_rev != *seen_error {
        *details_open = error.is_some();
        *seen_error = error_rev;
    }
    let (dot, text) = match (error, diagnostics.running_revision) {
        (Some(e), _) if e.loops.iter().any(|l| !l.is_empty()) => {
            (palette.fault, "Loop error".to_string())
        }
        (Some(_), _) => (palette.fault, "Build error".to_string()),
        (None, None) if parts > 0 => (palette.on, "Building…".to_string()),
        _ => (
            palette.bus,
            format!("Built · {parts} part{}", if parts == 1 { "" } else { "s" }),
        ),
    };
    let mut toggle = false;
    egui::Area::new(egui::Id::new("hud_build"))
        .anchor(Align2::RIGHT_TOP, [-layout.margin, layout.margin])
        .order(Order::Middle)
        .show(ctx, |ui| {
            card(&palette)
                .inner_margin(egui::Margin::symmetric(16, 0))
                .show(ui, |ui| {
                    ui.set_height(crate::ui_style::TOP_CARD_HEIGHT - 5.0);
                    let response = ui
                        .horizontal_centered(|ui| {
                            let (r, _) =
                                ui.allocate_exact_size(egui::vec2(14.0, 14.0), Sense::hover());
                            ui.painter().circle(
                                r.center(),
                                6.0,
                                egui_color(dot),
                                stroke(2.0, egui_color(palette.ink)),
                            );
                            ui.label(egui::RichText::new(&text).font(heavy(14.0)));
                            if error.is_some() {
                                ui.label(caption(
                                    &palette,
                                    if *details_open { "hide" } else { "details" },
                                ));
                            }
                        })
                        .response
                        .interact(Sense::click());
                    let hover = match diagnostics.running_revision {
                        Some(rev) => format!("Running revision {rev}"),
                        None => "Compiling…".to_string(),
                    };
                    if response.on_hover_text(hover).clicked() {
                        toggle = true;
                    }
                });
        });
    if toggle && error.is_some() {
        *details_open = !*details_open;
    }
    if let (Some(error), true) = (error, *details_open) {
        let fault = egui_color(palette.fault_text);
        egui::Area::new(egui::Id::new("hud_build_details"))
            .anchor(Align2::RIGHT_TOP, [-layout.margin, layout.top_band()])
            .order(Order::Foreground)
            .show(ctx, |ui| {
                card(&palette).show(ui, |ui| {
                    ui.set_max_width(layout.inspector_width);
                    ui.label(
                        egui::RichText::new(format!(
                            "Edit {} not applied; the previous circuit keeps running.",
                            error.revision
                        ))
                        .font(heavy(14.0))
                        .color(fault),
                    );
                    for message in &error.messages {
                        ui.colored_label(fault, message);
                    }
                    if error.loops.iter().any(|l| !l.is_empty()) {
                        ui.label("The parts and wires on the loop are outlined in red.");
                    }
                });
            });
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Signals
// ─────────────────────────────────────────────────────────────────────────────

/// One row of the Signals card.
#[derive(Debug, Clone, PartialEq)]
pub struct SignalRow {
    pub entity: Entity,
    pub gate: Uuid,
    pub name: String,
    pub width: u8,
    pub value: u64,
    pub watched: bool,
}

/// Which gates the Signals card lists, in order: watched gates first (in
/// watch order), then the circuit's inputs and clocks, its registers and
/// latches, and its outputs and readouts, each group by name.
pub fn signal_order(kind: &GateKind, watched: Option<usize>) -> Option<(u8, usize)> {
    use GateKind as G;
    if let Some(i) = watched {
        return Some((0, i));
    }
    let group = match kind {
        G::Input { .. } | G::Clock { .. } => 1,
        G::Register { .. } | G::DLatch { .. } | G::SrLatch => 2,
        G::Output { .. } | G::Display { .. } => 3,
        _ => return None,
    };
    Some((group, 0))
}

/// The value a gate shows as a signal: its first output, or for a sink
/// (output port, readout) its first input.
fn signal_value(values: &GateValues) -> u64 {
    values
        .outputs
        .first()
        .or(values.inputs.first())
        .copied()
        .unwrap_or(0)
}

fn signal_width(library: &CircuitLibrary, kind: &GateKind) -> u8 {
    library
        .pins(kind)
        .and_then(|(ins, outs)| outs.first().or(ins.first()).copied())
        .unwrap_or(1)
}

/// Bottom right: live values of the watched gates and of the open
/// circuit's ports and registers. Click a name to select the part.
#[allow(clippy::too_many_arguments)]
pub fn signals_card_system(
    mut contexts: EguiContexts,
    theme: Res<Theme>,
    mut state: ResMut<EditorState>,
    mut selected: ResMut<SelectedGate>,
    library: Res<CircuitLibrary>,
    gates: Query<(Entity, &GateIdentity, &GateType, &GateLabel, &GateValues)>,
    game_state: Res<State<GameState>>,
    mut collapsed: Local<bool>,
) -> Result {
    if game_state.get() != &GameState::Simulating {
        return Ok(());
    }
    let ctx = contexts.ctx_mut()?;
    if !crate::ui_style::workshop_fonts_ready(ctx) {
        return Ok(());
    }
    let palette = theme.palette;
    let layout = hud_layout(ctx);
    let mut rows: Vec<((u8, usize), SignalRow)> = gates
        .iter()
        .filter_map(|(entity, id, ty, label, values)| {
            let watched = state.watched.iter().position(|g| *g == id.uuid);
            let order = signal_order(&ty.kind, watched)?;
            let name = if label.value.trim().is_empty() {
                kind_title(&ty.kind).to_string()
            } else {
                label.value.clone()
            };
            Some((
                order,
                SignalRow {
                    entity,
                    gate: id.uuid,
                    name,
                    width: signal_width(&library, &ty.kind),
                    value: signal_value(values),
                    watched: watched.is_some(),
                },
            ))
        })
        .collect();
    rows.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.name.cmp(&b.1.name)));

    let mut rect = layout.signals_rect();
    if *collapsed {
        rect.min.y = rect.max.y - 52.0;
    }
    let mut toggle_collapsed = false;
    let mut select = None;
    let mut unwatch = None;
    fixed_card(ctx, "hud_signals", rect, &palette, 12, |ui| {
        ui.horizontal(|ui| {
            let title = ui
                .add(
                    egui::Label::new(egui::RichText::new("Signals").font(heavy(15.0)))
                        .sense(Sense::click()),
                )
                .on_hover_text(if *collapsed { "Show" } else { "Hide" });
            if title.clicked() {
                toggle_collapsed = true;
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .small_button(if *collapsed { "show" } else { "hide" })
                    .clicked()
                {
                    toggle_collapsed = true;
                }
                ui.label(
                    egui::RichText::new("live")
                        .monospace()
                        .size(11.0)
                        .color(egui_color(palette.text_secondary)),
                );
            });
        });
        if *collapsed {
            return;
        }
        if rows.is_empty() {
            ui.label(
                egui::RichText::new("Watch a part from the inspector to list its value here.")
                    .color(egui_color(palette.text_secondary)),
            );
            return;
        }
        let row_h = 26.0;
        let fit = ((ui.available_height() - 4.0) / row_h).floor().max(1.0) as usize;
        let shown = if rows.len() > fit {
            fit - 1
        } else {
            rows.len()
        };
        let name_w = (ui.available_width() * 0.32).clamp(70.0, 140.0);
        for (_, row) in rows.iter().take(shown) {
            ui.horizontal(|ui| {
                ui.set_height(row_h - 6.0);
                let (name_rect, name_resp) =
                    ui.allocate_exact_size(egui::vec2(name_w, row_h - 6.0), Sense::click());
                let galley = ui.painter().layout(
                    row.name.clone(),
                    FontId::monospace(12.0),
                    egui_color(palette.text),
                    f32::INFINITY,
                );
                ui.painter().with_clip_rect(name_rect).galley(
                    egui::pos2(
                        name_rect.min.x,
                        name_rect.center().y - galley.size().y * 0.5,
                    ),
                    galley,
                    egui_color(palette.text),
                );
                if name_resp
                    .on_hover_text(format!("{}: select it", row.name))
                    .clicked()
                {
                    select = Some(row.entity);
                }
                let avail = ui.available_width() - if row.watched { 26.0 } else { 0.0 };
                let (bar, _) = ui
                    .allocate_exact_size(egui::vec2(avail.max(40.0), row_h - 6.0), Sense::hover());
                paint_signal(ui, &palette, bar, row.width, row.value);
                if row.watched
                    && ui
                        .small_button("×")
                        .on_hover_text("Stop watching")
                        .clicked()
                {
                    unwatch = Some(row.gate);
                }
            });
        }
        if shown < rows.len() {
            ui.label(caption(&palette, &format!("+{} more", rows.len() - shown)));
        }
    });
    if toggle_collapsed {
        *collapsed = !*collapsed;
    }
    if let Some(entity) = select {
        selected.0 = Some(entity);
    }
    if let Some(gate) = unwatch {
        state.toggle_watch(gate);
    }
    Ok(())
}

/// A signal's value as the waveform lane would end: a marigold bar for
/// an on bit, a slate line for an off one, a teal pill for a bus.
fn paint_signal(ui: &Ui, palette: &Palette, rect: egui::Rect, width: u8, value: u64) {
    let painter = ui.painter();
    let ink = egui_color(palette.ink);
    if width <= 1 {
        if value != 0 {
            let bar = egui::Rect::from_min_max(
                egui::pos2(rect.min.x, rect.center().y - 8.0),
                egui::pos2(rect.max.x, rect.center().y + 8.0),
            );
            painter.rect(
                bar,
                CornerRadius::same(5),
                egui_color(palette.on),
                stroke(2.0, ink),
                StrokeKind::Inside,
            );
        } else {
            let y = rect.center().y + 6.0;
            painter.line_segment(
                [egui::pos2(rect.min.x, y), egui::pos2(rect.max.x, y)],
                stroke(3.0, egui_color(palette.off)),
            );
        }
        return;
    }
    let (fill, text) = if value != 0 {
        (palette.bus, palette.tile)
    } else {
        (palette.bus_zero, palette.text)
    };
    painter.rect(
        rect,
        CornerRadius::same(8),
        egui_color(fill),
        stroke(2.0, ink),
        StrokeKind::Inside,
    );
    painter.text(
        rect.center(),
        Align2::CENTER_CENTER,
        format_live_value(width, value),
        FontId::monospace(12.0),
        egui_color(text),
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Toasts and keys
// ─────────────────────────────────────────────────────────────────────────────

/// Seconds a toast stays up.
const TOAST_SECS: f32 = 6.0;

/// Above the parts bar: the last authoring message (a refused edit, a
/// save) and the editor's workspace status, for a few seconds each.
pub fn toast_system(
    mut contexts: EguiContexts,
    theme: Res<Theme>,
    mut feedback: ResMut<AuthoringFeedback>,
    mut state: ResMut<EditorState>,
    time: Res<Time>,
    game_state: Res<State<GameState>>,
    mut shown_for: Local<f32>,
) -> Result {
    if game_state.get() != &GameState::Simulating {
        return Ok(());
    }
    if feedback.is_changed() || state.is_changed() && state.status.is_some() {
        *shown_for = 0.0;
    } else {
        *shown_for += time.delta_secs();
    }
    let messages: Vec<String> = feedback
        .message
        .iter()
        .chain(state.status.iter())
        .cloned()
        .collect();
    if messages.is_empty() || *shown_for > TOAST_SECS {
        return Ok(());
    }
    let ctx = contexts.ctx_mut()?;
    if !crate::ui_style::workshop_fonts_ready(ctx) {
        return Ok(());
    }
    let palette = theme.palette;
    let layout = hud_layout(ctx);
    let mut dismiss = false;
    egui::Area::new(egui::Id::new("hud_toast"))
        .anchor(
            Align2::CENTER_BOTTOM,
            [0.0, -(layout.margin + layout.parts.y + layout.gap)],
        )
        .order(Order::Foreground)
        .show(ctx, |ui| {
            card(&palette).show(ui, |ui| {
                ui.set_max_width(layout.parts.x - 40.0);
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        for message in &messages {
                            ui.label(egui::RichText::new(message).color(egui_color(palette.text)));
                        }
                    });
                    if ui.small_button("×").clicked() {
                        dismiss = true;
                    }
                });
            });
        });
    if dismiss {
        feedback.message = None;
        state.status = None;
    }
    Ok(())
}

/// `Esc`, in order: if a part is armed or being dragged from the parts
/// bar, the wire tool is on, or the Library drawer is open, drop all of
/// those; otherwise clear the selection (close the inspector). So one `Esc`
/// cancels the tool in hand and a second deselects.
pub fn escape(state: &mut EditorState, selected: &mut SelectedGate) {
    let armed = state.selected_gate_type.is_some()
        || state.dragging_part.is_some()
        || state.connection_mode
        || state.connection_source.is_some()
        || state.library_open;
    if armed {
        state.selected_gate_type = None;
        state.selected_gate_label = None;
        state.dragging_part = None;
        state.connection_mode = false;
        state.clear_connection();
        state.library_open = false;
    } else {
        selected.0 = None;
    }
}

/// `Esc` drops the armed part, the wire tool and the Library drawer, or
/// with none of those clears the selection (see [`escape`]); `W`
/// toggles the wire tool; `F` fits the view. Ignored while a text field
/// has the keyboard. (`1`–`9` are handled by the parts bar, `T` by the
/// theme, `Delete` by the inspector.)
pub fn editor_keys_system(
    mut contexts: EguiContexts,
    keys: Res<ButtonInput<KeyCode>>,
    mut state: ResMut<EditorState>,
    mut fit: MessageWriter<FitView>,
    game_state: Res<State<GameState>>,
    mut selected: Option<ResMut<SelectedGate>>,
) -> Result {
    if game_state.get() != &GameState::Simulating {
        return Ok(());
    }
    let ctx = contexts.ctx_mut()?;
    if !crate::ui_style::workshop_fonts_ready(ctx) {
        return Ok(());
    }
    if ctx.egui_wants_keyboard_input() {
        return Ok(());
    }
    if keys.just_pressed(KeyCode::Escape) {
        let mut next = selected.as_deref().copied().unwrap_or_default();
        escape(&mut state, &mut next);
        if let Some(selected) = selected.as_mut() {
            selected.set_if_neq(next);
        }
    }
    if keys.just_pressed(KeyCode::KeyW) {
        state.connection_mode = !state.connection_mode;
        state.selected_gate_type = None;
        state.clear_connection();
    }
    if keys.just_pressed(KeyCode::KeyF) {
        fit.write(FitView);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: Vec2, b: Vec2) -> bool {
        (a - b).length() < 1e-3
    }

    /// A context with the Workshop fonts installed.
    fn fonts_ctx() -> egui::Context {
        let ctx = egui::Context::default();
        ctx.set_fonts(crate::ui_style::workshop_fonts());
        for _ in 0..2 {
            let _ = ctx.run_ui(egui::RawInput::default(), |_| {});
        }
        assert!(crate::ui_style::workshop_fonts_ready(&ctx));
        ctx
    }

    /// Slots without a glyph show the inspector's name for the kind
    /// (`kind_title`), whole words, at a readable size, in both layouts.
    #[test]
    fn slot_text_is_the_inspector_name_and_fits() {
        let ctx = fonts_ctx();
        for screen in [Vec2::new(1920.0, 1080.0), Vec2::new(1280.0, 720.0)] {
            let side = crate::ui_style::HudLayout::new(screen).slot;
            for tab in PartTab::ALL {
                for kind in tab_kinds(tab, 8) {
                    if kind_has_glyph(&kind) {
                        continue;
                    }
                    let title = kind_title(&kind);
                    let (galley, fits) = slot_label(&ctx, title, side - SLOT_TEXT_PAD);
                    assert!(fits, "{title} does not fit a {side} slot");
                    assert_eq!(galley.text(), title);
                    let size = galley.job.sections[0].format.font_id.size;
                    assert!(size >= 8.0, "{title}: {size}");
                    if side >= 64.0 {
                        assert!(size >= 10.0, "{title}: {size} in a {side} slot");
                    }
                }
            }
        }
    }

    #[test]
    fn chips_tab_lists_saved_workspaces_then_demos_then_the_gallery() {
        let catalog = demos::catalog().unwrap();
        let state = EditorState {
            saved_workspaces: Some(vec!["mine".into()]),
            ..Default::default()
        };
        let items = slot_items(PartTab::Chips, &state);
        assert_eq!(items.len(), 1 + catalog.demos.len() + catalog.gallery.len());
        assert!(matches!(&items[0], SlotItem::Saved(n) if n == "mine"));
        let ids: Vec<&str> = items[1..]
            .iter()
            .map(|i| match i {
                SlotItem::Demo(d) => d.id.as_str(),
                _ => panic!("only demos and gallery items after the saved ones"),
            })
            .collect();
        let want: Vec<&str> = catalog
            .demos
            .iter()
            .chain(&catalog.gallery)
            .map(|d| d.id.as_str())
            .collect();
        assert_eq!(ids, want);
        assert!(ids.contains(&"cpu_fibonacci"));
    }

    #[test]
    fn escape_cancels_the_armed_tool_first_then_deselects() {
        let gate = World::new().spawn_empty().id();
        let mut selected = SelectedGate(Some(gate));
        let mut state = EditorState {
            library_open: true,
            ..Default::default()
        };
        state.arm(GateKind::Not { w: 1 }, None);
        escape(&mut state, &mut selected);
        assert!(state.selected_gate_type.is_none() && !state.library_open);
        assert_eq!(selected.0, Some(gate), "the first Esc only drops the tool");
        escape(&mut state, &mut selected);
        assert_eq!(selected.0, None, "the second Esc deselects");

        selected.0 = Some(gate);
        state.connection_mode = true;
        escape(&mut state, &mut selected);
        assert!(!state.connection_mode);
        assert_eq!(selected.0, Some(gate));
        escape(&mut state, &mut selected);
        assert_eq!(selected.0, None);
    }

    #[test]
    fn minimap_maps_world_into_the_map_at_one_scale() {
        let map = Rect::new(10.0, 20.0, 230.0, 130.0); // 220 x 110, y down
        let world = Rect::new(-100.0, -100.0, 300.0, 100.0); // 400 x 200, y up
        let t = MinimapTransform::new(world, map);
        assert!((t.scale() - 0.55).abs() < 1e-6);
        // Centres meet; world y up is map y down.
        assert!(close(t.to_map(world.center()), map.center()));
        assert!(close(
            t.to_map(Vec2::new(-100.0, 100.0)),
            Vec2::new(10.0, 20.0)
        ));
        assert!(close(
            t.to_map(Vec2::new(300.0, -100.0)),
            Vec2::new(230.0, 130.0)
        ));
        // Round trip.
        for p in [Vec2::new(0.0, 0.0), Vec2::new(123.0, -45.0)] {
            assert!(close(t.to_world(t.to_map(p)), p));
        }
        let r = t.rect_to_map(world);
        assert!(close(r.min, map.min) && close(r.max, map.max));
    }

    #[test]
    fn minimap_keeps_the_aspect_and_centres_the_short_side() {
        let map = Rect::new(0.0, 0.0, 200.0, 100.0);
        // A tall world: height decides, x is centred.
        let world = Rect::new(0.0, 0.0, 100.0, 400.0);
        let t = MinimapTransform::new(world, map);
        assert!((t.scale() - 0.25).abs() < 1e-6);
        let r = t.rect_to_map(world);
        assert!(close(r.size(), Vec2::new(25.0, 100.0)));
        assert!(close(r.center(), map.center()));
        // A click on the map's centre jumps to the world's centre.
        assert!(close(t.to_world(map.center()), world.center()));
    }

    #[test]
    fn minimap_world_pads_the_circuit() {
        let content = Rect::new(0.0, 0.0, 1000.0, 200.0);
        let world = minimap_world(content);
        assert!(world.contains(content.min) && world.contains(content.max));
        assert!(close(world.center(), content.center()));
        assert!(world.width() > content.width() + 100.0);
    }

    #[test]
    fn signals_list_watched_gates_then_ports_and_state() {
        let input = GateKind::Input { w: 1 };
        let reg = GateKind::Register { w: 8 };
        let out = GateKind::Display { w: 8 };
        let and = GateKind::And { w: 1 };
        assert_eq!(signal_order(&and, None), None);
        assert_eq!(signal_order(&and, Some(2)), Some((0, 2)));
        let o = |k| signal_order(k, None).unwrap();
        assert!(o(&input) < o(&reg) && o(&reg) < o(&out));
        assert!(signal_order(&out, Some(5)).unwrap() < o(&input));
    }
}
