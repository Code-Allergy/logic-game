//! Undo and redo (issue #92): every kind of edit undoes and redoes, a drag
//! is one step, a delete (part and wires) is one step, simulation state
//! carries over, the history clears on load, and the saved marker follows.

use std::collections::HashMap;

use bevy::ecs::entity_disabling::Disabled;

use super::*;
use crate::sim::history::HistoryWorld;
use crate::sim::{
    CircuitLibrary, CircuitView, GateLabelChanged, GateMoved, HistoryCommand, InstanceViewItem,
    OpenInstanceView, RegisterDefinitions, UndoHistory, WorkspaceRevision,
};

fn undo(app: &mut App) {
    send(app, HistoryCommand::Undo);
}

fn redo(app: &mut App) {
    send(app, HistoryCommand::Redo);
}

fn steps(app: &App) -> (usize, usize) {
    let h = app.world().resource::<UndoHistory>();
    (h.undo_len(), h.redo_len())
}

fn labels(app: &mut App) -> Vec<String> {
    let mut q = app.world_mut().query::<&GateLabel>();
    let mut v: Vec<String> = q.iter(app.world()).map(|l| l.value.clone()).collect();
    v.sort();
    v
}

fn has_gate(app: &mut App, label: &str) -> bool {
    labels(app).iter().any(|l| l == label)
}

fn dirty(app: &App) -> bool {
    app.world().resource::<WorkspaceRevision>().is_dirty()
}

/// The open root circuit as a document (for before/after comparisons).
fn root_doc(app: &mut App) -> CircuitDocument {
    app.world_mut()
        .run_system_once(|reader: CircuitReader| reader.root_document())
        .unwrap()
}

/// `A` → NOT `n` → `Y`, with one edit per gate and wire (5 steps).
fn not_circuit(app: &mut App) -> (Entity, Entity, Entity) {
    let a = create_gate(app, GateKind::Input { w: 1 }, "A");
    let n = create_gate(app, GateKind::Not { w: 1 }, "n");
    let y = create_gate(app, GateKind::Output { w: 1 }, "Y");
    create_wire(app, a, 0, n, 0);
    create_wire(app, n, 0, y, 0);
    (a, n, y)
}

#[test]
fn placing_undoes_and_redoes() {
    let mut app = app();
    let a = create_gate(&mut app, GateKind::Input { w: 1 }, "A");
    let id = uuid(&app, a);
    assert_eq!(steps(&app), (1, 0));
    undo(&mut app);
    assert!(labels(&mut app).is_empty());
    assert_eq!(root_node_count(&app), 0, "the design follows");
    assert_eq!(steps(&app), (0, 1));
    redo(&mut app);
    let a = gate(&mut app, "A");
    assert_eq!(uuid(&app, a), id, "the same part comes back");
    assert_eq!(root_node_count(&app), 1);
    assert_eq!(steps(&app), (1, 0));
    // Nothing more to redo: a no-op.
    redo(&mut app);
    assert_eq!(steps(&app), (1, 0));
}

#[test]
fn a_delete_with_its_wires_is_one_step() {
    let mut app = app();
    let (_, n, y) = not_circuit(&mut app);
    run(&mut app, 1);
    assert_eq!(input_value(&app, y, 0), 1);
    let before = root_doc(&mut app);
    let (undo_before, _) = steps(&app);

    send(&mut app, DeleteGate { entity: n });
    assert!(wires(&mut app).is_empty());
    assert_eq!(steps(&app).0, undo_before + 1, "part and wires: one step");

    undo(&mut app);
    assert_eq!(root_doc(&mut app), before, "part and both wires are back");
    assert!(wires(&mut app).iter().all(|(_, _, s)| s.connected));
    run(&mut app, 1);
    assert_eq!(value_at(&mut app, "Y"), 1, "and simulate");

    redo(&mut app);
    assert!(!has_gate(&mut app, "n"));
    assert!(wires(&mut app).is_empty());
}

#[test]
fn removing_a_wire_and_rewiring_undo() {
    let mut app = app();
    let (a, n, y) = not_circuit(&mut app);
    let b = create_gate(&mut app, GateKind::Input { w: 1 }, "B");
    let wired = root_doc(&mut app);

    // Rewire: B replaces A as the driver of the NOT gate's input.
    create_wire(&mut app, b, 0, n, 0);
    let (_, ep, _) = wires(&mut app)
        .into_iter()
        .find(|(_, ep, _)| ep.sink_gate == uuid(&app, n))
        .unwrap();
    assert_eq!(ep.source_gate, uuid(&app, b));
    undo(&mut app);
    assert_eq!(root_doc(&mut app), wired);
    let (_, ep, _) = wires(&mut app)
        .into_iter()
        .find(|(_, ep, _)| ep.sink_gate == uuid(&app, n))
        .unwrap();
    assert_eq!(ep.source_gate, uuid(&app, a), "A drives it again");

    // Remove the wire into Y.
    let (wire, ..) = wires(&mut app)
        .into_iter()
        .find(|(_, ep, _)| ep.sink_gate == uuid(&app, y))
        .unwrap();
    send(&mut app, DeleteWire { entity: wire });
    assert_eq!(wires(&mut app).len(), 1);
    undo(&mut app);
    assert_eq!(root_doc(&mut app), wired);
    run(&mut app, 1);
    assert_eq!(input_value(&app, y, 0), 1, "the restored wire simulates");
}

#[test]
fn label_and_kind_parameters_undo() {
    let mut app = app();
    let (_, n, _) = not_circuit(&mut app);
    let before = root_doc(&mut app);

    let gate_uuid = uuid(&app, n);

    send(
        &mut app,
        GateLabelChanged {
            gate_uuid,
            label: "inverter".into(),
        },
    );
    assert!(has_gate(&mut app, "inverter"));
    undo(&mut app);
    assert!(has_gate(&mut app, "n"));
    assert_eq!(root_doc(&mut app), before);
    redo(&mut app);
    assert!(has_gate(&mut app, "inverter"));
    undo(&mut app);

    // A width change (the wires of the old width no longer connect).
    send(
        &mut app,
        SetGateKind {
            entity: n,
            kind: GateKind::Not { w: 8 },
        },
    );
    assert_eq!(
        app.world().get::<GateType>(n).unwrap().kind,
        GateKind::Not { w: 8 }
    );
    undo(&mut app);
    assert_eq!(
        app.world().get::<GateType>(n).unwrap().kind,
        GateKind::Not { w: 1 },
        "the same entity, its old kind"
    );
    assert_eq!(root_doc(&mut app), before);
    assert!(wires(&mut app).iter().all(|(_, _, s)| s.connected));
}

#[test]
fn a_drag_is_one_step() {
    let mut app = app();
    let (a, ..) = not_circuit(&mut app);
    let (undo_before, _) = steps(&app);
    let start = app.world().get::<Transform>(a).unwrap().translation;

    let mut mouse = ButtonInput::<MouseButton>::default();
    mouse.press(MouseButton::Left);
    app.insert_resource(mouse);
    for i in 1..=10 {
        app.world_mut()
            .get_mut::<Transform>(a)
            .unwrap()
            .translation
            .x = start.x + 10.0 * i as f32;
        app.update();
    }
    assert_eq!(
        steps(&app).0,
        undo_before,
        "nothing while the button is down"
    );
    app.world_mut()
        .resource_mut::<ButtonInput<MouseButton>>()
        .release(MouseButton::Left);
    settle(&mut app);
    assert_eq!(steps(&app).0, undo_before + 1, "the whole drag is one step");

    undo(&mut app);
    assert_eq!(app.world().get::<Transform>(a).unwrap().translation, start);
    redo(&mut app);
    assert_eq!(
        app.world().get::<Transform>(a).unwrap().translation.x,
        start.x + 100.0
    );

    // A move message (as the inspector or a tool sends) is a step too.
    let gate_uuid = uuid(&app, a);
    send(
        &mut app,
        GateMoved {
            gate_uuid,
            position: Vec2::new(-500.0, 40.0),
        },
    );
    undo(&mut app);
    assert_eq!(
        app.world().get::<Transform>(a).unwrap().translation.x,
        start.x + 100.0
    );
}

/// A label typed while the editor holds the history (a text field has the
/// keyboard) is one step, however many keystrokes.
#[test]
fn typing_a_label_is_one_step() {
    let mut app = app();
    let (_, n, _) = not_circuit(&mut app);
    let (undo_before, _) = steps(&app);
    app.world_mut().resource_mut::<UndoHistory>().hold = true;
    for text in ["i", "in", "inv"] {
        let gate_uuid = uuid(&app, n);
        send(
            &mut app,
            GateLabelChanged {
                gate_uuid,
                label: text.into(),
            },
        );
    }
    assert_eq!(steps(&app).0, undo_before);
    app.world_mut().resource_mut::<UndoHistory>().hold = false;
    settle(&mut app);
    assert_eq!(steps(&app).0, undo_before + 1);
    undo(&mut app);
    assert!(has_gate(&mut app, "n"));
}

#[test]
fn a_new_edit_clears_redo() {
    let mut app = app();
    not_circuit(&mut app);
    undo(&mut app);
    undo(&mut app);
    assert_eq!(steps(&app).1, 2);
    create_gate(&mut app, GateKind::Input { w: 1 }, "B");
    assert_eq!(steps(&app).1, 0);
}

#[test]
fn placing_a_chip_undoes_and_redoes() {
    let mut app = app();
    let item = demo("full_adder");
    let (registration, kind) = RegisterDefinitions::chip(&item);
    send(&mut app, registration);
    let chip = create_gate(&mut app, kind.clone(), "FA");
    let a = create_gate(&mut app, GateKind::Input { w: 1 }, "A");
    let s = create_gate(&mut app, GateKind::Output { w: 1 }, "S");
    let (ins, outs) = app.world().resource::<CircuitLibrary>().pin_names(&kind);
    let pin = |names: &[String], name: &str| names.iter().position(|n| n == name).unwrap() as u8;
    create_wire(&mut app, a, 0, chip, pin(&ins, "A"));
    create_wire(&mut app, chip, pin(&outs, "Sum"), s, 0);
    set_input(&mut app, "A", 1);
    run(&mut app, 1);
    assert_eq!(input_value(&app, s, 0), 1);

    // Back to before the chip: its wires and the gates after it go.
    for _ in 0..5 {
        undo(&mut app);
    }
    assert!(labels(&mut app).is_empty());
    assert_eq!(root_node_count(&app), 0);
    for _ in 0..5 {
        redo(&mut app);
    }
    assert!(has_gate(&mut app, "FA"));
    set_input(&mut app, "A", 1);
    run(&mut app, 1);
    assert_eq!(value_at(&mut app, "S"), 1, "it runs again");
}

#[test]
fn undo_keeps_simulation_state() {
    let mut app = app();
    // An 8-bit counter: reg ← reg + 1.
    let reg = create_gate(&mut app, GateKind::Register { w: 8 }, "reg");
    let add = create_gate(&mut app, GateKind::Add { w: 8 }, "add");
    let one = create_gate(&mut app, GateKind::Const { w: 8, value: 1 }, "one");
    let en = create_gate(&mut app, GateKind::Const { w: 1, value: 1 }, "en");
    let n = create_gate(&mut app, GateKind::Output { w: 8 }, "N");
    create_wire(&mut app, en, 0, reg, 0);
    create_wire(&mut app, add, 0, reg, 1);
    create_wire(&mut app, reg, 0, add, 0);
    create_wire(&mut app, one, 0, add, 1);
    create_wire(&mut app, reg, 0, n, 0);
    run(&mut app, 5);
    let count = input_value(&app, n, 0);
    assert!(count >= 4, "it counts ({count})");

    create_gate(&mut app, GateKind::Not { w: 1 }, "spare");
    undo(&mut app);
    assert!(!has_gate(&mut app, "spare"));
    run(&mut app, 1);
    assert_eq!(
        input_value(&app, n, 0),
        count + 1,
        "the register kept its value across the undo"
    );
}

#[test]
fn loading_a_workspace_clears_the_history() {
    let mut app = app();
    not_circuit(&mut app);
    undo(&mut app);
    assert_ne!(steps(&app), (0, 0));
    load(&mut app, demo("half_adder"));
    assert_eq!(steps(&app), (0, 0));
    let loaded = root_doc(&mut app);
    undo(&mut app);
    assert_eq!(root_doc(&mut app), loaded, "nothing to undo after a load");
    redo(&mut app);
    assert_eq!(root_doc(&mut app), loaded);

    // A read-only demo has no history at all.
    app.world_mut().write_message(LoadWorkspaceEvent {
        name: "demo".into(),
        workspace: Arc::new(demo("gates")),
        read_only: true,
        unsaved: false,
    });
    settle(&mut app);
    let first = labels(&mut app)[0].clone();
    let n = gate(&mut app, &first);
    send(&mut app, DeleteGate { entity: n });
    undo(&mut app);
    assert_eq!(steps(&app), (0, 0));
}

#[test]
fn undo_after_save_is_unsaved_and_redo_is_saved_again() {
    let mut app = app();
    not_circuit(&mut app);
    app.world_mut()
        .resource_mut::<WorkspaceRevision>()
        .mark_saved();
    assert!(!dirty(&app));

    undo(&mut app);
    assert!(dirty(&app), "undo after save marks the workspace unsaved");
    redo(&mut app);
    assert!(!dirty(&app), "back at the saved circuit");

    undo(&mut app);
    undo(&mut app);
    redo(&mut app);
    assert!(dirty(&app));
    redo(&mut app);
    assert!(!dirty(&app));

    // A different edit after an undo is not the saved circuit, even though
    // as many edits were made.
    undo(&mut app);
    create_gate(&mut app, GateKind::Input { w: 1 }, "B");
    assert!(dirty(&app));
    // Settled frames after a restore do not mark it dirty again.
    undo(&mut app);
    redo(&mut app);
    undo(&mut app);
    settle(&mut app);
    settle(&mut app);
    assert!(dirty(&app));
}

/// A definition with `A` → NOT → `Y`.
fn inverter_def() -> CircuitDocument {
    let mut d = CircuitDocument::new("Inv");
    let a = d.add(DocNode::labeled(
        GateKind::Input { w: 1 },
        Vec2::new(-100.0, 0.0),
        "A",
    ));
    let n = d.add(DocNode::labeled(GateKind::Not { w: 1 }, Vec2::ZERO, "not"));
    let y = d.add(DocNode::labeled(
        GateKind::Output { w: 1 },
        Vec2::new(100.0, 0.0),
        "Y",
    ));
    d.connect(a, 0, n, 0);
    d.connect(n, 0, y, 0);
    d
}

fn place_inverter(app: &mut App) -> (Uuid, Entity) {
    let def = inverter_def();
    let id = def.id;
    send(
        app,
        RegisterDefinitions {
            definitions: HashMap::from([(id, Arc::new(def))]),
        },
    );
    let chip = create_gate(app, GateKind::SubCircuit { definition: id }, "inv");
    (id, chip)
}

#[test]
fn an_edit_in_a_subcircuit_view_undoes_in_the_view() {
    let mut app = app();
    let (def, chip) = place_inverter(&mut app);
    let original = app.world().resource::<CircuitLibrary>().get(def).cloned();
    send(&mut app, OpenInstanceView { entity: chip });
    assert!(!app.world().resource::<CircuitView>().is_root());

    let extra = create_gate(&mut app, GateKind::Not { w: 1 }, "extra");
    assert!(app.world().get::<InstanceViewItem>(extra).is_some());
    let edited = app.world().resource::<CircuitLibrary>().get(def).cloned();
    assert_ne!(edited, original, "the edit reached the definition");

    undo(&mut app);
    assert!(
        !app.world().resource::<CircuitView>().is_root(),
        "the view stays open"
    );
    assert!(!has_gate(&mut app, "extra"));
    assert!(has_gate(&mut app, "not"), "the view shows the definition");
    assert_eq!(
        app.world().resource::<CircuitLibrary>().get(def).cloned(),
        original
    );
    // The redo comes back in the view as well.
    redo(&mut app);
    assert!(has_gate(&mut app, "extra"));
    assert!(!app.world().resource::<CircuitView>().is_root());
    assert_eq!(steps(&app).1, 0);
}

#[test]
fn undo_that_needs_the_root_closes_the_view() {
    let mut app = app();
    let (_, chip) = place_inverter(&mut app);
    let a = create_gate(&mut app, GateKind::Input { w: 1 }, "In");
    send(&mut app, DeleteGate { entity: a });
    send(&mut app, OpenInstanceView { entity: chip });
    assert!(!app.world().resource::<CircuitView>().is_root());

    // Undoing the delete brings back a root part: back to the root.
    undo(&mut app);
    assert!(app.world().resource::<CircuitView>().is_root());
    let mut q = app
        .world_mut()
        .query_filtered::<Entity, (With<InstanceViewItem>, Allow<Disabled>)>();
    assert_eq!(q.iter(app.world()).count(), 0, "the view items went");
    let mut q = app
        .world_mut()
        .query_filtered::<Entity, (With<GateIdentity>, With<Disabled>)>();
    assert_eq!(q.iter(app.world()).count(), 0, "nothing stays parked");
    assert!(has_gate(&mut app, "In"));
    assert!(has_gate(&mut app, "inv"));
}

/// Snapshot and restore cost on the biggest gallery item. Run with
/// `cargo test --lib undo_cost -- --ignored --nocapture`.
#[test]
#[ignore = "a measurement, not a check"]
fn undo_cost_on_cpu16() {
    let mut app = app();
    load(&mut app, demo("cpu16"));
    let root = root_doc(&mut app);
    let defs = app.world().resource::<CircuitLibrary>().definitions.len();
    println!(
        "cpu16: root {} nodes, {} wires; {defs} definitions in the library",
        root.nodes.len(),
        root.wires.len()
    );
    let reps = 20u32;
    let (snapshot, compare, restore_same) = app
        .world_mut()
        .run_system_once(move |mut world: HistoryWorld| {
            let t = Instant::now();
            let mut s = None;
            for _ in 0..reps {
                s = Some(world.snapshot(0));
            }
            let snapshot = t.elapsed() / reps;
            let s = s.unwrap();
            let other = world.snapshot(0);
            let t = Instant::now();
            for _ in 0..reps {
                assert!(s.same_circuit(&other));
            }
            let compare = t.elapsed() / reps;
            let t = Instant::now();
            world.restore(&s);
            (snapshot, compare, t.elapsed())
        })
        .unwrap();
    println!("snapshot {snapshot:?}, compare {compare:?}, restore (no change) {restore_same:?}");

    // A real step: delete a part, then time the undo frame and the one
    // after (the bridge's resync), against a frame with nothing to do.
    let t = Instant::now();
    app.update();
    let idle = t.elapsed();
    let mut q = app
        .world_mut()
        .query_filtered::<Entity, (With<GateIdentity>, Without<InstanceViewItem>)>();
    let victim = q.iter(app.world()).next().unwrap();
    app.world_mut().write_message(DeleteGate { entity: victim });
    let t = Instant::now();
    app.update();
    let delete_frame = t.elapsed();
    app.update();
    app.world_mut().write_message(HistoryCommand::Undo);
    let t = Instant::now();
    app.update();
    let undo_frame = t.elapsed();
    let t = Instant::now();
    app.update();
    let next_frame = t.elapsed();
    assert_eq!(root_doc(&mut app), root);
    println!(
        "idle frame {idle:?}, delete frame {delete_frame:?}, undo frame {undo_frame:?}, \
         next frame {next_frame:?}"
    );
}
