//! RTL import (issue #42): a Yosys JSON netlist imports, compiles and
//! computes what it describes; the layout is deterministic, on the grid and
//! without overlaps; unsupported input is refused with a clear message.

use bevy::math::Vec2;
use serde_json::{Value, json};

use super::*;
use crate::editor::workspace::workspace_to_json;
use crate::sim::layout::{self, GRID, overlaps};
use crate::sim::rtl::{Import, ImportOptions, import_yosys_json};
use crate::sim::source::{self, Externals};

/// The checked-in fixture (hand-written in the `write_json` format; the
/// Verilog it stands for is `examples/rtl/top.v`).
const FIXTURE: &str = include_str!("../../../examples/rtl/top.json");

fn import(text: &str) -> Import {
    import_yosys_json(text, &ImportOptions::default()).unwrap_or_else(|e| panic!("{e}"))
}

/// Load an import and check it compiled with every wire connected.
fn open_import(import: &Import) -> App {
    let mut app = app();
    load(&mut app, import.workspace.clone());
    let error = &app.world().resource::<SimDiagnostics>().compile_error;
    assert!(error.is_none(), "the import compiles: {error:?}");
    assert!(
        wires(&mut app).iter().all(|(_, _, s)| s.connected),
        "every wire simulated"
    );
    app
}

fn circuits(ws: &WorkspaceDocument) -> Vec<&CircuitDocument> {
    let mut all: Vec<&CircuitDocument> = std::iter::once(&ws.root)
        .chain(ws.definitions.values().map(|d| &**d))
        .collect();
    all.sort_by_key(|c| c.id);
    all
}

// ─────────────────────────────────────────────────────────────────────────────
// The fixture
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn fixture_maps_modules_to_definitions() {
    let imported = import(FIXTURE);
    assert_eq!(imported.name, "top");
    assert_eq!(imported.source.root, "top");
    let names: Vec<&str> = imported
        .source
        .circuits
        .iter()
        .map(|c| c.name.as_str())
        .collect();
    assert_eq!(names, ["top", "adder4", "full_adder", "alu4", "counter"]);
    let ws = &imported.workspace;
    assert_eq!(ws.definitions.len(), 4, "one definition per module");
    let adder4 = ws
        .definitions
        .values()
        .find(|d| d.name == "adder4")
        .unwrap();
    let instances = adder4
        .nodes
        .iter()
        .filter(|n| matches!(n.kind, GateKind::SubCircuit { .. }))
        .count();
    assert_eq!(instances, 4, "four full adder instances");
    // Word-level cells keep their widths.
    let alu = ws.definitions.values().find(|d| d.name == "alu4").unwrap();
    assert!(alu.nodes.iter().any(|n| n.kind == GateKind::Add { w: 4 }));
    assert_eq!(
        alu.nodes
            .iter()
            .filter(|n| n.kind == GateKind::Mux { w: 4 })
            .count(),
        3
    );
    let counter = ws
        .definitions
        .values()
        .find(|d| d.name == "counter")
        .unwrap();
    assert!(
        counter
            .nodes
            .iter()
            .any(|n| n.kind == GateKind::Delay { w: 4 })
    );
    assert!(
        imported
            .warnings
            .iter()
            .any(|w| w.contains("clocked by the global tick")),
        "{:?}",
        imported.warnings
    );
}

#[test]
fn fixture_adds_like_arithmetic() {
    let imported = import(FIXTURE);
    let mut app = open_import(&imported);
    for a in 0..16u64 {
        for b in 0..16u64 {
            let got = eval(
                &mut app,
                &[("a", a), ("b", b)],
                &["sum", "cout", "total", "msb", "hi"],
            );
            let s = a + b;
            assert_eq!(
                got,
                [s & 15, s >> 4, s, (a >> 3) & (b >> 3), a >> 2],
                "a={a} b={b}"
            );
        }
    }
}

#[test]
fn fixture_alu_matches_its_truth_table() {
    let imported = import(FIXTURE);
    let mut app = open_import(&imported);
    for op in 0..4u64 {
        for a in [0u64, 1, 5, 9, 10, 15] {
            for b in [0u64, 3, 6, 12, 15] {
                let got = eval(&mut app, &[("op", op), ("a", a), ("b", b)], &["y"])[0];
                let want = match op {
                    0 => (a + b) & 15,
                    1 => a & b,
                    2 => a | b,
                    _ => !a & 15,
                };
                assert_eq!(got, want, "op={op} a={a} b={b}");
            }
        }
    }
}

#[test]
fn fixture_counter_counts_while_enabled() {
    let imported = import(FIXTURE);
    let mut app = open_import(&imported);
    let q = |app: &mut App| value_at(app, "q");
    assert_eq!(q(&mut app), 0, "state powers up at 0");
    set_input(&mut app, "en", 1);
    run(&mut app, 1);
    let mut last = q(&mut app);
    for _ in 0..20 {
        run(&mut app, 1);
        let now = q(&mut app);
        assert_eq!(now, (last + 1) & 15, "counts up by one per tick, wrapping");
        assert_eq!(value_at(&mut app, "max"), u64::from(now == 15));
        last = now;
    }
    set_input(&mut app, "en", 0);
    run(&mut app, 2);
    let held = q(&mut app);
    run(&mut app, 5);
    assert_eq!(q(&mut app), held, "holds while not enabled");
}

// ─────────────────────────────────────────────────────────────────────────────
// Layout and round trips
// ─────────────────────────────────────────────────────────────────────────────

/// Check every circuit: centres on the grid, no two boxes overlap, inputs
/// in the first column and outputs in the last.
fn assert_readable_layout(ws: &WorkspaceDocument) {
    for c in circuits(ws) {
        let boxes: Vec<(Vec2, Vec2)> = c
            .nodes
            .iter()
            .map(|n| (n.position(), n.kind.size()))
            .collect();
        assert_eq!(overlaps(&boxes), None, "{}: boxes overlap", c.name);
        for n in &c.nodes {
            assert_eq!(n.x % GRID, 0.0, "{}: x on the grid", c.name);
            assert_eq!(n.y % GRID, 0.0, "{}: y on the grid", c.name);
        }
        let xs = |pred: fn(&GateKind) -> bool| -> Vec<f32> {
            c.nodes
                .iter()
                .filter(|n| pred(&n.kind))
                .map(|n| n.x)
                .collect()
        };
        let inputs = xs(|k| matches!(k, GateKind::Input { .. }));
        let outputs = xs(|k| matches!(k, GateKind::Output { .. }));
        let others = xs(|k| !k.is_port());
        let min_other = others.iter().copied().fold(f32::INFINITY, f32::min);
        let max_other = others.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        assert!(
            inputs.iter().all(|&x| x < min_other),
            "{}: inputs left",
            c.name
        );
        assert!(
            outputs.iter().all(|&x| x > max_other),
            "{}: outputs right",
            c.name
        );
    }
}

#[test]
fn fixture_layout_is_deterministic_and_readable() {
    let first = import(FIXTURE);
    let second = import(FIXTURE);
    assert_eq!(first.source, second.source);
    assert_eq!(first.workspace, second.workspace);
    assert_readable_layout(&first.workspace);
}

#[test]
fn import_round_trips_as_version_6_and_version_5() {
    let imported = import(FIXTURE);
    // Version 6 text: canonical, and it expands to the same documents.
    let text = source::to_text(&imported.source);
    let parsed = source::parse(&text).unwrap();
    assert_eq!(parsed, imported.source);
    assert_eq!(source::to_text(&parsed), text);
    let expanded = source::expand(&parsed, "workspaces/top", &Externals::default()).unwrap();
    assert_eq!(expanded.workspace, imported.workspace);
    // No adapters are generated on expansion: the importer placed them all.
    let nodes: usize = imported.source.circuits.iter().map(|c| c.nodes.len()).sum();
    let expanded_nodes: usize = expanded.circuits.iter().map(|(_, c)| c.nodes.len()).sum();
    assert_eq!(nodes, expanded_nodes);

    // Version 5: save and load in a workspace folder.
    let dir = temp_dir();
    save_workspace_in(&dir, "top", &imported.workspace).unwrap();
    assert_eq!(read_workspace_in(&dir, "top").unwrap(), imported.workspace);
    assert!(
        workspace_to_json("top", &imported.workspace)
            .unwrap()
            .contains("\"version\": 5")
    );
    // And the version 6 text as a workspace file loads to the same.
    std::fs::create_dir_all(dir.join("top6")).unwrap();
    std::fs::write(dir.join("top6/manifest.json"), &text).unwrap();
    let loaded = read_workspace_in(&dir, "top6").unwrap();
    assert_eq!(circuits(&loaded).len(), 5);
    std::fs::remove_dir_all(dir).ok();

    // Through the editor: what it reads back is what was loaded.
    let mut app = open_import(&imported);
    assert_eq!(read_back(&mut app), imported.workspace);
}

/// The layout also places any existing circuit: every demo, laid out
/// again, is still readable and lays out the same way twice.
#[test]
fn layout_circuit_places_every_demo() {
    for demo in &demos().demos {
        let mut ws = (*demo.workspace).clone();
        layout::layout_circuit(&mut ws.root);
        let mut again = ws.root.clone();
        layout::layout_circuit(&mut again);
        assert_eq!(again, ws.root, "{}", demo.id);
        let boxes: Vec<(Vec2, Vec2)> = ws
            .root
            .nodes
            .iter()
            .map(|n| (n.position(), n.kind.size()))
            .collect();
        assert_eq!(overlaps(&boxes), None, "{}", demo.id);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Cell mapping, one cell type at a time
// ─────────────────────────────────────────────────────────────────────────────

/// A one-module netlist. Ports are `(name, direction, bits)`; cells are
/// `(type, parameters, connections)`.
fn netlist(ports: &[(&str, &str, Value)], cells: &[(&str, Value, Value)]) -> String {
    let ports: serde_json::Map<String, Value> = ports
        .iter()
        .map(|(n, d, b)| (n.to_string(), json!({"direction": d, "bits": b})))
        .collect();
    let cells: serde_json::Map<String, Value> = cells
        .iter()
        .enumerate()
        .map(|(i, (t, p, c))| {
            (
                format!("$auto${i}"),
                json!({"hide_name": 1, "type": t, "parameters": p, "connections": c}),
            )
        })
        .collect();
    json!({"modules": {"m": {"ports": ports, "cells": cells}}}).to_string()
}

/// Bits `start .. start + n`.
fn bits(start: u64, n: u64) -> Value {
    json!((start..start + n).collect::<Vec<_>>())
}

fn binop(ty: &str, aw: u64, bw: u64, yw: u64, signed: bool) -> String {
    let s = u64::from(signed);
    netlist(
        &[
            ("a", "input", bits(2, aw)),
            ("b", "input", bits(20, bw)),
            ("y", "output", bits(40, yw)),
        ],
        &[(
            ty,
            json!({"A_SIGNED": s, "B_SIGNED": s, "A_WIDTH": aw, "B_WIDTH": bw, "Y_WIDTH": yw}),
            json!({"A": bits(2, aw), "B": bits(20, bw), "Y": bits(40, yw)}),
        )],
    )
}

#[test]
fn word_cells_compute() {
    type Op = fn(u64, u64) -> u64;
    let cases: [(&str, Op); 6] = [
        ("$and", |a, b| a & b),
        ("$or", |a, b| a | b),
        ("$xor", |a, b| a ^ b),
        ("$xnor", |a, b| !(a ^ b) & 15),
        ("$add", |a, b| (a + b) & 15),
        ("$sub", |a, b| a.wrapping_sub(b) & 15),
    ];
    for (ty, f) in cases {
        let mut app = open_import(&import(&binop(ty, 4, 4, 4, false)));
        for (a, b) in [(0, 0), (3, 5), (9, 12), (15, 1), (7, 7), (2, 11)] {
            let got = eval(&mut app, &[("a", a), ("b", b)], &["y"])[0];
            assert_eq!(got, f(a, b), "{ty} a={a} b={b}");
        }
    }
    for (ty, want) in [("$eq", 1u64), ("$ne", 0)] {
        let mut app = open_import(&import(&binop(ty, 4, 4, 2, false)));
        for (a, b) in [(3, 3), (3, 4), (0, 15)] {
            let got = eval(&mut app, &[("a", a), ("b", b)], &["y"])[0];
            assert_eq!(
                got,
                if a == b { want } else { 1 - want },
                "{ty} a={a} b={b}"
            );
        }
    }
}

#[test]
fn operands_are_extended_like_verilog() {
    // Unsigned: zero extension; signed: sign extension.
    let mut app = open_import(&import(&binop("$add", 2, 3, 6, false)));
    assert_eq!(eval(&mut app, &[("a", 3), ("b", 7)], &["y"])[0], 10);
    let mut app = open_import(&import(&binop("$add", 2, 3, 6, true)));
    // -1 + -1 = -2 in six bits.
    assert_eq!(eval(&mut app, &[("a", 3), ("b", 7)], &["y"])[0], 0b111110);
    // Truncation: only the low Y_WIDTH bits.
    let mut app = open_import(&import(&binop("$or", 4, 4, 2, false)));
    assert_eq!(
        eval(&mut app, &[("a", 0b1101), ("b", 0b0010)], &["y"])[0],
        0b11
    );
}

#[test]
fn reductions_and_unary_cells_compute() {
    type Op = fn(u64) -> u64;
    let cases: [(&str, Op); 8] = [
        ("$reduce_and", |a| u64::from(a == 15)),
        ("$reduce_or", |a| u64::from(a != 0)),
        ("$reduce_bool", |a| u64::from(a != 0)),
        ("$reduce_xor", |a| u64::from(a.count_ones() % 2 == 1)),
        ("$reduce_xnor", |a| u64::from(a.count_ones() % 2 == 0)),
        ("$logic_not", |a| u64::from(a == 0)),
        ("$not", |a| !a & 15),
        ("$pos", |a| a),
    ];
    for (ty, f) in cases {
        let yw = if matches!(ty, "$not" | "$pos") { 4 } else { 1 };
        let text = netlist(
            &[("a", "input", bits(2, 4)), ("y", "output", bits(40, yw))],
            &[(
                ty,
                json!({"A_SIGNED": 0, "A_WIDTH": 4, "Y_WIDTH": yw}),
                json!({"A": bits(2, 4), "Y": bits(40, yw)}),
            )],
        );
        let mut app = open_import(&import(&text));
        for a in [0, 1, 6, 7, 15] {
            assert_eq!(eval(&mut app, &[("a", a)], &["y"])[0], f(a), "{ty} a={a}");
        }
    }
}

#[test]
fn gate_level_cells_compute() {
    type Op = fn(u64, u64) -> u64;
    let cases: [(&str, Op); 8] = [
        ("$_AND_", |a, b| a & b),
        ("$_OR_", |a, b| a | b),
        ("$_XOR_", |a, b| a ^ b),
        ("$_NAND_", |a, b| 1 - (a & b)),
        ("$_NOR_", |a, b| 1 - (a | b)),
        ("$_XNOR_", |a, b| 1 - (a ^ b)),
        ("$_ANDNOT_", |a, b| a & (1 - b)),
        ("$_ORNOT_", |a, b| a | (1 - b)),
    ];
    for (ty, f) in cases {
        let text = netlist(
            &[
                ("a", "input", json!([2])),
                ("b", "input", json!([3])),
                ("y", "output", json!([4])),
            ],
            &[(ty, json!({}), json!({"A": [2], "B": [3], "Y": [4]}))],
        );
        let mut app = open_import(&import(&text));
        for a in 0..2 {
            for b in 0..2 {
                let got = eval(&mut app, &[("a", a), ("b", b)], &["y"])[0];
                assert_eq!(got, f(a, b), "{ty} a={a} b={b}");
            }
        }
    }
    for (ty, invert) in [("$_MUX_", 0), ("$_NMUX_", 1)] {
        let text = netlist(
            &[
                ("a", "input", json!([2])),
                ("b", "input", json!([3])),
                ("s", "input", json!([4])),
                ("y", "output", json!([5])),
            ],
            &[(
                ty,
                json!({}),
                json!({"A": [2], "B": [3], "S": [4], "Y": [5]}),
            )],
        );
        let mut app = open_import(&import(&text));
        for (a, b, s) in [(0, 1, 0), (0, 1, 1), (1, 0, 0), (1, 0, 1)] {
            let got = eval(&mut app, &[("a", a), ("b", b), ("s", s)], &["y"])[0];
            assert_eq!(got, (if s == 1 { b } else { a }) ^ invert, "{ty}");
        }
    }
}

#[test]
fn flip_flops_hold_state() {
    // $dffe with an active-low enable: loads while en is 0.
    let text = netlist(
        &[
            ("clk", "input", json!([2])),
            ("en", "input", json!([3])),
            ("d", "input", bits(4, 3)),
            ("q", "output", bits(10, 3)),
        ],
        &[(
            "$dffe",
            json!({"WIDTH": 3, "CLK_POLARITY": 1, "EN_POLARITY": 0}),
            json!({"CLK": [2], "EN": [3], "D": bits(4, 3), "Q": bits(10, 3)}),
        )],
    );
    let imported = import(&text);
    let counter = &imported.workspace.root;
    assert!(
        counter
            .nodes
            .iter()
            .any(|n| n.kind == GateKind::Register { w: 3 })
    );
    let mut app = open_import(&imported);
    set_input(&mut app, "en", 1);
    eval(&mut app, &[("d", 5)], &[]);
    run(&mut app, 2);
    assert_eq!(value_at(&mut app, "q"), 0, "disabled: holds 0");
    set_input(&mut app, "en", 0);
    run(&mut app, 2);
    assert_eq!(value_at(&mut app, "q"), 5, "enabled (active low): loads d");
    set_input(&mut app, "en", 1);
    set_input(&mut app, "d", 2);
    run(&mut app, 3);
    assert_eq!(value_at(&mut app, "q"), 5, "holds again");

    // $_DFF_P_: a one-bit delay.
    let text = netlist(
        &[
            ("c", "input", json!([2])),
            ("d", "input", json!([3])),
            ("q", "output", json!([4])),
        ],
        &[("$_DFF_P_", json!({}), json!({"C": [2], "D": [3], "Q": [4]}))],
    );
    let mut app = open_import(&import(&text));
    set_input(&mut app, "d", 1);
    run(&mut app, 2);
    assert_eq!(value_at(&mut app, "q"), 1);
}

#[test]
fn constants_slices_and_concatenations_become_adapters() {
    // y = {a[1:0], 2'b10, a[3]}, z = a[3:2], w = 1'bx.
    let text = netlist(
        &[
            ("a", "input", bits(2, 4)),
            ("y", "output", json!([5, "0", "1", 2, 3])),
            ("z", "output", json!([4, 5])),
            ("w", "output", json!(["x"])),
        ],
        &[],
    );
    let imported = import(&text);
    let root = &imported.workspace.root;
    let count = |pred: fn(&GateKind) -> bool| root.nodes.iter().filter(|n| pred(&n.kind)).count();
    assert_eq!(count(|k| matches!(k, GateKind::Merge { .. })), 1);
    assert_eq!(count(|k| matches!(k, GateKind::Const { .. })), 2);
    assert_eq!(count(|k| matches!(k, GateKind::Split { .. })), 3);
    assert!(
        imported.warnings.iter().any(|w| w.contains("x/z")),
        "{:?}",
        imported.warnings
    );
    assert_readable_layout(&imported.workspace);
    let mut app = open_import(&imported);
    let got = eval(&mut app, &[("a", 0b1001)], &["y", "z", "w"]);
    assert_eq!(got, [0b01_10_1 /* a[1:0]=01, 10, a[3]=1 */, 0b10, 0]);
}

// ─────────────────────────────────────────────────────────────────────────────
// What is refused
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn unsupported_input_is_refused_with_a_message() {
    let refused = |text: &str, options: &ImportOptions| -> String {
        match import_yosys_json(text, options) {
            Ok(_) => panic!("imported"),
            Err(e) => e.to_string(),
        }
    };
    let none = ImportOptions::default();
    let mul = binop("$mul", 4, 4, 8, false);
    let message = refused(&mul, &none);
    for part in [
        "module \"m\"",
        "cell \"$auto$0\"",
        "$mul is not supported",
        "$add",
    ] {
        assert!(message.contains(part), "{part:?} in {message:?}");
    }
    let wide = netlist(&[("a", "input", bits(2, 65))], &[]);
    assert!(refused(&wide, &none).contains("65 bits wide; words are 1 to 64 bits"));
    let inout = netlist(&[("a", "inout", bits(2, 1))], &[]);
    assert!(refused(&inout, &none).contains("inout ports are not supported"));
    assert!(refused("[1, 2]", &none).contains("no \"modules\""));
    assert!(refused("{", &none).contains("not valid JSON"));

    // Hierarchy: a missing module, a black box, several tops.
    let missing = json!({"modules": {"m": {"ports": {}, "cells": {
        "u": {"type": "nowhere", "connections": {}}}}}})
    .to_string();
    assert!(refused(&missing, &none).contains("module \"nowhere\", which is not in the netlist"));
    let blackbox = json!({"modules": {
        "m": {"ports": {}, "cells": {"u": {"type": "bb", "connections": {}}}},
        "bb": {"attributes": {"blackbox": "00000000000000000000000000000001"}, "ports": {}}}})
    .to_string();
    assert!(refused(&blackbox, &none).contains("a black box"));
    let two = json!({"modules": {"p": {"ports": {}}, "q": {"ports": {}}}}).to_string();
    assert!(refused(&two, &none).contains("several top-level modules (p, q)"));
    let pick = ImportOptions {
        top: Some("q".into()),
        name: Some("picked".into()),
    };
    let imported = import_yosys_json(&two, &pick).unwrap();
    assert_eq!(
        (imported.name.as_str(), imported.source.root.as_str()),
        ("picked", "q")
    );
    let unknown = ImportOptions {
        top: Some("r".into()),
        name: None,
    };
    assert!(refused(&two, &unknown).contains("no module \"r\""));
}
