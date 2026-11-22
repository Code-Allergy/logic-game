//! Selecting several parts at once ([`MultiSelection`]):
//!
//! - `Shift`+left click on a part adds it to the selection, or takes it out;
//! - `Shift`+left drag on empty canvas draws a box, and the parts whose
//!   centre is in it are added;
//! - a plain left press on the canvas goes back to a single selection (the
//!   part clicked, or none);
//! - a right drag on a selected part moves the whole group.
//!
//! The part last added is the primary selection ([`SelectedGate`]). With
//! two or more parts selected the editor shows the selection card (Make
//! chip, Delete) instead of the inspector.
//!
//! Plain left drags on empty canvas are left alone (they may pan the view
//! one day).

use bevy::prelude::*;
use bevy::window::PrimaryWindow;

use super::{GateIdentity, GateType, MultiSelection, Selected, SelectedGate};
use crate::UiPointerState;

/// A `Shift`+drag box being drawn, in world coordinates.
#[derive(Resource, Debug, Default, Clone, Copy, PartialEq)]
pub struct BoxSelect {
    pub start: Option<Vec2>,
    pub current: Option<Vec2>,
}

impl BoxSelect {
    /// The box, once it is more than a click.
    pub fn rect(&self) -> Option<Rect> {
        let (a, b) = (self.start?, self.current?);
        let rect = Rect::from_corners(a, b);
        (rect.width() > 2.0 || rect.height() > 2.0).then_some(rect)
    }
}

/// Whether a `Shift` key is held.
pub fn shift_held(keys: &ButtonInput<KeyCode>) -> bool {
    keys.any_pressed([KeyCode::ShiftLeft, KeyCode::ShiftRight])
}

/// The parts (of `parts`: entity and centre) whose centre is in `rect`,
/// in a stable order (top to bottom, then left to right).
pub fn parts_in_box(rect: Rect, parts: impl Iterator<Item = (Entity, Vec2)>) -> Vec<Entity> {
    let mut inside: Vec<(Entity, Vec2)> = parts.filter(|(_, c)| rect.contains(*c)).collect();
    inside.sort_by(|a, b| b.1.y.total_cmp(&a.1.y).then(a.1.x.total_cmp(&b.1.x)));
    inside.into_iter().map(|(e, _)| e).collect()
}

fn cursor_world(
    windows: &Query<&Window, With<PrimaryWindow>>,
    cameras: &Query<(&Camera, &GlobalTransform)>,
) -> Option<Vec2> {
    let window = windows.single().ok()?;
    let (camera, transform) = cameras.single().ok()?;
    let ray = camera
        .viewport_to_world(transform, window.cursor_position()?)
        .ok()?;
    let distance = -ray.origin.z / ray.direction.z;
    Some((ray.origin + ray.direction * distance).truncate())
}

fn hit(transform: &Transform, kind: &super::GateKind, world: Vec2) -> bool {
    let half = kind.size() * 0.5;
    let local = transform
        .to_matrix()
        .inverse()
        .transform_point3(world.extend(0.0))
        .truncate();
    local.x.abs() <= half.x && local.y.abs() <= half.y
}

/// Shift+click, Shift+drag box and the plain press that ends a group (see
/// the module docs). Inactive while a part is armed or the wire tool is on.
#[allow(clippy::too_many_arguments)]
pub fn multi_select_pointer_system(
    ui_pointer: Res<UiPointerState>,
    buttons: Res<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    windows: Query<&Window, With<PrimaryWindow>>,
    cameras: Query<(&Camera, &GlobalTransform)>,
    gates: Query<(Entity, &Transform, &GateType), With<GateIdentity>>,
    editor: Option<Res<crate::editor::EditorState>>,
    mut multi: ResMut<MultiSelection>,
    mut selected: ResMut<SelectedGate>,
    mut boxed: ResMut<BoxSelect>,
) {
    let tool_in_hand = editor
        .as_ref()
        .is_some_and(|e| e.connection_mode || e.selected_gate_type.is_some());
    let world = cursor_world(&windows, &cameras);
    if buttons.just_pressed(MouseButton::Left) && !ui_pointer.over_egui && !tool_in_hand {
        if !shift_held(&keys) {
            if !multi.is_empty() {
                multi.clear();
            }
        } else if let Some(world) = world {
            let target = gates
                .iter()
                .find(|(_, t, ty)| hit(t, &ty.kind, world))
                .map(|(e, ..)| e);
            match target {
                Some(entity) => {
                    let primary = multi.toggle(entity, selected.0);
                    selected.set_if_neq(SelectedGate(primary));
                }
                None => {
                    *boxed = BoxSelect {
                        start: Some(world),
                        current: Some(world),
                    };
                }
            }
        }
    }
    if boxed.start.is_none() {
        return;
    }
    if let Some(world) = world {
        boxed.current = Some(world);
    }
    if buttons.pressed(MouseButton::Left) {
        return;
    }
    // Released: select what is in the box.
    if let Some(rect) = boxed.rect() {
        let inside = parts_in_box(
            rect,
            gates.iter().map(|(e, t, _)| (e, t.translation.truncate())),
        );
        if !inside.is_empty() {
            if multi.is_empty()
                && let Some(primary) = selected.0
            {
                multi.insert(primary);
            }
            for &e in &inside {
                multi.insert(e);
            }
            if selected.0.is_none() {
                selected.0 = inside.first().copied();
            }
        }
    }
    *boxed = BoxSelect::default();
}

/// Keep the group consistent: parts that are gone (deleted, or parked when
/// a view opens) leave it; without a primary selection, or with one
/// outside it, the group ends; a group of one is just the primary part.
pub fn prune_multi_selection_system(
    mut multi: ResMut<MultiSelection>,
    selected: Res<SelectedGate>,
    gates: Query<(), With<GateIdentity>>,
) {
    if multi.is_empty() {
        return;
    }
    let stale = multi.entities().iter().any(|e| gates.get(*e).is_err());
    let ends = selected.0.is_none_or(|p| !multi.contains(p));
    if !stale && !ends && multi.len() > 1 {
        return;
    }
    if ends {
        multi.clear();
        return;
    }
    multi.retain(|e| gates.get(*e).is_ok());
    if multi.len() <= 1 {
        multi.clear();
    }
}

type DraggedPart = (With<Selected>, With<GateIdentity>);

/// A right drag on a part of the group moves the whole group: the other
/// parts follow the dragged one.
pub fn move_group_system(
    multi: Res<MultiSelection>,
    buttons: Res<ButtonInput<MouseButton>>,
    dragged: Query<(Entity, &Transform), DraggedPart>,
    mut parts: Query<&mut Transform, (With<GateIdentity>, Without<Selected>)>,
    mut last: Local<Option<(Entity, Vec2)>>,
) {
    let held = buttons.pressed(MouseButton::Right) || buttons.pressed(MouseButton::Left);
    let Some((entity, transform)) = dragged.iter().find(|(e, _)| multi.contains(*e)) else {
        *last = None;
        return;
    };
    let at = transform.translation.truncate();
    if !held {
        *last = None;
        return;
    }
    let delta = match *last {
        Some((e, from)) if e == entity => at - from,
        _ => Vec2::ZERO,
    };
    *last = Some((entity, at));
    if delta == Vec2::ZERO {
        return;
    }
    for &other in multi.entities() {
        if other == entity {
            continue;
        }
        if let Ok(mut t) = parts.get_mut(other) {
            t.translation.x += delta.x;
            t.translation.y += delta.y;
        }
    }
}

/// Draw the `Shift`+drag box.
pub fn draw_box_select_system(
    boxed: Res<BoxSelect>,
    theme: Res<crate::theme::Theme>,
    mut gizmos: Gizmos,
) {
    if let Some(rect) = boxed.rect() {
        gizmos.rect_2d(
            Isometry2d::from_translation(rect.center()),
            rect.size(),
            theme.palette.io,
        );
    }
}
