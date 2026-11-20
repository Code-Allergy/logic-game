//! Change-driven visuals for gates and wires.
//!
//! Every system here reacts to component changes (Bevy change detection)
//! and writes a component only when the new value differs, so an idle
//! circuit (simulation paused, no input) costs no per-entity work:
//!
//! - gate sprites are recoloured when [`GateValues`] or [`GateType`]
//!   change;
//! - display text is re-formatted only when the shown value, width or
//!   [`DisplayConfig`] change ([`DisplayedValue`]);
//! - wire geometry is recomputed only for wires whose endpoints changed or
//!   whose endpoint gate's `Transform` changed (a drag moves only that
//!   gate's wires);
//! - wire colour, thickness and the value label of multi-bit wires follow
//!   [`WireState`] and the loop highlight;
//! - a combinational-loop compile error highlights the gates and wires on
//!   the loop in the open view ([`LoopHighlight`]) until a later compile
//!   succeeds.
//!
//! [`SimulationVisualsPlugin`] has no rendering, window or egui
//! dependencies, so headless tests run the same systems.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};

use bevy::prelude::*;
use bevy::transform::TransformSystems;
use uuid::Uuid;

use super::bridge::sync_design_system;
use super::view::CircuitView;
use super::{
    DisplayConfig, GateIdentity, GateKind, GateType, GateValues, LogicDisplay, SimDiagnostics,
    WireEndpoints, WireState, calculate_connection_geometry, format_value, get_gate_sprite,
};

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

/// The red outline drawn behind a highlighted gate (a child of it).
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct LoopOutline;

/// What a [`LogicDisplay`] text currently shows: value, width and format.
#[derive(Component, Debug, Clone, Copy, Default, PartialEq)]
pub struct DisplayedValue(pub Option<(u64, u8, DisplayConfig)>);

/// Hex value label of a multi-bit wire (a child of the wire), with the
/// value and width it shows.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub struct WireValueLabel {
    pub value: u64,
    pub width: u8,
}

// ─────────────────────────────────────────────────────────────────────────────
// Styles
// ─────────────────────────────────────────────────────────────────────────────

pub const LOOP_COLOR: Color = Color::srgb(1.0, 0.15, 0.1);
const WIRE_THICKNESS: f32 = 2.0;
const BUS_THICKNESS: f32 = 5.0;
const LOOP_THICKNESS: f32 = 6.0;
const LOOP_OUTLINE_MARGIN: f32 = 10.0;
/// Offset of a wire's value label from the line, in the wire's frame.
const WIRE_LABEL_OFFSET: Vec3 = Vec3::new(0.0, 12.0, 1.0);

/// Colour and thickness of a wire.
pub fn wire_style(state: &WireState, on_loop: bool) -> (Color, f32) {
    let bus = state.width > 1;
    let thickness = if on_loop {
        LOOP_THICKNESS
    } else if bus {
        BUS_THICKNESS
    } else {
        WIRE_THICKNESS
    };
    let color = if on_loop {
        LOOP_COLOR
    } else if !state.connected {
        Color::srgba(0.8, 0.6, 0.2, 1.0)
    } else if bus {
        // Buses: blue, brighter when any bit is set.
        if state.value != 0 { Color::srgb(0.45, 0.85, 1.0) } else { Color::srgb(0.2, 0.45, 0.9) }
    } else if state.value != 0 {
        Color::srgb(0.0, 1.0, 0.0)
    } else {
        Color::WHITE
    };
    (color, thickness)
}

/// `0x` and one hex digit per 4 bits.
pub fn wire_label_text(value: u64, width: u8) -> String {
    format!("0x{value:0digits$X}", digits = usize::from(width.max(1)).div_ceil(4))
}

/// Colour of a gate whose colour follows its value (inputs and outputs).
fn value_colour(kind: &GateKind, values: &GateValues) -> Option<Color> {
    match kind {
        GateKind::Input { .. } => Some(if values.outputs.first().copied().unwrap_or(0) != 0 {
            Color::srgb(0.0, 1.0, 0.0)
        } else {
            Color::srgb(0.8, 0.0, 0.0)
        }),
        GateKind::Output { .. } => Some(if values.inputs.first().copied().unwrap_or(0) != 0 {
            Color::srgb(1.0, 1.0, 1.0)
        } else {
            Color::srgb(0.1, 0.1, 0.1)
        }),
        _ => None,
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Attachment
// ─────────────────────────────────────────────────────────────────────────────

/// Give new or re-kinded gates their sprite and value text.
pub fn attach_gate_visuals_system(
    mut commands: Commands,
    gates: Query<(Entity, &GateType, Option<&Children>), Changed<GateType>>,
    texts: Query<(), With<LogicDisplay>>,
    work: Option<Res<EntityWorkCounter>>,
) {
    for (entity, ty, children) in &gates {
        EntityWorkCounter::add(&work, 1);
        for child in children.into_iter().flat_map(|c| c.iter()) {
            if texts.contains(child) {
                commands.entity(child).despawn();
            }
        }
        commands.entity(entity).try_insert(get_gate_sprite(&ty.kind));
        if matches!(ty.kind, GateKind::Output { .. } | GateKind::Display { .. }) {
            commands.spawn((
                Text2d::new(""),
                TextFont::from_font_size(22.0),
                TextColor(Color::srgb(0.3, 1.0, 0.5)),
                Transform::from_xyz(0.0, 0.0, 1.0),
                LogicDisplay,
                DisplayedValue::default(),
                ChildOf(entity),
            ));
        }
    }
}

/// Give new wires a line sprite; [`update_wire_geometry_system`] places it
/// and [`update_wire_style_system`] colours it.
pub fn attach_wire_visuals_system(
    mut commands: Commands,
    wires: Query<Entity, Added<WireEndpoints>>,
    work: Option<Res<EntityWorkCounter>>,
) {
    for entity in &wires {
        EntityWorkCounter::add(&work, 1);
        commands.entity(entity).try_insert((
            Sprite { color: Color::WHITE, custom_size: Some(Vec2::new(1.0, WIRE_THICKNESS)), ..default() },
            Transform::from_xyz(0.0, 0.0, crate::temp::CONNECTION_Z_LEVEL),
            Visibility::Visible,
        ));
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Loop highlight
// ─────────────────────────────────────────────────────────────────────────────

/// The loops of the last failed compile as seen from the open view: per
/// loop, the gate ids of the open circuit on it. A node inside a nested
/// instance is shown as that instance's gate.
fn loops_in_view(diagnostics: &SimDiagnostics, view: &CircuitView) -> Vec<HashSet<Uuid>> {
    let Some(error) = &diagnostics.compile_error else { return Vec::new() };
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

/// Mark the gates and wires of the open view that lie on a combinational
/// loop of the last failed compile; clear the marks once a compile
/// succeeds. A wire is marked when both its ends are on the same loop
/// (every edge inside a strongly connected component lies on a cycle).
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub fn apply_loop_highlight_system(
    mut commands: Commands,
    diagnostics: Res<SimDiagnostics>,
    view: Res<CircuitView>,
    new_gates: Query<(), Added<GateIdentity>>,
    new_wires: Query<(), Added<WireEndpoints>>,
    gates: Query<(Entity, &GateIdentity, &GateType, Has<LoopHighlight>)>,
    wires: Query<(Entity, &WireEndpoints, Has<LoopHighlight>)>,
    marked: Query<(), With<LoopHighlight>>,
    outlines: Query<(Entity, &ChildOf), With<LoopOutline>>,
    work: Option<Res<EntityWorkCounter>>,
) {
    if !diagnostics.is_changed() && !view.is_changed() && new_gates.is_empty() && new_wires.is_empty()
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
            let size = ty.kind.size() + Vec2::splat(LOOP_OUTLINE_MARGIN);
            commands.entity(entity).insert(LoopHighlight);
            commands.spawn((
                Sprite { color: LOOP_COLOR, custom_size: Some(size), ..default() },
                Transform::from_xyz(0.0, 0.0, -0.5),
                LoopOutline,
                ChildOf(entity),
            ));
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
        let wanted =
            loops.iter().any(|l| l.contains(&ep.source_gate) && l.contains(&ep.sink_gate));
        if wanted && !marked {
            commands.entity(entity).insert(LoopHighlight);
        } else if !wanted && marked {
            commands.entity(entity).remove::<LoopHighlight>();
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Gates
// ─────────────────────────────────────────────────────────────────────────────

type GateColourChanged = Or<(Changed<GateValues>, Changed<GateType>)>;

/// Recolour input and output gates when their value or kind changes.
pub fn update_gate_colours_system(
    mut gates: Query<(&mut Sprite, &GateType, &GateValues), GateColourChanged>,
    work: Option<Res<EntityWorkCounter>>,
) {
    for (mut sprite, ty, values) in &mut gates {
        EntityWorkCounter::add(&work, 1);
        if let Some(color) = value_colour(&ty.kind, values)
            && sprite.color != color
        {
            sprite.color = color;
        }
    }
}

type DisplaySourceChanged =
    Or<(Changed<GateValues>, Changed<DisplayConfig>, Changed<GateType>, Changed<Children>)>;

/// Show output and display values as text. The text is formatted only when
/// the value, width or display format changed.
pub fn update_logic_displays_system(
    gates: Query<(&GateType, &GateValues, Option<&DisplayConfig>, &Children), DisplaySourceChanged>,
    mut texts: Query<(&mut Text2d, &mut DisplayedValue), With<LogicDisplay>>,
    work: Option<Res<EntityWorkCounter>>,
) {
    for (ty, values, config, children) in &gates {
        EntityWorkCounter::add(&work, 1);
        let shown = (
            values.inputs.first().copied().unwrap_or(0),
            ty.kind.width().unwrap_or(1),
            config.copied().unwrap_or_default(),
        );
        for &child in children {
            let Ok((mut text, mut displayed)) = texts.get_mut(child) else { continue };
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

/// Place wire sprites between their gates: only wires whose endpoints
/// changed, and the wires of gates whose `Transform` changed.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub fn update_wire_geometry_system(
    mut index: Local<WireIndex>,
    view: Res<CircuitView>,
    new_gates: Query<(), Added<GateIdentity>>,
    mut removed_gates: RemovedComponents<GateIdentity>,
    mut removed_wires: RemovedComponents<WireEndpoints>,
    moved: Query<&GateIdentity, Changed<Transform>>,
    rewired: Query<Entity, Changed<WireEndpoints>>,
    all_gates: Query<(Entity, &GateIdentity)>,
    all_wires: Query<(Entity, &WireEndpoints)>,
    positions: Query<&Transform, (With<GateIdentity>, Without<WireEndpoints>)>,
    mut wires: Query<
        (&WireEndpoints, &mut Transform, &mut Sprite, Option<&Children>),
        Without<GateIdentity>,
    >,
    mut labels: Query<&mut Transform, (With<WireValueLabel>, Without<WireEndpoints>, Without<GateIdentity>)>,
    work: Option<Res<EntityWorkCounter>>,
) {
    let removed = removed_gates.read().count() + removed_wires.read().count() > 0;
    if view.is_changed() || removed || !new_gates.is_empty() || !rewired.is_empty() {
        EntityWorkCounter::add(&work, all_gates.iter().len() + all_wires.iter().len());
        index.gates = all_gates.iter().map(|(e, id)| (id.uuid, e)).collect();
        index.wires_of.clear();
        for (entity, ep) in &all_wires {
            index.wires_of.entry(ep.source_gate).or_default().push(entity);
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
    let position = |uuid: &Uuid| index.gates.get(uuid).and_then(|e| positions.get(*e).ok()).map(|t| t.translation);
    for entity in dirty {
        let Ok((ep, mut transform, mut sprite, children)) = wires.get_mut(entity) else { continue };
        EntityWorkCounter::add(&work, 1);
        let (Some(from), Some(to)) = (position(&ep.source_gate), position(&ep.sink_gate)) else {
            continue;
        };
        let (mid, rot, dist) = calculate_connection_geometry(&from, &to);
        let translation = mid.with_z(crate::temp::CONNECTION_Z_LEVEL);
        if transform.translation != translation || transform.rotation != rot {
            transform.translation = translation;
            transform.rotation = rot;
        }
        let thickness = sprite.custom_size.map_or(WIRE_THICKNESS, |s| s.y);
        let size = Some(Vec2::new(dist, thickness));
        if sprite.custom_size != size {
            sprite.custom_size = size;
        }
        // Keep value labels upright.
        for &child in children.into_iter().flatten() {
            if let Ok(mut label) = labels.get_mut(child) {
                let upright = rot.inverse();
                if label.rotation != upright {
                    label.rotation = upright;
                }
            }
        }
    }
}

type WireStyleChanged = (With<WireEndpoints>, Or<(Changed<WireState>, Added<LoopHighlight>)>);

/// Colour wires by value and width, draw loop wires in red, and keep the
/// hex value label of multi-bit wires (its text changes only when the
/// value does).
#[allow(clippy::type_complexity)]
pub fn update_wire_style_system(
    mut commands: Commands,
    changed: Query<Entity, WireStyleChanged>,
    mut unmarked: RemovedComponents<LoopHighlight>,
    mut wires: Query<(&WireState, Has<LoopHighlight>, &Transform, &mut Sprite, Option<&Children>)>,
    mut labels: Query<(&mut WireValueLabel, &mut Text2d)>,
    work: Option<Res<EntityWorkCounter>>,
) {
    let mut dirty: Vec<Entity> = changed.iter().chain(unmarked.read()).collect();
    if dirty.is_empty() {
        return;
    }
    dirty.sort_unstable();
    dirty.dedup();
    for entity in dirty {
        let Ok((state, on_loop, transform, mut sprite, children)) = wires.get_mut(entity) else {
            continue;
        };
        EntityWorkCounter::add(&work, 1);
        let (color, thickness) = wire_style(state, on_loop);
        if sprite.color != color {
            sprite.color = color;
        }
        let length = sprite.custom_size.map_or(1.0, |s| s.x);
        let size = Some(Vec2::new(length, thickness));
        if sprite.custom_size != size {
            sprite.custom_size = size;
        }

        let wanted = (state.width > 1 && state.connected)
            .then_some(WireValueLabel { value: state.value, width: state.width });
        let existing = children.into_iter().flatten().copied().find(|c| labels.contains(*c));
        match (wanted, existing) {
            (Some(want), Some(label)) => {
                let (mut shown, mut text) = labels.get_mut(label).expect("label");
                if *shown != want {
                    *shown = want;
                    text.0 = wire_label_text(want.value, want.width);
                }
            }
            (Some(want), None) => {
                commands.spawn((
                    Text2d::new(wire_label_text(want.value, want.width)),
                    TextFont::from_font_size(14.0),
                    TextColor(Color::srgb(0.75, 0.9, 1.0)),
                    Transform {
                        translation: WIRE_LABEL_OFFSET,
                        rotation: transform.rotation.inverse(),
                        ..default()
                    },
                    want,
                    ChildOf(entity),
                ));
            }
            (None, Some(label)) => {
                commands.entity(label).despawn();
            }
            (None, None) => {}
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
        app.register_type::<LoopHighlight>().add_systems(
            PostUpdate,
            (
                (attach_gate_visuals_system, attach_wire_visuals_system),
                apply_loop_highlight_system,
                (update_gate_colours_system, update_logic_displays_system),
                update_wire_geometry_system,
                update_wire_style_system,
            )
                .chain()
                .after(sync_design_system)
                .before(TransformSystems::Propagate),
        );
    }
}
