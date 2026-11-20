//! Change-driven visuals and the combinational-loop UX (issue #30): idle
//! frames do no per-entity work, drags move only the dragged gate's wires
//! and never recompile, displays and bus labels re-format only on value
//! changes, and loop errors highlight the gates and wires on the loop (also
//! inside an instance view) until a compile succeeds.

use std::collections::{HashMap, HashSet};

use super::*;
use crate::sim::bridge::loop_message;
use crate::sim::visuals::{DisplayedValue, LoopOutline, WireValueLabel};
use crate::sim::{
    CircuitLibrary, CloseView, EntityWorkCounter, GateMoved, GateProbes, InstanceViewItem,
    LogicDisplay, LoopHighlight, OpenInstanceView, RegisterDefinitions, SimulationVisualsPlugin,
};

// ─────────────────────────────────────────────────────────────────────────────
// Harness
// ─────────────────────────────────────────────────────────────────────────────

/// Entities whose components changed, collected in `Last` every frame
/// until [`clear_changes`].
#[derive(Resource, Default, Debug)]
struct Changes {
    sprites: HashSet<Entity>,
    transforms: HashSet<Entity>,
    texts: HashSet<Entity>,
    values: HashSet<Entity>,
    wire_states: HashSet<Entity>,
    probes: HashSet<Entity>,
}

impl Changes {
    fn total(&self) -> usize {
        self.sprites.len()
            + self.transforms.len()
            + self.texts.len()
            + self.values.len()
            + self.wire_states.len()
            + self.probes.len()
    }
}

#[allow(clippy::too_many_arguments)]
fn record_changes(
    mut changes: ResMut<Changes>,
    sprites: Query<Entity, Changed<Sprite>>,
    transforms: Query<Entity, Changed<Transform>>,
    texts: Query<Entity, Changed<Text2d>>,
    values: Query<Entity, Changed<GateValues>>,
    wire_states: Query<Entity, Changed<WireState>>,
    probes: Query<Entity, Changed<GateProbes>>,
) {
    let c = changes.bypass_change_detection();
    c.sprites.extend(sprites.iter());
    c.transforms.extend(transforms.iter());
    c.texts.extend(texts.iter());
    c.values.extend(values.iter());
    c.wire_states.extend(wire_states.iter());
    c.probes.extend(probes.iter());
}

fn visual_app_with(runtime: SimRuntime) -> App {
    let mut app = app_with(runtime);
    app.add_plugins(SimulationVisualsPlugin)
        .init_resource::<EntityWorkCounter>()
        .init_resource::<Changes>()
        .add_systems(Last, record_changes);
    app
}

fn visual_app() -> App {
    visual_app_with(SimRuntime::Threadless)
}

fn clear_changes(app: &mut App) {
    *app.world_mut().resource_mut::<Changes>() = Changes::default();
}

fn changes(app: &App) -> &Changes {
    app.world().resource::<Changes>()
}

/// Changed texts with component `C` (display texts or bus labels).
fn changed_texts<C: Component>(app: &App) -> HashSet<Entity> {
    changes(app)
        .texts
        .iter()
        .copied()
        .filter(|e| app.world().get::<C>(*e).is_some())
        .collect()
}

fn work(app: &App) -> u64 {
    app.world().resource::<EntityWorkCounter>().get()
}

fn text_of_display(app: &mut App, gate: Entity) -> (Entity, String) {
    let children: Vec<Entity> = app.world().get::<Children>(gate).unwrap().iter().collect();
    let text = children
        .into_iter()
        .find(|c| app.world().get::<LogicDisplay>(*c).is_some())
        .expect("display text");
    (text, app.world().get::<Text2d>(text).unwrap().0.clone())
}

fn wire_into(app: &mut App, sink: Entity) -> Entity {
    let sink = uuid(app, sink);
    wires(app)
        .into_iter()
        .find(|(_, ep, _)| ep.sink_gate == sink)
        .map(|(e, ..)| e)
        .unwrap()
}

fn wire_between(app: &mut App, source: Entity, sink: Entity) -> Entity {
    let (source, sink) = (uuid(app, source), uuid(app, sink));
    wires(app)
        .into_iter()
        .find(|(_, ep, _)| ep.source_gate == source && ep.sink_gate == sink)
        .map(|(e, ..)| e)
        .unwrap()
}

fn wire_label(app: &App, wire: Entity) -> Option<(Entity, String)> {
    let children = app.world().get::<Children>(wire)?;
    children
        .iter()
        .find(|c| app.world().get::<WireValueLabel>(*c).is_some())
        .map(|c| (c, app.world().get::<Text2d>(c).unwrap().0.clone()))
}

fn highlighted(app: &App, entity: Entity) -> bool {
    app.world().get::<LoopHighlight>(entity).is_some()
}

fn highlighted_count(app: &mut App) -> usize {
    let mut q = app
        .world_mut()
        .query_filtered::<Entity, With<LoopHighlight>>();
    q.iter(app.world()).count()
}

fn outline_count(app: &mut App) -> usize {
    let mut q = app
        .world_mut()
        .query_filtered::<Entity, With<LoopOutline>>();
    q.iter(app.world()).count()
}

fn compile_messages(app: &App) -> Vec<String> {
    app.world()
        .resource::<SimDiagnostics>()
        .compile_error
        .as_ref()
        .map(|e| e.messages.clone())
        .unwrap_or_default()
}

fn idle_frames(app: &mut App, frames: usize) {
    for _ in 0..frames {
        app.update();
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Acceptance: paused and idle means no per-entity work
// ─────────────────────────────────────────────────────────────────────────────

fn assert_idle_does_no_work(app: &mut App, what: &str) {
    idle_frames(app, 5);
    let before = work(app);
    clear_changes(app);
    idle_frames(app, 30);
    assert_eq!(
        work(app),
        before,
        "{what}: per-entity work while paused and idle"
    );
    assert_eq!(
        changes(app).total(),
        0,
        "{what}: components written while idle: {:?}",
        changes(app)
    );
}

#[test]
fn paused_idle_circuit_does_no_per_entity_work() {
    let mut app = visual_app();
    load(&mut app, demo("counter"));
    assert!(
        !app.world().resource::<SimBridge>().is_running(),
        "the bridge starts paused"
    );
    set_input(&mut app, "Enable", 1);
    run(&mut app, 12);
    assert!(value_at(&mut app, "Count") > 0, "the counter ran");
    // The visuals exist, so the idle check below is meaningful.
    let display = gate(&mut app, "Count");
    assert_ne!(text_of_display(&mut app, display).1, "");
    assert!(
        wires(&mut app)
            .iter()
            .all(|(e, ..)| app.world().get::<Sprite>(*e).is_some())
    );

    assert_idle_does_no_work(&mut app, "root view");

    // Running a tick does work (the counter is live) and goes idle again.
    let before = work(&app);
    run(&mut app, 1);
    assert!(
        work(&app) > before,
        "a tick projects probes and updates visuals"
    );
    assert_idle_does_no_work(&mut app, "after a tick");
}

#[test]
fn paused_idle_instance_view_and_threaded_engine_do_no_per_entity_work() {
    for runtime in [SimRuntime::Threadless, SimRuntime::Threaded] {
        let mut app = visual_app_with(runtime);
        load(&mut app, demo("counter"));
        app.world_mut()
            .resource_mut::<SimBridge>()
            .wait_until_current(Duration::from_secs(5));
        assert_idle_does_no_work(&mut app, &format!("{runtime:?} root"));

        let mut q = app.world_mut().query::<(Entity, &GateType)>();
        let instance = q
            .iter(app.world())
            .find(|(_, t)| matches!(t.kind, GateKind::SubCircuit { .. }))
            .map(|(e, _)| e)
            .expect("an instance");
        send(&mut app, OpenInstanceView { entity: instance });
        app.world_mut()
            .resource_mut::<SimBridge>()
            .wait_until_current(Duration::from_secs(5));
        assert_idle_does_no_work(&mut app, &format!("{runtime:?} instance view"));
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Dragging
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn dragging_a_gate_moves_only_its_wires_and_does_not_recompile() {
    let mut app = visual_app();
    let a = create_gate(&mut app, GateKind::Input { w: 1 }, "A");
    let b = create_gate(&mut app, GateKind::Input { w: 1 }, "B");
    let and = create_gate(&mut app, GateKind::And { w: 1 }, "and");
    let y = create_gate(&mut app, GateKind::Output { w: 1 }, "Y");
    let z = create_gate(&mut app, GateKind::Output { w: 1 }, "Z");
    create_wire(&mut app, a, 0, and, 0);
    create_wire(&mut app, b, 0, and, 1);
    create_wire(&mut app, and, 0, y, 0);
    create_wire(&mut app, b, 0, z, 0);
    idle_frames(&mut app, 3);
    let revision = app.world().resource::<SimBridge>().revision();
    let running = app.world().resource::<SimDiagnostics>().running_revision;
    let into_y = wire_into(&mut app, y);
    let into_z = wire_into(&mut app, z);

    // Drag `and` over several frames, as `drag_selected_system` does.
    clear_changes(&mut app);
    for step in 1..=5 {
        app.world_mut()
            .get_mut::<Transform>(and)
            .unwrap()
            .translation
            .x = 40.0 * step as f32;
        app.update();
    }
    assert_eq!(
        app.world().resource::<SimBridge>().revision(),
        revision,
        "no new design revision"
    );
    assert_eq!(
        app.world().resource::<SimDiagnostics>().running_revision,
        running,
        "no recompile"
    );
    let moved: HashSet<Entity> = changes(&app).transforms.iter().copied().collect();
    let and_wires = [
        wire_between(&mut app, a, and),
        wire_between(&mut app, b, and),
        into_y,
    ];
    for wire in and_wires {
        assert!(
            moved.contains(&wire),
            "a wire of the dragged gate follows it"
        );
    }
    assert!(!moved.contains(&into_z), "an unrelated wire is not touched");
    assert_eq!(
        moved,
        HashSet::from([and, and_wires[0], and_wires[1], and_wires[2]])
    );
    assert!(changes(&app).values.is_empty() && changes(&app).probes.is_empty());

    // The wire sits between its gates.
    let to = app.world().get::<Transform>(y).unwrap().translation;
    let from = app.world().get::<Transform>(and).unwrap().translation;
    let wire = app.world().get::<Transform>(into_y).unwrap().translation;
    assert_eq!(wire.truncate(), ((from + to) / 2.0).truncate());
    let length = app
        .world()
        .get::<Sprite>(into_y)
        .unwrap()
        .custom_size
        .unwrap()
        .x;
    assert!((length - (to - from).length()).abs() < 1e-3);

    // A move command is a drag too.
    let gate_uuid = uuid(&app, a);
    send(
        &mut app,
        GateMoved {
            gate_uuid,
            position: Vec2::new(-300.0, 10.0),
        },
    );
    assert_eq!(app.world().resource::<SimBridge>().revision(), revision);
}

#[test]
fn dragging_in_an_instance_view_updates_the_definition_without_recompiling() {
    let mut def = CircuitDocument::new("Inv");
    let i = def.add(DocNode::labeled(GateKind::Input { w: 1 }, Vec2::ZERO, "A"));
    let n = def.add(DocNode::labeled(
        GateKind::Not { w: 1 },
        Vec2::new(100.0, 0.0),
        "inv",
    ));
    let o = def.add(DocNode::labeled(
        GateKind::Output { w: 1 },
        Vec2::new(200.0, 0.0),
        "Y",
    ));
    def.connect(i, 0, n, 0);
    def.connect(n, 0, o, 0);
    let def_id = def.id;
    let mut app = visual_app();
    send(
        &mut app,
        RegisterDefinitions {
            definitions: HashMap::from([(def_id, Arc::new(def))]),
        },
    );
    let cell = create_gate(
        &mut app,
        GateKind::SubCircuit { definition: def_id },
        "cell",
    );
    idle_frames(&mut app, 3);
    send(&mut app, OpenInstanceView { entity: cell });
    let revision = app.world().resource::<SimBridge>().revision();
    let running = app.world().resource::<SimDiagnostics>().running_revision;

    let inv = gate(&mut app, "inv");
    for step in 1..=4 {
        app.world_mut()
            .get_mut::<Transform>(inv)
            .unwrap()
            .translation
            .y = 25.0 * step as f32;
        app.update();
    }
    settle(&mut app);
    assert_eq!(
        app.world().resource::<SimBridge>().revision(),
        revision,
        "no new design revision"
    );
    assert_eq!(
        app.world().resource::<SimDiagnostics>().running_revision,
        running,
        "no recompile"
    );
    let stored = app
        .world()
        .resource::<CircuitLibrary>()
        .get(def_id)
        .unwrap()
        .node(n)
        .unwrap()
        .y;
    assert_eq!(stored, 100.0, "the move is written back to the definition");
}

// ─────────────────────────────────────────────────────────────────────────────
// Displays and bus labels
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn display_text_changes_only_when_the_value_changes() {
    let mut app = visual_app();
    let a = create_gate(&mut app, GateKind::Input { w: 8 }, "A");
    let display = create_gate(&mut app, GateKind::Display { w: 8 }, "D");
    create_gate(&mut app, GateKind::Input { w: 1 }, "B");
    create_wire(&mut app, a, 0, display, 0);
    run(&mut app, 1);
    let (text, shown) = text_of_display(&mut app, display);
    assert_eq!(shown, "0x00");

    set_input(&mut app, "A", 0x2A);
    clear_changes(&mut app);
    run(&mut app, 1);
    assert_eq!(text_of_display(&mut app, display).1, "0x2A");
    assert_eq!(
        changed_texts::<LogicDisplay>(&app),
        HashSet::from([text]),
        "re-formatted once"
    );

    // Ticks, edits and moves that leave the value alone keep the text.
    clear_changes(&mut app);
    run(&mut app, 3);
    set_input(&mut app, "B", 1);
    run(&mut app, 1);
    app.world_mut()
        .get_mut::<Transform>(display)
        .unwrap()
        .translation
        .x = 50.0;
    idle_frames(&mut app, 2);
    // A GateValues write with the same value (e.g. a probe rebind).
    app.world_mut()
        .get_mut::<GateValues>(display)
        .unwrap()
        .set_changed();
    idle_frames(&mut app, 2);
    assert!(
        changed_texts::<LogicDisplay>(&app).is_empty(),
        "text written without a value change"
    );
    assert_eq!(
        app.world()
            .get::<DisplayedValue>(text)
            .unwrap()
            .0
            .map(|v| v.0),
        Some(0x2A)
    );

    // The format is part of what is shown.
    let gate_uuid = uuid(&app, display);
    send(
        &mut app,
        crate::sim::DisplayConfigChanged {
            gate_uuid,
            radix: crate::sim::DisplayRadix::Decimal,
            leading_zeroes: false,
        },
    );
    assert_eq!(text_of_display(&mut app, display).1, "42");
}

#[test]
fn multi_bit_wires_are_thicker_and_show_their_value() {
    let mut app = visual_app();
    let a = create_gate(&mut app, GateKind::Input { w: 12 }, "A");
    let y = create_gate(&mut app, GateKind::Output { w: 12 }, "Y");
    let b = create_gate(&mut app, GateKind::Input { w: 1 }, "B");
    let z = create_gate(&mut app, GateKind::Output { w: 1 }, "Z");
    create_wire(&mut app, a, 0, y, 0);
    create_wire(&mut app, b, 0, z, 0);
    run(&mut app, 1);
    let (bus, bit) = (wire_into(&mut app, y), wire_into(&mut app, z));
    let thickness =
        |app: &App, e: Entity| app.world().get::<Sprite>(e).unwrap().custom_size.unwrap().y;
    assert!(thickness(&app, bus) > thickness(&app, bit));
    assert_ne!(
        app.world().get::<Sprite>(bus).unwrap().color,
        app.world().get::<Sprite>(bit).unwrap().color
    );
    let (label, text) = wire_label(&app, bus).expect("bus label");
    assert_eq!(text, "0x000");
    assert!(wire_label(&app, bit).is_none(), "1-bit wires have no label");

    set_input(&mut app, "A", 0xABC);
    clear_changes(&mut app);
    run(&mut app, 1);
    assert_eq!(wire_label(&app, bus).unwrap().1, "0xABC");
    assert_eq!(
        changed_texts::<WireValueLabel>(&app),
        HashSet::from([label])
    );

    clear_changes(&mut app);
    set_input(&mut app, "B", 1);
    run(&mut app, 2);
    assert!(
        changed_texts::<WireValueLabel>(&app).is_empty(),
        "the bus label is not rewritten"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Combinational loops
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn loop_message_names_the_gates_and_the_state_elements() {
    let one = loop_message(&["NOT \"n\"".to_string()]);
    assert_eq!(
        one,
        "Combinational loop through 1 gate: NOT \"n\". A feedback loop needs a state element: \
         put a Delay (or a Register, DLatch, SrLatch or Ram) on the loop."
    );
    let many: Vec<String> = (0..10).map(|i| format!("And \"g{i}\"")).collect();
    let text = loop_message(&many);
    assert!(
        text.starts_with("Combinational loop through 10 gates: And \"g0\", "),
        "{text}"
    );
    assert!(text.contains("And \"g7\" and 2 more."), "{text}");
    assert!(!text.contains("g8"), "{text}");
}

#[test]
fn loop_highlights_its_gates_and_wires_and_clears_when_fixed() {
    let mut app = visual_app();
    let a = create_gate(&mut app, GateKind::Input { w: 1 }, "A");
    let y = create_gate(&mut app, GateKind::Output { w: 1 }, "Y");
    create_wire(&mut app, a, 0, y, 0);
    let x = create_gate(&mut app, GateKind::Xor { w: 1 }, "x");
    let n = create_gate(&mut app, GateKind::Not { w: 1 }, "n");
    create_wire(&mut app, a, 0, x, 0);
    create_wire(&mut app, x, 0, n, 0);
    assert_eq!(highlighted_count(&mut app), 0);
    create_wire(&mut app, n, 0, x, 1);

    let messages = compile_messages(&app);
    assert_eq!(messages.len(), 1);
    let message = &messages[0];
    assert!(message.contains("2 gates"), "{message}");
    assert!(
        message.contains("NOT \"n\"") && message.contains("XOR \"x\""),
        "{message}"
    );
    assert!(message.contains("Delay"), "{message}");
    assert!(
        message.contains("Register, DLatch, SrLatch or Ram"),
        "{message}"
    );

    let (xn, nx, ax, ay) = (
        wire_between(&mut app, x, n),
        wire_between(&mut app, n, x),
        wire_between(&mut app, a, x),
        wire_between(&mut app, a, y),
    );
    for e in [x, n, xn, nx] {
        assert!(highlighted(&app, e), "on the loop");
    }
    for e in [a, y, ax, ay] {
        assert!(!highlighted(&app, e), "not on the loop");
    }
    assert_eq!(outline_count(&mut app), 2, "one outline per loop gate");
    let loop_colour = crate::sim::visuals::LOOP_COLOR;
    assert_eq!(app.world().get::<Sprite>(nx).unwrap().color, loop_colour);
    assert_ne!(app.world().get::<Sprite>(ax).unwrap().color, loop_colour);

    // The previous program keeps running meanwhile.
    set_input(&mut app, "A", 1);
    run(&mut app, 1);
    assert_eq!(input_value(&app, y, 0), 1);

    // Breaking the loop compiles and clears the highlight.
    send(&mut app, DeleteWire { entity: nx });
    assert!(
        app.world()
            .resource::<SimDiagnostics>()
            .compile_error
            .is_none()
    );
    assert_eq!(highlighted_count(&mut app), 0);
    assert_eq!(outline_count(&mut app), 0);
    assert_ne!(
        app.world().get::<Sprite>(xn).unwrap().color,
        loop_colour,
        "wire colour restored"
    );
}

/// `Loopy`: `A -> xor -> not -> Y`, with `not` fed back into `xor`.
fn loopy_definition() -> CircuitDocument {
    let mut d = CircuitDocument::new("Loopy");
    let a = d.add(DocNode::labeled(
        GateKind::Input { w: 1 },
        Vec2::new(-100.0, 0.0),
        "A",
    ));
    let x = d.add(DocNode::labeled(GateKind::Xor { w: 1 }, Vec2::ZERO, "x"));
    let n = d.add(DocNode::labeled(
        GateKind::Not { w: 1 },
        Vec2::new(100.0, 0.0),
        "inv",
    ));
    let y = d.add(DocNode::labeled(
        GateKind::Output { w: 1 },
        Vec2::new(200.0, 0.0),
        "Y",
    ));
    d.connect(a, 0, x, 0);
    d.connect(x, 0, n, 0);
    d.connect(n, 0, x, 1);
    d.connect(n, 0, y, 0);
    d
}

#[test]
fn loop_inside_a_definition_is_highlighted_per_instance_and_in_its_view() {
    let def = loopy_definition();
    let def_id = def.id;
    let mut app = visual_app();
    send(
        &mut app,
        RegisterDefinitions {
            definitions: HashMap::from([(def_id, Arc::new(def))]),
        },
    );
    let l1 = create_gate(&mut app, GateKind::SubCircuit { definition: def_id }, "L1");
    let l2 = create_gate(&mut app, GateKind::SubCircuit { definition: def_id }, "L2");
    let a = create_gate(&mut app, GateKind::Input { w: 1 }, "A");
    create_wire(&mut app, a, 0, l1, 0);

    // Reported once per instance; the root shows the instance gates.
    let messages = compile_messages(&app);
    assert_eq!(messages.len(), 2, "{messages:?}");
    assert!(
        messages
            .iter()
            .any(|m| m.contains("L1 / NOT \"inv\"") && m.contains("L1 / XOR \"x\""))
    );
    assert!(messages.iter().any(|m| m.contains("L2 / NOT \"inv\"")));
    assert!(messages.iter().all(|m| m.contains("Delay")));
    assert!(highlighted(&app, l1) && highlighted(&app, l2));
    assert!(!highlighted(&app, a));
    let into_l1 = wire_into(&mut app, l1);
    assert!(
        !highlighted(&app, into_l1),
        "the input wire is not on the loop"
    );

    // Inside L1's view: the loop gates and wires of that instance.
    send(&mut app, OpenInstanceView { entity: l1 });
    let (x, n, va, vy) = (
        gate(&mut app, "x"),
        gate(&mut app, "inv"),
        gate(&mut app, "A"),
        gate(&mut app, "Y"),
    );
    assert!(app.world().get::<InstanceViewItem>(x).is_some());
    assert!(highlighted(&app, x) && highlighted(&app, n));
    assert!(!highlighted(&app, va) && !highlighted(&app, vy));
    let (xn, nx, ax, ny) = (
        wire_between(&mut app, x, n),
        wire_between(&mut app, n, x),
        wire_between(&mut app, va, x),
        wire_between(&mut app, n, vy),
    );
    assert!(highlighted(&app, xn) && highlighted(&app, nx));
    assert!(!highlighted(&app, ax) && !highlighted(&app, ny));
    assert_eq!(
        highlighted_count(&mut app),
        4,
        "parked root gates are not counted"
    );

    // Fix the definition in the view: both instances compile.
    send(&mut app, DeleteWire { entity: nx });
    settle(&mut app);
    assert!(
        compile_messages(&app).is_empty(),
        "{:?}",
        compile_messages(&app)
    );
    assert_eq!(highlighted_count(&mut app), 0);
    send(&mut app, CloseView { to_root: true });
    assert!(
        !highlighted(&app, l1) && !highlighted(&app, l2),
        "root highlight cleared too"
    );
    assert_eq!(outline_count(&mut app), 0);
}
