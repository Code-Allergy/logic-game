//! Mouse input on the canvas ([`canvas_pointer_system`]): click to select
//! and toggle a 1-bit input, left or right drag to move a part on the
//! snap grid, left drag from a pin to make a wire. The cursor is set in
//! [`CanvasCursor`] directly (there is no window in a headless app).

use bevy::prelude::*;

use super::*;
use crate::editor::EditorState;
use crate::sim::art::PinLayout;
use crate::sim::layout::SNAP;
use crate::sim::systems::{CanvasCursor, CanvasPointer, DRAG_THRESHOLD, canvas_pointer_system};
use crate::sim::wiring::{DragMark, PIN_REACH_MAX, PinPointer, wire_drag_visuals_system};
use crate::sim::{
    ActiveCircuit, AuthoringFeedback, InputDrive, Selected, SelectedGate, SimulationVisualsPlugin,
};
use crate::temp::GateComponentSelectionEvent;
use crate::{UiPointerState, sim::systems::update_selected_gate};

fn pointer_app() -> App {
    let mut app = app();
    app.init_resource::<UiPointerState>()
        .init_resource::<ButtonInput<MouseButton>>()
        .insert_resource(CanvasCursor {
            world_per_pixel: 1.0,
            ..default()
        })
        .init_resource::<CanvasPointer>()
        .init_resource::<PinPointer>()
        .init_resource::<SelectedGate>()
        .add_message::<GateComponentSelectionEvent>()
        .add_systems(
            Update,
            (canvas_pointer_system, update_selected_gate).chain(),
        );
    app
}

fn place(app: &mut App, kind: GateKind, label: &str, at: Vec2) -> Entity {
    send(
        app,
        CreateGate {
            kind,
            position: at,
            label: Some(label.into()),
        },
    );
    gate(app, label)
}

/// Point at `world` (the window shows the world 1:1 unless [`zoom`]ed, y
/// flipped).
fn point(app: &mut App, world: Vec2) {
    let mut cursor = app.world_mut().resource_mut::<CanvasCursor>();
    let wpp = cursor.world_per_pixel;
    *cursor = CanvasCursor {
        screen: Some(Vec2::new(500.0 + world.x / wpp, 500.0 - world.y / wpp)),
        world: Some(world),
        world_per_pixel: wpp,
    };
}

/// Zoom to `wpp` world units per screen pixel.
fn zoom(app: &mut App, wpp: f32) {
    app.world_mut()
        .resource_mut::<CanvasCursor>()
        .world_per_pixel = wpp;
}

fn buttons(app: &mut App) -> Mut<'_, ButtonInput<MouseButton>> {
    app.world_mut().resource_mut::<ButtonInput<MouseButton>>()
}

fn press(app: &mut App, button: MouseButton, at: Vec2) {
    point(app, at);
    buttons(app).press(button);
    app.update();
    // As the input plugin does at the start of the next frame.
    buttons(app).clear();
}

fn move_to(app: &mut App, at: Vec2) {
    point(app, at);
    app.update();
}

fn release(app: &mut App, button: MouseButton, at: Vec2) {
    point(app, at);
    buttons(app).release(button);
    app.update();
    buttons(app).clear();
    settle(app);
}

fn click(app: &mut App, at: Vec2) {
    press(app, MouseButton::Left, at);
    release(app, MouseButton::Left, at);
}

fn drive(app: &App, entity: Entity) -> u64 {
    app.world().get::<InputDrive>(entity).unwrap().value
}

fn position(app: &App, entity: Entity) -> Vec2 {
    app.world()
        .get::<Transform>(entity)
        .unwrap()
        .translation
        .truncate()
}

fn selected(app: &App) -> Option<Entity> {
    app.world().resource::<SelectedGate>().0
}

#[test]
fn a_click_toggles_a_one_bit_input_and_selects_it() {
    let mut app = pointer_app();
    let a = place(&mut app, GateKind::Input { w: 1 }, "A", Vec2::ZERO);
    let b = place(
        &mut app,
        GateKind::Input { w: 1 },
        "B",
        Vec2::new(200.0, 0.0),
    );
    click(&mut app, Vec2::new(5.0, -5.0));
    assert_eq!(drive(&app, a), 1);
    assert_eq!(selected(&app), Some(a));
    click(&mut app, Vec2::new(200.0, 0.0));
    assert_eq!((drive(&app, a), drive(&app, b)), (1, 1));
    assert_eq!(selected(&app), Some(b));
    click(&mut app, Vec2::ZERO);
    assert_eq!(drive(&app, a), 0);
    // A click on empty canvas clears the selection and toggles nothing.
    click(&mut app, Vec2::new(-300.0, 0.0));
    assert_eq!(selected(&app), None);
    assert_eq!((drive(&app, a), drive(&app, b)), (0, 1));
    // Clicks do not move parts.
    assert_eq!(position(&app, a), Vec2::ZERO);
}

#[test]
fn a_click_on_a_wider_input_only_selects_it() {
    let mut app = pointer_app();
    let bus = place(&mut app, GateKind::Input { w: 8 }, "Bus", Vec2::ZERO);
    set_input(&mut app, "Bus", 5);
    click(&mut app, Vec2::ZERO);
    assert_eq!(selected(&app), Some(bus));
    assert_eq!(drive(&app, bus), 5);
}

#[test]
fn inputs_toggle_in_a_read_only_demo_but_nothing_moves() {
    let mut app = pointer_app();
    let a = place(&mut app, GateKind::Input { w: 1 }, "A", Vec2::ZERO);
    app.world_mut().resource_mut::<ActiveCircuit>().read_only = true;
    click(&mut app, Vec2::ZERO);
    assert_eq!(drive(&app, a), 1);
    for button in [MouseButton::Left, MouseButton::Right] {
        press(&mut app, button, Vec2::ZERO);
        move_to(&mut app, Vec2::new(80.0, 60.0));
        release(&mut app, button, Vec2::new(80.0, 60.0));
        assert_eq!(position(&app, a), Vec2::ZERO);
    }
    assert_eq!(drive(&app, a), 1, "a drag is not a click");
}

#[test]
fn with_the_wire_tool_or_a_part_armed_a_click_only_picks() {
    let mut app = pointer_app();
    let a = place(&mut app, GateKind::Input { w: 1 }, "A", Vec2::ZERO);
    app.insert_resource(EditorState {
        connection_mode: true,
        ..default()
    });
    click(&mut app, Vec2::ZERO);
    assert_eq!(drive(&app, a), 0);
    assert_eq!(
        selected(&app),
        Some(a),
        "the wire tool picks via the selection"
    );
    press(&mut app, MouseButton::Left, Vec2::ZERO);
    move_to(&mut app, Vec2::new(80.0, 0.0));
    release(&mut app, MouseButton::Left, Vec2::new(80.0, 0.0));
    assert_eq!(position(&app, a), Vec2::ZERO);

    app.insert_resource(EditorState {
        selected_gate_type: Some(GateKind::And { w: 1 }),
        ..default()
    });
    click(&mut app, Vec2::ZERO);
    assert_eq!(drive(&app, a), 0);
}

#[test]
fn a_left_drag_moves_a_part_and_drops_it_on_the_grid() {
    let mut app = pointer_app();
    let and = place(&mut app, GateKind::And { w: 1 }, "And", Vec2::ZERO);
    let a = place(
        &mut app,
        GateKind::Input { w: 1 },
        "A",
        Vec2::new(-200.0, 0.0),
    );
    // Grabbed off centre; a small wobble is still a click.
    press(&mut app, MouseButton::Left, Vec2::new(3.0, 2.0));
    assert_eq!(selected(&app), Some(and), "pressing selects");
    move_to(&mut app, Vec2::new(3.0 + DRAG_THRESHOLD - 1.0, 2.0));
    assert_eq!(position(&app, and), Vec2::ZERO);
    // Past the threshold, it follows the pointer at the grab offset, from
    // point to point of the snap grid: (47, 31) lands on (50, 30).
    move_to(&mut app, Vec2::new(50.0, 33.0));
    assert_eq!(position(&app, and), Vec2::new(5.0 * SNAP, 3.0 * SNAP));
    assert!(app.world().get::<Selected>(and).is_some(), "drawn on top");
    // A small move within a grid cell does not move it.
    move_to(&mut app, Vec2::new(52.0, 35.0));
    assert_eq!(position(&app, and), Vec2::new(50.0, 30.0));
    move_to(&mut app, Vec2::new(70.0, 37.0));
    assert_eq!(position(&app, and), Vec2::new(70.0, 40.0));
    release(&mut app, MouseButton::Left, Vec2::new(70.0, 37.0));
    assert_eq!(position(&app, and), Vec2::new(70.0, 40.0));
    assert_eq!(selected(&app), Some(and));

    // Dragging an input moves it without toggling it.
    press(&mut app, MouseButton::Left, Vec2::new(-200.0, 0.0));
    move_to(&mut app, Vec2::new(-200.0, -44.0));
    release(&mut app, MouseButton::Left, Vec2::new(-200.0, -44.0));
    assert_eq!(position(&app, a), Vec2::new(-200.0, -40.0));
    assert_eq!(drive(&app, a), 0);
    assert!(app.world().get::<Selected>(and).is_none());
    assert!(app.world().get::<Selected>(a).is_some());
}

#[test]
fn a_right_drag_still_moves_a_part_on_the_snap_grid() {
    let mut app = pointer_app();
    let and = place(&mut app, GateKind::And { w: 1 }, "And", Vec2::ZERO);
    press(&mut app, MouseButton::Right, Vec2::new(1.0, 1.0));
    move_to(&mut app, Vec2::new(-29.0, 52.0));
    assert_eq!(position(&app, and), Vec2::new(-30.0, 50.0));
    release(&mut app, MouseButton::Right, Vec2::new(-29.0, 52.0));
    assert_eq!(position(&app, and), Vec2::new(-30.0, 50.0));
    // A right click without a move leaves a part where it is, even off
    // the grid (as an older file may have it).
    let b = place(&mut app, GateKind::Or { w: 1 }, "Or", Vec2::new(200.0, 0.0));
    app.world_mut().get_mut::<Transform>(b).unwrap().translation = Vec3::new(203.0, 7.0, 0.0);
    press(&mut app, MouseButton::Right, Vec2::new(203.0, 7.0));
    move_to(&mut app, Vec2::new(205.0, 7.0));
    release(&mut app, MouseButton::Right, Vec2::new(205.0, 7.0));
    assert_eq!(position(&app, b), Vec2::new(203.0, 7.0));
}

#[test]
fn placed_parts_land_on_the_snap_grid() {
    let mut app = pointer_app();
    let a = place(
        &mut app,
        GateKind::Not { w: 1 },
        "N",
        Vec2::new(23.0, -36.0),
    );
    assert_eq!(position(&app, a), Vec2::new(20.0, -40.0));
    let b = place(
        &mut app,
        GateKind::Not { w: 1 },
        "M",
        Vec2::new(105.0, 14.9),
    );
    assert_eq!(position(&app, b), Vec2::new(110.0, 10.0));
}

#[test]
fn presses_over_the_hud_are_ignored() {
    let mut app = pointer_app();
    let a = place(&mut app, GateKind::Input { w: 1 }, "A", Vec2::ZERO);
    app.world_mut().resource_mut::<UiPointerState>().over_egui = true;
    click(&mut app, Vec2::ZERO);
    assert_eq!(drive(&app, a), 0);
    assert_eq!(selected(&app), None);
}

// ─────────────────────────────────────────────────────────────────────────────
// Drag to wire
// ─────────────────────────────────────────────────────────────────────────────

/// The pointer app with the tiles built (pin layouts) and the drag art.
fn wire_app() -> App {
    let mut app = pointer_app();
    app.add_plugins(SimulationVisualsPlugin)
        .init_resource::<ButtonInput<KeyCode>>()
        .add_systems(
            Update,
            wire_drag_visuals_system.after(canvas_pointer_system),
        );
    app
}

/// World position of a pin of a part, from its tile's layout.
fn pin(app: &App, entity: Entity, output: bool, index: u8) -> Vec2 {
    let layout = app.world().get::<PinLayout>(entity).expect("a built tile");
    let at = position(app, entity);
    at + if output {
        layout.output(index)
    } else {
        layout.input(index)
    }
}

/// Left-drag from `from` to `to` in a few steps and release there.
fn drag_wire(app: &mut App, from: Vec2, to: Vec2) {
    press(app, MouseButton::Left, from);
    for step in 1..=4 {
        move_to(app, from.lerp(to, step as f32 / 4.0));
    }
    release(app, MouseButton::Left, to);
}

/// The wires as (source, source pin, sink, sink pin).
fn wire_ends(app: &mut App) -> Vec<(Uuid, u8, Uuid, u8)> {
    let mut ends: Vec<_> = wires(app)
        .into_iter()
        .map(|(_, ep, _)| (ep.source_gate, ep.source_pin, ep.sink_gate, ep.sink_pin))
        .collect();
    ends.sort();
    ends
}

fn toast(app: &App) -> Option<String> {
    app.world().resource::<AuthoringFeedback>().message.clone()
}

fn hovered(app: &App) -> Option<(Entity, bool, u8)> {
    app.world()
        .resource::<PinPointer>()
        .hover
        .map(|p| (p.gate, p.output, p.index))
}

fn mark_shown(app: &mut App, mark: DragMark) -> bool {
    let mut q = app.world_mut().query::<(&DragMark, &Visibility)>();
    q.iter(app.world())
        .any(|(m, v)| *m == mark && *v == Visibility::Visible)
}

#[test]
fn pins_are_hit_within_their_dot_plus_a_slack_that_follows_the_zoom() {
    let mut app = wire_app();
    let and = place(&mut app, GateKind::And { w: 1 }, "And", Vec2::ZERO);
    let a = pin(&app, and, false, 0);
    let b = pin(&app, and, false, 1);
    let y = pin(&app, and, true, 0);
    let radius = app.world().get::<PinLayout>(and).unwrap().radius;

    // 1:1: the dot plus 6 px.
    let reach = radius + 6.0;
    move_to(&mut app, a + Vec2::new(-(reach - 0.5), 0.0));
    assert_eq!(hovered(&app), Some((and, false, 0)));
    move_to(&mut app, a + Vec2::new(-(reach + 0.5), 0.0));
    assert_eq!(hovered(&app), None);
    move_to(&mut app, y + Vec2::new(0.0, reach - 0.5));
    assert_eq!(hovered(&app), Some((and, true, 0)));
    // Between two pins, the nearer one.
    move_to(&mut app, b + Vec2::new(0.0, 2.0));
    assert_eq!(hovered(&app), Some((and, false, 1)));
    // The middle of the tile is the tile.
    move_to(&mut app, Vec2::ZERO);
    assert_eq!(hovered(&app), None);

    // Zoomed in 4x: 6 px is 1.5 units.
    zoom(&mut app, 0.25);
    move_to(&mut app, a + Vec2::new(-(radius + 1.0), 0.0));
    assert_eq!(hovered(&app), Some((and, false, 0)));
    move_to(&mut app, a + Vec2::new(-(radius + 2.0), 0.0));
    assert_eq!(hovered(&app), None);

    // Zoomed out 4x: 6 px is 24 units, capped so the tile stays a tile.
    zoom(&mut app, 4.0);
    move_to(&mut app, a + Vec2::new(-(PIN_REACH_MAX - 0.5), 0.0));
    assert_eq!(hovered(&app), Some((and, false, 0)));
    move_to(&mut app, a + Vec2::new(PIN_REACH_MAX + 0.5, 0.0));
    assert_eq!(hovered(&app), None);
    // A press there grabs the part, not a wire.
    press(&mut app, MouseButton::Left, Vec2::new(0.0, 2.0));
    move_to(&mut app, Vec2::new(40.0, 2.0));
    release(&mut app, MouseButton::Left, Vec2::new(40.0, 2.0));
    assert_eq!(position(&app, and), Vec2::new(40.0, 0.0));
    assert!(wire_ends(&mut app).is_empty());
}

#[test]
fn a_drag_from_an_output_to_an_input_makes_one_wire() {
    let mut app = wire_app();
    let a = place(
        &mut app,
        GateKind::Input { w: 1 },
        "A",
        Vec2::new(-200.0, 0.0),
    );
    let y = place(
        &mut app,
        GateKind::Output { w: 1 },
        "Y",
        Vec2::new(200.0, 40.0),
    );
    let from = pin(&app, a, true, 0) + Vec2::new(1.0, -2.0);
    let to = pin(&app, y, false, 0) + Vec2::new(-2.0, 1.0);
    press(&mut app, MouseButton::Left, from);
    move_to(&mut app, Vec2::ZERO);
    // In hand: the band and the marks are drawn; nothing moves.
    let drag = app
        .world()
        .resource::<PinPointer>()
        .drag
        .expect("a wire in hand");
    assert_eq!((drag.from.gate, drag.from.output), (a, true));
    assert!(mark_shown(&mut app, DragMark::BandCore));
    assert!(
        mark_shown(&mut app, DragMark::RingCore),
        "Y's input is a fit"
    );
    move_to(&mut app, to);
    release(&mut app, MouseButton::Left, to);
    let (ua, uy) = (uuid(&app, a), uuid(&app, y));
    assert_eq!(wire_ends(&mut app), vec![(ua, 0, uy, 0)]);
    assert_eq!(toast(&app), None);
    assert_eq!(position(&app, a), Vec2::new(-200.0, 0.0));
    assert_eq!(drive(&app, a), 0, "a wire drag is not a click");
    assert!(app.world().resource::<PinPointer>().drag.is_none());
    assert!(!mark_shown(&mut app, DragMark::BandCore));

    // The same wire again adds nothing.
    drag_wire(&mut app, from, to);
    assert_eq!(wire_ends(&mut app).len(), 1);
}

#[test]
fn a_drag_from_an_input_back_to_an_output_makes_the_wire_too() {
    let mut app = wire_app();
    let a = place(
        &mut app,
        GateKind::Input { w: 1 },
        "A",
        Vec2::new(-200.0, 0.0),
    );
    let and = place(&mut app, GateKind::And { w: 1 }, "And", Vec2::ZERO);
    let and_in1 = pin(&app, and, false, 1);
    let a_out0 = pin(&app, a, true, 0);
    drag_wire(&mut app, and_in1, a_out0);
    let (ua, uand) = (uuid(&app, a), uuid(&app, and));
    assert_eq!(wire_ends(&mut app), vec![(ua, 0, uand, 1)]);
    assert_eq!(position(&app, and), Vec2::ZERO);
}

#[test]
fn a_width_mismatch_or_a_drop_on_nothing_is_refused_with_a_toast() {
    let mut app = wire_app();
    let bus = place(
        &mut app,
        GateKind::Input { w: 8 },
        "Bus",
        Vec2::new(-200.0, 0.0),
    );
    let y = place(
        &mut app,
        GateKind::Output { w: 1 },
        "Y",
        Vec2::new(200.0, 0.0),
    );
    let b = place(
        &mut app,
        GateKind::Input { w: 1 },
        "B",
        Vec2::new(0.0, 200.0),
    );
    let from = pin(&app, bus, true, 0);
    // Over Y's 1-bit input, the drop is marked as refused.
    press(&mut app, MouseButton::Left, from);
    move_to(&mut app, Vec2::ZERO);
    assert!(
        mark_shown(&mut app, DragMark::Veils),
        "no pin fits an 8-bit bus"
    );
    let y_in0 = pin(&app, y, false, 0);
    move_to(&mut app, y_in0);
    assert_eq!(hovered(&app), Some((y, false, 0)));
    let y_in0 = pin(&app, y, false, 0);
    release(&mut app, MouseButton::Left, y_in0);
    assert!(wire_ends(&mut app).is_empty());
    let message = toast(&app).expect("a toast");
    assert!(
        message.contains("8 bit") && message.contains("1 bit"),
        "{message}"
    );

    // Output to output.
    let b_out0 = pin(&app, b, true, 0);
    drag_wire(&mut app, b_out0, from);
    assert!(wire_ends(&mut app).is_empty());
    assert!(toast(&app).unwrap().contains("both outputs"));

    // On empty canvas.
    let b_out0 = pin(&app, b, true, 0);
    drag_wire(&mut app, b_out0, Vec2::new(0.0, -300.0));
    assert!(wire_ends(&mut app).is_empty());
    assert!(toast(&app).unwrap().contains("drop it on an input pin"));
    for part in [bus, y, b] {
        assert!(app.world().get::<Selected>(part).is_none(), "nothing moved");
    }
}

#[test]
fn a_drop_on_an_occupied_input_replaces_its_wire() {
    let mut app = wire_app();
    let a = place(
        &mut app,
        GateKind::Input { w: 1 },
        "A",
        Vec2::new(-200.0, 0.0),
    );
    let b = place(
        &mut app,
        GateKind::Input { w: 1 },
        "B",
        Vec2::new(-200.0, 100.0),
    );
    let y = place(
        &mut app,
        GateKind::Output { w: 1 },
        "Y",
        Vec2::new(200.0, 0.0),
    );
    let a_out0 = pin(&app, a, true, 0);
    let y_in0 = pin(&app, y, false, 0);
    drag_wire(&mut app, a_out0, y_in0);
    let b_out0 = pin(&app, b, true, 0);
    let y_in0 = pin(&app, y, false, 0);
    drag_wire(&mut app, b_out0, y_in0);
    let (ub, uy) = (uuid(&app, b), uuid(&app, y));
    assert_eq!(wire_ends(&mut app), vec![(ub, 0, uy, 0)]);
    assert_eq!(toast(&app).as_deref(), Some("Replaced the wire into Y"));
}

#[test]
fn escape_drops_the_wire_in_hand() {
    let mut app = wire_app();
    let a = place(
        &mut app,
        GateKind::Input { w: 1 },
        "A",
        Vec2::new(-200.0, 0.0),
    );
    let y = place(
        &mut app,
        GateKind::Output { w: 1 },
        "Y",
        Vec2::new(200.0, 0.0),
    );
    let a_out0 = pin(&app, a, true, 0);
    press(&mut app, MouseButton::Left, a_out0);
    move_to(&mut app, Vec2::ZERO);
    assert!(app.world().resource::<CanvasPointer>().wiring());
    app.world_mut()
        .resource_mut::<ButtonInput<KeyCode>>()
        .press(KeyCode::Escape);
    let y_in0 = pin(&app, y, false, 0);
    move_to(&mut app, y_in0);
    app.world_mut()
        .resource_mut::<ButtonInput<KeyCode>>()
        .clear();
    assert!(app.world().resource::<PinPointer>().drag.is_none());
    assert!(!mark_shown(&mut app, DragMark::BandCore));
    let y_in0 = pin(&app, y, false, 0);
    release(&mut app, MouseButton::Left, y_in0);
    assert!(wire_ends(&mut app).is_empty());
    assert_eq!(toast(&app), None);
}

#[test]
fn a_read_only_demo_makes_no_wires_from_a_drag() {
    let mut app = wire_app();
    let a = place(
        &mut app,
        GateKind::Input { w: 1 },
        "A",
        Vec2::new(-200.0, 0.0),
    );
    let y = place(
        &mut app,
        GateKind::Output { w: 1 },
        "Y",
        Vec2::new(200.0, 0.0),
    );
    app.world_mut().resource_mut::<ActiveCircuit>().read_only = true;
    // The pin still names itself on hover.
    let a_out0 = pin(&app, a, true, 0);
    move_to(&mut app, a_out0);
    assert_eq!(hovered(&app), Some((a, true, 0)));
    let a_out0 = pin(&app, a, true, 0);
    let y_in0 = pin(&app, y, false, 0);
    drag_wire(&mut app, a_out0, y_in0);
    assert!(wire_ends(&mut app).is_empty());
    assert!(app.world().resource::<PinPointer>().drag.is_none());
    assert_eq!(position(&app, a), Vec2::new(-200.0, 0.0));
}

#[test]
fn a_click_on_a_pin_is_a_click_on_its_part() {
    let mut app = wire_app();
    let a = place(&mut app, GateKind::Input { w: 1 }, "A", Vec2::ZERO);
    let a_out0 = pin(&app, a, true, 0);
    click(&mut app, a_out0);
    assert_eq!(selected(&app), Some(a));
    assert_eq!(drive(&app, a), 1);
    assert!(wire_ends(&mut app).is_empty());
    assert_eq!(toast(&app), None);
}
