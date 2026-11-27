//! Subcircuit workflow (issue #94): make chip from a selection (behaviour
//! kept, checked against the reference simulator; a port for every
//! crossing wire), editing a definition in place (all instances, ports
//! followed by name, dropped wires reported), definition identity (content
//! hash, revision, merges that never replace silently), the refresh of a
//! stale chip, and every new operation as one undo step.

use std::collections::{HashMap, HashSet};

use logi_sim::reference::Reference;

use super::*;
use crate::sim::chip::stays_outside;
use crate::sim::{
    AuthoringFeedback, CircuitLibrary, CircuitView, CloseView, DocWire, HistoryCommand, Incoming,
    LoadedRoot, MakeChip, MultiSelection, OpenInstanceView, PendingChipUpdates, RefreshDefinitions,
    RegisterDefinitions, UndoHistory, WorkspaceRevision, plan_chip,
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

fn ids_labelled(doc: &CircuitDocument, labels: &[&str]) -> HashSet<Uuid> {
    doc.nodes
        .iter()
        .filter(|n| labels.contains(&n.label.as_deref().unwrap_or("")))
        .map(|n| n.id)
        .collect()
}

/// Every node that can go into a chip.
fn all_logic(doc: &CircuitDocument) -> HashSet<Uuid> {
    doc.nodes
        .iter()
        .filter(|n| !stays_outside(&n.kind))
        .map(|n| n.id)
        .collect()
}

/// `ws` with a chip made of `selection` in its root.
fn with_chip(ws: &WorkspaceDocument, selection: &HashSet<Uuid>) -> WorkspaceDocument {
    let library = CircuitLibrary {
        definitions: ws.definitions.clone(),
    };
    let plan = plan_chip(&ws.root, selection, "Chip", &library).expect("a chip");
    let mut out = ws.clone();
    out.root = plan.apply(&ws.root);
    out.define(plan.definition);
    out
}

/// A small deterministic generator for input stimuli.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 17
    }
}

/// Run both workspaces on the reference simulator with the same random
/// inputs for `ticks` ticks; every output (ports and displays) must match
/// at every tick. Returns the number of output values compared.
fn same_behaviour(before: &WorkspaceDocument, after: &WorkspaceDocument, ticks: u64) -> usize {
    let design_a = crate::sim::netlist::design_of(before).expect("design before");
    let design_b = crate::sim::netlist::design_of(after).expect("design after");
    let mut ra = Reference::new(&design_a).expect("reference before");
    let mut rb = Reference::new(&design_b).expect("reference after");
    let inputs = design_a.root_def().inputs.len();
    assert_eq!(inputs, design_b.root_def().inputs.len(), "same input ports");
    assert_eq!(ra.outputs().len(), rb.outputs().len(), "same output ports");
    assert_eq!(ra.outputs(), rb.outputs(), "same outputs at reset");
    let mut rng = Lcg(0x5eed);
    let mut compared = 0;
    for tick in 0..ticks {
        for port in 0..inputs {
            let v = rng.next();
            ra.set_input(port, v);
            rb.set_input(port, v);
        }
        ra.tick().expect("tick");
        rb.tick().expect("tick");
        assert_eq!(ra.outputs(), rb.outputs(), "outputs differ at tick {tick}");
        compared += ra.outputs().len();
    }
    compared
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

/// Load `doc` as an editable workspace and let the history take its
/// baseline.
fn open_bench(app: &mut App, doc: CircuitDocument) {
    load(app, WorkspaceDocument::new(doc));
    settle(app);
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
// Make chip: the plan
// ─────────────────────────────────────────────────────────────────────────────

/// Combinational and stateful logic moved into a chip behaves exactly as
/// before, tick by tick, on the reference simulator.
#[test]
fn make_chip_preserves_behaviour() {
    let ws = WorkspaceDocument::new(adder_circuit());
    // The whole adder and counter (ports selected too: they stay outside).
    let everything: HashSet<Uuid> = ws.root.nodes.iter().map(|n| n.id).collect();
    let after = with_chip(&ws, &everything);
    assert!(same_behaviour(&ws, &after, 64) > 0);
    // Only part of it: the boundary cuts through the adder.
    let part = ids_labelled(&ws.root, &["s", "carry", "reg"]);
    let mut part = part;
    part.extend(
        ws.root
            .nodes
            .iter()
            .filter(|n| matches!(n.kind, GateKind::Xor { .. }) && n.label.is_none())
            .map(|n| n.id),
    );
    let after = with_chip(&ws, &part);
    same_behaviour(&ws, &after, 64);
    // A chip around that chip and the remaining AND gates.
    let outer: HashSet<Uuid> = after
        .root
        .nodes
        .iter()
        .filter(|n| matches!(n.kind, GateKind::And { .. } | GateKind::SubCircuit { .. }))
        .map(|n| n.id)
        .collect();
    assert_eq!(outer.len(), 3);
    let nested = with_chip(&after, &outer);
    same_behaviour(&ws, &nested, 64);
}

/// Every demo: its root's logic made into a chip (all of it, and every
/// other part) behaves the same on the reference simulator.
#[test]
fn make_chip_preserves_the_demos() {
    let mut checked = 0;
    for demo in demos().all() {
        let ws = (*demo.workspace).clone();
        let Ok(design) = crate::sim::netlist::design_of(&ws) else {
            continue;
        };
        if Reference::new(&design).is_err() {
            continue;
        }
        let logic = all_logic(&ws.root);
        if logic.is_empty() {
            continue;
        }
        let after = with_chip(&ws, &logic);
        same_behaviour(&ws, &after, 40);
        let mut ids: Vec<Uuid> = logic.into_iter().collect();
        ids.sort();
        let half: HashSet<Uuid> = ids.into_iter().step_by(2).collect();
        let after = with_chip(&ws, &half);
        same_behaviour(&ws, &after, 40);
        checked += 1;
    }
    assert!(checked >= 5, "only {checked} demos checked");
}

/// Every wire that crosses the selection boundary becomes a port: the
/// instance takes its place, on the pin of a port wired inside to the
/// same pin it was on.
#[test]
fn ports_are_created_for_every_crossing_wire() {
    let ws = WorkspaceDocument::new(adder_circuit());
    let root = &ws.root;
    let selection = ids_labelled(root, &["s", "carry"]);
    let plan = plan_chip(root, &selection, "Chip", &CircuitLibrary::default()).unwrap();
    let def = &plan.definition;
    let (ins, outs) = def.interface();
    let instance = plan.instance.id;
    let crossing: Vec<&DocWire> = root
        .wires
        .iter()
        .filter(|w| selection.contains(&w.from.node) != selection.contains(&w.to.node))
        .collect();
    assert!(!crossing.is_empty());
    for w in crossing {
        assert!(plan.removed_wires.contains(&w.id));
        if selection.contains(&w.to.node) {
            // Into the chip: outside driver → instance pin p; inside, port p
            // drives the old sink.
            let new = plan
                .new_wires
                .iter()
                .find(|n| n.from == w.from && n.to.node == instance)
                .unwrap_or_else(|| panic!("no instance wire for {w:?}"));
            let port = ins[usize::from(new.to.pin)];
            assert!(
                def.wires
                    .iter()
                    .any(|i| i.from.node == port.id && i.to == w.to),
                "port {:?} does not reach the sink",
                port.label
            );
        } else {
            let new = plan
                .new_wires
                .iter()
                .find(|n| n.to == w.to && n.from.node == instance)
                .unwrap_or_else(|| panic!("no instance wire for {w:?}"));
            let port = outs[usize::from(new.from.pin)];
            assert!(
                def.wires
                    .iter()
                    .any(|i| i.to.node == port.id && i.from == w.from)
            );
        }
    }
    // Wires from one outside driver share a port: x1 drives both the
    // selected XOR `s` and the unselected AND, A and Cin drive one each.
    let names: Vec<&str> = ins.iter().map(|n| n.label.as_deref().unwrap()).collect();
    let outs: Vec<&str> = outs.iter().map(|n| n.label.as_deref().unwrap()).collect();
    // Unlabelled drivers fall back to the sink pin's name (`a`, `b`).
    assert_eq!(names.len(), 4, "{names:?}");
    assert!(names.contains(&"Cin"), "{names:?}");
    // Output ports are named after their inside driver.
    assert_eq!(outs, vec!["carry", "s"]);
}

/// Port names: the source's label, the pin name for a part with several
/// outputs, fallbacks, and duplicates made unique.
#[test]
fn port_names_follow_the_sources_and_are_unique() {
    let mut d = CircuitDocument::new("names");
    let at = Vec2::new;
    let a1 = d.add(DocNode::labeled(
        GateKind::Input { w: 4 },
        at(-200.0, 0.0),
        "x",
    ));
    let a2 = d.add(DocNode::labeled(
        GateKind::Input { w: 4 },
        at(-200.0, -60.0),
        "x",
    ));
    let free = d.add(DocNode::new(
        GateKind::Const { w: 4, value: 3 },
        at(-200.0, -120.0),
    ));
    let add = d.add(DocNode::labeled(
        GateKind::Add { w: 4 },
        at(-100.0, 0.0),
        "alu",
    ));
    let not = d.add(DocNode::new(GateKind::Not { w: 4 }, at(0.0, 0.0)));
    let sum = d.add(DocNode::labeled(
        GateKind::Output { w: 4 },
        at(200.0, 0.0),
        "Total",
    ));
    let carry = d.add(DocNode::labeled(
        GateKind::Output { w: 1 },
        at(200.0, -60.0),
        "C",
    ));
    d.connect(a1, 0, add, 0);
    d.connect(a2, 0, add, 1);
    d.connect(free, 0, not, 0);
    d.connect(add, 1, carry, 0);
    d.connect(not, 0, sum, 0);
    // The Add's cin is left open.
    let sel = [add, not].into_iter().collect();
    let plan = plan_chip(&d, &sel, "  Named ", &CircuitLibrary::default()).unwrap();
    assert_eq!(plan.definition.name, "Named");
    let (ins, outs) = plan.definition.interface();
    let mut names: Vec<&str> = ins.iter().map(|n| n.label.as_deref().unwrap()).collect();
    names.sort();
    // Two inputs both labelled `x`, an unlabelled Const (the NOT's pin).
    assert_eq!(names, vec!["in", "x", "x 2"]);
    let mut outs: Vec<&str> = outs.iter().map(|n| n.label.as_deref().unwrap()).collect();
    outs.sort();
    // `alu.cout` (several outputs), the unlabelled NOT → its sink `Total`.
    assert_eq!(outs, vec!["Total", "alu.cout"]);
    assert!(plan_chip(&d, &sel, " ", &CircuitLibrary::default()).is_err());
    let ports_only = [sum, carry].into_iter().collect();
    assert!(plan_chip(&d, &ports_only, "P", &CircuitLibrary::default()).is_err());
}

/// A selection whose chip would need more ports on a side than a wire can
/// name is refused: port 256 used to be wired to pin 0.
#[test]
fn a_chip_with_more_ports_than_a_wire_can_name_is_refused() {
    let build = |gates: usize| {
        let mut d = CircuitDocument::new("Wide");
        let at = Vec2::new;
        let mut sel = HashSet::new();
        for i in 0..gates {
            let y = i as f32 * 40.0;
            let input = d.add(DocNode::new(GateKind::Input { w: 1 }, at(-200.0, y)));
            let not = d.add(DocNode::new(GateKind::Not { w: 1 }, at(0.0, y)));
            let out = d.add(DocNode::new(GateKind::Output { w: 1 }, at(200.0, y)));
            d.connect(input, 0, not, 0);
            d.connect(not, 0, out, 0);
            sel.insert(not);
        }
        (d, sel)
    };
    let (d, sel) = build(256);
    let plan = plan_chip(&d, &sel, "Wide", &CircuitLibrary::default()).expect("256 fit");
    let mut pins: Vec<u8> = plan
        .new_wires
        .iter()
        .filter(|w| w.to.node == plan.instance.id)
        .map(|w| w.to.pin)
        .collect();
    pins.sort_unstable();
    assert_eq!(pins, (0..=255).collect::<Vec<u8>>());
    let (d, sel) = build(257);
    let e = plan_chip(&d, &sel, "Wide", &CircuitLibrary::default()).unwrap_err();
    assert!(e.contains("257 inputs") && e.contains("at most 256"), "{e}");
}

// ─────────────────────────────────────────────────────────────────────────────
// Make chip in the editor
// ─────────────────────────────────────────────────────────────────────────────

/// `MakeChip` replaces the parts with one instance wired the same way: the
/// circuit simulates the same, the definition is in the library (and in a
/// save), and the whole change is one undo step.
#[test]
fn make_chip_in_the_editor_is_one_undo_step() {
    let mut app = app();
    let doc = adder_circuit();
    open_bench(&mut app, doc.clone());
    let inputs: [(&str, u64); 3] = [("A", 1), ("B", 0), ("Cin", 1)];
    let before_values = eval(&mut app, &inputs, &["Sum", "Cout"]);
    let before = root_doc(&mut app);
    let steps_before = steps(&app);

    let gates: Vec<Uuid> = doc.nodes.iter().map(|n| n.id).collect();
    send(
        &mut app,
        MakeChip {
            gates,
            name: "Full adder".into(),
        },
    );
    settle(&mut app);
    let chips = chip_instances(&mut app);
    assert_eq!(chips.len(), 1);
    let (chip, def) = chips[0];
    assert_eq!(library(&app).get(def).unwrap().name, "Full adder");
    assert_eq!(steps(&app).0, steps_before.0 + 1, "one step");
    assert!(feedback(&app).contains("Made chip"), "{}", feedback(&app));
    assert!(wires(&mut app).iter().all(|(_, _, s)| s.connected));
    // Ports and the display stay; the logic is gone from the root.
    let root = root_doc(&mut app);
    assert_eq!(
        root.nodes.len(),
        4 + 3 + 1 + 1,
        "inputs, outputs, display, chip"
    );
    assert!(root.nodes.iter().any(|n| n.id == uuid(&app, chip)));
    let after_values = eval(&mut app, &inputs, &["Sum", "Cout"]);
    assert_eq!(after_values, before_values);
    set_input(&mut app, "En", 1);
    run(&mut app, 1);
    let count = value_at(&mut app, "Count");
    run(&mut app, 3);
    assert_eq!(
        value_at(&mut app, "Count"),
        count + 3,
        "the counter runs inside"
    );
    // Saved with the workspace.
    let ws = app
        .world_mut()
        .run_system_once(|reader: CircuitReader| reader.workspace())
        .unwrap();
    assert!(ws.definitions.contains_key(&def));

    // Undo: the parts are back, as they were.
    send(&mut app, HistoryCommand::Undo);
    assert!(chip_instances(&mut app).is_empty());
    assert!(
        root_doc(&mut app).same_content(&before),
        "undo restores the circuit"
    );
    assert_eq!(eval(&mut app, &inputs, &["Sum", "Cout"]), before_values);
    send(&mut app, HistoryCommand::Redo);
    assert_eq!(chip_instances(&mut app).len(), 1);
    assert_eq!(eval(&mut app, &inputs, &["Sum", "Cout"]), before_values);
}

/// In a read-only demo, Make chip is refused and nothing changes.
#[test]
fn make_chip_is_refused_in_a_read_only_demo() {
    let mut app = app();
    let ws = demo("full_adder");
    let before = ws.root.clone();
    app.world_mut().write_message(LoadWorkspaceEvent {
        name: "demo".into(),
        workspace: Arc::new(ws),
        read_only: true,
        unsaved: false,
    });
    settle(&mut app);
    send(
        &mut app,
        MakeChip {
            gates: before.nodes.iter().map(|n| n.id).collect(),
            name: "X".into(),
        },
    );
    assert!(chip_instances(&mut app).len() == before.instanced_definitions().count());
    assert!(feedback(&app).contains("read-only"), "{}", feedback(&app));
}

/// Inside an open definition, Make chip nests a new chip in it: every
/// instance of the outer definition gets it, and it still simulates.
#[test]
fn make_chip_inside_a_definition_nests_it() {
    let mut app = app();
    let mut cell = adder_circuit();
    cell.name = "Cell".into();
    // The display is left out of definitions; drop it.
    cell.nodes.retain(|n| n.label.as_deref() != Some("Shown"));
    cell.wires
        .retain(|w| cell.nodes.iter().any(|n| n.id == w.to.node));
    let cell_id = cell.id;
    let mut root = CircuitDocument::new("Top");
    let ports = ["A", "B", "Cin", "En"];
    let mut top_inputs = Vec::new();
    for (i, p) in ports.iter().enumerate() {
        top_inputs.push(root.add(DocNode::labeled(
            GateKind::Input { w: 1 },
            Vec2::new(-300.0, -60.0 * i as f32),
            *p,
        )));
    }
    let inst1 = root.add(DocNode::new(
        GateKind::SubCircuit {
            definition: cell_id,
        },
        Vec2::ZERO,
    ));
    let inst2 = root.add(DocNode::new(
        GateKind::SubCircuit {
            definition: cell_id,
        },
        Vec2::new(0.0, -300.0),
    ));
    let s1 = root.add(DocNode::labeled(
        GateKind::Output { w: 1 },
        Vec2::new(300.0, 0.0),
        "S1",
    ));
    let s2 = root.add(DocNode::labeled(
        GateKind::Output { w: 1 },
        Vec2::new(300.0, -300.0),
        "S2",
    ));
    // Cell ports: inputs A, B, Cin, En (sorted); outputs Count, Cout, Sum.
    for inst in [inst1, inst2] {
        for (pin, input) in top_inputs.iter().enumerate() {
            root.connect(*input, 0, inst, pin as u8);
        }
    }
    root.connect(inst1, 2, s1, 0);
    root.connect(inst2, 2, s2, 0);
    let mut ws = WorkspaceDocument::new(root);
    ws.define(cell.clone());
    load(&mut app, ws);
    settle(&mut app);
    let first = eval(&mut app, &[("A", 1), ("B", 1), ("Cin", 1)], &["S1", "S2"]);
    assert_eq!(first, vec![1, 1]);

    let e1 = chip_instances(&mut app)
        .into_iter()
        .find(|(_, d)| *d == cell_id)
        .unwrap()
        .0;
    send(&mut app, OpenInstanceView { entity: e1 });
    let steps_before = steps(&app).0;
    let inner = ids_labelled(&cell, &["s", "carry"]);
    send(
        &mut app,
        MakeChip {
            gates: inner.into_iter().collect(),
            name: "Inner".into(),
        },
    );
    settle(&mut app);
    assert_eq!(steps(&app).0, steps_before + 1, "one step");
    let lib = library(&app);
    let cell_now = lib.get(cell_id).unwrap();
    let nested: Vec<Uuid> = cell_now.instanced_definitions().collect();
    assert_eq!(nested.len(), 1, "the cell instances the new chip");
    assert_eq!(lib.get(nested[0]).unwrap().name, "Inner");
    assert!(cell_now.revision > cell.revision, "the edit counts");
    send(&mut app, CloseView { to_root: true });
    let again = eval(&mut app, &[("A", 1), ("B", 0), ("Cin", 1)], &["S1", "S2"]);
    assert_eq!(again, vec![0, 0], "both instances run the nested chip");
    let again = eval(&mut app, &[("A", 1), ("B", 0), ("Cin", 0)], &["S1", "S2"]);
    assert_eq!(again, vec![1, 1]);
}

/// Deleting several parts in one frame (a multi-selection) is one step.
#[test]
fn deleting_a_selection_is_one_undo_step() {
    let mut app = app();
    open_bench(&mut app, adder_circuit());
    let before = steps(&app).0;
    let doomed: Vec<Entity> = ["s", "carry", "reg"]
        .iter()
        .map(|l| gate(&mut app, l))
        .collect();
    for entity in doomed {
        app.world_mut().write_message(DeleteGate { entity });
    }
    settle(&mut app);
    assert_eq!(steps(&app).0, before + 1);
    send(&mut app, HistoryCommand::Undo);
    for l in ["s", "carry", "reg"] {
        gate(&mut app, l);
    }
}

#[test]
fn multi_selection_toggles_and_keeps_a_primary() {
    let mut world = World::new();
    let [a, b, c] = [(); 3].map(|_| world.spawn_empty().id());
    let mut m = MultiSelection::default();
    // Shift+click on b with a in the inspector: a group of a and b.
    assert_eq!(m.toggle(b, Some(a)), Some(b));
    assert_eq!(m.entities(), &[a, b]);
    assert_eq!(m.toggle(c, Some(b)), Some(c));
    // Taking the primary out makes the last one primary.
    assert_eq!(m.toggle(c, Some(c)), Some(b));
    // Taking another one out keeps the primary.
    assert_eq!(m.toggle(a, Some(b)), Some(b));
    assert_eq!(m.entities(), &[b]);
    assert_eq!(m.all(Some(b)), vec![b]);
    m.clear();
    assert_eq!(m.all(Some(a)), vec![a]);
    assert!(m.all(None).is_empty());
}

#[test]
fn box_select_takes_parts_whose_centre_is_inside() {
    use crate::sim::multi_select::{BoxSelect, parts_in_box};
    let mut world = World::new();
    let [a, b, c] = [(); 3].map(|_| world.spawn_empty().id());
    let parts = [
        (a, Vec2::new(0.0, 0.0)),
        (b, Vec2::new(50.0, 40.0)),
        (c, Vec2::new(500.0, 0.0)),
    ];
    let rect = BoxSelect {
        start: Some(Vec2::new(100.0, -20.0)),
        current: Some(Vec2::new(-20.0, 60.0)),
    }
    .rect()
    .unwrap();
    assert_eq!(parts_in_box(rect, parts.into_iter()), vec![b, a]);
    let click = BoxSelect {
        start: Some(Vec2::ONE),
        current: Some(Vec2::ONE),
    };
    assert_eq!(click.rect(), None, "a click is not a box");
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
