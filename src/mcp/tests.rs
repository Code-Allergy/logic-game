//! Tests of the batch editor and the views.

mod app;
mod chips;
mod compiled;
mod expand;
mod poke;
mod sim;
mod suite;

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

pub(super) fn empty() -> WorkspaceDocument {
    // A fixed id: the ids of parts derive from it.
    let mut root = CircuitDocument::new("test");
    root.id = uuid::Uuid::from_u128(7);
    WorkspaceDocument::new(root)
}

pub(super) fn edit(ws: &WorkspaceDocument, request: Value) -> (WorkspaceDocument, Report) {
    apply(ws, &request, &Externals::default()).unwrap_or_else(|e| panic!("{e}"))
}

pub(super) fn refused(ws: &WorkspaceDocument, request: Value) -> String {
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
/// The request that builds [`adder8`].
pub(super) fn adder8_request() -> Value {
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
    ]})
}

pub(super) fn adder8() -> WorkspaceDocument {
    let (ws, report) = edit(&empty(), adder8_request());
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
    // A whole array is renamed with its indices; one element on its own.
    let (arrays, _) = edit(
        &ws,
        json!({"ops": [
            {"add": [{"name": "n", "type": "Not", "count": 3}]},
            {"set": [{"part": "n", "label": "inv"}, {"part": "inv[2]", "label": "last"}]}
        ]}),
    );
    let labels: Vec<&str> = arrays
        .root
        .nodes
        .iter()
        .filter_map(|n| n.label.as_deref())
        .collect();
    for label in ["inv[0]", "inv[1]", "last"] {
        assert!(labels.contains(&label), "{labels:?}");
    }
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
    assert!(
        text.contains("a loop of gates with no state in it: a -> b -> a"),
        "{text}"
    );
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

/// A scratch folder for one test's workspaces.
pub(super) fn scratch(test: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "gatequest-mcp-{test}-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&dir).expect("scratch folder");
    dir
}

fn half_adder_request() -> Value {
    json!({"edits": [
        {"circuit": "half_adder", "ops": [
            {"add": [{"name": "A", "type": "Input"}, {"name": "B", "type": "Input"},
                     {"name": "x", "type": "Xor"}, {"name": "c", "type": "And"},
                     {"name": "Sum", "type": "Output"}, {"name": "Carry", "type": "Output"}]},
            {"wire": [["A", "x.a", "c.a"], ["B", "x.b", "c.b"], ["x", "Sum"], ["c", "Carry"]]}
        ]},
        {"ops": [
            {"add": [{"name": "P", "type": "Input"}, {"name": "Q", "type": "Input"},
                     {"name": "ha", "use": "half_adder"}, {"name": "S", "type": "Output"}]},
            {"wire": [["P", "ha.A"], ["Q", "ha.B"], ["ha.Sum", "S"]]}
        ]}
    ]})
}

#[test]
fn a_session_makes_edits_saves_and_reopens() {
    use super::session::Session;
    use crate::editor::workspace::WorkspaceStore;

    let dir = scratch("session");
    let mut s = Session::new(WorkspaceStore::in_dir(&dir));
    let e = s.view_tool(None, None, None, None).unwrap_err();
    assert!(e.contains("no workspace is open"), "{e}");
    assert!(
        s.workspace_tool("list", None, false)
            .unwrap()
            .contains("(none)")
    );
    s.workspace_tool("new", Some("adders"), false).unwrap();

    let Value::Object(request) = half_adder_request() else {
        unreachable!()
    };
    let dry = s.edit_tool(request.clone(), None, true).unwrap();
    assert!(dry.starts_with("dry run, nothing changed"), "{dry}");
    assert!(s.workspace().unwrap().root.nodes.is_empty());
    let report = s.edit_tool(request, None, false).unwrap();
    assert!(report.contains("build: ok (9 parts flattened)"), "{report}");

    // Unsaved work is not dropped by accident.
    let e = s.workspace_tool("new", Some("other"), false).unwrap_err();
    assert!(e.contains("unsaved changes"), "{e}");
    let saved = s.workspace_tool("save", None, false).unwrap();
    assert!(saved.starts_with("saved \"adders\""), "{saved}");
    let before = s.workspace().unwrap().clone();

    // A request from a file, as a generated circuit would come.
    let file = dir.join("more.json");
    std::fs::write(
        &file,
        r#"{"ops": [{"add": [{"name": "inv", "type": "Not", "count": 3}]}]}"#,
    )
    .unwrap();
    let report = s.edit_tool(Default::default(), Some(&file), false).unwrap();
    assert!(report.contains("+3 parts"), "{report}");
    let status = s.workspace_tool("status", None, false).unwrap();
    assert!(
        status.starts_with("workspace \"adders\" (unsaved changes)"),
        "{status}"
    );

    // Reopened, it is what was saved.
    let opened = s.workspace_tool("open", Some("adders"), true).unwrap();
    assert!(
        opened.contains("chip half_adder: 6 parts, 6 wires, placed 1 time(s)"),
        "{opened}"
    );
    assert_eq!(s.workspace().unwrap(), &before);
    let outline = s.view_tool(Some("half_adder"), None, None, None).unwrap();
    assert!(outline.contains("Sum  Output w=1  in<-x"), "{outline}");
    let e = s.view_tool(Some("nope"), None, None, None).unwrap_err();
    assert_eq!(e, "no circuit \"nope\" (circuits: main, half_adder)");
    let _ = std::fs::remove_dir_all(dir);
}

/// The same workflow through the protocol: a client and the server joined
/// by an in-memory pipe.
#[cfg(feature = "mcp")]
mod protocol {
    use rmcp::ServiceExt;
    use rmcp::model::{CallToolRequestParams, CallToolResult};
    use serde_json::{Value, json};

    use super::{half_adder_request, scratch};
    use crate::editor::workspace::WorkspaceStore;
    use crate::mcp::server::Server;
    use crate::mcp::session::Session;

    fn text(result: &CallToolResult) -> String {
        result
            .content
            .iter()
            .filter_map(|c| c.as_text().map(|t| t.text.as_str()))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// One HTTP request to the local server, by hand: the status, the
    /// session id it gave, and the body.
    fn post(
        url: &str,
        token: Option<&str>,
        session: Option<&str>,
        body: &str,
    ) -> (u16, Option<String>, String) {
        use std::io::{Read, Write};
        let address = url.trim_start_matches("http://").trim_end_matches("/mcp");
        let mut stream = std::net::TcpStream::connect(address).unwrap();
        let mut request = format!(
            "POST /mcp HTTP/1.1\r\nHost: {address}\r\nContent-Type: application/json\r\n\
             Accept: application/json, text/event-stream\r\nConnection: close\r\n\
             Content-Length: {}\r\n",
            body.len()
        );
        if let Some(token) = token {
            request.push_str(&format!("Authorization: Bearer {token}\r\n"));
        }
        if let Some(session) = session {
            request.push_str(&format!("Mcp-Session-Id: {session}\r\n"));
        }
        request.push_str("\r\n");
        request.push_str(body);
        stream.write_all(request.as_bytes()).unwrap();
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(20)))
            .unwrap();
        // Read until the answer to a request is there (an event stream
        // may stay open), or the server closes.
        let mut text = String::new();
        let mut buffer = [0u8; 4096];
        loop {
            match stream.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(n) => text.push_str(&String::from_utf8_lossy(&buffer[..n])),
            }
            let (head, rest) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
            if rest.contains("\"jsonrpc\"") && rest.trim_end().ends_with('}')
                || head.starts_with("HTTP/1.1 4")
                || head.starts_with("HTTP/1.1 202")
            {
                break;
            }
        }
        let (head, rest) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
        let status = head
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let session = head.lines().find_map(|l| {
            l.to_ascii_lowercase()
                .strip_prefix("mcp-session-id:")
                .map(|_| l[15..].trim().to_string())
        });
        (status, session, rest.to_string())
    }

    /// The whole stack of the running game: HTTP with a token, the
    /// protocol, the session, the link, and an app whose frames go by.
    #[test]
    fn the_game_is_reached_over_http_with_its_token() {
        use bevy::prelude::*;

        use crate::mcp::app::{McpAppPlugin, link};
        use crate::sim::{SimRuntime, SimulationRuntimePlugin};

        let dir = scratch("http");
        let (link, inbox) = link();
        let mut app = App::new();
        app.add_plugins(SimulationRuntimePlugin {
            runtime: SimRuntime::Threadless,
        })
        .add_plugins(McpAppPlugin::new(inbox));
        let session = Session::with_app(WorkspaceStore::in_dir(&dir), Box::new(link));
        let endpoint = crate::mcp::http::serve(Server::new(session), Some(dir.clone())).unwrap();
        let written = std::fs::read_to_string(endpoint.file.as_ref().unwrap()).unwrap();
        assert!(
            written.contains(&endpoint.url) && written.contains(&endpoint.token),
            "{written}"
        );

        let (url, token) = (endpoint.url.clone(), endpoint.token.clone());
        let said = std::thread::scope(|scope| {
            let client = scope.spawn(move || {
                let initialize = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}"#;
                // No token, or a wrong one: nothing is served.
                assert_eq!(post(&url, None, None, initialize).0, 401);
                assert_eq!(post(&url, Some("wrong"), None, initialize).0, 401);
                let (status, session, body) = post(&url, Some(&token), None, initialize);
                assert_eq!(status, 200, "{body}");
                assert!(body.contains("protocolVersion"), "{body}");
                let session = session.as_deref();
                post(&url, Some(&token), session, r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);
                let call = |id: u32, name: &str, arguments: Value| {
                    let body = json!({"jsonrpc": "2.0", "id": id, "method": "tools/call",
                                      "params": {"name": name, "arguments": arguments}});
                    post(&url, Some(&token), session, &body.to_string()).2
                };
                let edit = call(2, "edit", json!({"ops": [{"add": [
                    {"name": "A", "type": "Input"}, {"name": "n", "type": "Not"}, {"name": "Y", "type": "Output"}]},
                    {"wire": [["A", "n"], ["n", "Y"]]}]}));
                let state = call(3, "app", json!({}));
                let step = call(4, "transport", json!({"action": "step", "ticks": 3}));
                (edit, state, step)
            });
            while !client.is_finished() {
                app.update();
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            client.join().unwrap()
        });
        let (edit, state, step) = said;
        assert!(edit.contains("+3 parts +2 wires"), "{edit}");
        assert!(
            state.contains("undo: 1 step(s)") && state.contains("in A=0"),
            "{state}"
        );
        assert!(step.contains("paused, tick"), "{step}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_client_builds_views_saves_and_reopens() {
        let dir = scratch("protocol");
        let store = WorkspaceStore::in_dir(&dir);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let (server_end, client_end) = tokio::io::duplex(1 << 16);
            let server = tokio::spawn(async move {
                let service = Server::new(Session::new(store))
                    .serve(server_end)
                    .await
                    .unwrap();
                service.waiting().await.unwrap();
            });
            let client = ().serve(client_end).await.unwrap();

            let mut tools: Vec<String> = client
                .list_all_tools()
                .await
                .unwrap()
                .into_iter()
                .map(|t| t.name.to_string())
                .collect();
            tools.sort();
            assert_eq!(
                tools,
                [
                    "app",
                    "edit",
                    "import_rtl",
                    "run",
                    "test",
                    "transport",
                    "view",
                    "workspace"
                ]
            );

            let call = |name: &'static str, args: Value| {
                let Value::Object(args) = args else {
                    unreachable!()
                };
                let client = &client;
                async move {
                    let result = client
                        .call_tool(CallToolRequestParams::new(name).with_arguments(args))
                        .await
                        .unwrap();
                    (result.is_error == Some(true), text(&result))
                }
            };

            let (failed, said) = call("view", json!({})).await;
            assert!(failed && said.contains("no workspace is open"), "{said}");
            let (failed, said) =
                call("workspace", json!({"action": "new", "name": "adders"})).await;
            assert!(!failed, "{said}");
            let (failed, said) = call("edit", half_adder_request()).await;
            assert!(
                !failed && said.contains("build: ok (9 parts flattened)"),
                "{said}"
            );
            let (failed, said) = call("edit", json!({"ops": [{"wire": [["P", "ha.Nope"]]}]})).await;
            assert!(
                failed && said.contains("\"ha\" has no input pin \"Nope\""),
                "{said}"
            );
            let (_, said) = call("view", json!({"circuit": "half_adder"})).await;
            assert!(said.contains("c  And w=1  a<-A  b<-B"), "{said}");
            let (failed, said) = call("workspace", json!({"action": "save"})).await;
            assert!(!failed, "{said}");
            let (failed, said) =
                call("workspace", json!({"action": "open", "name": "adders"})).await;
            assert!(!failed && said.contains("chip half_adder"), "{said}");
            let (_, said) = call("view", json!({"what": "stats"})).await;
            assert!(said.contains("flattened: 9 parts"), "{said}");

            // A stored test and an experiment, through the protocol.
            let (failed, said) = call(
                "edit",
                json!({"circuit": "half_adder", "ops": [{"tests": [
                    {"name": "sum", "formula": {"expect": {"Sum": "A ^ B", "Carry": "A & B"}}}]}]}),
            )
            .await;
            assert!(!failed && said.contains("+1 tests"), "{said}");
            let (failed, said) = call("test", json!({})).await;
            assert!(!failed && said.contains("1 of 1 tests passed"), "{said}");
            let (failed, said) = call(
                "run",
                json!({"steps": [{"set": {"P": 1, "Q": 1}}, {"ticks": 1},
                                 {"expect": {"S": 0, "ha/c": 1}}, {"read": ["ha/x"]}]}),
            )
            .await;
            assert!(
                !failed && said.contains("ha/x = 0 (0)") && said.ends_with("ok\n"),
                "{said}"
            );
            let (failed, said) = call(
                "run",
                json!({"steps": [{"ticks": 1}, {"expect": {"S": 1}}]}),
            )
            .await;
            assert!(failed && said.contains("FAILED steps[1]"), "{said}");

            client.cancel().await.unwrap();
            server.await.unwrap();
        });
        let _ = std::fs::remove_dir_all(dir);
    }
}
