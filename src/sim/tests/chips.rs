//! Subcircuit workflow (issue #94): make chip from a selection (behaviour
//! kept, checked against the reference simulator; a port for every
//! crossing wire), editing a definition in place (all instances, ports
//! followed by name, dropped wires reported), definition identity (content
//! hash, revision, merges that never replace silently), the refresh of a
//! stale chip, and every new operation as one undo step.

use std::collections::HashMap;

use super::*;
use crate::sim::{
    AuthoringFeedback, CircuitLibrary, CircuitView, CloseView, HistoryCommand, Incoming,
    LoadedRoot, OpenInstanceView, PendingChipUpdates, RefreshDefinitions, RegisterDefinitions,
    UndoHistory, WorkspaceRevision,
};

// ─────────────────────────────────────────────────────────────────────────────
// Helpers
// ─────────────────────────────────────────────────────────────────────────────

/// A full adder at the root: `A`, `B`, `Cin` → `Sum`, `Cout`, plus a
/// 4-bit counter `Count` enabled by `En` (state across the boundary), and
/// a `Display` reading the counter.
fn adder_circuit() -> CircuitDocument {
    let mut d = CircuitDocument::new("Adder bench");
    let at = Vec2::new;
    let a = d.add(DocNode::labeled(
        GateKind::Input { w: 1 },
        at(-300.0, 100.0),
        "A",
    ));
    let b = d.add(DocNode::labeled(
        GateKind::Input { w: 1 },
        at(-300.0, 0.0),
        "B",
    ));
    let cin = d.add(DocNode::labeled(
        GateKind::Input { w: 1 },
        at(-300.0, -100.0),
        "Cin",
    ));
    let en = d.add(DocNode::labeled(
        GateKind::Input { w: 1 },
        at(-300.0, -200.0),
        "En",
    ));
    let x1 = d.add(DocNode::new(GateKind::Xor { w: 1 }, at(-100.0, 80.0)));
    let x2 = d.add(DocNode::labeled(GateKind::Xor { w: 1 }, at(0.0, 80.0), "s"));
    let a1 = d.add(DocNode::new(GateKind::And { w: 1 }, at(-100.0, -20.0)));
    let a2 = d.add(DocNode::new(GateKind::And { w: 1 }, at(0.0, -20.0)));
    let or = d.add(DocNode::labeled(
        GateKind::Or { w: 1 },
        at(100.0, -20.0),
        "carry",
    ));
    let reg = d.add(DocNode::labeled(
        GateKind::Register { w: 4 },
        at(0.0, -200.0),
        "reg",
    ));
    let add = d.add(DocNode::new(GateKind::Add { w: 4 }, at(-100.0, -260.0)));
    let one = d.add(DocNode::labeled(
        GateKind::Const { w: 4, value: 1 },
        at(-200.0, -300.0),
        "one",
    ));
    let zero = d.add(DocNode::new(
        GateKind::Const { w: 1, value: 0 },
        at(-200.0, -340.0),
    ));
    let sum = d.add(DocNode::labeled(
        GateKind::Output { w: 1 },
        at(300.0, 80.0),
        "Sum",
    ));
    let cout = d.add(DocNode::labeled(
        GateKind::Output { w: 1 },
        at(300.0, -20.0),
        "Cout",
    ));
    let count = d.add(DocNode::labeled(
        GateKind::Output { w: 4 },
        at(300.0, -200.0),
        "Count",
    ));
    let shown = d.add(DocNode::labeled(
        GateKind::Display { w: 4 },
        at(300.0, -300.0),
        "Shown",
    ));
    d.connect(a, 0, x1, 0);
    d.connect(b, 0, x1, 1);
    d.connect(x1, 0, x2, 0);
    d.connect(cin, 0, x2, 1);
    d.connect(a, 0, a1, 0);
    d.connect(b, 0, a1, 1);
    d.connect(x1, 0, a2, 0);
    d.connect(cin, 0, a2, 1);
    d.connect(a1, 0, or, 0);
    d.connect(a2, 0, or, 1);
    d.connect(x2, 0, sum, 0);
    d.connect(or, 0, cout, 0);
    // Register: d, en → q. Add: a, b, cin → sum, cout.
    d.connect(reg, 0, add, 0);
    d.connect(one, 0, add, 1);
    d.connect(zero, 0, add, 2);
    d.connect(en, 0, reg, 0);
    d.connect(add, 0, reg, 1);
    d.connect(reg, 0, count, 0);
    d.connect(reg, 0, shown, 0);
    d
}

fn steps(app: &App) -> (usize, usize) {
    let h = app.world().resource::<UndoHistory>();
    (h.undo_len(), h.redo_len())
}

fn root_doc(app: &mut App) -> CircuitDocument {
    app.world_mut()
        .run_system_once(|reader: CircuitReader| reader.root_document())
        .unwrap()
}

fn feedback(app: &App) -> String {
    app.world()
        .resource::<AuthoringFeedback>()
        .message
        .clone()
        .unwrap_or_default()
}

fn library(app: &App) -> &CircuitLibrary {
    app.world().resource::<CircuitLibrary>()
}

fn chip_instances(app: &mut App) -> Vec<(Entity, Uuid)> {
    let mut q = app.world_mut().query::<(Entity, &GateType)>();
    q.iter(app.world())
        .filter_map(|(e, t)| match t.kind {
            GateKind::SubCircuit { definition } => Some((e, definition)),
            _ => None,
        })
        .collect()
}

// ─────────────────────────────────────────────────────────────────────────────
// Identity: hash, revision, merge
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn content_hash_ignores_order_and_revision_but_not_content() {
    let d = adder_circuit();
    let mut shuffled = d.clone();
    shuffled.nodes.reverse();
    shuffled.wires.reverse();
    shuffled.revision = 9;
    assert_eq!(d.content_hash(), shuffled.content_hash());
    assert!(d.same_content(&shuffled));
    let mut moved = d.clone();
    moved.nodes[0].x += 10.0;
    assert_ne!(d.content_hash(), moved.content_hash());
    assert!(!d.same_content(&moved));
    let mut relabelled = d.clone();
    relabelled.nodes[0].label = Some("Z".into());
    assert_ne!(d.content_hash(), relabelled.content_hash());
    // Properties are a map: their order never matters.
    let mut p1 = d.clone();
    let mut p2 = d.clone();
    for (k, v) in [("a", "1"), ("b", "2"), ("c", "3")] {
        p1.nodes[0].properties.insert(k.into(), v.into());
    }
    for (k, v) in [("c", "3"), ("a", "1"), ("b", "2")] {
        p2.nodes[0].properties.insert(k.into(), v.into());
    }
    assert_eq!(p1.content_hash(), p2.content_hash());
}

/// A merge adds new ids and reports, but never replaces, a definition
/// with different content; how it compares follows the revisions.
#[test]
fn a_merge_never_replaces_different_content() {
    let mut def = adder_circuit();
    def.revision = 2;
    let id = def.id;
    let mut library = CircuitLibrary::default();
    let one = |d: &CircuitDocument| HashMap::from([(d.id, Arc::new(d.clone()))]);
    let report = library.merge(&one(&def));
    assert_eq!(report.added, vec![id]);
    assert!(
        library.merge(&one(&def)).conflicts.is_empty(),
        "same content"
    );
    let mut same_but_counted = def.clone();
    same_but_counted.revision = 7;
    assert!(library.merge(&one(&same_but_counted)).conflicts.is_empty());

    let mut newer = def.clone();
    newer.nodes[0].x += 20.0;
    newer.revision = 3;
    let mut diverged = newer.clone();
    diverged.revision = 2;
    let mut older = newer.clone();
    older.revision = 1;
    for (offered, how) in [
        (&newer, Incoming::Newer),
        (&diverged, Incoming::Diverged),
        (&older, Incoming::Older),
    ] {
        let report = library.merge(&one(offered));
        assert_eq!(report.conflicts, vec![(id, how)]);
        assert!(report.added.is_empty());
        assert!(library.get(id).unwrap().same_content(&def), "not replaced");
    }
}

/// The saved root carries a revision: the loaded one while unchanged, one
/// more once edited; it is saved in the file.
#[test]
fn a_saved_workspace_root_counts_its_revisions() {
    let dir = temp_dir();
    let mut app = app();
    let mut doc = adder_circuit();
    doc.revision = 4;
    load(&mut app, WorkspaceDocument::new(doc));
    assert_eq!(app.world().resource::<LoadedRoot>().revision, 4);
    assert_eq!(root_doc(&mut app).revision, 4, "unchanged");
    let s = gate(&mut app, "s");
    let gate_uuid = uuid(&app, s);
    send(
        &mut app,
        crate::sim::GateMoved {
            gate_uuid,
            position: Vec2::new(40.0, 40.0),
        },
    );
    assert_eq!(root_doc(&mut app).revision, 5, "edited");
    let ws = app
        .world_mut()
        .run_system_once(|reader: CircuitReader| reader.workspace())
        .unwrap();
    save_workspace_in(&dir, "bench", &ws).unwrap();
    let back = read_workspace_in(&dir, "bench").unwrap();
    assert_eq!(back.root.revision, 5, "saved in the file");
    std::fs::remove_dir_all(dir).ok();
}

// ─────────────────────────────────────────────────────────────────────────────
// Stale chips: placing offers a refresh
// ─────────────────────────────────────────────────────────────────────────────

/// A chip from a saved workspace, placed twice: `Sum` of a half adder.
fn place_chip_twice(app: &mut App, chip: &CircuitDocument) -> [Entity; 2] {
    let mut root = CircuitDocument::new("Bench");
    let a = root.add(DocNode::labeled(
        GateKind::Input { w: 1 },
        Vec2::new(-200.0, 0.0),
        "A",
    ));
    let b = root.add(DocNode::labeled(
        GateKind::Input { w: 1 },
        Vec2::new(-200.0, -60.0),
        "B",
    ));
    let i1 = root.add(DocNode::new(
        GateKind::SubCircuit {
            definition: chip.id,
        },
        Vec2::ZERO,
    ));
    let i2 = root.add(DocNode::new(
        GateKind::SubCircuit {
            definition: chip.id,
        },
        Vec2::new(0.0, -200.0),
    ));
    let y1 = root.add(DocNode::labeled(
        GateKind::Output { w: 1 },
        Vec2::new(200.0, 0.0),
        "Y1",
    ));
    let y2 = root.add(DocNode::labeled(
        GateKind::Output { w: 1 },
        Vec2::new(200.0, -200.0),
        "Y2",
    ));
    let c1 = root.add(DocNode::labeled(
        GateKind::Output { w: 1 },
        Vec2::new(200.0, -60.0),
        "C1",
    ));
    for i in [i1, i2] {
        root.connect(a, 0, i, 0);
        root.connect(b, 0, i, 1);
    }
    // Chip outputs: Carry (0), Sum (1).
    root.connect(i1, 1, y1, 0);
    root.connect(i2, 1, y2, 0);
    root.connect(i1, 0, c1, 0);
    let mut ws = WorkspaceDocument::new(root);
    ws.define(chip.clone());
    load(app, ws);
    settle(app);
    let chips = chip_instances(app);
    [chips[0].0, chips[1].0]
}

/// A half adder chip: inputs A, B; outputs Carry, Sum.
fn half_adder_chip() -> CircuitDocument {
    let mut d = CircuitDocument::new("Half adder");
    d.revision = 1;
    let a = d.add(DocNode::labeled(
        GateKind::Input { w: 1 },
        Vec2::new(-100.0, 0.0),
        "A",
    ));
    let b = d.add(DocNode::labeled(
        GateKind::Input { w: 1 },
        Vec2::new(-100.0, -60.0),
        "B",
    ));
    let x = d.add(DocNode::labeled(GateKind::Xor { w: 1 }, Vec2::ZERO, "x"));
    let n = d.add(DocNode::labeled(
        GateKind::And { w: 1 },
        Vec2::new(0.0, -60.0),
        "n",
    ));
    let s = d.add(DocNode::labeled(
        GateKind::Output { w: 1 },
        Vec2::new(100.0, 0.0),
        "Sum",
    ));
    let c = d.add(DocNode::labeled(
        GateKind::Output { w: 1 },
        Vec2::new(100.0, -60.0),
        "Carry",
    ));
    d.connect(a, 0, x, 0);
    d.connect(b, 0, x, 1);
    d.connect(a, 0, n, 0);
    d.connect(b, 0, n, 1);
    d.connect(x, 0, s, 0);
    d.connect(n, 0, c, 0);
    d
}

/// Placing a chip whose saved version is newer does not swap it silently:
/// the library keeps this workspace's copy and a refresh is offered.
/// Refreshing updates every instance (one undo step); wires stay on
/// ports of the same name, wires to a port that is gone are reported.
#[test]
fn a_stale_chip_is_detected_and_refreshed() {
    let mut app = app();
    let chip = half_adder_chip();
    place_chip_twice(&mut app, &chip);
    assert_eq!(
        eval(&mut app, &[("A", 1), ("B", 1)], &["Y1", "Y2", "C1"]),
        vec![0, 0, 1]
    );

    // The saved version: Sum becomes XNOR, the Carry port is deleted and a
    // new `Sum` port node replaces the old one (same name, new id).
    let mut newer = chip.clone();
    newer.revision = 2;
    for n in &mut newer.nodes {
        if n.label.as_deref() == Some("x") {
            n.kind = GateKind::Xnor { w: 1 };
        }
    }
    let old_sum = newer
        .nodes
        .iter()
        .find(|n| n.label.as_deref() == Some("Sum"))
        .unwrap()
        .id;
    let carry = newer
        .nodes
        .iter()
        .find(|n| n.label.as_deref() == Some("Carry"))
        .unwrap()
        .id;
    let new_sum = DocNode::labeled(GateKind::Output { w: 1 }, Vec2::new(120.0, 0.0), "Sum");
    let new_sum_id = new_sum.id;
    newer.nodes.retain(|n| n.id != old_sum && n.id != carry);
    newer.nodes.push(new_sum);
    for w in &mut newer.wires {
        if w.to.node == old_sum {
            w.to.node = new_sum_id;
        }
    }
    newer.wires.retain(|w| w.to.node != carry);

    let offered = HashMap::from([(chip.id, Arc::new(newer.clone()))]);
    send(
        &mut app,
        RegisterDefinitions {
            definitions: offered.clone(),
        },
    );
    assert!(
        library(&app).get(chip.id).unwrap().same_content(&chip),
        "not swapped silently"
    );
    let pending = app.world().resource::<PendingChipUpdates>();
    assert_eq!(pending.updates.len(), 1);
    let change = &pending.updates[0].changed[0];
    assert_eq!(
        (change.have, change.offered, change.how),
        (1, 2, Incoming::Newer)
    );
    // Asked again: still one question.
    send(
        &mut app,
        RegisterDefinitions {
            definitions: offered.clone(),
        },
    );
    assert_eq!(
        app.world().resource::<PendingChipUpdates>().updates.len(),
        1
    );

    let before = steps(&app).0;
    let wires_before = wires(&mut app).len();
    send(
        &mut app,
        RefreshDefinitions {
            definitions: offered,
        },
    );
    settle(&mut app);
    assert_eq!(steps(&app).0, before + 1, "one undo step");
    assert!(library(&app).get(chip.id).unwrap().same_content(&newer));
    let message = feedback(&app);
    assert!(message.contains("Refreshed"), "{message}");
    assert!(
        message.contains("\"Carry\""),
        "dropped wires reported: {message}"
    );
    assert_eq!(wires(&mut app).len(), wires_before - 1, "the Carry wire");
    // Both instances changed; Sum followed by name to the new port.
    assert_eq!(
        eval(&mut app, &[("A", 1), ("B", 1)], &["Y1", "Y2"]),
        vec![1, 1]
    );
    assert_eq!(
        eval(&mut app, &[("A", 1), ("B", 0)], &["Y1", "Y2"]),
        vec![0, 0]
    );
    assert!(wires(&mut app).iter().all(|(_, _, s)| s.connected));

    send(&mut app, HistoryCommand::Undo);
    assert!(library(&app).get(chip.id).unwrap().same_content(&chip));
    assert_eq!(wires(&mut app).len(), wires_before);
    assert_eq!(
        eval(&mut app, &[("A", 1), ("B", 1)], &["Y1", "Y2", "C1"]),
        vec![0, 0, 1]
    );
}

/// An older saved version is ignored with a note; no question is asked.
#[test]
fn an_older_saved_chip_keeps_the_workspace_copy() {
    let mut app = app();
    let mut chip = half_adder_chip();
    chip.revision = 5;
    place_chip_twice(&mut app, &chip);
    let mut older = chip.clone();
    older.revision = 3;
    older.nodes[0].x -= 40.0;
    send(
        &mut app,
        RegisterDefinitions {
            definitions: HashMap::from([(chip.id, Arc::new(older))]),
        },
    );
    assert!(
        app.world()
            .resource::<PendingChipUpdates>()
            .updates
            .is_empty()
    );
    assert!(library(&app).get(chip.id).unwrap().same_content(&chip));
    assert!(feedback(&app).contains("newer"), "{}", feedback(&app));
}

/// Editing inside a chip updates every instance; a new port renumbers the
/// instances' pins and their wires follow; a port that is gone drops its
/// wires, and the editor says so.
#[test]
fn editing_a_definition_follows_ports_by_name_and_reports_dropped_wires() {
    let mut app = app();
    let chip = half_adder_chip();
    let [i1, _] = place_chip_twice(&mut app, &chip);
    send(&mut app, OpenInstanceView { entity: i1 });
    assert!(!app.world().resource::<CircuitView>().is_root());
    let revision_before = library(&app).get(chip.id).unwrap().revision;

    // A new output port `Aux` sorts before `Carry` and `Sum`: the
    // instances' wires move to the new pin numbers.
    send(
        &mut app,
        CreateGate {
            kind: GateKind::Output { w: 1 },
            position: Vec2::new(150.0, 60.0),
            label: Some("Aux".into()),
        },
    );
    let (x, aux) = (gate(&mut app, "x"), gate(&mut app, "Aux"));
    create_wire(&mut app, x, 0, aux, 0);
    assert!(library(&app).get(chip.id).unwrap().revision > revision_before);
    // Delete the Carry port: its wire in the root goes, and is reported.
    let carry = gate(&mut app, "Carry");
    send(&mut app, DeleteGate { entity: carry });
    let message = feedback(&app);
    assert!(message.contains("\"Carry\""), "{message}");
    send(&mut app, CloseView { to_root: true });
    let c1 = gate(&mut app, "C1");
    let c1 = uuid(&app, c1);
    assert!(
        !wires(&mut app).iter().any(|(_, ep, _)| ep.sink_gate == c1),
        "the wire from the removed Carry port is gone"
    );
    // Both instances: Y still reads Sum (now pin 2).
    assert_eq!(
        eval(&mut app, &[("A", 1), ("B", 0)], &["Y1", "Y2"]),
        vec![1, 1]
    );
    assert_eq!(
        eval(&mut app, &[("A", 1), ("B", 1)], &["Y1", "Y2"]),
        vec![0, 0]
    );
}

/// Loading a workspace takes its definitions as they are and clears any
/// pending question.
#[test]
fn loading_takes_the_files_definitions() {
    let mut app = app();
    let chip = half_adder_chip();
    place_chip_twice(&mut app, &chip);
    let mut newer = chip.clone();
    newer.revision = 2;
    newer.nodes[0].x += 10.0;
    send(
        &mut app,
        RegisterDefinitions {
            definitions: HashMap::from([(chip.id, Arc::new(newer.clone()))]),
        },
    );
    assert_eq!(
        app.world().resource::<PendingChipUpdates>().updates.len(),
        1
    );
    let mut root = CircuitDocument::new("Other");
    root.add(DocNode::new(
        GateKind::SubCircuit {
            definition: chip.id,
        },
        Vec2::ZERO,
    ));
    let mut ws = WorkspaceDocument::new(root);
    ws.define(newer.clone());
    load(&mut app, ws);
    assert!(library(&app).get(chip.id).unwrap().same_content(&newer));
    assert!(
        app.world()
            .resource::<PendingChipUpdates>()
            .updates
            .is_empty()
    );
    assert!(!app.world().resource::<WorkspaceRevision>().is_dirty());
}

/// Ports are followed by id, then by name and width: a port node replaced
/// by a new one of the same name keeps its pin's wires; a port with no
/// match is dropped.
#[test]
fn pin_remap_follows_ids_then_names() {
    use crate::sim::view::PinRemap;
    let old = half_adder_chip();
    let mut new = old.clone();
    // Replace the `Sum` node (new id, same name), drop `Carry`, add `Aux`.
    let sum = new
        .nodes
        .iter()
        .position(|n| n.label.as_deref() == Some("Sum"))
        .unwrap();
    new.nodes[sum].id = Uuid::new_v4();
    new.nodes.retain(|n| n.label.as_deref() != Some("Carry"));
    new.add(DocNode::labeled(
        GateKind::Output { w: 1 },
        Vec2::ZERO,
        "Aux",
    ));
    let remap = PinRemap::between(&old, &new).expect("pins move");
    // Outputs before: Carry (0), Sum (1); after: Aux (0), Sum (1).
    assert_eq!(remap.inputs, vec![Some(0), Some(1)]);
    assert_eq!(remap.outputs, vec![None, Some(1)]);
    // A name match needs the same width.
    let mut wide = new.clone();
    let sum = wide
        .nodes
        .iter()
        .position(|n| n.label.as_deref() == Some("Sum"))
        .unwrap();
    wide.nodes[sum].kind = GateKind::Output { w: 8 };
    assert_eq!(
        PinRemap::between(&old, &wide).unwrap().outputs,
        vec![None, None]
    );
}
