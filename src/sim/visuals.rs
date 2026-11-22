//! Change-driven visuals for gates and wires, in the Studio Workshop look.
//!
//! Every system here reacts to component changes (Bevy change detection)
//! and writes a component only when the new value differs, so an idle
//! circuit (simulation paused, no input) costs no per-entity work:
//!
//! - a gate is a tile (ink outline with a solid bottom edge, a face tinted
//!   by family, a symbol or caption, pins) built from cached meshes when
//!   its [`GateType`] changes ([`attach_gate_visuals_system`]);
//! - pins, lamps and switches follow [`GateValues`] by swapping shared
//!   materials;
//! - display text is re-formatted only when the shown value, width or
//!   [`DisplayConfig`] change ([`DisplayedValue`]);
//! - wires are routed on the grid ([`super::routing`]) by
//!   [`route_wires_system`] only when gates or wires come or go, a pin
//!   layout or the view changes, or a gate moves; a move keeps every sink
//!   path whose pins stayed put, so a drag re-routes only that gate's wires;
//! - a wire is cased: an ink casing under a coloured core (thicker for
//!   buses), meshes along the routed path with rounded corners, a junction
//!   dot where it branches off its net, a gap where another wire passes
//!   over it, and a value pill on a bus; a value change only swaps the
//!   core's shared material;
//! - a combinational-loop compile error highlights the gates and wires on
//!   the loop in the open view ([`LoopHighlight`]) until a later compile
//!   succeeds;
//! - a [`Theme`] change recolours the shared materials, wires and texts
//!   once;
//! - the gate being dragged comes to the front, with its wires above the
//!   other wires, and stays there after the drop until another gate is
//!   dragged ([`OnTop`]).
//!
//! [`SimulationVisualsPlugin`] has no rendering, window or egui
//! dependencies (it creates the mesh and material stores when they are
//! missing), so headless tests run the same systems.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};

use bevy::prelude::*;
use bevy::transform::TransformSystems;
use uuid::Uuid;

use super::art::{
    BIT_CORE, BUS_CORE, CanvasArt, EDGE, Glyph, MeshKey, OUTLINE, Paint, PinLayout, Tone,
    casing_for, family, pill_size, shorten, tile_caption, tile_radius,
};
use super::bridge::sync_design_system;
use super::routing::{self, NetRoute, NetSpec, RouteProblem, RouteStats, SinkPath};
use super::shapes::{Shape, corner_radius, round_corners};
use super::view::CircuitView;
use super::{
    CircuitLibrary, DisplayConfig, GateIdentity, GateKind, GateLabel, GateType, GateValues,
    LogicDisplay, Selected, SelectedGate, SimDiagnostics, WireEndpoints, WireState, format_value,
};
use crate::theme::Theme;

// ─────────────────────────────────────────────────────────────────────────────
// Work counter
// ─────────────────────────────────────────────────────────────────────────────

/// Counts the entities the per-frame simulation and visual systems
/// touched. Only counts while the resource exists; nothing inserts it in
/// the app, tests and profiling do.
#[derive(Resource, Debug, Default)]
pub struct EntityWorkCounter(AtomicU64);

impl EntityWorkCounter {
    pub fn get(&self) -> u64 {
        self.0.load(Ordering::Relaxed)
    }

    /// Record `n` entities touched, if counting.
    pub fn add(counter: &Option<Res<EntityWorkCounter>>, n: usize) {
        if let Some(counter) = counter
            && n > 0
        {
            counter.0.fetch_add(n as u64, Ordering::Relaxed);
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Components
// ─────────────────────────────────────────────────────────────────────────────

/// A gate or wire on the combinational loop of the last failed compile,
/// in the open view.
#[derive(Component, Debug, Clone, Copy, Default, Reflect)]
#[reflect(Component)]
pub struct LoopHighlight;

/// The tomato plate drawn behind a highlighted gate (a child of it).
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct LoopOutline;

/// What a [`LogicDisplay`] text currently shows: value, width and format.
#[derive(Component, Debug, Clone, Copy, Default, PartialEq)]
pub struct DisplayedValue(pub Option<(u64, u8, DisplayConfig)>);

/// Hex value label of a multi-bit wire (a child of the wire), with the
/// value and width it shows. It sits on a pill.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub struct WireValueLabel {
    pub value: u64,
    pub width: u8,
}

/// A piece of a gate's tile (a child of the gate). Rebuilt when the kind
/// changes.
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct TileArt;

/// A pin dot of a gate: which pin and its width.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub struct PinDot {
    pub output: bool,
    pub index: u8,
    pub width: u8,
}

/// A part of a tile that shows the gate's value.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueArt {
    /// The bulb of a 1-bit output.
    Lamp,
    /// The track of a 1-bit input's switch.
    Track,
    /// The knob of a 1-bit input's switch (moves right when on).
    Knob,
}

/// The value text of a word input (formatted like a display).
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct InputReadout;

/// The name of a port, written under its tile.
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct NameTag;

/// The halo behind the selected gate.
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct SelectionHalo;

/// The ink casing under a wire's core (a child of the wire, with a mesh
/// of its own).
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct WireCasing;

/// The coloured core of a wire (a child of the wire, with a mesh of its
/// own).
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct WireCore;

/// A piece of the junction dot where a wire branches off its net (a child
/// of the wire): the ink ring, or the core-coloured face.
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct WireJunction {
    pub core: bool,
}

/// The gaps wires cut in the casings of the wires they pass over: one
/// entity for the open view, one mesh.
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct WireGaps;

/// The routed shape of a wire: its own part of its net's tree, in world
/// coordinates. Written by [`route_wires_system`] only when it changes.
#[derive(Component, Debug, Clone, PartialEq, Default)]
pub struct WirePath {
    /// Corner points, horizontal and vertical runs, ending at the sink
    /// pin; the first wire of a net starts at the driver pin, the others
    /// at their junction.
    pub points: Vec<Vec2>,
    /// The junction dot, for a branch.
    pub junction: Option<Vec2>,
    /// Where the net's value pill sits (on the longest horizontal run of a
    /// bus; one wire of the net carries it).
    pub pill: Option<Vec2>,
}

/// How a wire is drawn: the core's paint and width.
#[derive(Component, Debug, Clone, Copy, PartialEq)]
pub struct WireStyle {
    pub paint: Paint,
    pub core: f32,
}

/// The core width a wire's meshes were built for.
#[derive(Component, Debug, Clone, Copy, PartialEq, Default)]
pub struct WireBuilt(pub f32);

/// What a tile was built from; a library change rebuilds a subcircuit's
/// tile only when this changes.
#[derive(Component, Debug, Clone, PartialEq, Default)]
pub struct TileSpec {
    pub inputs: Vec<u8>,
    pub outputs: Vec<u8>,
    pub caption: Option<String>,
}

// ─────────────────────────────────────────────────────────────────────────────
// Styles
// ─────────────────────────────────────────────────────────────────────────────

const LOOP_OUTLINE_MARGIN: f32 = 6.0;
const HALO_MARGIN: f32 = 6.0;
/// Knob travel of an input switch.
const KNOB_TRAVEL: f32 = 6.0;
/// Corner radius of a wire (one grid step, less on short runs).
pub const WIRE_CORNER: f32 = 20.0;
/// Bench showing on each side of a wire where it passes over another.
const GAP: f32 = 4.0;
/// Outer radius of a junction dot (ink ring included): the design's r 9
/// dot with a 2.5 ring, a little wider than the casing on a bus.
fn junction_radius(core: f32) -> f32 {
    (9.0 + OUTLINE * 0.5).max(casing_for(core) * 0.5 + 4.0)
}
/// Depth slots for nets, and the depth between two slots.
const NET_SLOTS: u32 = 2048;
const NET_STAGGER: f32 = 0.1;
/// Depths within a net's slot: casing, core, junction ring and face; a
/// crossing gap sits just below the upper net's casing.
const CASING_Z: f32 = 0.0;
const CORE_Z: f32 = 0.02;
const JUNCTION_INK_Z: f32 = 0.04;
const JUNCTION_CORE_Z: f32 = 0.06;
const GAP_BELOW: f32 = 0.02;
/// Depth of the value pills: above every wire, below the gates.
const PILL_Z: f32 = -5.0;

/// The paint and core width of a wire: tomato on a loop, the fault colour
/// when the simulator refused it, else by value and width; buses are
/// thicker. The casing is [`casing_for`] the core.
pub fn wire_look(state: &WireState, on_loop: bool) -> WireStyle {
    let bus = state.width > 1;
    WireStyle {
        paint: if on_loop {
            Paint::Fault
        } else if !state.connected {
            Paint::FaultText
        } else {
            Paint::signal(state.width, state.value)
        },
        core: if bus { BUS_CORE } else { BIT_CORE },
    }
}

/// One hex digit per 4 bits, as written in a bus's value pill.
pub fn wire_label_text(value: u64, width: u8) -> String {
    format!(
        "{value:0digits$X}",
        digits = usize::from(width.max(1)).div_ceil(4)
    )
}

/// z of the [`OnTop`] gate. Every other gate sits at 0, and a tile's art
/// spans local z -0.7 (the selection halo) to 0.5 (texts), so the lifted
/// tile with all its pins, labels and caption draws above every other.
pub const ON_TOP_Z: f32 = 2.0;

/// The gate last dragged: drawn in front of the others (z [`ON_TOP_Z`]),
/// its wires above the other wires. At most one gate has it.
#[derive(Component, Debug)]
pub struct OnTop;

/// Value of a gate's main signal: the output of an input, the input of
/// anything else.
fn shown_value(kind: &GateKind, values: &GateValues) -> u64 {
    if kind.is_input() {
        values.outputs.first().copied().unwrap_or(0)
    } else {
        values.inputs.first().copied().unwrap_or(0)
    }
}

fn value_paint(part: ValueArt, on: bool) -> Paint {
    match (part, on) {
        (ValueArt::Lamp, true) | (ValueArt::Track, true) => Paint::On,
        (ValueArt::Lamp, false) | (ValueArt::Knob, _) => Paint::PinOff,
        (ValueArt::Track, false) => Paint::Off,
    }
}

fn knob_x(on: bool) -> f32 {
    if on { KNOB_TRAVEL } else { -KNOB_TRAVEL }
}

type Piece = (Mesh2d, MeshMaterial2d<ColorMaterial>, Transform);

// ─────────────────────────────────────────────────────────────────────────────
// Attachment
// ─────────────────────────────────────────────────────────────────────────────

/// The pin widths and caption of a tile.
fn tile_spec(kind: &GateKind, library: &CircuitLibrary) -> TileSpec {
    let (ins, outs) = library.pins(kind).unwrap_or_default();
    let caption = match kind {
        GateKind::SubCircuit { definition } => Some(
            library
                .get(*definition)
                .map_or_else(|| "?".to_string(), |d| shorten(&d.name, 7)),
        ),
        k => tile_caption(k),
    };
    TileSpec {
        inputs: ins.iter().copied().collect(),
        outputs: outs.iter().copied().collect(),
        caption,
    }
}

/// Build a gate's tile: ink outline and edge, tinted face, symbol or
/// caption, pins, value parts, display text and name tag.
#[allow(clippy::too_many_arguments)]
fn build_tile(
    commands: &mut Commands,
    art: &mut CanvasArt,
    meshes: &mut Assets<Mesh>,
    entity: Entity,
    kind: &GateKind,
    spec: &TileSpec,
    label: Option<&GateLabel>,
    values: Option<&GateValues>,
) {
    let size = kind.size();
    let fam = family(kind);
    let layout = PinLayout::new(size, spec.inputs.len(), spec.outputs.len());
    let child = |commands: &mut Commands, piece: Piece| {
        commands.spawn((piece, TileArt, ChildOf(entity)));
    };

    let ink = art.piece(
        meshes,
        MeshKey::tile_ink(size),
        Paint::Ink,
        Vec3::new(0.0, 0.0, -0.4),
    );
    child(commands, ink);
    let face = art.piece(
        meshes,
        MeshKey::tile_fill(size),
        Paint::family_fill(fam),
        Vec3::new(0.0, 0.0, -0.3),
    );
    child(commands, face);

    // Pins: ink rings (one mesh per tile), then a dot per pin.
    if !spec.inputs.is_empty() || !spec.outputs.is_empty() {
        let rings = art.piece(
            meshes,
            MeshKey::pin_rings(size, spec.inputs.len(), spec.outputs.len()),
            Paint::Ink,
            Vec3::new(0.0, 0.0, 0.3),
        );
        child(commands, rings);
    }
    let dot = MeshKey::dot(layout.radius);
    let value_of = |output: bool, i: usize| {
        values
            .and_then(|v| {
                if output {
                    v.outputs.get(i)
                } else {
                    v.inputs.get(i)
                }
            })
            .copied()
            .unwrap_or(0)
    };
    for (output, widths, positions) in [
        (false, &spec.inputs, &layout.inputs),
        (true, &spec.outputs, &layout.outputs),
    ] {
        for (i, (&width, pos)) in widths.iter().zip(positions).enumerate() {
            let piece = art.piece(
                meshes,
                dot,
                Paint::pin(width, value_of(output, i)),
                pos.extend(0.4),
            );
            commands.spawn((
                piece,
                PinDot {
                    output,
                    index: i as u8,
                    width,
                },
                TileArt,
                ChildOf(entity),
            ));
        }
    }

    // The symbol or caption.
    if let Some(glyph) = Glyph::of(kind) {
        let piece = art.piece(
            meshes,
            MeshKey::Glyph(glyph),
            Paint::family_text(fam),
            Vec3::new(0.0, 0.0, 0.1),
        );
        child(commands, piece);
    } else if let Some(caption) = &spec.caption {
        let chars = caption.chars().count().max(1) as f32;
        let font_size = ((size.x - 14.0) / (chars * 0.66)).clamp(9.0, 13.0);
        let font = if matches!(kind, GateKind::Const { .. }) {
            art.fonts.mono(font_size)
        } else {
            art.fonts.name(font_size)
        };
        commands.spawn((
            Text2d::new(caption.clone()),
            font,
            TextColor(Color::WHITE),
            Tone(Paint::family_text(fam)),
            Transform::from_xyz(0.0, 0.0, 0.5),
            TileArt,
            ChildOf(entity),
        ));
    }

    let on = values.is_some_and(|v| shown_value(kind, v) != 0);
    match kind {
        GateKind::Input { w: 1 } => {
            let track = Vec2::new(28.0, 16.0);
            let ink = art.piece(
                meshes,
                MeshKey::rounded_rect(track + Vec2::splat(OUTLINE), 9.25),
                Paint::Ink,
                Vec3::new(0.0, 0.0, 0.05),
            );
            child(commands, ink);
            let fill = art.piece(
                meshes,
                MeshKey::rounded_rect(track, 8.0),
                value_paint(ValueArt::Track, on),
                Vec3::new(0.0, 0.0, 0.1),
            );
            commands.spawn((fill, ValueArt::Track, TileArt, ChildOf(entity)));
            let knob = art.piece(
                meshes,
                MeshKey::dot(6.5),
                Paint::Ink,
                Vec3::new(knob_x(on), 0.0, 0.2),
            );
            let knob_face = art.piece(
                meshes,
                MeshKey::dot(4.25),
                Paint::PinOff,
                Vec3::new(0.0, 0.0, 0.01),
            );
            commands
                .spawn((knob, ValueArt::Knob, TileArt, ChildOf(entity)))
                .with_child(knob_face);
        }
        GateKind::Output { w: 1 } => {
            let ring = art.piece(
                meshes,
                MeshKey::dot(12.0),
                Paint::Ink,
                Vec3::new(0.0, 0.0, 0.05),
            );
            child(commands, ring);
            let bulb = art.piece(
                meshes,
                MeshKey::dot(9.5),
                value_paint(ValueArt::Lamp, on),
                Vec3::new(0.0, 0.0, 0.1),
            );
            commands.spawn((bulb, ValueArt::Lamp, TileArt, ChildOf(entity)));
        }
        GateKind::Output { .. } | GateKind::Display { .. } | GateKind::Input { .. } => {
            let readout = !kind.is_input();
            if readout {
                let screen = size - Vec2::new(14.0, 16.0);
                let piece = art.piece(
                    meshes,
                    MeshKey::rounded_rect(screen, tile_radius(size) * 0.6),
                    Paint::Screen,
                    Vec3::new(0.0, 0.0, 0.05),
                );
                child(commands, piece);
            }
            let font_size = if matches!(kind, GateKind::Display { .. }) {
                22.0
            } else {
                13.0
            };
            let mut text = commands.spawn((
                Text2d::new(""),
                art.fonts.mono(font_size),
                TextColor(Color::WHITE),
                Tone(if readout { Paint::On } else { Paint::Text }),
                Transform::from_xyz(0.0, 0.0, 0.5),
                DisplayedValue::default(),
                TileArt,
                ChildOf(entity),
            ));
            if readout {
                text.insert(LogicDisplay);
            } else {
                text.insert(InputReadout);
            }
        }
        _ => {}
    }

    if kind.is_port() {
        let text = label.map(|l| l.value.clone()).unwrap_or_default();
        commands.spawn((
            Text2d::new(text),
            art.fonts.name(13.0),
            TextColor(Color::WHITE),
            Tone(Paint::Text),
            Transform::from_xyz(0.0, -size.y * 0.5 - EDGE - 13.0, 0.5),
            NameTag,
            TileArt,
            ChildOf(entity),
        ));
    }

    commands
        .entity(entity)
        .try_insert((layout, spec.clone()))
        .try_remove::<Sprite>();
}

/// Give new or re-kinded gates their tile; rebuild subcircuit tiles whose
/// definition's pins or name changed.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub fn attach_gate_visuals_system(
    mut commands: Commands,
    library: Res<CircuitLibrary>,
    mut art: ResMut<CanvasArt>,
    mut meshes: ResMut<Assets<Mesh>>,
    changed: Query<Entity, Changed<GateType>>,
    subcircuits: Query<(Entity, &GateType, Option<&TileSpec>)>,
    gates: Query<(
        &GateType,
        Option<&GateLabel>,
        Option<&GateValues>,
        Option<&Children>,
    )>,
    pieces: Query<(), With<TileArt>>,
    work: Option<Res<EntityWorkCounter>>,
) {
    let mut dirty: Vec<(Entity, TileSpec)> = Vec::new();
    for entity in &changed {
        if let Ok((ty, ..)) = gates.get(entity) {
            dirty.push((entity, tile_spec(&ty.kind, &library)));
        }
    }
    if library.is_changed() {
        for (entity, ty, spec) in &subcircuits {
            if !matches!(ty.kind, GateKind::SubCircuit { .. })
                || dirty.iter().any(|d| d.0 == entity)
            {
                continue;
            }
            let wanted = tile_spec(&ty.kind, &library);
            if spec != Some(&wanted) {
                dirty.push((entity, wanted));
            }
        }
    }
    for (entity, spec) in dirty {
        let Ok((ty, label, values, children)) = gates.get(entity) else {
            continue;
        };
        EntityWorkCounter::add(&work, 1);
        for child in children.into_iter().flat_map(|c| c.iter()) {
            if pieces.contains(child) {
                commands.entity(child).despawn();
            }
        }
        build_tile(
            &mut commands,
            &mut art,
            &mut meshes,
            entity,
            &ty.kind,
            &spec,
            label,
            values,
        );
    }
}

/// Give new wires their pieces: an ink casing and a coloured core, each a
/// mesh of their own (rebuilt when the route changes) with a shared
/// material (swapped when the value changes). [`route_wires_system`] routes
/// them and [`update_wire_visuals_system`] draws them.
pub fn attach_wire_visuals_system(
    mut commands: Commands,
    wires: Query<Entity, Added<WireEndpoints>>,
    art: Res<CanvasArt>,
    mut meshes: ResMut<Assets<Mesh>>,
    work: Option<Res<EntityWorkCounter>>,
) {
    for entity in &wires {
        EntityWorkCounter::add(&work, 1);
        commands.entity(entity).try_insert((
            WirePath::default(),
            WireStyle {
                paint: Paint::Off,
                core: BIT_CORE,
            },
            WireBuilt::default(),
            Transform::from_xyz(0.0, 0.0, crate::temp::CONNECTION_Z_LEVEL),
            Visibility::Visible,
        ));
        commands.spawn((
            Mesh2d(meshes.add(empty_mesh())),
            MeshMaterial2d(art.material(Paint::Ink)),
            Transform::from_xyz(0.0, 0.0, CASING_Z),
            WireCasing,
            ChildOf(entity),
        ));
        commands.spawn((
            Mesh2d(meshes.add(empty_mesh())),
            MeshMaterial2d(art.material(Paint::Off)),
            Transform::from_xyz(0.0, 0.0, CORE_Z),
            WireCore,
            ChildOf(entity),
        ));
    }
}

/// A mesh with nothing to draw (one degenerate triangle).
fn empty_mesh() -> Mesh {
    Shape {
        positions: vec![Vec2::ZERO; 3],
        indices: vec![0, 1, 2],
    }
    .into_mesh()
}

// ─────────────────────────────────────────────────────────────────────────────
// Loop highlight and selection
// ─────────────────────────────────────────────────────────────────────────────

/// The loops of the last failed compile as seen from the open view: per
/// loop, the gate ids of the open circuit on it. A node inside a nested
/// instance is shown as that instance's gate.
fn loops_in_view(diagnostics: &SimDiagnostics, view: &CircuitView) -> Vec<HashSet<Uuid>> {
    let Some(error) = &diagnostics.compile_error else {
        return Vec::new();
    };
    let here = if view.is_root() {
        Vec::new()
    } else {
        match view.instance_path() {
            Some(path) => path,
            // A bare definition is not simulated: nothing to show.
            None => return Vec::new(),
        }
    };
    error
        .loops
        .iter()
        .map(|nodes| {
            nodes
                .iter()
                .filter(|n| n.path.starts_with(&here))
                .map(|n| n.path.get(here.len()).copied().unwrap_or(n.gate))
                .collect::<HashSet<Uuid>>()
        })
        .filter(|set| !set.is_empty())
        .collect()
}

/// A rounded plate behind a tile, `margin` larger on every side and
/// covering the bottom edge.
fn plate(
    art: &mut CanvasArt,
    meshes: &mut Assets<Mesh>,
    size: Vec2,
    margin: f32,
    paint: Paint,
    z: f32,
) -> Piece {
    let plate = size + Vec2::new(margin * 2.0, margin * 2.0 + EDGE);
    art.piece(
        meshes,
        MeshKey::rounded_rect(plate, tile_radius(size) + margin),
        paint,
        Vec3::new(0.0, -EDGE * 0.5, z),
    )
}

/// Mark the gates and wires of the open view that lie on a combinational
/// loop of the last failed compile; clear the marks once a compile
/// succeeds. A wire is marked when both its ends are on the same loop
/// (every edge inside a strongly connected component lies on a cycle).
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub fn apply_loop_highlight_system(
    mut commands: Commands,
    diagnostics: Res<SimDiagnostics>,
    view: Res<CircuitView>,
    mut art: ResMut<CanvasArt>,
    mut meshes: ResMut<Assets<Mesh>>,
    new_gates: Query<(), Added<GateIdentity>>,
    new_wires: Query<(), Added<WireEndpoints>>,
    gates: Query<(Entity, &GateIdentity, &GateType, Has<LoopHighlight>)>,
    wires: Query<(Entity, &WireEndpoints, Has<LoopHighlight>)>,
    marked: Query<(), With<LoopHighlight>>,
    outlines: Query<(Entity, &ChildOf), With<LoopOutline>>,
    work: Option<Res<EntityWorkCounter>>,
) {
    if !diagnostics.is_changed()
        && !view.is_changed()
        && new_gates.is_empty()
        && new_wires.is_empty()
    {
        return;
    }
    let loops = loops_in_view(&diagnostics, &view);
    if loops.is_empty() && marked.is_empty() {
        return;
    }
    EntityWorkCounter::add(&work, gates.iter().len() + wires.iter().len());
    let on_loop = |uuid: &Uuid| loops.iter().any(|l| l.contains(uuid));
    for (entity, id, ty, marked) in &gates {
        let wanted = on_loop(&id.uuid);
        if wanted && !marked {
            commands.entity(entity).insert(LoopHighlight);
            let piece = plate(
                &mut art,
                &mut meshes,
                ty.kind.size(),
                LOOP_OUTLINE_MARGIN,
                Paint::Fault,
                -0.6,
            );
            commands.spawn((piece, LoopOutline, ChildOf(entity)));
        } else if !wanted && marked {
            commands.entity(entity).remove::<LoopHighlight>();
            for (outline, parent) in &outlines {
                if parent.parent() == entity {
                    commands.entity(outline).despawn();
                }
            }
        }
    }
    for (entity, ep, marked) in &wires {
        let wanted = loops
            .iter()
            .any(|l| l.contains(&ep.source_gate) && l.contains(&ep.sink_gate));
        if wanted && !marked {
            commands.entity(entity).insert(LoopHighlight);
        } else if !wanted && marked {
            commands.entity(entity).remove::<LoopHighlight>();
        }
    }
}

/// A gate being dragged: it has [`Selected`] and moved.
type BeingDragged = (With<Selected>, Changed<Transform>);
/// The gate in front, when it is not the one being dragged.
type InFrontNotDragged = (With<OnTop>, Without<Selected>);

/// Bring the gate being dragged (it has [`Selected`] and moved) to the
/// front: z [`ON_TOP_Z`] and [`OnTop`]; the gate that was in front goes
/// back to z 0. The order holds after the drop ("last dragged on top").
pub fn bring_dragged_gate_to_front_system(
    mut commands: Commands,
    mut dragged: Query<(Entity, &mut Transform), BeingDragged>,
    mut on_top: Query<(Entity, &mut Transform), InFrontNotDragged>,
) {
    let mut lifted = false;
    for (entity, mut transform) in &mut dragged {
        if transform.translation.z != ON_TOP_Z {
            transform.translation.z = ON_TOP_Z;
            commands.entity(entity).try_insert(OnTop);
            lifted = true;
        }
    }
    if !lifted {
        return;
    }
    for (entity, mut transform) in &mut on_top {
        transform.translation.z = 0.0;
        commands.entity(entity).try_remove::<OnTop>();
    }
}

/// Lift the wires of the [`OnTop`] gate above the other wires, and put
/// those of a gate that left the front back. Runs only when the front
/// gate changes.
pub fn lift_front_wires_system(
    added: Query<&GateIdentity, Added<OnTop>>,
    mut removed: RemovedComponents<OnTop>,
    identities: Query<&GateIdentity>,
    front: Query<&GateIdentity, With<OnTop>>,
    routing: Res<WireRouting>,
    mut wires: Query<(Entity, &WireEndpoints, &mut Transform), Without<GateIdentity>>,
) {
    let dropped: HashSet<Uuid> = removed
        .read()
        .filter_map(|e| identities.get(e).ok().map(|id| id.uuid))
        .collect();
    if added.is_empty() && dropped.is_empty() {
        return;
    }
    let lifted: HashSet<Uuid> = front.iter().map(|id| id.uuid).collect();
    let touched: HashSet<NetKey> = wires
        .iter()
        .filter(|(_, ep, _)| {
            [ep.source_gate, ep.sink_gate]
                .iter()
                .any(|id| lifted.contains(id) || dropped.contains(id))
        })
        .map(|(_, ep, _)| (ep.source_gate, ep.source_pin))
        .collect();
    for key in touched {
        let Some(net) = routing.nets.get(&key) else {
            continue;
        };
        let sinks: Vec<(SinkKey, Entity)> = wires
            .iter()
            .filter(|(_, ep, _)| (ep.source_gate, ep.source_pin) == key)
            .map(|(e, ep, _)| ((ep.sink_gate, ep.sink_pin), e))
            .collect();
        let z = depth_of(&key, &sinks, &lifted)(net.slot);
        for (_, entity) in &sinks {
            if let Ok((_, _, mut transform)) = wires.get_mut(*entity)
                && transform.translation.z != z
            {
                transform.translation.z = z;
            }
        }
    }
}

/// Draw a halo behind the selected gate (the one whose properties are
/// open). Runs only when the selection changes.
pub fn update_selection_halo_system(
    mut commands: Commands,
    selected: Option<Res<SelectedGate>>,
    mut art: ResMut<CanvasArt>,
    mut meshes: ResMut<Assets<Mesh>>,
    gates: Query<&GateType>,
    halos: Query<Entity, With<SelectionHalo>>,
    mut shown: Local<Option<Entity>>,
) {
    let Some(selected) = selected else { return };
    if !selected.is_changed() || *shown == selected.0 {
        return;
    }
    for halo in &halos {
        commands.entity(halo).despawn();
    }
    *shown = selected.0;
    if let Some(entity) = selected.0
        && let Ok(ty) = gates.get(entity)
    {
        let piece = plate(
            &mut art,
            &mut meshes,
            ty.kind.size(),
            HALO_MARGIN,
            Paint::Halo,
            -0.7,
        );
        commands.spawn((piece, SelectionHalo, ChildOf(entity)));
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Gates
// ─────────────────────────────────────────────────────────────────────────────

type GateStateChanged = Or<(Changed<GateValues>, Changed<GateType>)>;

/// Light pins, lamps and switches when a gate's values (or kind) change,
/// by swapping shared materials.
#[allow(clippy::type_complexity)]
pub fn update_gate_state_system(
    art: Res<CanvasArt>,
    gates: Query<(&GateType, &GateValues, &Children), GateStateChanged>,
    mut pieces: Query<
        (
            &mut MeshMaterial2d<ColorMaterial>,
            &mut Transform,
            Option<&PinDot>,
            Option<&ValueArt>,
        ),
        Or<(With<PinDot>, With<ValueArt>)>,
    >,
    work: Option<Res<EntityWorkCounter>>,
) {
    for (ty, values, children) in &gates {
        EntityWorkCounter::add(&work, 1);
        let on = shown_value(&ty.kind, values) != 0;
        for &child in children {
            let Ok((mut material, mut transform, pin, part)) = pieces.get_mut(child) else {
                continue;
            };
            let paint = match (pin, part) {
                (Some(pin), _) => {
                    let list = if pin.output {
                        &values.outputs
                    } else {
                        &values.inputs
                    };
                    let value = list.get(usize::from(pin.index)).copied().unwrap_or(0);
                    Paint::pin(pin.width, value)
                }
                (None, Some(part)) => {
                    if *part == ValueArt::Knob {
                        let x = knob_x(on);
                        if transform.translation.x != x {
                            transform.translation.x = x;
                        }
                    }
                    value_paint(*part, on)
                }
                (None, None) => continue,
            };
            let handle = art.material(paint);
            if material.0 != handle {
                material.0 = handle;
            }
        }
    }
}

type DisplaySourceChanged = Or<(
    Changed<GateValues>,
    Changed<DisplayConfig>,
    Changed<GateType>,
    Changed<Children>,
)>;

/// Texts that show a gate's value.
type ValueText = Or<(With<LogicDisplay>, With<InputReadout>)>;

/// Show output, display and word-input values as text. The text is
/// formatted only when the value, width or display format changed.
pub fn update_logic_displays_system(
    gates: Query<(&GateType, &GateValues, Option<&DisplayConfig>, &Children), DisplaySourceChanged>,
    mut texts: Query<(&mut Text2d, &mut DisplayedValue), ValueText>,
    work: Option<Res<EntityWorkCounter>>,
) {
    for (ty, values, config, children) in &gates {
        EntityWorkCounter::add(&work, 1);
        let shown = (
            shown_value(&ty.kind, values),
            ty.kind.width().unwrap_or(1),
            config.copied().unwrap_or_default(),
        );
        for &child in children {
            let Ok((mut text, mut displayed)) = texts.get_mut(child) else {
                continue;
            };
            if displayed.0 == Some(shown) {
                continue;
            }
            displayed.0 = Some(shown);
            let formatted = format_value(shown.0, shown.1, &shown.2);
            if text.0 != formatted {
                text.0 = formatted;
            }
        }
    }
}

/// Keep port name tags in step with their labels.
pub fn update_name_tags_system(
    gates: Query<(&GateLabel, &Children), Changed<GateLabel>>,
    mut tags: Query<&mut Text2d, With<NameTag>>,
) {
    for (label, children) in &gates {
        for &child in children {
            if let Ok(mut text) = tags.get_mut(child)
                && text.0 != label.value
            {
                text.0 = label.value.clone();
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Wires
// ─────────────────────────────────────────────────────────────────────────────

/// A net of the open view: its driver pin.
type NetKey = (Uuid, u8);
/// A sink of a net: its gate and input pin.
type SinkKey = (Uuid, u8);

/// One routed net of the open view.
#[derive(Debug, Clone)]
struct RoutedNet {
    /// Depth slot (see [`net_depth`]).
    slot: u32,
    driver: Vec2,
    sinks: Vec<(SinkKey, Vec2)>,
    route: NetRoute,
}

/// The routes of the open view's wires, by net, and the state of the
/// incremental routing (see [`route_wires_system`]).
#[derive(Resource, Debug, Default)]
pub struct WireRouting {
    nets: HashMap<NetKey, RoutedNet>,
    /// Kept paths may run through boxes after a drag: route strictly on
    /// the next quiet frame.
    repair: bool,
    /// Counters of the last routing pass.
    pub stats: RouteStats,
    /// Routing passes so far.
    pub passes: u64,
}

impl WireRouting {
    /// The routed tree of the net driven by `pin` of gate `driver`.
    pub fn net(&self, driver: Uuid, pin: u8) -> Option<&NetRoute> {
        self.nets.get(&(driver, pin)).map(|n| &n.route)
    }
}

/// Depth of a net's wires. Nets get distinct depth slots, so where two
/// cross, one is cleanly above the other.
fn net_depth(slot: u32) -> f32 {
    crate::temp::CONNECTION_Z_LEVEL - slot as f32 * NET_STAGGER
}

/// Depth of a net with an end on the [`OnTop`] gate: above every other
/// net (still distinct per slot), below the value pills and the tiles.
fn lifted_net_depth(slot: u32) -> f32 {
    crate::temp::CONNECTION_Z_LEVEL + 2.0 - slot as f32 * (1.0 / NET_SLOTS as f32)
}

/// The depth of a net's wires, lifted when it touches a front gate.
fn depth_of(
    key: &NetKey,
    sinks: &[(SinkKey, Entity)],
    front: &HashSet<Uuid>,
) -> impl Fn(u32) -> f32 {
    let lifted = front.contains(&key.0) || sinks.iter().any(|((g, _), _)| front.contains(g));
    move |slot| {
        if lifted {
            lifted_net_depth(slot)
        } else {
            net_depth(slot)
        }
    }
}

/// A free depth slot for a net, starting from a hash of its key so a net
/// keeps its depth while others come and go.
fn free_slot(key: &NetKey, used: &HashSet<u32>) -> u32 {
    let bits = key.0.as_u128();
    let mixed = ((bits as u64) ^ ((bits >> 64) as u64) ^ u64::from(key.1))
        .wrapping_mul(0x9E37_79B9_7F4A_7C15);
    let start = ((mixed >> 32) % u64::from(NET_SLOTS)) as u32;
    (0..NET_SLOTS)
        .map(|i| (start + i) % NET_SLOTS)
        .find(|s| !used.contains(s))
        .unwrap_or(start)
}

/// What wire `sink` of a routed net draws: its own part of the tree (and
/// the net's pill when `pill` names it).
pub fn wire_path(net: &NetRoute, sink: usize, pill: Option<(usize, Vec2)>) -> WirePath {
    let own = &net.sinks[sink];
    let mut points = own.points.clone();
    // A branch leaving the tree at a corner: the corner is rounded, so the
    // branch carries on the leg it continues up to where the curve starts.
    if let (Some(at), Some(parent)) = (own.junction(), own.parent)
        && points.len() >= 2
        && let Some(parent) = net.sinks.get(parent)
    {
        let pp = &parent.points;
        if let Some(i) = (1..pp.len().saturating_sub(1)).find(|&i| pp[i].distance(at) < 1e-3) {
            let r = corner_radius(pp, i, WIRE_CORNER);
            let d = (points[1] - points[0]).normalize_or_zero();
            if r > 0.0 && d != Vec2::ZERO {
                points.insert(0, at - d * r);
            }
        }
    }
    WirePath {
        points,
        junction: own.junction(),
        pill: pill.filter(|p| p.0 == sink).map(|p| p.1),
    }
}

/// Route the wires of the open view and give each wire its [`WirePath`].
///
/// Runs only when something changed: gates or wires added or removed, a
/// pin layout, the view, or a gate's position. It keeps the path of every
/// sink whose two pins stayed put ([`routing::route`] with kept paths), so
/// dragging a gate re-routes only that gate's wires (and the branches
/// hanging off them) and never touches the `Design`. While a gate moves,
/// kept paths it now covers stay as they are; on the first frame without
/// movement they are routed around it. A wire's [`WirePath`] is written
/// only when it changed.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub fn route_wires_system(
    mut commands: Commands,
    mut routing: ResMut<WireRouting>,
    view: Res<CircuitView>,
    art: Res<CanvasArt>,
    mut meshes: ResMut<Assets<Mesh>>,
    new_gates: Query<(), Added<GateIdentity>>,
    mut removed_gates: RemovedComponents<GateIdentity>,
    mut removed_wires: RemovedComponents<WireEndpoints>,
    moved: Query<Ref<GateIdentity>, (Changed<Transform>, Without<WireEndpoints>)>,
    relaid: Query<(), (With<GateIdentity>, Changed<PinLayout>)>,
    rewired: Query<(), Changed<WireEndpoints>>,
    gates: Query<
        (
            &GateIdentity,
            &Transform,
            &GateType,
            Option<&PinLayout>,
            Option<&TileSpec>,
        ),
        Without<WireEndpoints>,
    >,
    mut wires: Query<
        (Entity, &WireEndpoints, &mut WirePath, &mut Transform),
        Without<GateIdentity>,
    >,
    gaps: Query<&Mesh2d, With<WireGaps>>,
    (work, front): (
        Option<Res<EntityWorkCounter>>,
        Query<&GateIdentity, With<OnTop>>,
    ),
) {
    let front: HashSet<Uuid> = front.iter().map(|id| id.uuid).collect();
    let removed = removed_gates.read().count() + removed_wires.read().count() > 0;
    let moving = moved.iter().any(|id| !id.is_added());
    let structural = view.is_changed()
        || removed
        || !new_gates.is_empty()
        || !relaid.is_empty()
        || !rewired.is_empty();
    if !structural && !moving && !routing.repair {
        return;
    }
    let strict = !moving;

    // The problem: a box per gate, a net per driven output pin, ordered by
    // driver and sink ids as `routing::document_nets` orders a document.
    let mut problem = RouteProblem::default();
    let mut pins: HashMap<Uuid, (Vec2, Option<&PinLayout>, Option<&TileSpec>)> = HashMap::new();
    for (id, transform, ty, layout, spec) in &gates {
        let at = transform.translation.truncate();
        problem.boxes.push(routing::gate_box(&ty.kind, at));
        pins.insert(id.uuid, (at, layout, spec));
    }
    let mut grouped: BTreeMap<NetKey, Vec<(SinkKey, Entity)>> = BTreeMap::new();
    for (entity, ep, ..) in &wires {
        if pins.contains_key(&ep.source_gate) && pins.contains_key(&ep.sink_gate) {
            grouped
                .entry((ep.source_gate, ep.source_pin))
                .or_default()
                .push(((ep.sink_gate, ep.sink_pin), entity));
        }
    }
    let pin_at = |gate: &Uuid, pin: u8, output: bool| {
        let (at, layout, _) = pins[gate];
        at + layout.map_or(Vec2::ZERO, |l| {
            if output { l.output(pin) } else { l.input(pin) }
        })
    };
    let mut keep: Vec<Vec<Option<SinkPath>>> = Vec::with_capacity(grouped.len());
    let mut buses: Vec<bool> = Vec::with_capacity(grouped.len());
    let mut dirty = 0;
    for (key, sinks) in grouped.iter_mut() {
        sinks.sort_by_key(|s| s.0);
        let driver = pin_at(&key.0, key.1, true);
        let at: Vec<Vec2> = sinks
            .iter()
            .map(|(s, _)| pin_at(&s.0, s.1, false))
            .collect();
        // A net whose driver moved or whose sinks changed is routed afresh
        // (a tree depends on its sink order); otherwise sinks whose pins
        // stayed put keep their paths.
        let old = routing.nets.get(key).filter(|old| {
            old.driver == driver
                && old.sinks.len() == sinks.len()
                && old.sinks.iter().zip(sinks.iter()).all(|(o, n)| o.0 == n.0)
        });
        let kept: Vec<Option<SinkPath>> = sinks
            .iter()
            .zip(&at)
            .map(|((sink, _), pos)| {
                let old = old?;
                let index = |k: &SinkKey| sinks.iter().position(|(s, _)| s == k);
                let i = old.sinks.iter().position(|(s, p)| s == sink && p == pos)?;
                let mut path = old.route.sinks[i].clone();
                if let Some(parent) = path.parent {
                    path.parent = Some(index(&old.sinks[parent].0)?);
                }
                Some(path)
            })
            .collect();
        dirty += kept.iter().filter(|k| k.is_none()).count();
        keep.push(kept);
        buses.push(
            pins[&key.0]
                .2
                .and_then(|s| s.outputs.get(usize::from(key.1)))
                .is_some_and(|&w| w > 1),
        );
        problem.nets.push(NetSpec { driver, sinks: at });
    }
    if !strict && dirty == 0 && !structural {
        // A gate without wires moved: nothing to route until it settles.
        routing.repair = true;
        return;
    }
    let result = routing::route(&problem, &keep, strict);
    routing.repair = !strict;
    routing.stats = result.stats;
    routing.passes += 1;

    // Depth slots: kept for nets that stay, new ones for new nets.
    let mut used: HashSet<u32> = grouped
        .keys()
        .filter_map(|k| routing.nets.get(k).map(|n| n.slot))
        .collect();
    let mut nets: HashMap<NetKey, RoutedNet> = HashMap::with_capacity(grouped.len());
    let mut depths: Vec<f32> = Vec::with_capacity(grouped.len());
    let mut written = 0;
    for (ni, ((key, sinks), route)) in grouped.iter().zip(result.nets).enumerate() {
        let slot = match routing.nets.get(key) {
            Some(old) => old.slot,
            None => {
                let slot = free_slot(key, &used);
                used.insert(slot);
                slot
            }
        };
        let z = depth_of(key, sinks, &front)(slot);
        depths.push(z);
        let pill = buses[ni]
            .then(|| route.longest_horizontal())
            .flatten()
            .map(|(s, a, b)| (s, (a + b) * 0.5));
        for (k, (_, entity)) in sinks.iter().enumerate() {
            let Ok((_, _, mut path, mut transform)) = wires.get_mut(*entity) else {
                continue;
            };
            let want = wire_path(&route, k, pill);
            if *path != want {
                *path = want;
                written += 1;
            }
            let at = Vec3::new(0.0, 0.0, z);
            if transform.translation != at {
                transform.translation = at;
            }
        }
        nets.insert(
            *key,
            RoutedNet {
                slot,
                driver: problem.nets[ni].driver,
                sinks: sinks
                    .iter()
                    .zip(&problem.nets[ni].sinks)
                    .map(|((s, _), p)| (*s, *p))
                    .collect(),
                route,
            },
        );
    }
    routing.nets = nets;
    EntityWorkCounter::add(&work, written);

    // Crossing gaps: one mesh for the view, each gap at the depth just
    // below the upper wire.
    let casing = |net: usize| casing_for(if buses[net] { BUS_CORE } else { BIT_CORE });
    let rects: Vec<(Vec2, Vec2, f32)> = result
        .crossings
        .iter()
        .map(|c| {
            let (h, v) = (c.horizontal.0, c.vertical.0);
            let h_on_top = depths[h] >= depths[v];
            let (upper, lower) = if h_on_top { (h, v) } else { (v, h) };
            let across = casing(upper) * 0.5 + GAP;
            let along = casing(lower) * 0.5 + 1.0;
            let half = if h_on_top {
                Vec2::new(along, across)
            } else {
                Vec2::new(across, along)
            };
            (c.at, half, depths[upper] - GAP_BELOW)
        })
        .collect();
    let mesh = gap_mesh(&rects);
    match gaps.single() {
        Ok(handle) => {
            let _ = meshes.insert(handle.id(), mesh);
        }
        Err(_) => {
            commands.spawn((
                Mesh2d(meshes.add(mesh)),
                MeshMaterial2d(art.material(Paint::Bench)),
                Transform::IDENTITY,
                Visibility::Visible,
                WireGaps,
            ));
        }
    }
}

/// The mesh of the crossing gaps: a rectangle per `(centre, half size,
/// depth)`.
fn gap_mesh(rects: &[(Vec2, Vec2, f32)]) -> Mesh {
    if rects.is_empty() {
        return empty_mesh();
    }
    let mut positions: Vec<[f32; 3]> = Vec::with_capacity(rects.len() * 4);
    let mut indices: Vec<u32> = Vec::with_capacity(rects.len() * 6);
    for (c, h, z) in rects {
        let base = positions.len() as u32;
        for (sx, sy) in [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)] {
            positions.push([c.x + sx * h.x, c.y + sy * h.y, *z]);
        }
        indices.extend([base, base + 1, base + 2, base, base + 2, base + 3]);
    }
    let n = positions.len();
    Mesh::new(
        bevy::mesh::PrimitiveTopology::TriangleList,
        bevy::asset::RenderAssetUsages::RENDER_WORLD,
    )
    .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions)
    .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, vec![[0.0, 0.0, 1.0]; n])
    .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, vec![[0.0, 0.0]; n])
    .with_inserted_indices(bevy::mesh::Indices::U32(indices))
}

type WireLookChanged = (
    With<WireEndpoints>,
    Or<(Changed<WireState>, Changed<WirePath>, Added<LoopHighlight>)>,
);

fn label_paint(value: u64) -> Paint {
    if value == 0 {
        Paint::TextFaint
    } else {
        Paint::BusText
    }
}

/// Spawn a bus's value label on its pill, at `at` (local to the wire).
fn spawn_value_label(
    commands: &mut Commands,
    art: &mut CanvasArt,
    meshes: &mut Assets<Mesh>,
    wire: Entity,
    at: Vec3,
    want: WireValueLabel,
) {
    let digits = usize::from(want.width.max(1)).div_ceil(4);
    let size = pill_size(digits);
    let ink = art.piece(
        meshes,
        MeshKey::rounded_rect(size + Vec2::splat(OUTLINE), size.y * 0.5 + OUTLINE * 0.5),
        Paint::Ink,
        Vec3::new(0.0, 0.0, -0.2),
    );
    let face = art.piece(
        meshes,
        MeshKey::rounded_rect(size, size.y * 0.5),
        Paint::Tile,
        Vec3::new(0.0, 0.0, -0.1),
    );
    commands
        .spawn((
            Text2d::new(wire_label_text(want.value, want.width)),
            art.fonts.mono(13.0),
            TextColor(Color::WHITE),
            Tone(label_paint(want.value)),
            Transform::from_translation(at),
            want,
            ChildOf(wire),
        ))
        .with_children(|pill| {
            pill.spawn(ink);
            pill.spawn(face);
        });
}

/// Draw wires: rebuild a wire's casing and core meshes (rounded corners)
/// and its junction dot when its [`WirePath`] or thickness changed; on a
/// value change only swap the core's shared material (marigold, slate,
/// teal, tomato on a loop) and update the bus's value pill, whose text
/// changes only when the value does.
#[allow(clippy::type_complexity, clippy::too_many_arguments)]
pub fn update_wire_visuals_system(
    mut commands: Commands,
    theme: Res<Theme>,
    mut art: ResMut<CanvasArt>,
    mut meshes: ResMut<Assets<Mesh>>,
    changed: Query<Entity, WireLookChanged>,
    mut unmarked: RemovedComponents<LoopHighlight>,
    mut wires: Query<
        (
            Option<&WireState>,
            Has<LoopHighlight>,
            Ref<WirePath>,
            &mut WireStyle,
            &mut WireBuilt,
            &Transform,
            Option<&Children>,
        ),
        With<WireEndpoints>,
    >,
    mut pieces: Query<
        (&Mesh2d, &mut MeshMaterial2d<ColorMaterial>, Has<WireCore>),
        (
            Or<(With<WireCasing>, With<WireCore>)>,
            Without<WireJunction>,
        ),
    >,
    mut junctions: Query<(&WireJunction, &mut MeshMaterial2d<ColorMaterial>), Without<WireCasing>>,
    mut labels: Query<
        (
            &mut WireValueLabel,
            &mut Text2d,
            &mut Tone,
            &mut TextColor,
            &mut Transform,
        ),
        Without<WireEndpoints>,
    >,
    work: Option<Res<EntityWorkCounter>>,
) {
    let mut dirty: Vec<Entity> = changed.iter().chain(unmarked.read()).collect();
    if dirty.is_empty() {
        return;
    }
    dirty.sort_unstable();
    dirty.dedup();
    let palette = theme.palette;
    for entity in dirty {
        let Ok((state, on_loop, path, mut style, mut built, transform, children)) =
            wires.get_mut(entity)
        else {
            continue;
        };
        EntityWorkCounter::add(&work, 1);
        let state = state.copied().unwrap_or_default();
        let look = wire_look(&state, on_loop);
        if *style != look {
            *style = look;
        }
        let children: Vec<Entity> = children.into_iter().flatten().copied().collect();

        // Geometry.
        if path.is_changed() || built.0 != look.core {
            if built.0 != look.core {
                built.0 = look.core;
            }
            let round = round_corners(&path.points, WIRE_CORNER);
            let stroke = |width: f32| {
                if round.len() >= 2 {
                    Shape::path_stroke(&round, width).into_mesh()
                } else {
                    empty_mesh()
                }
            };
            let (casing_mesh, core_mesh) = (stroke(casing_for(look.core)), stroke(look.core));
            for &child in &children {
                if let Ok((mesh, _, core)) = pieces.get(child) {
                    let m = if core {
                        core_mesh.clone()
                    } else {
                        casing_mesh.clone()
                    };
                    let _ = meshes.insert(mesh.id(), m);
                }
                if junctions.contains(child) {
                    commands.entity(child).despawn();
                }
            }
            if let Some(at) = path.junction {
                let outer = junction_radius(look.core);
                for (core, radius, z, paint) in [
                    (false, outer, JUNCTION_INK_Z, Paint::Ink),
                    (true, outer - OUTLINE, JUNCTION_CORE_Z, look.paint),
                ] {
                    let piece = art.piece(&mut meshes, MeshKey::dot(radius), paint, at.extend(z));
                    commands.spawn((piece, WireJunction { core }, ChildOf(entity)));
                }
            }
        }

        // Colour: shared materials.
        let handle = art.material(look.paint);
        for &child in &children {
            if let Ok((_, mut material, true)) = pieces.get_mut(child)
                && material.0 != handle
            {
                material.0 = handle.clone();
            }
            if let Ok((junction, mut material)) = junctions.get_mut(child)
                && junction.core
                && material.0 != handle
            {
                material.0 = handle.clone();
            }
        }

        // The value pill of a bus.
        let wanted = (state.width > 1 && state.connected)
            .then_some(WireValueLabel {
                value: state.value,
                width: state.width,
            })
            .zip(path.pill);
        let existing = children.iter().copied().find(|c| labels.contains(*c));
        let local = |at: Vec2| at.extend(PILL_Z - transform.translation.z);
        match (wanted, existing) {
            (Some((want, at)), Some(label)) => {
                let (mut shown, mut text, mut tone, mut text_colour, mut label_transform) =
                    labels.get_mut(label).expect("label");
                if shown.width != want.width {
                    // The pill's size depends on the width.
                    commands.entity(label).despawn();
                    spawn_value_label(
                        &mut commands,
                        &mut art,
                        &mut meshes,
                        entity,
                        local(at),
                        want,
                    );
                } else {
                    if *shown != want {
                        *shown = want;
                        text.0 = wire_label_text(want.value, want.width);
                        let paint = label_paint(want.value);
                        if tone.0 != paint {
                            tone.0 = paint;
                            text_colour.0 = paint.color(&palette);
                        }
                    }
                    if label_transform.translation != local(at) {
                        label_transform.translation = local(at);
                    }
                }
            }
            (Some((want, at)), None) => {
                spawn_value_label(
                    &mut commands,
                    &mut art,
                    &mut meshes,
                    entity,
                    local(at),
                    want,
                );
            }
            (None, Some(label)) => {
                commands.entity(label).despawn();
            }
            (None, None) => {}
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Theme
// ─────────────────────────────────────────────────────────────────────────────

/// Colour newly spawned theme-following texts.
pub fn colour_new_texts_system(
    theme: Res<Theme>,
    mut texts: Query<(&Tone, &mut TextColor), Added<Tone>>,
) {
    for (tone, mut colour) in &mut texts {
        let c = tone.0.color(&theme.palette);
        if colour.0 != c {
            colour.0 = c;
        }
    }
}

/// On a theme change: recolour the shared materials (tiles, pins and
/// wires) and every theme-following text. Nothing runs while the theme
/// stays put.
pub fn apply_theme_to_canvas_system(
    theme: Res<Theme>,
    art: Res<CanvasArt>,
    materials: Option<ResMut<Assets<ColorMaterial>>>,
    mut texts: Query<(&Tone, &mut TextColor)>,
) {
    if !theme.is_changed() {
        return;
    }
    let palette = theme.palette;
    if let Some(mut materials) = materials {
        art.apply_palette(&palette, &mut materials);
    }
    for (tone, mut colour) in &mut texts {
        let c = tone.0.color(&palette);
        if colour.0 != c {
            colour.0 = c;
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Plugin
// ─────────────────────────────────────────────────────────────────────────────

/// Gate and wire visuals, driven by change detection. Expects
/// [`super::SimulationRuntimePlugin`]; needs no window or renderer.
pub struct SimulationVisualsPlugin;

impl Plugin for SimulationVisualsPlugin {
    fn build(&self, app: &mut App) {
        // Headless apps (tests, tools) have no render plugins; the stores
        // the tiles are built in are all the visuals need.
        if !app.world().contains_resource::<Assets<Mesh>>() {
            app.init_resource::<Assets<Mesh>>();
        }
        if !app.world().contains_resource::<Assets<ColorMaterial>>() {
            app.init_resource::<Assets<ColorMaterial>>();
        }
        app.init_resource::<Theme>()
            .init_resource::<CanvasArt>()
            .init_resource::<WireRouting>()
            .register_type::<LoopHighlight>()
            .add_systems(
                PostUpdate,
                (
                    (attach_gate_visuals_system, attach_wire_visuals_system),
                    apply_loop_highlight_system,
                    update_selection_halo_system,
                    bring_dragged_gate_to_front_system,
                    lift_front_wires_system,
                    (
                        update_gate_state_system,
                        update_logic_displays_system,
                        update_name_tags_system,
                    ),
                    route_wires_system,
                    update_wire_visuals_system,
                    (colour_new_texts_system, apply_theme_to_canvas_system).chain(),
                )
                    .chain()
                    .after(sync_design_system)
                    .before(TransformSystems::Propagate),
            );
    }
}
