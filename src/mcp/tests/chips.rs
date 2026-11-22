//! Tests of `make_chip`, `copy_of` and `expand`.

use serde_json::{Value, json};

use super::{adder8, edit, empty, refused};
use crate::mcp::{chips, testing, view};
use crate::sim::{CircuitLibrary, GateKind};

/// One part of `kind` between ports named after its pins, as chip `name`.
fn wrapper(name: &str, entry: Value) -> Value {
    let kind: GateKind = serde_json::from_value(entry.clone()).expect("a kind");
    let library = CircuitLibrary::default();
    let (ins, outs) = library.pins(&kind).unwrap();
    let (in_names, out_names) = library.pin_names(&kind);
    let mut add = vec![];
    let mut wire = vec![];
    let mut part = entry.as_object().unwrap().clone();
    part.insert("name".into(), "part".into());
    add.push(Value::Object(part));
    for (w, n) in ins.iter().zip(&in_names) {
        add.push(json!({"name": format!("i_{n}"), "type": "Input", "w": w}));
        wire.push(json!([format!("i_{n}"), format!("part.{n}")]));
    }
    for (w, n) in outs.iter().zip(&out_names) {
        add.push(json!({"name": format!("o_{n}"), "type": "Output", "w": w}));
        wire.push(json!([format!("part.{n}"), format!("o_{n}")]));
    }
    json!({"circuit": name, "ops": [{"add": add}, {"wire": wire}]})
}

#[test]
fn every_word_level_kind_expands_to_gates_that_do_the_same() {
    let kinds = [
        json!({"type": "Not", "w": 5}),
        json!({"type": "And", "w": 5}),
        json!({"type": "Or", "w": 5}),
        json!({"type": "Xor", "w": 5}),
        json!({"type": "Nand", "w": 5}),
        json!({"type": "Nor", "w": 5}),
        json!({"type": "Xnor", "w": 5}),
        json!({"type": "Mux", "w": 5}),
        json!({"type": "Switch", "w": 5}),
        json!({"type": "Add", "w": 6}),
        json!({"type": "Add", "w": 1}),
        json!({"type": "Eq", "w": 5}),
        json!({"type": "LtU", "w": 5}),
        json!({"type": "ReduceOr", "w": 5}),
        json!({"type": "ReduceAnd", "w": 5}),
        json!({"type": "ReduceXor", "w": 5}),
        json!({"type": "Delay", "w": 4}),
        json!({"type": "Register", "w": 4}),
        json!({"type": "DLatch", "w": 4}),
    ];
    let mut edits = Vec::new();
    for (i, kind) in kinds.iter().enumerate() {
        edits.push(wrapper(&format!("word{i}"), kind.clone()));
        edits.push(json!({"circuit": format!("gates{i}"), "ops": [
            {"copy_of": format!("word{i}")},
            {"expand": ["part"]},
            {"tests": [{"name": "same", "equivalent": {"to": format!("word{i}"), "samples": 400, "seed": i}}]}
        ]}));
    }
    let (ws, report) = edit(&empty(), json!({"edits": edits}));
    assert!(report.build.is_ok(), "{report}");
    let suite = testing::run_suite(&ws, None, None).unwrap();
    assert_eq!(suite.failed(), 0, "{}", suite.text());
    assert_eq!(suite.results.len(), kinds.len());

    // The gate chips hold 1-bit gates and wiring only.
    for def in ws
        .definitions
        .values()
        .filter(|d| d.name.ends_with("_gates"))
    {
        for n in &def.nodes {
            let one_bit = n.kind.is_port()
                || matches!(n.kind, GateKind::Split { .. } | GateKind::Merge { .. })
                || n.kind.width() == Some(1);
            assert!(one_bit, "{}: {:?}", def.name, n.kind);
        }
    }
    let adder = ws
        .definitions
        .values()
        .find(|d| d.name == "Add_6_gates")
        .unwrap();
    let text = view::outline(&ws, adder, None, 200);
    assert!(
        text.contains("ports: in a[6] b[6] cin | out cout sum[6]"),
        "{text}"
    );
    // Six full adders of five gates.
    assert!(
        text.starts_with("circuit \"Add_6_gates\" (chip): 35 parts"),
        "{text}"
    );
    assert!(text.contains("g0  Xor w=1  a<-a:0  b<-b:0"), "{text}");
}

#[test]
fn expand_names_its_chip_once_and_refuses_what_has_no_gates() {
    let (ws, report) = edit(
        &empty(),
        json!({"ops": [
            {"add": [{"name": "x", "type": "Add", "w": 8, "count": 3}, {"name": "n", "type": "Not"},
                     {"name": "r", "type": "Rom", "addr_bits": 2, "w": 4}]},
            {"expand": true}
        ]}),
    );
    // The three adders share one chip; the 1-bit Not and the Rom stay.
    assert_eq!(report.circuits[0].expanded, 3, "{report}");
    assert!(
        report.to_string().contains("(3 expanded to gates)"),
        "{report}"
    );
    assert_eq!(ws.definitions.len(), 1);
    assert_eq!(
        chips::gate_chip_name(&GateKind::Add { w: 8 }),
        "Add_8_gates"
    );
    let e = refused(&ws, json!({"ops": [{"expand": ["r"]}]}));
    assert!(e.contains("has no gate-level form"), "{e}");
    // Expanding again finds nothing left to expand.
    let (_, report) = edit(&ws, json!({"ops": [{"expand": true}]}));
    assert_eq!(report.circuits[0].expanded, 0);
}

#[test]
fn make_chip_moves_parts_into_a_chip_wired_the_same_way() {
    let ws = adder8();
    let (ws, _) = edit(
        &ws,
        json!({"ops": [{"tests": [
            {"name": "adds", "formula": {"expect": {"Sum": "A + B + Cin", "Cout": "(A + B + Cin) >> 8"},
                                         "samples": 300}}]}]}),
    );
    let request = json!({"ops": [{"make_chip": {"parts": ["fa[0..4]"], "name": "low4"}}]});
    let (chipped, report) = edit(&ws, request.clone());
    let c = &report.circuits[0];
    assert_eq!((c.parts_removed, c.parts_added), (4, 1), "{report}");
    assert!(report.build.is_ok(), "{report}");
    // It still adds, and the same request gives the same workspace.
    assert_eq!(
        testing::run_suite(&chipped, Some("main"), None)
            .unwrap()
            .failed(),
        0
    );
    assert_eq!(edit(&ws, request).0, chipped);

    let chip = chipped
        .definitions
        .values()
        .find(|d| d.name == "low4")
        .unwrap();
    let text = view::outline(&chipped, chip, None, 200);
    assert!(text.contains("chips: full_adder x4"), "{text}");
    // Ports are names, so they can be wired by name later.
    let (ins, outs) = chip.interface();
    for port in ins.iter().chain(&outs) {
        let label = port.label.as_deref().unwrap();
        assert!(crate::sim::source::valid_name(label), "{label}");
    }
    let root = view::outline(&chipped, &chipped.root, None, 200);
    assert!(root.contains("low4  low4  "), "{root}");
    assert!(
        root.contains("fa[4]  full_adder  A<-A:4  B<-B:4  Cin<-low4."),
        "{root}"
    );

    // The elements left keep their names; the ones that moved are gone.
    let (_, report) = edit(
        &chipped,
        json!({"ops": [{"unwire": ["fa[5].Cin"]}, {"wire": [["fa[4].Cout", "fa[5].Cin"]]}]}),
    );
    assert!(report.build.is_ok(), "{report}");
    let e = refused(&chipped, json!({"ops": [{"remove": ["fa[2]"]}]}));
    assert!(e.contains("\"fa[2]\" is not there any more"), "{e}");
    let e = refused(&chipped, json!({"ops": [{"unwire": ["fa.Cin"]}]}));
    assert!(
        e.contains("\"fa[0]\" is not there any more; name the elements that are"),
        "{e}"
    );
    let (rest, report) = edit(&chipped, json!({"ops": [{"remove": ["fa"]}]}));
    assert_eq!(report.circuits[0].parts_removed, 4, "{report}");
    assert!(!view::outline(&rest, &rest.root, None, 200).contains("full_adder  "));

    // The chip is a part like any other from here on.
    let (again, _) = edit(
        &chipped,
        json!({"ops": [{"add": [{"name": "spare", "use": "low4"}]}]}),
    );
    assert_eq!(view::status(&again).matches("chip low4").count(), 1);

    for (bad, said) in [
        (
            json!({"parts": ["A"], "name": "x"}),
            "Select at least one part",
        ),
        (
            json!({"parts": ["fa[0]"], "name": "full_adder"}),
            "a chip named \"full_adder\" exists already",
        ),
        (
            json!({"parts": ["fa[0]"], "name": "two words"}),
            "a chip's name is letters, digits and '_'",
        ),
        (
            json!({"parts": ["fa[0]"], "name": "Sum"}),
            "a part is already labelled \"Sum\"",
        ),
        (
            json!({"parts": ["nobody"], "name": "x"}),
            "no node \"nobody\"",
        ),
        (json!({"name": "x"}), "takes {\"parts\""),
    ] {
        let e = refused(&ws, json!({"ops": [{"make_chip": bad.clone()}]}));
        assert!(e.contains(said), "{bad}: {e}");
    }
}

#[test]
fn copy_of_starts_a_chip_from_another() {
    let ws = adder8();
    let (ws, report) = edit(
        &ws,
        json!({"circuit": "fa_variant", "ops": [
            {"copy_of": "full_adder"},
            {"remove": ["carry"]}, {"add": [{"name": "carry", "type": "Xor"}]},
            {"wire": [["h1.Carry", "carry.a"], ["h2.Carry", "carry.b"], ["carry", "Cout"]]}
        ]}),
    );
    assert!(report.circuits[0].created, "{report}");
    let original = ws
        .definitions
        .values()
        .find(|d| d.name == "full_adder")
        .unwrap();
    let copy = ws
        .definitions
        .values()
        .find(|d| d.name == "fa_variant")
        .unwrap();
    assert_eq!(original.nodes.len(), copy.nodes.len());
    assert!(copy.nodes.iter().any(|n| n.kind == GateKind::Xor { w: 1 }));
    assert!(
        !original
            .nodes
            .iter()
            .any(|n| n.kind == GateKind::Xor { w: 1 })
    );

    let e = refused(
        &ws,
        json!({"circuit": "full_adder", "ops": [{"copy_of": "half_adder"}]}),
    );
    assert!(
        e.contains("copy_of starts a new chip; \"full_adder\" exists already"),
        "{e}"
    );
    let e = refused(
        &ws,
        json!({"circuit": "n", "ops": [{"copy_of": "nothing"}]}),
    );
    assert!(e.contains("no circuit \"nothing\""), "{e}");
    let e = refused(
        &ws,
        json!({"circuit": "n", "ops": [{"layout": true}, {"copy_of": "half_adder"}]}),
    );
    assert!(e.contains("ops[1]: copy_of: must be the first op"), "{e}");
    // An op after copy_of is numbered as it was written.
    let e = refused(
        &ws,
        json!({"circuit": "n", "ops": [{"copy_of": "half_adder"}, {"remove": ["zz"]}]}),
    );
    assert!(e.contains("ops[1]: remove"), "{e}");
}

#[test]
fn a_chip_whose_ports_change_keeps_its_placements_wired_by_port() {
    let ws = adder8();
    let (ws, _) = edit(
        &ws,
        json!({"ops": [{"tests": [
            {"name": "adds", "formula": {"expect": {"Sum": "A + B + Cin", "Cout": "(A + B + Cin) >> 8"},
                                         "samples": 200}}]}]}),
    );
    // A new port that sorts before the others moves every pin of the
    // half adder; the full adders that use it must not notice.
    let (wider, report) = edit(
        &ws,
        json!({"circuit": "half_adder", "ops": [
            {"add": [{"name": "AA_debug", "type": "Output"}, {"name": "A0_spare", "type": "Input"}]},
            {"wire": [["x", "AA_debug"]]}
        ]}),
    );
    assert!(report.circuits[0].dropped_wires.is_empty(), "{report}");
    assert_eq!(
        testing::run_suite(&wider, Some("main"), None)
            .unwrap()
            .failed(),
        0
    );

    // A port that goes away takes the wires on it, and says so.
    let (narrower, report) = edit(
        &ws,
        json!({"circuit": "half_adder", "ops": [{"remove": ["Carry"]}]}),
    );
    let dropped = &report.circuits[0].dropped_wires;
    assert_eq!(dropped.len(), 2, "{report}");
    assert!(
        dropped[0].contains("in \"full_adder\" (the port of \"half_adder\" is gone"),
        "{report}"
    );
    assert!(
        testing::run_suite(&narrower, Some("main"), None)
            .unwrap()
            .failed()
            > 0
    );
}

fn rtl(file: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("examples/rtl")
        .join(file)
}

#[test]
fn a_netlist_is_imported_as_a_workspace_or_as_chips() {
    use crate::editor::workspace::WorkspaceStore;
    use crate::mcp::session::Session;

    let dir = super::scratch("import");
    let mut s = Session::new(WorkspaceStore::in_dir(&dir));
    let out = s
        .import_rtl_tool(&rtl("top.json"), None, None, None, false, false)
        .unwrap();
    assert!(
        out.contains("as new workspace \"top\" (not saved yet)"),
        "{out}"
    );
    assert!(out.contains("build: ok"), "{out}");
    // Every part has a name, so its signals can be read.
    let ws = s.workspace().unwrap();
    for doc in std::iter::once(&ws.root).chain(ws.definitions.values().map(|d| &**d)) {
        let unnamed = doc.nodes.iter().filter(|n| n.label.is_none()).count();
        let adapters = doc
            .nodes
            .iter()
            .filter(|n| {
                n.label.is_none()
                    && matches!(n.kind, GateKind::Split { .. } | GateKind::Merge { .. })
            })
            .count();
        assert_eq!(unnamed, adapters, "{}", doc.name);
    }
    s.workspace_tool("save", None, false).unwrap();

    // Into an open workspace the modules are chips, the top one too.
    s.workspace_tool("new", Some("design"), false).unwrap();
    let out = s
        .import_rtl_tool(&rtl("top.json"), None, None, None, false, false)
        .unwrap();
    assert!(
        out.contains("imported") && out.contains("added chip top:"),
        "{out}"
    );
    let chips = s.workspace().unwrap().definitions.len();
    assert!(chips >= 2, "{out}");
    // Again: refused, or replaced in place.
    let e = s
        .import_rtl_tool(&rtl("top.json"), None, None, None, false, false)
        .unwrap_err();
    assert!(
        e.contains("already has chips named") && e.contains("\"replace\": true"),
        "{e}"
    );
    let out = s
        .import_rtl_tool(&rtl("top.json"), None, None, None, true, false)
        .unwrap();
    assert!(out.contains("replaced chip top:"), "{out}");
    assert_eq!(s.workspace().unwrap().definitions.len(), chips);

    let e = s
        .import_rtl_tool(&rtl("nothing.json"), None, None, None, false, false)
        .unwrap_err();
    assert!(e.contains("nothing.json"), "{e}");
    let e = s
        .import_rtl_tool(
            &rtl("top.json"),
            Some("nomodule"),
            None,
            Some("open"),
            false,
            false,
        )
        .unwrap_err();
    assert!(e.contains("nomodule"), "{e}");
    let _ = std::fs::remove_dir_all(dir);
}

/// Verilog to a tested gate-level chip: the loop the server is for. Runs
/// only where Yosys is installed.
#[test]
fn verilog_is_the_reference_a_gate_level_chip_is_tested_against() {
    use crate::editor::workspace::WorkspaceStore;
    use crate::mcp::session::Session;

    if std::process::Command::new("yosys")
        .arg("-V")
        .output()
        .is_err()
    {
        eprintln!("yosys is not installed: skipped");
        return;
    }
    let dir = super::scratch("verilog");
    let file = dir.join("adder.v");
    std::fs::write(
        &file,
        "module add_ref(input [7:0] A, input [7:0] B, input Cin, output [7:0] Sum, output Cout);\n\
         \x20 assign {Cout, Sum} = A + B + Cin;\n\
         endmodule\n\
         module counter(input clk, input en, output reg [3:0] q);\n\
         \x20 always @(posedge clk) if (en) q <= q + 4'd1;\n\
         endmodule\n",
    )
    .unwrap();
    let mut s = Session::new(WorkspaceStore::in_dir(&dir));
    s.workspace_tool("new", Some("adders"), false).unwrap();
    // The gate-level adder of the other tests, as chip `add8`.
    let Value::Object(mut request) = super::adder8_request() else {
        unreachable!()
    };
    let Value::Array(edits) = &mut request["edits"] else {
        unreachable!()
    };
    edits[2]["circuit"] = "add8".into();
    s.edit_tool(request, None, false).unwrap();

    let out = s
        .import_rtl_tool(&file, Some("add_ref"), None, None, false, false)
        .unwrap();
    assert!(out.contains("added chip add_ref:"), "{out}");
    let Value::Object(tests) = json!({"circuit": "add8", "ops": [{"tests": [
        {"name": "same as the Verilog", "equivalent": {"to": "add_ref", "samples": 2000}}]}]})
    else {
        unreachable!()
    };
    s.edit_tool(tests, None, false).unwrap();
    let out = s.test_tool(Some("add8"), None).unwrap();
    assert_eq!(out, "1 of 1 tests passed in 1 circuit(s) (2000 cases)\n");

    // A sequential module, run on its own.
    let out = s
        .import_rtl_tool(&file, Some("counter"), None, None, false, false)
        .unwrap();
    assert!(out.contains("added chip counter:"), "{out}");
    let view = s.view_tool(Some("counter"), None, None, None).unwrap();
    assert!(view.contains("ports: in clk en | out q[4]"), "{view}");

    // A Verilog error comes back as Yosys said it.
    std::fs::write(
        &file,
        "module broken(input a, output b); assign b = ; endmodule\n",
    )
    .unwrap();
    let e = s
        .import_rtl_tool(&file, None, None, None, false, false)
        .unwrap_err();
    assert!(
        e.starts_with("yosys failed:") && e.contains("syntax error"),
        "{e}"
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_large_circuit_gets_groups_that_make_chips() {
    // Four separate chains of 50 inverters each, off one input.
    let mut add = vec![json!({"name": "In", "type": "Input"})];
    let mut wire = Vec::new();
    for c in ["p", "q", "r", "s"] {
        add.push(json!({"name": c, "type": "Not", "count": 50}));
        add.push(json!({"name": format!("{c}_out"), "type": "Output"}));
        wire.push(json!(["In", format!("{c}[0]")]));
        wire.push(json!([format!("{c}[0..49]"), format!("{c}[1..50]")]));
        wire.push(json!([format!("{c}[49]"), format!("{c}_out")]));
    }
    let (ws, report) = edit(&empty(), json!({"ops": [{"add": add}, {"wire": wire}]}));
    let advice = report.advisories.join("\n");
    assert!(advice.contains("has 205 parts in one circuit"), "{advice}");
    assert!(
        advice.contains(
            "4 groups of parts would make chips, the largest of 50 parts with 2 nets crossing"
        ),
        "{advice}"
    );

    let text = view::groups(&ws, &ws.root);
    assert!(
        text.contains("group 1: 50 parts, 2 nets crossing\n  [\"p[0..50]\"]"),
        "{text}"
    );
    // A group is a make_chip away from being a chip.
    let (chipped, report) = edit(
        &ws,
        json!({"ops": [{"make_chip": {"parts": ["p[0..50]"], "name": "chain_p"}}]}),
    );
    assert_eq!(chipped.root.nodes.len(), 156, "{report}");
    assert_eq!(
        crate::mcp::batch::compress_refs(
            ["a[2]", "x", "a[0]", "a[1]", "a[5]", "b[3]"]
                .map(String::from)
                .to_vec()
        ),
        ["a[0..3]", "a[5]", "b[3]", "x"]
    );
    // A small circuit gets no advice.
    assert!(view::groups(&adder8(), &adder8().root).contains("0 group(s)"));
}

#[test]
fn a_chip_that_is_not_placed_yet_is_built_too() {
    // A loop inside a chip nobody uses is still reported.
    let (_, report) = edit(
        &empty(),
        json!({"circuit": "ring", "ops": [
            {"add": [{"name": "a", "type": "Not"}, {"name": "b", "type": "Not"}]},
            {"wire": [["a", "b"], ["b", "a"]]}
        ]}),
    );
    let errors = report.build.unwrap_err();
    assert!(
        errors[0].starts_with("chip \"ring\": a loop of gates with no state in it: a -> b -> a"),
        "{errors:?}"
    );
}
