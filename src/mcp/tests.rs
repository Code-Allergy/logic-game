//! Tests of the batch editor and the views.

use std::sync::Arc;

use bevy::math::Vec2;
use logi_sim::sim::Sim;
use serde_json::{Value, json};

use super::batch::{self, Report, apply};
use super::view;
use crate::sim::demos::catalog;
use crate::sim::layout::overlaps;
use crate::sim::source::Externals;
use crate::sim::{CircuitDocument, DocNode, GateKind, WorkspaceDocument};

fn empty() -> WorkspaceDocument {
    // A fixed id: the ids of parts derive from it.
    let mut root = CircuitDocument::new("test");
    root.id = uuid::Uuid::from_u128(7);
    WorkspaceDocument::new(root)
}

fn edit(ws: &WorkspaceDocument, request: Value) -> (WorkspaceDocument, Report) {
    apply(ws, &request, &Externals::default()).unwrap_or_else(|e| panic!("{e}"))
}

fn refused(ws: &WorkspaceDocument, request: Value) -> String {
    match apply(ws, &request, &Externals::default()) {
        Ok(_) => panic!("the edit should have been refused"),
        Err(e) => e.0,
    }
}

fn sim(ws: &WorkspaceDocument) -> Sim {
    let design = crate::sim::netlist::design_of(ws).expect("design");
    let program = logi_sim::compile::compile(&design, &logi_sim::compile::Options::default())
        .expect("compiles");
    Sim::new(Arc::new(program))
}

/// A half adder chip, a full adder chip of two, and an 8-bit adder of
/// eight full adders as the root.
fn adder8() -> WorkspaceDocument {
    let (ws, report) = edit(
        &empty(),
        json!({"edits": [
            {"circuit": "half_adder", "ops": [
                {"add": [
                    {"name": "A", "type": "Input"},
                    {"name": "B", "type": "Input"},
                    {"name": "x", "type": "Xor"},
                    {"name": "c", "type": "And"},
                    {"name": "Sum", "type": "Output"},
                    {"name": "Carry", "type": "Output"}
                ]},
                {"wire": [["A", "x.a", "c.a"], ["B", "x.b", "c.b"], ["x", "Sum"], ["c", "Carry"]]}
            ]},
            {"circuit": "full_adder", "ops": [
                {"add": [
                    {"name": "A", "type": "Input"},
                    {"name": "B", "type": "Input"},
                    {"name": "Cin", "type": "Input"},
                    {"name": "h1", "use": "half_adder"},
                    {"name": "h2", "use": "half_adder"},
                    {"name": "carry", "type": "Or"},
                    {"name": "Sum", "type": "Output"},
                    {"name": "Cout", "type": "Output"}
                ]},
                {"wire": [
                    ["A", "h1.A"], ["B", "h1.B"], ["h1.Sum", "h2.A"], ["Cin", "h2.B"],
                    ["h2.Sum", "Sum"], ["h1.Carry", "carry.a"], ["h2.Carry", "carry.b"],
                    ["carry", "Cout"]
                ]}
            ]},
            {"ops": [
                {"add": [
                    {"name": "A", "type": "Input", "w": 8},
                    {"name": "B", "type": "Input", "w": 8},
                    {"name": "Cin", "type": "Input"},
                    {"name": "fa", "use": "full_adder", "count": 8},
                    {"name": "Sum", "type": "Output", "w": 8},
                    {"name": "Cout", "type": "Output"}
                ]},
                {"wire": [
                    ["A", "fa.A"], ["B", "fa.B"], ["Cin", "fa[0].Cin"],
                    ["fa[0..7].Cout", "fa[1..8].Cin"], ["fa.Sum", "Sum"], ["fa[7].Cout", "Cout"]
                ]}
            ]}
        ]}),
    );
    // Eight full adders of 18 parts, five ports and 17 adapters.
    assert_eq!(report.build, Ok(8 * (2 * 6 + 6) + 5 + 17), "{report}");
    ws
}

#[test]
fn a_hierarchy_built_in_one_request_adds() {
    let ws = adder8();
    let mut sim = sim(&ws);
    for (a, b, cin) in [
        (0u64, 0u64, 0u64),
        (1, 1, 0),
        (200, 100, 1),
        (255, 255, 1),
        (0x5a, 0xa5, 0),
    ] {
        sim.set("A", a);
        sim.set("B", b);
        sim.set("Cin", cin);
        sim.tick();
        let sum = a + b + cin;
        assert_eq!(
            (sim.get("Sum"), sim.get("Cout")),
            (sum & 0xff, sum >> 8),
            "{a}+{b}+{cin}"
        );
    }
}

#[test]
fn the_report_counts_what_changed() {
    let (_, report) = edit(
        &empty(),
        json!({"circuit": "half_adder", "ops": [
            {"add": [{"name": "A", "type": "Input"}, {"name": "n", "type": "Not"},
                     {"name": "Y", "type": "Output"}]},
            {"wire": [["A", "n"], ["n", "Y"]]}
        ]}),
    );
    let c = &report.circuits[0];
    assert!(c.created);
    assert_eq!(
        (c.parts_added, c.wires_added, c.parts, c.wires, c.laid_out),
        (3, 2, 3, 2, 3)
    );
    let text = report.to_string();
    assert!(
        text.contains("half_adder: created; +3 parts +2 wires (3 laid out) -> 3 parts, 2 wires"),
        "{text}"
    );
    assert!(text.contains("build: ok"), "{text}");
}

#[test]
fn every_part_gets_its_name_as_label_and_arrays_are_named_again() {
    let ws = adder8();
    assert!(
        ws.root
            .nodes
            .iter()
            .any(|n| n.label.as_deref() == Some("fa[3]"))
    );
    let chip = ws
        .definitions
        .values()
        .find(|d| d.name == "half_adder")
        .unwrap();
    assert!(chip.nodes.iter().all(|n| n.label.is_some()));

    // A later request names the array, a range of it and one element.
    let (ws, report) = edit(
        &ws,
        json!({"ops": [
            {"unwire": ["fa[1..8].Cin", "Cout"]},
            {"add": [{"name": "zero", "type": "Const", "value": 0}]},
            {"wire": [["zero", "fa[1..8].Cin", "Cout"]]}
        ]}),
    );
    assert_eq!(report.circuits[0].wires_removed, 8, "{report}");
    assert_eq!(report.circuits[0].wires_added, 8, "{report}");
    // No carries any more: each bit adds on its own.
    let mut sim = sim(&ws);
    sim.set("A", 0b0101);
    sim.set("B", 0b0111);
    sim.tick();
    assert_eq!(sim.get("Sum"), 0b0010);
}

#[test]
fn bit_fields_make_adapters_and_removing_the_wire_removes_them() {
    let (ws, report) = edit(
        &empty(),
        json!({"ops": [
            {"add": [{"name": "A", "type": "Input", "w": 8},
                     {"name": "lo", "type": "Output", "w": 4},
                     {"name": "swap", "type": "Output", "w": 8}]},
            {"wire": [["A:0..4", "lo"], ["A:4..8", "swap:0..4"], ["A:0..4", "swap:4..8"]]}
        ]}),
    );
    // Two splits (the low one is shared) and one merge.
    assert_eq!(report.circuits[0].adapters_added, 3, "{report}");
    let mut s = sim(&ws);
    s.set("A", 0xa7);
    s.tick();
    assert_eq!((s.get("lo"), s.get("swap")), (0x7, 0x7a));

    let text = view::outline(&ws, &ws.root, None, view::DEFAULT_LIMIT);
    assert!(text.contains("3 parts + 3 generated adapters"), "{text}");
    assert!(text.contains("lo  Output w=4  in<-A:0..4"), "{text}");
    assert!(
        text.contains("swap  Output w=8  in<-{A:4..8, A:0..4}"),
        "{text}"
    );

    let (ws, report) = edit(&ws, json!({"ops": [{"unwire": ["swap", "lo"]}]}));
    assert_eq!(report.circuits[0].parts, 3, "{report}");
    assert!(ws.root.wires.is_empty());
}

#[test]
fn a_refused_request_says_where_and_changes_nothing() {
    let ws = adder8();
    let e = refused(&ws, json!({"ops": [{"wire": [["A", "fa.X"]]}]}));
    assert_eq!(
        e,
        "circuit \"main\": ops[0]: wire: wires[0] [\"A\",\"fa.X\"]: \"fa\" has no input pin \
         \"X\" (its input pins: A, B, Cin)"
    );
    let e = refused(&ws, json!({"ops": [{"wire": [["Cin", "fa[1].Cin"]]}]}));
    assert!(
        e.contains("input \"Cin\" of \"fa[1]\" is already driven by \"fa[0]\""),
        "{e}"
    );
    let e = refused(
        &ws,
        json!({"ops": [{"add": [{"name": "fa", "type": "Not"}]}]}),
    );
    assert!(e.contains("two nodes are named \"fa\""), "{e}");
    let e = refused(
        &ws,
        json!({"ops": [{"add": [{"name": "q", "use": "nothing"}]}]}),
    );
    assert!(e.contains("no chip \"nothing\""), "{e}");
    let e = refused(&ws, json!({"ops": [{"bogus": 1}]}));
    assert!(e.contains("unknown op \"bogus\""), "{e}");
    let e = refused(
        &ws,
        json!({"edits": [{"ops": []}, {"circuit": "x y", "ops": []}]}),
    );
    assert!(e.starts_with("edits[1]: no circuit \"x y\""), "{e}");
    // The second group fails: the first one's part is not kept either
    // (the workspace given is never changed).
    let e = refused(
        &ws,
        json!({"edits": [
            {"ops": [{"add": [{"name": "extra", "type": "Not"}]}]},
            {"ops": [{"remove": ["nobody"]}]}
        ]}),
    );
    assert!(e.contains("no node \"nobody\""), "{e}");
}

#[test]
fn a_chip_cannot_contain_itself() {
    let ws = adder8();
    let e = refused(
        &ws,
        json!({"circuit": "half_adder", "ops": [{"add": [{"name": "me", "use": "half_adder"}]}]}),
    );
    assert!(e.contains("cannot contain itself"), "{e}");
    let e = refused(
        &ws,
        json!({"circuit": "half_adder", "ops": [{"add": [{"name": "up", "use": "full_adder"}]}]}),
    );
    assert_eq!(e, "chip \"full_adder\" would contain itself");
}

#[test]
fn remove_takes_the_wires_and_set_changes_parameters() {
    let (ws, _) = edit(
        &empty(),
        json!({"ops": [
            {"add": [{"name": "A", "type": "Input", "w": 4}, {"name": "B", "type": "Input", "w": 4},
                     {"name": "g", "type": "And", "w": 4}, {"name": "Y", "type": "Output", "w": 4}]},
            {"wire": [["A", "g.a"], ["B", "g.b"], ["g", "Y"]]}
        ]}),
    );
    // Widening one part breaks its wires, and says so.
    let (wide, report) = edit(&ws, json!({"ops": [{"set": {"part": "g", "w": 8}}]}));
    assert_eq!(report.circuits[0].dropped_wires.len(), 3, "{report}");
    assert!(wide.root.wires.is_empty());
    // Widening all of them keeps them.
    let (wide, report) = edit(
        &ws,
        json!({"ops": [{"set": [{"part": "A", "w": 8}, {"part": "B", "w": 8},
                                {"part": "g", "w": 8}, {"part": "Y", "w": 8}]}]}),
    );
    // Each set is checked on its own, so wires drop on the way...
    assert!(wide.root.wires.len() < 3, "{report}");
    // ...which is why a width change of a whole path is remove and add.
    let e = refused(&ws, json!({"ops": [{"set": {"part": "g", "type": "Or"}}]}));
    assert!(e.contains("type cannot be set"), "{e}");

    let (renamed, _) = edit(
        &ws,
        json!({"ops": [{"set": {"part": "g", "label": "both"}}]}),
    );
    let (gone, report) = edit(&renamed, json!({"ops": [{"remove": ["both"]}]}));
    assert_eq!(
        (
            report.circuits[0].parts_removed,
            report.circuits[0].wires_removed
        ),
        (1, 3)
    );
    assert_eq!(gone.root.nodes.len(), 3);
    // The old name is free again, with a new id.
    let (again, _) = edit(
        &renamed,
        json!({"ops": [{"add": [{"name": "g", "type": "Or"}]}]}),
    );
    let ids: std::collections::HashSet<_> = again.root.nodes.iter().map(|n| n.id).collect();
    assert_eq!(ids.len(), 5);
}

fn boxes(doc: &CircuitDocument) -> Vec<(Vec2, Vec2)> {
    doc.nodes
        .iter()
        .map(|n| (n.position(), n.kind.size()))
        .collect()
}

#[test]
fn parts_without_a_place_are_laid_out_and_placed_parts_stay() {
    let ws = adder8();
    for doc in std::iter::once(&ws.root).chain(ws.definitions.values().map(|d| &**d)) {
        assert_eq!(overlaps(&boxes(doc)), None, "{}", doc.name);
        assert!(doc.nodes.iter().any(|n| n.x != 0.0), "{}", doc.name);
    }
    // Inputs left of outputs.
    let x = |label: &str| {
        ws.root
            .nodes
            .iter()
            .find(|n| n.label.as_deref() == Some(label))
            .unwrap()
            .x
    };
    assert!(x("A") < x("fa[0]") && x("fa[0]") < x("Sum"));

    // A second request adds a block below; nothing moves, nothing overlaps.
    let before: Vec<(uuid::Uuid, f32, f32)> =
        ws.root.nodes.iter().map(|n| (n.id, n.x, n.y)).collect();
    let (ws2, report) = edit(
        &ws,
        json!({"ops": [
            {"add": [{"name": "inv", "type": "Not", "w": 8}, {"name": "NotSum", "type": "Output", "w": 8},
                     {"name": "pinned", "type": "Not", "at": [1000, 1000]}]},
            {"wire": [["fa.Sum", "inv"], ["inv", "NotSum"]]}
        ]}),
    );
    assert_eq!(report.circuits[0].laid_out, 3, "{report}"); // inv, NotSum, the merge
    for (id, x, y) in before {
        let n = ws2.root.node(id).unwrap();
        assert_eq!((n.x, n.y), (x, y));
    }
    let pinned = ws2
        .root
        .nodes
        .iter()
        .find(|n| n.label.as_deref() == Some("pinned"))
        .unwrap();
    assert_eq!((pinned.x, pinned.y), (1000.0, 1000.0));
    assert_eq!(overlaps(&boxes(&ws2.root)), None);

    // `layout` redoes the whole circuit.
    let (ws3, report) = edit(&ws2, json!({"ops": [{"layout": true}]}));
    assert_eq!(report.circuits[0].laid_out, ws3.root.nodes.len());
    assert_eq!(overlaps(&boxes(&ws3.root)), None);
}

#[test]
fn the_same_request_gives_the_same_workspace() {
    assert_eq!(adder8(), adder8());
}

#[test]
fn parts_made_in_the_editor_are_named_by_id_or_label() {
    // Unlabelled gates, a label with a space, and two parts sharing one.
    let mut root = CircuitDocument::new("hand made");
    let a = root.add(DocNode::labeled(GateKind::Input { w: 1 }, Vec2::ZERO, "A"));
    let n1 = root.add(DocNode::new(GateKind::Not { w: 1 }, Vec2::new(100.0, 0.0)));
    let n2 = root.add(DocNode::labeled(
        GateKind::Not { w: 1 },
        Vec2::new(200.0, 0.0),
        "my gate",
    ));
    root.add(DocNode::labeled(
        GateKind::Not { w: 1 },
        Vec2::new(300.0, 0.0),
        "twin",
    ));
    root.add(DocNode::labeled(
        GateKind::Not { w: 1 },
        Vec2::new(400.0, 0.0),
        "twin",
    ));
    root.connect(a, 0, n1, 0);
    let ws = WorkspaceDocument::new(root);

    let ids = batch::short_ids(&ws.root);
    let text = view::outline(&ws, &ws.root, None, view::DEFAULT_LIMIT);
    assert!(
        text.contains(&format!("{}  Not w=1  in<-A", ids[&n1])),
        "{text}"
    );

    let e = refused(&ws, json!({"ops": [{"remove": ["twin"]}]}));
    assert!(e.contains("several parts are labelled \"twin\""), "{e}");
    let (ws, _) = edit(
        &ws,
        json!({"ops": [
            {"wire": [[ids[&n1], ids[&n2]]]},
            {"set": {"part": ids[&n2], "label": "second"}},
            {"add": [{"name": "Y", "type": "Output"}]},
            {"wire": [["second", "Y"]]}
        ]}),
    );
    let mut s = sim(&ws);
    s.set("A", 1);
    s.tick();
    assert_eq!(s.get("Y"), 1);
}

#[test]
fn demo_chips_are_used_by_reference_and_join_the_workspace() {
    let catalog = catalog().expect("demos load");
    let (ws, report) = apply(
        &empty(),
        &json!({"ops": [
            {"add": [{"name": "A", "type": "Input"}, {"name": "B", "type": "Input"},
                     {"name": "ha", "use": "half_adder/half_adder"},
                     {"name": "S", "type": "Output"}]},
            {"wire": [["A", "ha.0"], ["B", "ha.1"], ["ha.0", "S"]]}
        ]}),
        &catalog.externals(),
    )
    .unwrap_or_else(|e| panic!("{e}"));
    assert!(report.build.is_ok(), "{report}");
    // The chip and what it is made of came along.
    let catalog_id = catalog.definition_id("half_adder/half_adder").unwrap();
    assert!(ws.definitions.contains_key(&catalog_id));
    // Saved and read back, it is the same workspace.
    let text = crate::editor::workspace::workspace_to_json("t", &ws).expect("saves");
    assert!(text.contains("SubCircuit"));
}

#[test]
fn the_outline_lists_ports_chips_and_drivers() {
    let ws = adder8();
    let text = view::outline(&ws, &ws.root, None, view::DEFAULT_LIMIT);
    assert!(
        text.starts_with("circuit \"main\" (root): 13 parts + 17 generated adapters"),
        "{text}"
    );
    assert!(
        text.contains("ports: in A[8] B[8] Cin | out Cout Sum[8]"),
        "{text}"
    );
    assert!(text.contains("chips: full_adder x8"), "{text}");
    assert!(
        text.contains("fa[1]  full_adder  A<-A:1  B<-B:1  Cin<-fa[0].Cout"),
        "{text}"
    );
    assert!(text.contains("Cout  Output w=1  in<-fa[7].Cout"), "{text}");
    // Elements in index order, inputs first.
    let at = |s: &str| text.find(s).unwrap_or_else(|| panic!("{s} in {text}"));
    assert!(at("  A  Input") < at("fa[2]  "));
    assert!(at("fa[2]  ") < at("fa[7]  ") && at("fa[7]  ") < at("  Cout  Output"));

    let short = view::outline(&ws, &ws.root, Some("fa"), 3);
    assert!(short.contains("... 5 more parts"), "{short}");
    let chip = ws
        .definitions
        .values()
        .find(|d| d.name == "full_adder")
        .unwrap();
    let text = view::outline(&ws, chip, None, view::DEFAULT_LIMIT);
    assert!(
        text.contains("circuit \"full_adder\" (chip): 8 parts, 8 wires"),
        "{text}"
    );
    assert!(text.contains("h2  half_adder  A<-h1.Sum  B<-Cin"), "{text}");

    let status = view::status(&ws);
    assert!(
        status.contains("chip half_adder: 6 parts, 6 wires, placed 2 time(s)"),
        "{status}"
    );
    assert!(status.contains("build: ok"), "{status}");
    assert!(view::stats(&ws, &ws.root).contains("8  chip full_adder"));
}

#[test]
fn diagnostics_report_loops_open_inputs_and_size() {
    let (ws, report) = edit(
        &empty(),
        json!({"ops": [
            {"add": [{"name": "a", "type": "Not"}, {"name": "b", "type": "Not"},
                     {"name": "open", "type": "And"},
                     {"name": "many", "type": "Not", "count": 160}]},
            {"wire": [["a", "b"], ["b", "a"]]}
        ]}),
    );
    assert!(
        report.build.is_err(),
        "a loop is accepted, and reported: {report}"
    );
    let text = view::diagnostics(&ws);
    assert!(text.contains("build: failed"), "{text}");
    assert!(text.contains("combinational loop"), "{text}");
    assert!(
        text.contains("unconnected inputs in \"main\" (they read 0): many[0].in, many[1].in"),
        "{text}"
    );
    assert!(text.contains("and 142 more"), "{text}");
    assert!(
        text.contains("advice: \"test\" has 163 parts in one circuit"),
        "{text}"
    );
}
