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
//! - wire geometry is recomputed only for wires whose endpoints changed or
//!   whose endpoint gate moved (a drag moves only that gate's wires); wires
//!   run from pin to pin;
//! - a wire is cased: an ink casing under a coloured core (thicker for
//!   buses), with a value pill on buses; colour, thickness and pill follow
//!   [`WireState`] and the loop highlight;
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

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};

use bevy::prelude::*;
use bevy::transform::TransformSystems;
use uuid::Uuid;

use super::art::{
    BIT_CORE, BUS_CORE, CanvasArt, EDGE, Glyph, MeshKey, OUTLINE, Paint, PinLayout, Tone,
    casing_for, family, pill_size, shorten, tile_caption, tile_radius,
};
use super::bridge::sync_design_system;
use super::view::CircuitView;
use super::{
    CircuitLibrary, DisplayConfig, GateIdentity, GateKind, GateLabel, GateType, GateValues,
    LogicDisplay, Selected, SelectedGate, SimDiagnostics, WireEndpoints, WireState,
    calculate_connection_geometry, format_value,
};
use crate::theme::{Palette, Theme};

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

/// The ink casing under a wire's core (a child of the wire).
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct WireCasing;

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
/// Local z of a wire's value label (above every wire, below the gates).
const WIRE_LABEL_Z: f32 = 4.5;
/// Knob travel of an input switch.
const KNOB_TRAVEL: f32 = 6.0;
/// Wires are staggered in depth, so where two cross, the upper one's
/// casing cuts a gap in the lower one (an overpass), not a join.
const WIRE_STAGGER: f32 = 0.004;

/// Core colour and core thickness of a wire. The casing is
/// [`casing_for`] the thickness.
pub fn wire_style(state: &WireState, on_loop: bool, palette: &Palette) -> (Color, f32) {
    let bus = state.width > 1;
    let thickness = if bus { BUS_CORE } else { BIT_CORE };
    let color = if on_loop {
        palette.fault
    } else if !state.connected {
        palette.fault_text
    } else {
        palette.signal(state.width, state.value)
    };
    (color, thickness)
}

/// One hex digit per 4 bits, as written in a bus's value pill.
pub fn wire_label_text(value: u64, width: u8) -> String {
    format!(
        "{value:0digits$X}",
        digits = usize::from(width.max(1)).div_ceil(4)
    )
}

fn wire_depth(entity: Entity) -> f32 {
    crate::temp::CONNECTION_Z_LEVEL - (entity.to_bits() % 997) as f32 * WIRE_STAGGER
}

/// Depth of a wire of the [`OnTop`] gate: above every other wire (keeping
/// the stagger among its own), still below the wire labels and gates.
fn lifted_wire_depth(entity: Entity) -> f32 {
    wire_depth(entity) + 998.0 * WIRE_STAGGER
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

/// Give new wires a core sprite and an ink casing;
/// [`update_wire_geometry_system`] places them and
/// [`update_wire_style_system`] colours them.
pub fn attach_wire_visuals_system(
    mut commands: Commands,
    wires: Query<Entity, Added<WireEndpoints>>,
    theme: Res<Theme>,
    work: Option<Res<EntityWorkCounter>>,
) {
    for entity in &wires {
        EntityWorkCounter::add(&work, 1);
        commands.entity(entity).try_insert((
            Sprite {
                color: theme.palette.off,
                custom_size: Some(Vec2::new(1.0, BIT_CORE)),
                ..default()
            },
            Transform::from_xyz(0.0, 0.0, wire_depth(entity)),
            Visibility::Visible,
        ));
        commands.spawn((
            Sprite {
                color: theme.palette.ink,
                custom_size: Some(Vec2::new(1.0, casing_for(BIT_CORE))),
                ..default()
            },
            Transform::from_xyz(0.0, 0.0, -WIRE_STAGGER * 0.5),
            WireCasing,
            ChildOf(entity),
        ));
    }
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
    mut wires: Query<(Entity, &WireEndpoints, &mut Transform), Without<GateIdentity>>,
) {
    let dropped: Vec<Uuid> = removed
        .read()
        .filter_map(|e| identities.get(e).ok().map(|id| id.uuid))
        .collect();
    if added.is_empty() && dropped.is_empty() {
        return;
    }
    let lifted: HashSet<Uuid> = front.iter().map(|id| id.uuid).collect();
    for (entity, ep, mut transform) in &mut wires {
        let ends = [ep.source_gate, ep.sink_gate];
        let z = if ends.iter().any(|id| lifted.contains(id)) {
            lifted_wire_depth(entity)
        } else if ends.iter().any(|id| dropped.contains(id)) {
            wire_depth(entity)
        } else {
            continue;
        };
        if transform.translation.z != z {
            transform.translation.z = z;
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

/// Gate entities by id and wire entities by endpoint gate, for the open
/// view. Rebuilt only after structural changes.
#[derive(Default)]
pub struct WireIndex {
    gates: HashMap<Uuid, Entity>,
    wires_of: HashMap<Uuid, Vec<Entity>>,
}

/// World positions of a wire's two pins (driver output, sink input), or
/// gate centres where a gate has no pin layout yet.
pub fn wire_ends(
    ep: &WireEndpoints,
    source: (Vec3, Option<&PinLayout>),
    sink: (Vec3, Option<&PinLayout>),
) -> (Vec3, Vec3) {
    let from = source.0
        + source
            .1
            .map_or(Vec2::ZERO, |l| l.output(ep.source_pin))
            .extend(0.0);
    let to = sink.0
        + sink
            .1
            .map_or(Vec2::ZERO, |l| l.input(ep.sink_pin))
            .extend(0.0);
    (from, to)
}

/// Place wire sprites between their pins: only wires whose endpoints
/// changed, and the wires of gates whose `Transform` or pin layout
/// changed.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub fn update_wire_geometry_system(
    mut index: Local<WireIndex>,
    view: Res<CircuitView>,
    new_gates: Query<(), Added<GateIdentity>>,
    mut removed_gates: RemovedComponents<GateIdentity>,
    mut removed_wires: RemovedComponents<WireEndpoints>,
    moved: Query<&GateIdentity, Or<(Changed<Transform>, Changed<PinLayout>)>>,
    rewired: Query<Entity, Changed<WireEndpoints>>,
    all_gates: Query<(Entity, &GateIdentity)>,
    all_wires: Query<(Entity, &WireEndpoints)>,
    positions: Query<
        (&Transform, Option<&PinLayout>),
        (With<GateIdentity>, Without<WireEndpoints>),
    >,
    mut wires: Query<
        (
            &WireEndpoints,
            &mut Transform,
            &mut Sprite,
            Option<&Children>,
        ),
        Without<GateIdentity>,
    >,
    mut labels: Query<
        &mut Transform,
        (
            With<WireValueLabel>,
            Without<WireEndpoints>,
            Without<GateIdentity>,
        ),
    >,
    mut casings: Query<&mut Sprite, (With<WireCasing>, Without<WireEndpoints>)>,
    work: Option<Res<EntityWorkCounter>>,
) {
    let removed = removed_gates.read().count() + removed_wires.read().count() > 0;
    if view.is_changed() || removed || !new_gates.is_empty() || !rewired.is_empty() {
        EntityWorkCounter::add(&work, all_gates.iter().len() + all_wires.iter().len());
        index.gates = all_gates.iter().map(|(e, id)| (id.uuid, e)).collect();
        index.wires_of.clear();
        for (entity, ep) in &all_wires {
            index
                .wires_of
                .entry(ep.source_gate)
                .or_default()
                .push(entity);
            if ep.sink_gate != ep.source_gate {
                index.wires_of.entry(ep.sink_gate).or_default().push(entity);
            }
        }
    }

    let mut dirty: Vec<Entity> = rewired.iter().collect();
    for id in &moved {
        if let Some(list) = index.wires_of.get(&id.uuid) {
            dirty.extend_from_slice(list);
        }
    }
    if dirty.is_empty() {
        return;
    }
    dirty.sort_unstable();
    dirty.dedup();
    let gate = |uuid: &Uuid| {
        index
            .gates
            .get(uuid)
            .and_then(|e| positions.get(*e).ok())
            .map(|(t, l)| (t.translation, l))
    };
    for entity in dirty {
        let Ok((ep, mut transform, mut sprite, children)) = wires.get_mut(entity) else {
            continue;
        };
        EntityWorkCounter::add(&work, 1);
        let (Some(source), Some(sink)) = (gate(&ep.source_gate), gate(&ep.sink_gate)) else {
            continue;
        };
        let (from, to) = wire_ends(ep, source, sink);
        let (mid, rot, dist) = calculate_connection_geometry(&from, &to);
        let translation = mid.with_z(transform.translation.z);
        if transform.translation != translation || transform.rotation != rot {
            transform.translation = translation;
            transform.rotation = rot;
        }
        let thickness = sprite.custom_size.map_or(BIT_CORE, |s| s.y);
        let size = Some(Vec2::new(dist, thickness));
        if sprite.custom_size != size {
            sprite.custom_size = size;
        }
        for &child in children.into_iter().flatten() {
            // Keep value labels upright.
            if let Ok(mut label) = labels.get_mut(child) {
                let upright = rot.inverse();
                if label.rotation != upright {
                    label.rotation = upright;
                }
            }
            if let Ok(mut casing) = casings.get_mut(child) {
                let size = Some(Vec2::new(dist, casing_for(thickness)));
                if casing.custom_size != size {
                    casing.custom_size = size;
                }
            }
        }
    }
}

type WireStyleChanged = (
    With<WireEndpoints>,
    Or<(Changed<WireState>, Added<LoopHighlight>)>,
);

fn label_paint(value: u64) -> Paint {
    if value == 0 {
        Paint::TextFaint
    } else {
        Paint::BusText
    }
}

/// Spawn a bus's value label on its pill.
fn spawn_value_label(
    commands: &mut Commands,
    art: &mut CanvasArt,
    meshes: &mut Assets<Mesh>,
    wire: Entity,
    rotation: Quat,
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
            Transform {
                translation: Vec3::new(0.0, 0.0, WIRE_LABEL_Z),
                rotation: rotation.inverse(),
                ..default()
            },
            want,
            ChildOf(wire),
        ))
        .with_children(|pill| {
            pill.spawn(ink);
            pill.spawn(face);
        });
}

/// Colour wires by value and width, draw loop wires in tomato, and keep the
/// value pill of multi-bit wires (its text changes only when the value
/// does).
#[allow(clippy::type_complexity, clippy::too_many_arguments)]
pub fn update_wire_style_system(
    mut commands: Commands,
    theme: Res<Theme>,
    mut art: ResMut<CanvasArt>,
    mut meshes: ResMut<Assets<Mesh>>,
    changed: Query<Entity, WireStyleChanged>,
    mut unmarked: RemovedComponents<LoopHighlight>,
    mut wires: Query<(
        &WireState,
        Has<LoopHighlight>,
        &Transform,
        &mut Sprite,
        Option<&Children>,
    )>,
    mut casings: Query<&mut Sprite, (With<WireCasing>, Without<WireState>)>,
    mut labels: Query<(&mut WireValueLabel, &mut Text2d, &mut Tone, &mut TextColor)>,
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
        let Ok((state, on_loop, transform, mut sprite, children)) = wires.get_mut(entity) else {
            continue;
        };
        EntityWorkCounter::add(&work, 1);
        let (color, thickness) = wire_style(state, on_loop, &palette);
        if sprite.color != color {
            sprite.color = color;
        }
        let length = sprite.custom_size.map_or(1.0, |s| s.x);
        let size = Some(Vec2::new(length, thickness));
        if sprite.custom_size != size {
            sprite.custom_size = size;
        }
        for &child in children.into_iter().flatten() {
            if let Ok(mut casing) = casings.get_mut(child) {
                let size = Some(Vec2::new(length, casing_for(thickness)));
                if casing.custom_size != size {
                    casing.custom_size = size;
                }
            }
        }

        let wanted = (state.width > 1 && state.connected).then_some(WireValueLabel {
            value: state.value,
            width: state.width,
        });
        let existing = children
            .into_iter()
            .flatten()
            .copied()
            .find(|c| labels.contains(*c));
        match (wanted, existing) {
            (Some(want), Some(label)) => {
                let (mut shown, mut text, mut tone, mut text_colour) =
                    labels.get_mut(label).expect("label");
                if shown.width != want.width {
                    // The pill's size depends on the width.
                    commands.entity(label).despawn();
                    spawn_value_label(
                        &mut commands,
                        &mut art,
                        &mut meshes,
                        entity,
                        transform.rotation,
                        want,
                    );
                } else if *shown != want {
                    *shown = want;
                    text.0 = wire_label_text(want.value, want.width);
                    let paint = label_paint(want.value);
                    if tone.0 != paint {
                        tone.0 = paint;
                        text_colour.0 = paint.color(&palette);
                    }
                }
            }
            (Some(want), None) => spawn_value_label(
                &mut commands,
                &mut art,
                &mut meshes,
                entity,
                transform.rotation,
                want,
            ),
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

/// On a theme change: recolour the shared materials, every wire and every
/// theme-following text. Nothing runs while the theme stays put.
#[allow(clippy::type_complexity)]
pub fn apply_theme_to_canvas_system(
    theme: Res<Theme>,
    art: Res<CanvasArt>,
    materials: Option<ResMut<Assets<ColorMaterial>>>,
    mut wires: Query<(&WireState, Has<LoopHighlight>, &mut Sprite), With<WireEndpoints>>,
    mut casings: Query<&mut Sprite, (With<WireCasing>, Without<WireEndpoints>)>,
    mut texts: Query<(&Tone, &mut TextColor)>,
) {
    if !theme.is_changed() {
        return;
    }
    let palette = theme.palette;
    if let Some(mut materials) = materials {
        art.apply_palette(&palette, &mut materials);
    }
    for (state, on_loop, mut sprite) in &mut wires {
        let (color, _) = wire_style(state, on_loop, &palette);
        if sprite.color != color {
            sprite.color = color;
        }
    }
    for mut casing in &mut casings {
        if casing.color != palette.ink {
            casing.color = palette.ink;
        }
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
                    update_wire_geometry_system,
                    update_wire_style_system,
                    (colour_new_texts_system, apply_theme_to_canvas_system).chain(),
                )
                    .chain()
                    .after(sync_design_system)
                    .before(TransformSystems::Propagate),
            );
    }
}
