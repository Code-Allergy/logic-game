//! Drag to wire: press on a pin, drag, and release on a pin of the other
//! direction and the same width to make a wire.
//!
//! - [`pin_under`] finds the pin under the pointer: the nearest pin whose
//!   dot, grown by a slack of [`PIN_SLACK_PX`] screen pixels (so the
//!   target keeps its size on screen as the view zooms, up to
//!   [`PIN_REACH_MAX`] world units), holds the pointer;
//! - the canvas pointer ([`super::systems::canvas_pointer_system`]) keeps
//!   [`PinPointer`] up to date (the hovered pin and the wire being
//!   dragged) and on release decides with [`drop_outcome`]: a wire goes
//!   through [`CreateWire`] like any other edit, a refusal is a toast;
//! - [`wire_drag_visuals_system`] draws it in the Workshop look: a cased
//!   rubber band routed like a wire (to the pin under the pointer, else to
//!   the pointer on the grid; tomato over a pin it cannot drop on),
//!   marigold rings on the hovered and the compatible pins, the other pins
//!   faded under a veil of the bench colour;
//! - [`pin_tooltip_system`] names the pin under the pointer (`q · 8 bit`).

use bevy::prelude::*;
use bevy_egui::{EguiContexts, egui};
use uuid::Uuid;

use super::art::{BIT_CORE, BUS_CORE, CanvasArt, PIN_RING, Paint, PinLayout, casing_for};
use super::routing::{self, GRID, NetSpec, RouteProblem};
use super::shapes::{Shape, circle_segments, round_corners};
use super::systems::CanvasCursor;
use super::visuals::{TileSpec, WIRE_CORNER};
use super::{CircuitLibrary, CreateWire, GateIdentity, GateKind, GateLabel, GateType};
use crate::theme::{Palette, Theme};
use crate::ui_style::{egui_color, heavy, stroke, window_to_egui};

// ─────────────────────────────────────────────────────────────────────────────
// Pins and hit-testing
// ─────────────────────────────────────────────────────────────────────────────

/// Screen pixels around a pin dot that still count as the pin.
pub const PIN_SLACK_PX: f32 = 6.0;
/// The most a pin reaches from its centre, in world units (half the pin
/// pitch): zoomed far out, the rest of a tile stays the tile.
pub const PIN_REACH_MAX: f32 = 10.0;

/// How far from its centre a pin of dot `radius` takes the pointer, at
/// `world_per_pixel` world units per screen pixel.
pub fn pin_reach(radius: f32, world_per_pixel: f32) -> f32 {
    (radius + PIN_SLACK_PX * world_per_pixel.max(0.0)).min(PIN_REACH_MAX.max(radius))
}

/// A pin on the canvas.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PinRef {
    pub gate: Entity,
    pub uuid: Uuid,
    pub output: bool,
    pub index: u8,
    pub width: u8,
    /// World position of the pin's centre.
    pub at: Vec2,
    /// Dot radius.
    pub radius: f32,
}

impl PinRef {
    /// A wire can join the two: an output and an input of one width.
    pub fn compatible(&self, other: &PinRef) -> bool {
        self.output != other.output && self.width == other.width
    }

    pub fn same_pin(&self, other: &PinRef) -> bool {
        (self.uuid, self.output, self.index) == (other.uuid, other.output, other.index)
    }

    /// The wire from the output of the two to the input.
    pub fn wire_to(&self, other: &PinRef) -> CreateWire {
        let (source, sink) = if self.output {
            (self, other)
        } else {
            (other, self)
        };
        CreateWire {
            source_gate: source.uuid,
            source_pin: source.index,
            sink_gate: sink.uuid,
            sink_pin: sink.index,
        }
    }
}

/// The pins of a gate at `at` (inputs, then outputs), as its tile draws
/// them.
pub fn gate_pins<'a>(
    gate: Entity,
    uuid: Uuid,
    at: Vec2,
    layout: &'a PinLayout,
    spec: &'a TileSpec,
) -> impl Iterator<Item = PinRef> + 'a {
    let side = move |output: bool, widths: &'a [u8], spots: &'a [Vec2]| {
        widths
            .iter()
            .zip(spots)
            .enumerate()
            .map(move |(i, (&width, &spot))| PinRef {
                gate,
                uuid,
                output,
                index: i as u8,
                width,
                at: at + spot,
                radius: layout.radius,
            })
    };
    side(false, &spec.inputs, &layout.inputs).chain(side(true, &spec.outputs, &layout.outputs))
}

/// The pin under `world`: the nearest whose reach ([`pin_reach`]) holds it.
pub fn pin_under(
    pins: impl IntoIterator<Item = PinRef>,
    world: Vec2,
    world_per_pixel: f32,
) -> Option<PinRef> {
    pins.into_iter()
        .filter_map(|pin| {
            let d = pin.at.distance(world);
            (d <= pin_reach(pin.radius, world_per_pixel)).then_some((d, pin))
        })
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, pin)| pin)
}

// ─────────────────────────────────────────────────────────────────────────────
// State and outcome
// ─────────────────────────────────────────────────────────────────────────────

/// The wire being dragged out of a pin.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WireDrag {
    pub from: PinRef,
    /// The pointer, in world coordinates.
    pub pointer: Vec2,
}

/// Pins under the pointer, for drawing: the hovered pin and the wire in
/// hand. Written by the canvas pointer only when it changes.
#[derive(Resource, Debug, Default, Clone, PartialEq)]
pub struct PinPointer {
    pub hover: Option<PinRef>,
    pub drag: Option<WireDrag>,
}

impl PinPointer {
    /// The pin a drag would drop on (not the one it started from).
    pub fn target(&self) -> Option<PinRef> {
        let drag = self.drag?;
        self.hover.filter(|pin| !pin.same_pin(&drag.from))
    }
}

/// What releasing a wire drag does.
#[derive(Debug, Clone, PartialEq)]
pub enum DropOutcome {
    /// Make this wire.
    Connect(CreateWire),
    /// Make nothing, and say why.
    Refuse(String),
    /// Make nothing, quietly (dropped back on its own pin).
    Cancel,
}

/// The outcome of dropping a wire dragged from `from` on `target`;
/// `name` names a pin for the messages ("q of Count").
pub fn drop_outcome(
    from: &PinRef,
    target: Option<&PinRef>,
    name: impl Fn(&PinRef) -> String,
) -> DropOutcome {
    let Some(target) = target else {
        return DropOutcome::Refuse(format!(
            "No wire made: drop it on {} pin.",
            if from.output { "an input" } else { "an output" }
        ));
    };
    if target.same_pin(from) {
        return DropOutcome::Cancel;
    }
    if target.output == from.output {
        let side = if from.output { "outputs" } else { "inputs" };
        return DropOutcome::Refuse(format!(
            "No wire made: {} and {} are both {side}. A wire runs from an output to an input.",
            name(from),
            name(target)
        ));
    }
    if target.width != from.width {
        return DropOutcome::Refuse(format!(
            "No wire made: {} is {} and {} is {}. Split, Merge or Extend changes widths.",
            name(from),
            bits(from.width),
            name(target),
            bits(target.width)
        ));
    }
    DropOutcome::Connect(from.wire_to(target))
}

fn bits(width: u8) -> String {
    format!("{width} bit")
}

/// A pin's name ("q", "in0"), or its number when the kind has no names.
pub fn pin_name(library: &CircuitLibrary, kind: &GateKind, output: bool, index: u8) -> String {
    let (ins, outs) = library.pin_names(kind);
    let names = if output { outs } else { ins };
    names
        .get(usize::from(index))
        .cloned()
        .unwrap_or_else(|| format!("{}{index}", if output { "out" } else { "in" }))
}

/// The tooltip text of a pin: `q · 8 bit`.
pub fn pin_text(name: &str, width: u8) -> String {
    format!("{name} · {}", bits(width))
}

/// A pin in a message: "q of Count" (the part by its label, else its kind).
pub fn pin_title(
    library: &CircuitLibrary,
    kind: &GateKind,
    label: Option<&GateLabel>,
    pin: &PinRef,
) -> String {
    let part = label
        .map(|l| l.value.trim())
        .filter(|l| !l.is_empty())
        .map_or_else(
            || crate::editor::parts::kind_title(kind).to_owned(),
            str::to_owned,
        );
    format!(
        "{} of {part}",
        pin_name(library, kind, pin.output, pin.index)
    )
}

// ─────────────────────────────────────────────────────────────────────────────
// Rubber band
// ─────────────────────────────────────────────────────────────────────────────

/// Tiles further than this from the two ends of the rubber band are left
/// out of its routing problem, so routing it costs the same in any circuit.
const BAND_MARGIN: f32 = 160.0;

/// An orthogonal path from `from` (a pin) to `to`: out of the pin
/// horizontally, across at the middle, and in horizontally (a Z; a
/// straight run when the ends line up). The band where routing finds
/// nothing.
pub fn band_preview(from: Vec2, to: Vec2) -> Vec<Vec2> {
    let mid = (from.x + to.x) * 0.5;
    let mut points = vec![from, Vec2::new(mid, from.y), Vec2::new(mid, to.y), to];
    routing::simplify(&mut points);
    points
}

/// The rubber band routed like a wire from `driver` to `sink` (both pins)
/// around the tiles `boxes` near them.
pub fn band_route(boxes: &[Rect], driver: Vec2, sink: Vec2) -> Vec<Vec2> {
    let area = Rect::from_corners(driver, sink).inflate(BAND_MARGIN);
    let problem = RouteProblem {
        boxes: boxes
            .iter()
            .filter(|b| !b.intersect(area).is_empty())
            .copied()
            .collect(),
        nets: vec![NetSpec {
            driver,
            sinks: vec![sink],
        }],
        buses: Vec::new(),
    };
    let routed = routing::route(&problem, &[], true);
    match routed.nets.first() {
        Some(net) if !net.sinks.is_empty() => net.full_path(0),
        _ => band_preview(driver, sink),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Visuals
// ─────────────────────────────────────────────────────────────────────────────

/// Depth of the rubber band: above the wires and their pills, below the
/// tiles (so its ends tuck under the pin dots like a wire's).
const BAND_Z: f32 = -3.0;
/// Depth of the pin marks: above every tile, the lifted one included.
const MARK_Z: f32 = 3.0;
/// Opacity of the veil over the pins a drag cannot drop on.
const VEIL_ALPHA: f32 = 0.72;

/// A piece of the drag art (one entity each, a mesh of its own).
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub enum DragMark {
    BandCasing,
    BandCore,
    /// Veils over the pins the dragged wire cannot drop on.
    Veils,
    /// Rings around the pins it can drop on: a fill between pin and
    /// ring, ink, then marigold.
    RingFill,
    RingInk,
    RingCore,
    /// The ring around the hovered pin: ink, then marigold (tomato over a
    /// pin a drag cannot drop on), with its fill.
    HoverFill,
    HoverInk,
    HoverCore,
}

impl DragMark {
    const ALL: [DragMark; 9] = [
        DragMark::BandCasing,
        DragMark::BandCore,
        DragMark::Veils,
        DragMark::RingFill,
        DragMark::RingInk,
        DragMark::RingCore,
        DragMark::HoverFill,
        DragMark::HoverInk,
        DragMark::HoverCore,
    ];

    fn z(self) -> f32 {
        match self {
            DragMark::BandCasing => BAND_Z,
            DragMark::BandCore => BAND_Z + 0.02,
            DragMark::Veils => MARK_Z,
            DragMark::RingFill => MARK_Z + 0.01,
            DragMark::RingInk => MARK_Z + 0.02,
            DragMark::RingCore => MARK_Z + 0.04,
            DragMark::HoverFill => MARK_Z + 0.05,
            DragMark::HoverInk => MARK_Z + 0.06,
            DragMark::HoverCore => MARK_Z + 0.08,
        }
    }
}

/// A ring of `width` at `radius` around `centre`: a true annulus, with
/// enough segments to stay round at the closest zoom.
fn ring(centre: Vec2, radius: f32, width: f32) -> Shape {
    let outer = radius + width * 0.5;
    let segments = circle_segments(outer / crate::camera::MIN_WPP);
    Shape::annulus(centre, radius - width * 0.5, outer, segments)
}

/// The cased ring of a pin mark: `(fill, ink, core)` shapes. The fill
/// closes the gap between the pin dot and the ring (in the tile colour),
/// so the tile's outline and the wire under the gap do not show through
/// as spokes.
fn pin_mark(pin: &PinRef, gap: f32, core: f32) -> (Shape, Shape, Shape) {
    let r = pin.radius + gap + core * 0.5 + 1.0;
    let dot = pin.radius + PIN_RING.min(pin.radius * 0.6);
    let fill = ring(pin.at, (dot + r) * 0.5, r - dot);
    (fill, ring(pin.at, r, core + 3.0), ring(pin.at, r, core))
}

/// A part as the drag art reads it: pins and box.
type DragGate = (
    Entity,
    &'static GateIdentity,
    &'static Transform,
    &'static GateType,
    Option<&'static PinLayout>,
    Option<&'static TileSpec>,
);

/// What the band was last built for: its points and core paint.
type BandKey = (Vec<Vec2>, Paint, bool);

/// Built state of the drag art.
#[derive(Default)]
pub struct DragArtState {
    veil: Handle<ColorMaterial>,
    band: Option<BandKey>,
    from: Option<PinRef>,
    hover: Option<(PinRef, Paint)>,
}

/// Where the rubber band ends: on the pin under the pointer, else at the
/// pointer snapped to the routing grid (so the band is routed again only
/// when the pointer crosses into another grid cell).
fn band_end(pointer: &PinPointer, drag: &WireDrag) -> Vec2 {
    pointer
        .target()
        .map_or_else(|| (drag.pointer / GRID).round() * GRID, |t| t.at)
}

/// The rubber band's points: routed like a wire between its pin and its
/// end, in the wire's direction.
fn band_points(drag: &WireDrag, end: Vec2, boxes: &[Rect]) -> Vec<Vec2> {
    if drag.from.output {
        band_route(boxes, drag.from.at, end)
    } else {
        band_route(boxes, end, drag.from.at)
    }
}

/// Draw the wire drag (see the module docs). Builds its meshes only when
/// [`PinPointer`] changes, and routes the band (one net, with the tiles
/// near it: well under a millisecond) only when its end moves to another
/// pin or grid cell.
#[allow(clippy::too_many_arguments)]
pub fn wire_drag_visuals_system(
    mut commands: Commands,
    pointer: Res<PinPointer>,
    theme: Res<Theme>,
    art: Res<CanvasArt>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<ColorMaterial>>,
    gates: Query<DragGate>,
    mut marks: Query<(
        &DragMark,
        &Mesh2d,
        &mut MeshMaterial2d<ColorMaterial>,
        &mut Visibility,
    )>,
    mut state: Local<DragArtState>,
) {
    if marks.is_empty() {
        state.veil = materials.add(ColorMaterial::from(veil_colour(&theme.palette)));
        for mark in DragMark::ALL {
            commands.spawn((
                mark,
                Mesh2d(meshes.add(Shape::default().into_mesh())),
                MeshMaterial2d(art.material(Paint::Ink)),
                Transform::from_xyz(0.0, 0.0, mark.z()),
                Visibility::Hidden,
            ));
        }
        return;
    }
    if theme.is_changed()
        && let Some(mut veil) = materials.get_mut(&state.veil)
    {
        veil.color = veil_colour(&theme.palette);
    }
    if !pointer.is_changed() {
        return;
    }
    let drag = pointer.drag;
    let mut shapes: Vec<(DragMark, Option<Shape>, Paint)> = Vec::new();

    // Rings on the compatible pins and veils on the others: per drag.
    let from = drag.map(|d| d.from);
    if from != state.from {
        state.from = from;
        let (mut veils, mut fill, mut ink, mut core) = (
            Shape::default(),
            Shape::default(),
            Shape::default(),
            Shape::default(),
        );
        if let Some(from) = from {
            for (entity, id, t, _, layout, spec) in &gates {
                let (Some(layout), Some(spec)) = (layout, spec) else {
                    continue;
                };
                for pin in gate_pins(entity, id.uuid, t.translation.truncate(), layout, spec) {
                    if pin.same_pin(&from) {
                        continue;
                    }
                    if pin.compatible(&from) {
                        let (f, i, c) = pin_mark(&pin, 1.5, 2.5);
                        fill.append(&f);
                        ink.append(&i);
                        core.append(&c);
                    } else {
                        veils.append(&Shape::dot(pin.at, pin.radius + 3.0));
                    }
                }
            }
        }
        let shown = from.is_some();
        shapes.push((DragMark::Veils, shown.then_some(veils), Paint::Bench));
        shapes.push((DragMark::RingFill, shown.then_some(fill), Paint::Tile));
        shapes.push((DragMark::RingInk, shown.then_some(ink), Paint::Ink));
        shapes.push((DragMark::RingCore, shown.then_some(core), Paint::On));
    }

    // The hovered pin: marigold, tomato where a drag cannot drop.
    let hover = pointer.hover.map(|pin| {
        let refused = drag.is_some_and(|d| !d.from.same_pin(&pin) && !d.from.compatible(&pin));
        (pin, if refused { Paint::Fault } else { Paint::On })
    });
    if hover != state.hover {
        state.hover = hover;
        let (fill, ink, core) = match hover {
            Some((pin, _)) => {
                let (f, i, c) = pin_mark(&pin, 3.5, 3.5);
                (Some(f), Some(i), Some(c))
            }
            None => (None, None, None),
        };
        let paint = hover.map_or(Paint::On, |h| h.1);
        shapes.push((DragMark::HoverFill, fill, Paint::Tile));
        shapes.push((DragMark::HoverInk, ink, Paint::Ink));
        shapes.push((DragMark::HoverCore, core, paint));
    }

    // The band.
    let band = drag.map(|d| {
        let target = pointer.target();
        let refused = target.is_some_and(|t| !t.compatible(&d.from));
        let paint = if refused {
            Paint::Fault
        } else if d.from.width > 1 {
            Paint::Bus
        } else {
            Paint::On
        };
        (d, paint)
    });
    let end = drag.map(|d| band_end(&pointer, &d));
    let key = band
        .zip(end)
        .map(|((d, paint), end)| (vec![d.from.at, end], paint, d.from.width > 1));
    if key != state.band {
        let built = match (&band, &key) {
            (Some((d, paint)), Some((ends, _, bus))) => {
                let boxes: Vec<Rect> = gates
                    .iter()
                    .map(|(_, _, t, ty, ..)| routing::gate_box(&ty.kind, t.translation.truncate()))
                    .collect();
                let points = round_corners(&band_points(d, ends[1], &boxes), WIRE_CORNER);
                let core = if *bus { BUS_CORE } else { BIT_CORE };
                Some((
                    Shape::path_stroke(&points, casing_for(core)),
                    Shape::path_stroke(&points, core),
                    *paint,
                ))
            }
            _ => None,
        };
        state.band = key;
        match built {
            Some((casing, core, paint)) => {
                shapes.push((DragMark::BandCasing, Some(casing), Paint::Ink));
                shapes.push((DragMark::BandCore, Some(core), paint));
            }
            None => {
                shapes.push((DragMark::BandCasing, None, Paint::Ink));
                shapes.push((DragMark::BandCore, None, Paint::On));
            }
        }
    }

    for (mark, mesh, mut material, mut visibility) in &mut marks {
        let Some((_, shape, paint)) = shapes.iter().find(|(m, ..)| m == mark) else {
            continue;
        };
        let want = if shape.is_some() {
            Visibility::Visible
        } else {
            Visibility::Hidden
        };
        visibility.set_if_neq(want);
        if let Some(shape) = shape {
            let _ = meshes.insert(mesh.id(), shape.clone().into_mesh());
            let handle = if *mark == DragMark::Veils {
                state.veil.clone()
            } else {
                art.material(*paint)
            };
            if material.0 != handle {
                material.0 = handle;
            }
        }
    }
}

fn veil_colour(palette: &Palette) -> Color {
    palette.bench.with_alpha(VEIL_ALPHA)
}

// ─────────────────────────────────────────────────────────────────────────────
// Tooltip
// ─────────────────────────────────────────────────────────────────────────────

/// Beside the pointer, the name and width of the pin under it
/// (`q · 8 bit`); while a wire is dragged over a pin it cannot drop on,
/// a second line says why.
pub fn pin_tooltip_system(
    mut contexts: EguiContexts,
    theme: Res<Theme>,
    pointer: Res<PinPointer>,
    cursor: Res<CanvasCursor>,
    library: Res<CircuitLibrary>,
    gates: Query<&GateType>,
) -> Result {
    let (Some(pin), Some(screen)) = (pointer.hover, cursor.screen) else {
        return Ok(());
    };
    let Ok(ty) = gates.get(pin.gate) else {
        return Ok(());
    };
    let ctx = contexts.ctx_mut()?;
    if !crate::ui_style::workshop_fonts_ready(ctx) {
        return Ok(());
    }
    let palette = theme.palette;
    let title = pin_text(
        &pin_name(&library, &ty.kind, pin.output, pin.index),
        pin.width,
    );
    let why = pointer.drag.and_then(|d| {
        if d.from.same_pin(&pin) || d.from.compatible(&pin) {
            None
        } else if d.from.output == pin.output {
            Some(format!(
                "also an {}",
                if pin.output { "output" } else { "input" }
            ))
        } else {
            Some(format!("needs {}", bits(d.from.width)))
        }
    });
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Tooltip,
        egui::Id::new("pin_tooltip"),
    ));
    let main = painter.layout_no_wrap(title, heavy(12.0), egui_color(palette.text));
    let note = why.map(|w| painter.layout_no_wrap(w, heavy(11.0), egui_color(palette.fault_text)));
    let pad = egui::vec2(8.0, 4.0);
    let mut size = main.size();
    if let Some(note) = &note {
        size = egui::vec2(size.x.max(note.size().x), size.y + note.size().y);
    }
    // The cursor is in window pixels; the painter in egui points.
    let at = window_to_egui(screen, ctx.zoom_factor()) + egui::vec2(14.0, 14.0);
    let rect = egui::Rect::from_min_size(at, size + pad * 2.0);
    painter.rect(
        rect,
        egui::CornerRadius::same(8),
        egui_color(palette.tile),
        stroke(2.0, egui_color(palette.ink)),
        egui::StrokeKind::Inside,
    );
    let y = main.size().y;
    painter.galley(rect.min + pad, main, egui_color(palette.text));
    if let Some(note) = note {
        painter.galley(
            rect.min + pad + egui::vec2(0.0, y),
            note,
            egui_color(palette.fault_text),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pin(output: bool, index: u8, width: u8) -> PinRef {
        PinRef {
            gate: Entity::PLACEHOLDER,
            uuid: Uuid::from_u128(u128::from(output) + 2),
            output,
            index,
            width,
            at: Vec2::ZERO,
            radius: 4.0,
        }
    }

    #[test]
    fn reach_grows_with_the_zoom_out_to_a_cap() {
        assert_eq!(pin_reach(4.0, 1.0), 10.0);
        assert_eq!(pin_reach(4.0, 0.5), 7.0);
        assert_eq!(pin_reach(4.0, 8.0), PIN_REACH_MAX);
        assert_eq!(pin_reach(4.0, 0.0), 4.0);
        // A dot larger than the cap is still hit all over.
        assert_eq!(pin_reach(12.0, 1.0), 12.0);
    }

    #[test]
    fn drops_connect_output_to_input_of_one_width() {
        let name = |p: &PinRef| format!("pin {}", p.index);
        let (q, a) = (pin(true, 0, 8), pin(false, 1, 8));
        let wire = CreateWire {
            source_gate: q.uuid,
            source_pin: 0,
            sink_gate: a.uuid,
            sink_pin: 1,
        };
        assert_eq!(drop_outcome(&q, Some(&a), name), DropOutcome::Connect(wire));
        assert_eq!(drop_outcome(&a, Some(&q), name), DropOutcome::Connect(wire));
        assert_eq!(drop_outcome(&q, Some(&q), name), DropOutcome::Cancel);
        let DropOutcome::Refuse(m) = drop_outcome(&q, Some(&pin(false, 0, 1)), name) else {
            panic!("a width mismatch connects");
        };
        assert!(m.contains("8 bit") && m.contains("1 bit"), "{m}");
        let DropOutcome::Refuse(m) = drop_outcome(&a, None, name) else {
            panic!("a drop on nothing connects");
        };
        assert!(m.contains("output pin"), "{m}");
    }

    #[test]
    fn the_preview_is_orthogonal() {
        let path = band_preview(Vec2::new(0.0, 0.0), Vec2::new(100.0, 40.0));
        assert_eq!(
            path,
            [
                Vec2::ZERO,
                Vec2::new(50.0, 0.0),
                Vec2::new(50.0, 40.0),
                Vec2::new(100.0, 40.0)
            ]
        );
        assert_eq!(band_preview(Vec2::ZERO, Vec2::new(60.0, 0.0)).len(), 2);
        assert_eq!(pin_text("q", 8), "q · 8 bit");
    }
}
