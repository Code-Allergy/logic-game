//! Mouse input on the canvas ([`canvas_pointer_system`]): click to select
//! and toggle a 1-bit input, left or right drag to move a part, snapped to
//! the grid on drop. The cursor is set in [`CanvasCursor`] directly (there
//! is no window in a headless app).

use bevy::prelude::*;

use super::*;
use crate::editor::EditorState;
use crate::sim::layout::GRID;
use crate::sim::systems::{CanvasCursor, CanvasPointer, DRAG_THRESHOLD, canvas_pointer_system};
use crate::sim::{ActiveCircuit, InputDrive, Selected, SelectedGate};
use crate::temp::GateComponentSelectionEvent;
use crate::{UiPointerState, sim::systems::update_selected_gate};

fn pointer_app() -> App {
    let mut app = app();
    app.init_resource::<UiPointerState>()
        .init_resource::<ButtonInput<MouseButton>>()
        .init_resource::<CanvasCursor>()
        .init_resource::<CanvasPointer>()
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

/// Point at `world` (the window shows the world 1:1, y flipped).
fn point(app: &mut App, world: Vec2) {
    *app.world_mut().resource_mut::<CanvasCursor>() = CanvasCursor {
        screen: Some(Vec2::new(500.0 + world.x, 500.0 - world.y)),
        world: Some(world),
    };
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
    // Past the threshold, it follows the pointer at the grab offset.
    move_to(&mut app, Vec2::new(50.0, 33.0));
    assert_eq!(position(&app, and), Vec2::new(47.0, 31.0));
    assert!(app.world().get::<Selected>(and).is_some(), "drawn on top");
    move_to(&mut app, Vec2::new(70.0, 37.0));
    assert_eq!(position(&app, and), Vec2::new(67.0, 35.0));
    release(&mut app, MouseButton::Left, Vec2::new(70.0, 37.0));
    assert_eq!(position(&app, and), Vec2::new(3.0 * GRID, 2.0 * GRID));
    assert_eq!(selected(&app), Some(and));

    // Dragging an input moves it without toggling it.
    press(&mut app, MouseButton::Left, Vec2::new(-200.0, 0.0));
    move_to(&mut app, Vec2::new(-200.0, -45.0));
    release(&mut app, MouseButton::Left, Vec2::new(-200.0, -45.0));
    assert_eq!(position(&app, a), Vec2::new(-200.0, -40.0));
    assert_eq!(drive(&app, a), 0);
    assert!(app.world().get::<Selected>(and).is_none());
    assert!(app.world().get::<Selected>(a).is_some());
}

#[test]
fn a_right_drag_still_moves_a_part_at_once_and_snaps() {
    let mut app = pointer_app();
    let and = place(&mut app, GateKind::And { w: 1 }, "And", Vec2::new(1.0, 1.0));
    press(&mut app, MouseButton::Right, Vec2::new(1.0, 1.0));
    move_to(&mut app, Vec2::new(4.0, 1.0));
    assert_eq!(position(&app, and), Vec2::new(4.0, 1.0));
    move_to(&mut app, Vec2::new(-29.0, 52.0));
    release(&mut app, MouseButton::Right, Vec2::new(-29.0, 52.0));
    assert_eq!(position(&app, and), Vec2::new(-20.0, 60.0));
    // A right click without a move leaves the part where it is.
    let b = place(&mut app, GateKind::Or { w: 1 }, "Or", Vec2::new(203.0, 7.0));
    press(&mut app, MouseButton::Right, Vec2::new(203.0, 7.0));
    release(&mut app, MouseButton::Right, Vec2::new(203.0, 7.0));
    assert_eq!(position(&app, b), Vec2::new(203.0, 7.0));
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
