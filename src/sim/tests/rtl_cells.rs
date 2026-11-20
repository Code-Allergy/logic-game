//! RTL import of the cells beyond the first set (arithmetic, shifts,
//! comparisons, selection, flip-flops with resets, latches, memories):
//! every fixture module in `examples/rtl/` imports, opens in the editor and
//! computes what its Verilog says, checked against an oracle written in
//! Rust (exhaustive inputs for small widths, seeded random inputs for wide
//! ones).
//!
//! The checks run the import on the compiled simulator directly (a
//! `logi_sim` design built from the imported version 6 file), which is fast
//! enough for exhaustive inputs; each module is also opened in the editor
//! once.

use std::collections::HashMap;
use std::sync::Arc;

use logi_sim::compile::{self, Options};
use logi_sim::netlist::{DefId, Design, NodeId};
use logi_sim::sim::Sim;
use logi_sim::testkit::Rng;
use serde_json::{Value, json};

use super::rtl::{assert_readable_layout, bits, netlist, open_import};
use crate::sim::rtl::{Import, ImportOptions, import_yosys_json};
use crate::sim::source::{NodeBody, SourceFile, primitive_kind};
use crate::sim::{CircuitLibrary, GateKind};

const ARITH: &str = include_str!("../../../examples/rtl/arith.json");
const SHIFT: &str = include_str!("../../../examples/rtl/shift.json");
const COMPARE: &str = include_str!("../../../examples/rtl/compare.json");
const SELECT: &str = include_str!("../../../examples/rtl/select.json");
const FLOPS: &str = include_str!("../../../examples/rtl/flops.json");
const MEMORY: &str = include_str!("../../../examples/rtl/memory.json");
const MAC: &str = include_str!("../../../examples/rtl/mac.json");

const FIXTURES: [(&str, &str); 6] = [
    ("arith", ARITH),
    ("shift", SHIFT),
    ("compare", COMPARE),
    ("select", SELECT),
    ("flops", FLOPS),
    ("memory", MEMORY),
];

// ─────────────────────────────────────────────────────────────────────────────
// Harness
// ─────────────────────────────────────────────────────────────────────────────

fn import_top(text: &str, top: &str) -> Import {
    let options = ImportOptions {
        top: Some(top.into()),
        name: None,
    };
    import_yosys_json(text, &options).unwrap_or_else(|e| panic!("{top}: {e}"))
}

/// The module names of a fixture file.
fn modules(text: &str) -> Vec<String> {
    let value: Value = serde_json::from_str(text).unwrap();
    value["modules"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect()
}

/// A `logi_sim` design of an import: one definition per circuit, ports
/// named after their labels.
fn design(source: &SourceFile) -> Design {
    let mut design = Design::new(source.root.clone());
    let root = design.root;
    let mut defs = HashMap::from([(source.root.clone(), root)]);
    build(&mut design, source, &source.root, root, &mut defs);
    design
}

fn build(
    design: &mut Design,
    source: &SourceFile,
    name: &str,
    def: DefId,
    defs: &mut HashMap<String, DefId>,
) {
    let circuit = source.circuits.iter().find(|c| c.name == name).unwrap();
    let library = CircuitLibrary::default();
    // Node name -> (id, input pin names, output pin names).
    let mut nodes: HashMap<&str, (NodeId, Vec<String>, Vec<String>)> = HashMap::new();
    for n in &circuit.nodes {
        let entry = match &n.body {
            NodeBody::Primitive { ty, params } => {
                let kind = primitive_kind(ty, params).unwrap();
                let id = design.add_node(def, kind.to_netlist().unwrap()).unwrap();
                if kind.is_port() {
                    let port = n.label.clone().unwrap_or_else(|| n.name.clone());
                    design.set_name(def, id, Some(port)).unwrap();
                }
                let (ins, outs) = library.pin_names(&kind);
                (id, ins, outs)
            }
            NodeBody::Use(child) => {
                let child_def = match defs.get(child) {
                    Some(&d) => d,
                    None => {
                        let d = design.add_definition(child.clone());
                        defs.insert(child.clone(), d);
                        build(design, source, child, d, defs);
                        d
                    }
                };
                let id = design.add_instance(def, child_def).unwrap();
                let child = &design.defs[child_def];
                let names = |ports: &[NodeId]| -> Vec<String> {
                    ports
                        .iter()
                        .map(|&p| child.name_of(p).unwrap().to_string())
                        .collect()
                };
                (id, names(&child.inputs), names(&child.outputs))
            }
        };
        nodes.insert(n.name.as_str(), entry);
    }
    let pin = |end: &str, output: bool| -> (NodeId, u8) {
        let (node, pin) = end.rsplit_once('.').unwrap();
        let (id, ins, outs) = &nodes[node];
        let names = if output { outs } else { ins };
        let i = names.iter().position(|n| n == pin).unwrap();
        (*id, i as u8)
    };
    for wire in &circuit.wires {
        let (from, from_pin) = pin(&wire[0], true);
        let net = design.defs[def].output_net(from, from_pin).unwrap();
        for sink in &wire[1..] {
            design.connect(def, net, pin(sink, false)).unwrap();
        }
    }
}

/// The import of `top` in `text`, compiled.
fn compiled(import: &Import) -> Sim {
    let design = design(&import.source);
    let program = compile::compile(&design, &Options::default())
        .unwrap_or_else(|e| panic!("the import compiles: {e:?}"));
    Sim::new(Arc::new(program))
}

fn sim(text: &str, top: &str) -> Sim {
    compiled(&import_top(text, top))
}

/// Set inputs, run one tick (the logic settles with them), read outputs.
/// State takes what the logic computed at the following tick.
fn step(sim: &mut Sim, inputs: &[(&str, u64)], outputs: &[&str]) -> Vec<u64> {
    for &(name, value) in inputs {
        sim.set(name, value);
    }
    sim.tick();
    outputs.iter().map(|name| sim.get(name)).collect()
}

/// `v` (`w` bits) as a signed number.
fn sext(v: u64, w: u32) -> i64 {
    ((v << (64 - w)) as i64) >> (64 - w)
}

fn mask(w: u32) -> u64 {
    if w >= 64 { u64::MAX } else { (1 << w) - 1 }
}

fn count(import: &Import, pred: impl Fn(&GateKind) -> bool) -> usize {
    import
        .workspace
        .root
        .nodes
        .iter()
        .filter(|n| pred(&n.kind))
        .count()
}

// ─────────────────────────────────────────────────────────────────────────────
// Every fixture module
// ─────────────────────────────────────────────────────────────────────────────

/// Each module imports deterministically, is laid out readably, and opens
/// in the editor with every wire connected.
#[test]
fn every_fixture_module_imports_and_opens() {
    for (file, text) in FIXTURES.iter().chain(&[("mac", MAC)]) {
        for top in modules(text) {
            let first = import_top(text, &top);
            let second = import_top(text, &top);
            assert_eq!(first.workspace, second.workspace, "{file}/{top}");
            assert_readable_layout(&first.workspace);
            let _ = open_import(&first);
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Arithmetic
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn multiplication_matches_the_oracle() {
    let mut s = sim(ARITH, "mul_u");
    for a in 0..64 {
        for b in 0..64 {
            let got = step(&mut s, &[("a", a), ("b", b)], &["y"])[0];
            assert_eq!(got, a * b, "mul_u {a}*{b}");
        }
    }
    let mut s = sim(ARITH, "mul_s");
    for a in 0..32 {
        for b in 0..16 {
            let got = step(&mut s, &[("a", a), ("b", b)], &["y"])[0];
            let want = (sext(a, 5) * sext(b, 4)) as u64 & mask(9);
            assert_eq!(got, want, "mul_s {a}*{b}");
        }
    }
    let mut s = sim(ARITH, "mul_w");
    let mut rng = Rng::new(0x6d75_6c77);
    for _ in 0..2000 {
        let (a, b) = (rng.next_u64() & mask(32), rng.next_u64() & mask(32));
        let got = step(&mut s, &[("a", a), ("b", b)], &["y"])[0];
        assert_eq!(got, a.wrapping_mul(b) & mask(32), "mul_w {a}*{b}");
    }
    // A constant factor 5 (101): two stages, one adder, no gating.
    let imported = import_top(ARITH, "mul_k");
    assert_eq!(count(&imported, |k| matches!(k, GateKind::Add { .. })), 1);
    assert_eq!(
        count(&imported, |k| matches!(k, GateKind::Switch { .. })),
        0
    );
    let mut s = compiled(&imported);
    for a in 0..256 {
        assert_eq!(step(&mut s, &[("a", a)], &["y"])[0], (a * 5) & mask(10));
    }
}

#[test]
fn division_matches_the_oracle() {
    // Division by 0 is x in Yosys: not checked.
    let mut s = sim(ARITH, "divmod_u");
    for a in 0..256 {
        for b in 1..16 {
            let got = step(&mut s, &[("a", a), ("b", b)], &["q", "r"]);
            assert_eq!(got, [a / b, a % b], "divmod_u {a}/{b}");
        }
    }
    // Signed: truncating, the remainder takes the sign of a (-32 / -1
    // overflows to -32, as in six-bit Verilog).
    let mut s = sim(ARITH, "divmod_s");
    for a in 0..64 {
        for b in 1..16 {
            let (sa, sb) = (sext(a, 6), sext(b, 4));
            let got = step(&mut s, &[("a", a), ("b", b)], &["q", "r"]);
            let want = [(sa / sb) as u64 & mask(6), (sa % sb) as u64 & mask(4)];
            assert_eq!(got, want, "divmod_s {sa}/{sb}");
        }
    }
    let mut s = sim(ARITH, "divmod_w");
    let mut rng = Rng::new(0x6469_7677);
    for i in 0..1500 {
        let a = rng.next_u64() & mask(24);
        // Small divisors too, so that quotients are large.
        let b = (rng.next_u64() & mask(if i % 2 == 0 { 12 } else { 4 })).max(1);
        let got = step(&mut s, &[("a", a), ("b", b)], &["q", "r"]);
        assert_eq!(got, [a / b, a % b], "divmod_w {a}/{b}");
    }
}

#[test]
fn negation_and_logic_operators_match_the_oracle() {
    let mut s = sim(ARITH, "neg_logic");
    for a in 0..16 {
        for b in 0..16 {
            let got = step(&mut s, &[("a", a), ("b", b)], &["n", "land", "lor"]);
            let want = [
                (-sext(a, 4)) as u64 & mask(6),
                u64::from(a != 0 && b != 0),
                u64::from(a != 0 || b != 0),
            ];
            assert_eq!(got, want, "a={a} b={b}");
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Shifts
// ─────────────────────────────────────────────────────────────────────────────

/// Bit `pos` of the `w`-bit `v`, 0 outside it.
fn bit_at(v: u64, w: i64, pos: i64) -> u64 {
    if (0..w).contains(&pos) {
        v >> pos & 1
    } else {
        0
    }
}

#[test]
fn shifts_match_the_oracle() {
    type Oracle = fn(u64, u64) -> u64;
    let cases: [(&str, &str, u32, u32, Oracle); 8] = [
        ("shl_u", "b", 8, 4, |a, b| a << b & mask(8)),
        ("shr_u", "b", 8, 4, |a, b| a >> b),
        // Arithmetic: the sign comes in from the top, into 10 bits.
        ("sshr_s", "b", 8, 4, |a, b| {
            (sext(a, 8) >> b) as u64 & mask(10)
        }),
        ("sshl_s", "b", 6, 3, |a, b| {
            (sext(a, 6) << b) as u64 & mask(8)
        }),
        // Logical shift of the value sign-extended to 8 bits.
        ("shr_s", "b", 6, 3, |a, b| {
            (sext(a, 6) as u64 & mask(8)) >> b
        }),
        ("part", "i", 16, 4, |a, i| a >> i & 15),
        ("part_s", "i", 16, 5, |a, i| {
            (0..4).fold(0, |y, j| y | bit_at(a, 16, sext(i, 5) + j) << j)
        }),
        ("shift_s", "b", 8, 4, |a, b| {
            let wide = sext(a, 8) as u64 & mask(10);
            (0..10).fold(0, |y, j| y | bit_at(wide, 10, j + sext(b, 4)) << j)
        }),
    ];
    let mut rng = Rng::new(0x7368_6966);
    for (top, amount, aw, bw, oracle) in cases {
        let mut s = sim(SHIFT, top);
        // Exhaustive in the amount; all values of a up to 8 bits, else
        // random ones (and the extremes).
        let values: Vec<u64> = if aw <= 8 {
            (0..1 << aw).collect()
        } else {
            let mut v: Vec<u64> = (0..200).map(|_| rng.next_u64() & mask(aw)).collect();
            v.extend([0, mask(aw), 1, 1 << (aw - 1)]);
            v
        };
        for &a in &values {
            for b in 0..1 << bw {
                let got = step(&mut s, &[("a", a), (amount, b)], &["y"])[0];
                assert_eq!(got, oracle(a, b), "{top}: a={a:#x} {amount}={b}");
            }
        }
    }
    let mut s = sim(SHIFT, "shl_w");
    for _ in 0..1000 {
        let (a, b) = (rng.next_u64() & mask(32), rng.next_u64() & mask(6));
        let got = step(&mut s, &[("a", a), ("b", b)], &["y"])[0];
        assert_eq!(got, a << b & mask(48), "shl_w {a:#x} << {b}");
    }
    // Constant shifts are wiring only.
    let imported = import_top(SHIFT, "shift_k");
    assert_eq!(count(&imported, |k| matches!(k, GateKind::Mux { .. })), 0);
    let mut s = compiled(&imported);
    for a in 0..256 {
        let got = step(&mut s, &[("a", a)], &["y", "z"]);
        assert_eq!(got, [a << 3, a >> 2]);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Comparisons
// ─────────────────────────────────────────────────────────────────────────────

fn compare_oracle(x: i64, y: i64) -> [u64; 6] {
    [x < y, x <= y, x > y, x >= y, x == y, x != y].map(u64::from)
}

#[test]
fn comparisons_match_the_oracle() {
    let outs = ["lt", "le", "gt", "ge", "eq", "ne"];
    let mut u = sim(COMPARE, "cmp_u");
    let mut s = sim(COMPARE, "cmp_s");
    let mut mix = sim(COMPARE, "cmp_mix");
    for a in 0..16 {
        for b in 0..32 {
            let got = step(&mut u, &[("a", a), ("b", b)], &outs);
            assert_eq!(got, compare_oracle(a as i64, b as i64), "unsigned {a} {b}");
            let got = step(&mut s, &[("a", a), ("b", b)], &outs);
            let want = compare_oracle(sext(a, 4), sext(b, 5));
            assert_eq!(got, want, "signed {} {}", sext(a, 4), sext(b, 5));
            // Signed with unsigned: both zero-extended.
            let got = step(&mut mix, &[("a", a), ("b", b)], &["lt", "ge"]);
            assert_eq!(got, [u64::from(a < b), u64::from(a >= b)], "mixed {a} {b}");
        }
    }
    let mut w = sim(COMPARE, "cmp_w");
    let mut rng = Rng::new(0x636d_7077);
    let edges = [0, 1, mask(39), 1 << 39, mask(40)];
    let mut pairs: Vec<(u64, u64)> = (0..2000)
        .map(|_| (rng.next_u64() & mask(40), rng.next_u64() & mask(40)))
        .collect();
    pairs.extend(edges.iter().flat_map(|&a| edges.map(|b| (a, b))));
    for (a, b) in pairs {
        let got = step(&mut w, &[("a", a), ("b", b)], &["lt", "ge"]);
        let (x, y) = (sext(a, 40), sext(b, 40));
        assert_eq!(got, [u64::from(x < y), u64::from(x >= y)], "{x} {y}");
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Selection
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn selection_cells_match_the_oracle() {
    let mut rng = Rng::new(0x7365_6c65);
    let mut s = sim(SELECT, "pmux");
    for _ in 0..50 {
        let w: Vec<u64> = (0..4).map(|_| rng.next_u64() & 15).collect();
        for sel in 0..4 {
            let inputs = [
                ("a", w[0]),
                ("b", w[1]),
                ("c", w[2]),
                ("d", w[3]),
                ("sel", sel),
            ];
            assert_eq!(step(&mut s, &inputs, &["y"])[0], w[sel as usize]);
        }
    }
    // One-hot (or no) select bits; several set is x in Yosys.
    let mut s = sim(SELECT, "pmux_1h");
    for _ in 0..50 {
        let (a, b) = (rng.next_u64() & 15, rng.next_u64() & mask(12));
        for (sel, want) in [(0, a), (1, b & 15), (2, b >> 4 & 15), (4, b >> 8)] {
            assert_eq!(
                step(&mut s, &[("a", a), ("b", b), ("s", sel)], &["y"])[0],
                want
            );
        }
    }
    let mut s = sim(SELECT, "bmux");
    for _ in 0..50 {
        let words = rng.next_u64() & mask(32);
        for sel in 0..8 {
            let got = step(&mut s, &[("words", words), ("sel", sel)], &["y"])[0];
            assert_eq!(got, words >> (4 * sel) & 15);
        }
    }
    let mut s = sim(SELECT, "demux");
    for a in 0..16 {
        for sel in 0..4 {
            let got = step(&mut s, &[("a", a), ("s", sel)], &["y"])[0];
            assert_eq!(got, a << (4 * sel));
        }
    }
    let mut s = sim(SELECT, "bitwise");
    for l in 0..64u64 {
        let (a, b, sel) = (
            rng.next_u64() & 15,
            rng.next_u64() & 15,
            rng.next_u64() & 15,
        );
        let inputs = [("a", a), ("b", b), ("s", sel), ("l", l)];
        let got = step(&mut s, &inputs, &["y", "e", "f"]);
        let want = [
            (a & !sel | b & sel) & 15,
            !(a ^ b) & 15,
            u64::from(l.count_ones() >= 3),
        ];
        assert_eq!(got, want, "a={a} b={b} s={sel} l={l}");
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Cells without a fixture of their own
// ─────────────────────────────────────────────────────────────────────────────

/// `$alu` (from `alumacc`): Y = A + (BI ? ~B : B) + CI, X = A ^ B', CO the
/// carry out of every bit. Unused X and CO logic is left out.
#[test]
fn alu_cell_computes_sum_xor_and_carries() {
    let text = |used: bool| {
        let mut ports = vec![
            ("a", "input", bits(2, 4)),
            ("b", "input", bits(10, 4)),
            ("bi", "input", json!([20])),
            ("ci", "input", json!([21])),
            ("y", "output", bits(30, 4)),
        ];
        if used {
            ports.push(("x", "output", bits(40, 4)));
            ports.push(("co", "output", bits(50, 4)));
        }
        netlist(
            &ports,
            &[(
                "$alu",
                json!({"A_SIGNED": 0, "B_SIGNED": 0, "A_WIDTH": 4, "B_WIDTH": 4, "Y_WIDTH": 4}),
                json!({"A": bits(2, 4), "B": bits(10, 4), "BI": [20], "CI": [21],
                       "Y": bits(30, 4), "X": bits(40, 4), "CO": bits(50, 4)}),
            )],
        )
    };
    let lean = import_yosys_json(&text(false), &ImportOptions::default()).unwrap();
    assert_eq!(
        count(&lean, |k| matches!(k, GateKind::Xor { .. })),
        1,
        "only B ^ BI"
    );
    let imported = import_yosys_json(&text(true), &ImportOptions::default()).unwrap();
    let mut s = compiled(&imported);
    for a in 0..16u64 {
        for b in 0..16u64 {
            for bi in 0..2 {
                for ci in 0..2 {
                    let inputs = [("a", a), ("b", b), ("bi", bi), ("ci", ci)];
                    let got = step(&mut s, &inputs, &["y", "x", "co"]);
                    let bb = if bi == 1 { !b & 15 } else { b };
                    let co = (0..4).fold(0, |co, i| {
                        let m = mask(i + 1);
                        co | ((a & m) + (bb & m) + ci) >> (i + 1) << i
                    });
                    assert_eq!(got, [(a + bb + ci) & 15, a ^ bb, co], "{inputs:?}");
                }
            }
        }
    }
}

/// `$slice`, `$concat` and the gate-level and-or-invert cells.
#[test]
fn slices_concatenations_and_aoi_gates_compute() {
    let text = netlist(
        &[
            ("a", "input", bits(2, 4)),
            ("b", "input", bits(10, 2)),
            ("s", "output", bits(20, 2)),
            ("c", "output", bits(30, 6)),
        ],
        &[
            (
                "$slice",
                json!({"OFFSET": 1, "A_WIDTH": 4, "Y_WIDTH": 2}),
                json!({"A": bits(2, 4), "Y": bits(20, 2)}),
            ),
            (
                "$concat",
                json!({"A_WIDTH": 4, "B_WIDTH": 2}),
                json!({"A": bits(2, 4), "B": bits(10, 2), "Y": bits(30, 6)}),
            ),
        ],
    );
    let mut s = compiled(&import_yosys_json(&text, &ImportOptions::default()).unwrap());
    for a in 0..16 {
        for b in 0..4 {
            let got = step(&mut s, &[("a", a), ("b", b)], &["s", "c"]);
            assert_eq!(got, [a >> 1 & 3, b << 4 | a]);
        }
    }
    type Gate = fn(u64, u64, u64, u64) -> u64;
    let gates: [(&str, Gate); 4] = [
        ("$_AOI3_", |a, b, c, _| 1 ^ (a & b | c)),
        ("$_OAI3_", |a, b, c, _| 1 ^ ((a | b) & c)),
        ("$_AOI4_", |a, b, c, d| 1 ^ (a & b | c & d)),
        ("$_OAI4_", |a, b, c, d| 1 ^ ((a | b) & (c | d))),
    ];
    for (ty, f) in gates {
        let mut conns = json!({"A": [2], "B": [3], "C": [4], "Y": [6]});
        if ty.ends_with("4_") {
            conns["D"] = json!([5]);
        }
        let text = netlist(
            &[
                ("a", "input", json!([2])),
                ("b", "input", json!([3])),
                ("c", "input", json!([4])),
                ("d", "input", json!([5])),
                ("y", "output", json!([6])),
            ],
            &[(ty, json!({}), conns)],
        );
        let mut s = compiled(&import_yosys_json(&text, &ImportOptions::default()).unwrap());
        for v in 0..16u64 {
            let (a, b, c, d) = (v & 1, v >> 1 & 1, v >> 2 & 1, v >> 3);
            let got = step(&mut s, &[("a", a), ("b", b), ("c", c), ("d", d)], &["y"])[0];
            assert_eq!(got, f(a, b, c, d), "{ty} {a}{b}{c}{d}");
        }
    }
}

/// Signed operands are sign-extended by one `Extend` each, not by a
/// `Merge` of single bits.
#[test]
fn sign_extension_uses_one_extend() {
    let text = netlist(
        &[
            ("a", "input", bits(2, 3)),
            ("b", "input", bits(10, 3)),
            ("y", "output", bits(20, 12)),
        ],
        &[(
            "$add",
            json!({"A_SIGNED": 1, "B_SIGNED": 1, "A_WIDTH": 3, "B_WIDTH": 3, "Y_WIDTH": 12}),
            json!({"A": bits(2, 3), "B": bits(10, 3), "Y": bits(20, 12)}),
        )],
    );
    let imported = import_yosys_json(&text, &ImportOptions::default()).unwrap();
    let extend = GateKind::Extend {
        from: 3,
        to: 12,
        signed: true,
    };
    assert_eq!(count(&imported, |k| *k == extend), 2);
    assert_eq!(count(&imported, |k| matches!(k, GateKind::Merge { .. })), 0);
    let mut s = compiled(&imported);
    for a in 0..8 {
        for b in 0..8 {
            let got = step(&mut s, &[("a", a), ("b", b)], &["y"])[0];
            assert_eq!(got, (sext(a, 3) + sext(b, 3)) as u64 & mask(12));
        }
    }
    // A bit repeated in the middle of a word: a one-bit Extend.
    let text = netlist(
        &[
            ("a", "input", bits(2, 4)),
            ("y", "output", json!([2, 4, 4, 4, 5])),
        ],
        &[],
    );
    let imported = import_yosys_json(&text, &ImportOptions::default()).unwrap();
    let rep = GateKind::Extend {
        from: 1,
        to: 3,
        signed: true,
    };
    assert_eq!(count(&imported, |k| *k == rep), 1);
    let mut s = compiled(&imported);
    for a in 0..16 {
        let b = |i: u64| a >> i & 1;
        let want = b(0) | (b(2) * 0b1110) | b(3) << 4;
        assert_eq!(step(&mut s, &[("a", a)], &["y"])[0], want, "a={a}");
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Sequential cells
// ─────────────────────────────────────────────────────────────────────────────

/// An input: name, width and, for one-bit inputs, the percent of ticks it
/// is 1.
type In = (&'static str, u32, u32);

/// Drive `sim` with seeded random inputs for `ticks` ticks and check every
/// output against a cycle model: `out(state, inputs)` is what the outputs
/// show after the tick that applies `inputs` (logic settles with them),
/// and `next(state, inputs)` the state that tick's logic computes, taken
/// at the next tick. The simulator settles once when it starts, with
/// every input 0, and its first tick takes the state computed then.
#[allow(clippy::too_many_arguments)]
fn check_cycles<S: Clone + std::fmt::Debug>(
    sim: &mut Sim,
    inputs: &[In],
    outputs: &[&str],
    mut state: S,
    seed: u64,
    ticks: usize,
    out: impl Fn(&S, &[u64]) -> Vec<u64>,
    next: impl Fn(&S, &[u64]) -> S,
) {
    let mut rng = Rng::new(seed);
    state = next(&state, &vec![0; inputs.len()]);
    for t in 0..ticks {
        let values: Vec<u64> = inputs
            .iter()
            .map(|&(_, w, p)| {
                if w == 1 {
                    u64::from(rng.percent(p))
                } else {
                    rng.next_u64() & mask(w)
                }
            })
            .collect();
        let named: Vec<(&str, u64)> = inputs.iter().map(|i| i.0).zip(values.clone()).collect();
        let got = step(sim, &named, outputs);
        assert_eq!(
            got,
            out(&state, &values),
            "tick {t}: {named:?}, state {state:?}"
        );
        state = next(&state, &values);
    }
}

fn pick(c: u64, a: u64, b: u64) -> u64 {
    if c != 0 { a } else { b }
}

#[test]
fn flip_flops_with_resets_match_the_oracle() {
    type Model = (fn(&u64, &[u64]) -> Vec<u64>, fn(&u64, &[u64]) -> u64);
    let d4: In = ("d", 4, 0);
    let cases: Vec<(&str, Vec<In>, Model)> = vec![
        (
            "adff",
            vec![("rst", 1, 25), d4],
            (|s, i| vec![pick(i[0], 9, *s)], |_, i| pick(i[0], 9, i[1])),
        ),
        (
            "adffe",
            vec![("rst_n", 1, 75), ("en", 1, 50), d4],
            (
                |s, i| vec![pick(i[0], *s, 6)],
                |s, i| pick(i[0], pick(i[1], i[2], *s), 6),
            ),
        ),
        (
            "sdff",
            vec![("rst", 1, 25), d4],
            (|s, _| vec![*s], |_, i| pick(i[0], 5, i[1])),
        ),
        (
            "sdffe",
            vec![("rst", 1, 25), ("en", 1, 50), d4],
            (|s, _| vec![*s], |s, i| pick(i[0], 5, pick(i[1], i[2], *s))),
        ),
        (
            "sdffce",
            vec![("rst_n", 1, 70), ("en", 1, 50), d4],
            (|s, _| vec![*s], |s, i| pick(i[1], pick(i[0], i[2], 10), *s)),
        ),
        (
            "dffsr",
            vec![("set", 4, 0), ("clr", 4, 0), d4],
            (
                |s, i| vec![(s | i[0]) & !i[1] & 15],
                |_, i| (i[2] | i[0]) & !i[1] & 15,
            ),
        ),
        (
            "dffsre",
            vec![("en", 1, 50), ("set", 4, 0), ("clr_n", 4, 0), d4],
            (
                |s, i| vec![(s | i[1]) & i[2]],
                |s, i| (pick(i[0], i[3], *s) | i[1]) & i[2],
            ),
        ),
        (
            "aldff",
            vec![("load", 1, 25), ("ad", 4, 0), d4],
            (
                |s, i| vec![pick(i[0], i[1], *s)],
                |_, i| pick(i[0], i[1], i[2]),
            ),
        ),
        ("ff", vec![d4], (|s, _| vec![*s], |_, i| i[0])),
    ];
    for (k, (top, inputs, (out, next))) in cases.into_iter().enumerate() {
        let imported = import_top(FLOPS, top);
        let asynchronous = matches!(top, "adff" | "adffe" | "dffsr" | "dffsre" | "aldff");
        assert_eq!(
            imported.warnings.iter().any(|w| w.contains("asynchronous")),
            asynchronous,
            "{top}: {:?}",
            imported.warnings
        );
        let mut s = compiled(&imported);
        check_cycles(&mut s, &inputs, &["q"], 0, 100 + k as u64, 300, out, next);
    }
}

#[test]
fn gate_level_flip_flops_match_the_oracle() {
    let inputs: [In; 7] = [
        ("c", 1, 50),
        ("r", 1, 30),
        ("s", 1, 30),
        ("e", 1, 50),
        ("l", 1, 30),
        ("ad", 1, 50),
        ("d", 10, 0),
    ];
    // Per bit of q: (output, next) from (state bit, r, s, e, l, ad, d bit).
    type Bit = fn(u64, u64, u64, u64, u64, u64, u64) -> (u64, u64);
    let bits: [Bit; 10] = [
        |st, r, _, _, _, _, d| (pick(r, st, 0), pick(r, d, 0)),
        |st, r, _, _, _, _, d| (pick(r, 1, st), pick(r, 1, d)),
        |st, r, _, e, _, _, d| (pick(r, st, 0), pick(r, pick(e, d, st), 0)),
        |st, r, _, _, _, _, d| (st, pick(r, 0, d)),
        |st, r, _, e, _, _, d| (st, pick(r, pick(e, d, st), 1)),
        |st, r, _, e, _, _, d| (st, pick(e, st, pick(r, 1, d))),
        |st, r, s, _, _, _, d| ((st | (1 - s)) & (1 - r), (d | (1 - s)) & (1 - r)),
        |st, r, s, e, _, _, d| ((st | s) & r, (pick(e, d, st) | s) & r),
        |st, _, _, _, l, ad, d| (pick(l, ad, st), pick(l, ad, d)),
        |st, _, _, e, l, ad, d| (pick(l, st, ad), pick(l, pick(e, d, st), ad)),
    ];
    let eval = |state: &u64, i: &[u64]| -> (u64, u64) {
        bits.iter().enumerate().fold((0, 0), |(out, next), (b, f)| {
            let (o, n) = f(state >> b & 1, i[1], i[2], i[3], i[4], i[5], i[6] >> b & 1);
            (out | o << b, next | n << b)
        })
    };
    let mut s = sim(FLOPS, "gates");
    check_cycles(
        &mut s,
        &inputs,
        &["q"],
        0u64,
        7,
        600,
        |st, i| vec![eval(st, i).0],
        |st, i| eval(st, i).1,
    );
}

#[test]
fn latches_match_the_oracle() {
    let inputs: [In; 5] = [
        ("e", 1, 40),
        ("r", 1, 20),
        ("d", 4, 0),
        ("s", 2, 0),
        ("k", 2, 0),
    ];
    // State: q, qa, qs, qd, qg. A latch's output is also its next state.
    let out = |st: &[u64; 5], i: &[u64]| -> [u64; 5] {
        let (e, r, d, s, k) = (i[0], i[1], i[2], i[3], i[4]);
        let q = pick(e, d, st[0]);
        let qa = pick(r, 0b1010, pick(e, d, st[1]));
        let qs = (st[2] | s) & !k & 3;
        let qd = (pick(e, d & 3, st[3]) | s) & !k & 3;
        let g = |b: usize| st[4] >> b & 1;
        let d_ = |b: usize| d >> b & 1;
        let qg = [
            pick(e, d_(0), g(0)),
            pick(r, 0, pick(e, g(1), d_(1))),
            (g(2) | (s & 1)) & (k & 1),
            (pick(e, d_(3), g(3)) | s >> 1) & (1 - (k >> 1)),
        ]
        .iter()
        .enumerate()
        .fold(0, |v, (b, x)| v | x << b);
        [q, qa, qs, qd, qg]
    };
    let mut s = sim(FLOPS, "latches");
    check_cycles(
        &mut s,
        &inputs,
        &["q", "qa", "qs", "qd", "qg"],
        [0u64; 5],
        11,
        600,
        |st, i| out(st, i).to_vec(),
        |st, i| out(st, i),
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Memories
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn memories_match_the_oracle() {
    // A ROM per read port, one of them clocked.
    let imported = import_top(MEMORY, "rom");
    assert_eq!(count(&imported, |k| matches!(k, GateKind::Rom { .. })), 2);
    let rom: Vec<u64> = (0..16).map(|i| (37 * i + 5) & 255).collect();
    check_cycles(
        &mut compiled(&imported),
        &[("a1", 4, 0), ("a2", 4, 0)],
        &["d1", "d2"],
        0u64,
        21,
        200,
        |r, i| vec![rom[i[0] as usize], *r],
        |_, i| rom[i[1] as usize],
    );

    // Single port: a Ram (initial contents through a Rom), read first.
    let imported = import_top(MEMORY, "spram");
    assert_eq!(count(&imported, |k| matches!(k, GateKind::Ram { .. })), 1);
    let init: Vec<u64> = (0..16).map(|i| 0xA0 + i).collect();
    check_cycles(
        &mut compiled(&imported),
        &[("we", 1, 40), ("addr", 4, 0), ("din", 8, 0)],
        &["dout"],
        (init, 0u64),
        22,
        400,
        |(_, r), _| vec![*r],
        |(mem, _), i| {
            let mut mem = mem.clone();
            let old = mem[i[1] as usize];
            if i[0] == 1 {
                mem[i[1] as usize] = i[2];
            }
            (mem, old)
        },
    );

    // Write first ($mem): the word being written is read.
    check_cycles(
        &mut sim(MEMORY, "spram_wf"),
        &[("we", 1, 40), ("addr", 3, 0), ("din", 8, 0)],
        &["dout"],
        (vec![0u64; 8], 0u64),
        23,
        400,
        |(_, r), _| vec![*r],
        |(mem, _), i| {
            let mut mem = mem.clone();
            if i[0] == 1 {
                mem[i[1] as usize] = i[2];
            }
            (mem.clone(), mem[i[1] as usize])
        },
    );

    // Byte enables, read at once.
    check_cycles(
        &mut sim(MEMORY, "bytes"),
        &[("we", 2, 0), ("addr", 3, 0), ("din", 16, 0)],
        &["dout"],
        vec![0u64; 8],
        24,
        400,
        |mem, i| vec![mem[i[1] as usize]],
        |mem, i| {
            let mut mem = mem.clone();
            let keep =
                (if i[0] & 1 == 1 { 0 } else { 0xFF }) | (if i[0] & 2 == 2 { 0 } else { 0xFF00 });
            let a = i[1] as usize;
            mem[a] = mem[a] & keep | i[2] & !keep & 0xFFFF;
            mem
        },
    );

    // Separate read and write addresses: registers, with initial contents.
    let imported = import_top(MEMORY, "regfile");
    assert!(
        imported
            .warnings
            .iter()
            .any(|w| w.contains("built from registers")),
        "{:?}",
        imported.warnings
    );
    assert_eq!(count(&imported, |k| matches!(k, GateKind::Ram { .. })), 0);
    let init: Vec<u64> = (0..8).map(|i| 0x11 * i).collect();
    check_cycles(
        &mut compiled(&imported),
        &[
            ("we", 1, 40),
            ("wa", 3, 0),
            ("ra", 3, 0),
            ("rb", 3, 0),
            ("wd", 8, 0),
        ],
        &["r1", "r2"],
        (init, 0u64),
        25,
        600,
        |(mem, r2), i| vec![mem[i[2] as usize], *r2],
        |(mem, _), i| {
            let (we, wa, rb, wd) = (i[0], i[1] as usize, i[3] as usize, i[4]);
            let r2 = if we == 1 && wa == rb { wd } else { mem[rb] };
            let mut mem = mem.clone();
            if we == 1 {
                mem[wa] = wd;
            }
            (mem, r2)
        },
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// A larger design
// ─────────────────────────────────────────────────────────────────────────────

/// The multiply-accumulate unit (`examples/rtl/mac.v`): $mem_v2 built from
/// registers, $mul, $add, $sub, $sshr, $eq, $pmux, $adff with an
/// active-low reset, and a signed $gt.
#[test]
fn multiply_accumulate_unit_matches_the_oracle() {
    #[derive(Clone, Debug)]
    struct Mac {
        coef: Vec<u64>,
        idx: u64,
        prod: u64,
        acc: u64,
    }
    let imported = import_top(MAC, "mac");
    for ty in [
        GateKind::Register { w: 8 },
        GateKind::Delay { w: 20 },
        GateKind::LtU { w: 20 },
    ] {
        assert!(count(&imported, |k| *k == ty) > 0, "{ty:?}");
    }
    let coef = [1i64, 2, -3, 4, 5, -6, 7, -8];
    let inputs: [In; 6] = [
        ("rst_n", 1, 95),
        ("x", 8, 0),
        ("mode", 2, 0),
        ("cwe", 1, 10),
        ("caddr", 3, 0),
        ("cdata", 8, 0),
    ];
    let state = Mac {
        coef: coef.iter().map(|&c| c as u64 & 255).collect(),
        idx: 0,
        prod: 0,
        acc: 0,
    };
    // The reset shows on the outputs at once.
    let shown =
        |m: &Mac, i: &[u64]| -> (u64, u64) { if i[0] == 0 { (0, 0) } else { (m.acc, m.idx) } };
    check_cycles(
        &mut compiled(&imported),
        &inputs,
        &["acc", "idx", "over"],
        state,
        31,
        3000,
        |m, i| {
            let (acc, idx) = shown(m, i);
            vec![acc, idx, u64::from(sext(acc, 20) > 1000)]
        },
        |m, i| {
            let (rst_n, x, mode, cwe, caddr, cdata) = (i[0], i[1], i[2], i[3], i[4], i[5]);
            let mut next = m.clone();
            if cwe == 1 {
                next.coef[caddr as usize] = cdata;
            }
            if rst_n == 0 {
                next.idx = 0;
                next.prod = 0;
                next.acc = 0;
                return next;
            }
            let c = m.coef[shown(m, i).1 as usize];
            next.idx = (m.idx + 1) & 7;
            next.prod = (sext(x, 8) * sext(c, 8)) as u64 & mask(16);
            let (acc, prod) = (sext(m.acc, 20), sext(m.prod, 16));
            next.acc = match mode {
                0 => acc + prod,
                1 => acc - prod,
                2 => acc >> 1,
                _ => 0,
            } as u64
                & mask(20);
            next
        },
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// What is refused
// ─────────────────────────────────────────────────────────────────────────────

fn refused(text: &str) -> String {
    match import_yosys_json(text, &ImportOptions::default()) {
        Ok(_) => panic!("imported"),
        Err(e) => e.to_string(),
    }
}

/// A one-port-each memory netlist: `size` words of 8 bits, read at `ra`,
/// written at `wa` (nets), with `wr` write ports.
fn memory(size: u64, abits: u64, same_address: bool, wr: u64) -> String {
    let ra = bits(2, abits);
    let wa = if same_address {
        ra.clone()
    } else {
        bits(30, abits)
    };
    let wa_all: Vec<Value> = (0..wr)
        .flat_map(|_| wa.as_array().unwrap().clone())
        .collect();
    netlist(
        &[
            ("ra", "input", ra.clone()),
            ("wa", "input", bits(30, abits)),
            ("we", "input", json!([60])),
            ("wd", "input", bits(70, 8)),
            ("rd", "output", bits(80, 8)),
        ],
        &[(
            "$mem_v2",
            json!({"MEMID": "\\m", "SIZE": size, "ABITS": abits, "WIDTH": 8, "OFFSET": 0,
                   "RD_PORTS": 1, "WR_PORTS": wr, "RD_CLK_ENABLE": "0", "INIT": "x"}),
            json!({"RD_CLK": ["x"], "RD_EN": ["1"], "RD_ARST": ["0"], "RD_SRST": ["0"],
                   "RD_ADDR": ra, "RD_DATA": bits(80, 8),
                   "WR_CLK": vec![json!("x"); wr as usize], "WR_EN": vec![json!(60); 8 * wr as usize],
                   "WR_ADDR": wa_all, "WR_DATA": vec![json!(70); 8 * wr as usize]}),
        )],
    )
}

#[test]
fn exotic_cells_are_refused_with_a_message() {
    // Memories: big with two addresses, two write ports, unmerged ports.
    assert!(import_yosys_json(&memory(1024, 10, true, 1), &ImportOptions::default()).is_ok());
    assert!(import_yosys_json(&memory(64, 6, false, 1), &ImportOptions::default()).is_ok());
    let message = refused(&memory(128, 7, false, 1));
    for part in [
        "memory m",
        "128 words",
        "built from registers, up to 64 words",
    ] {
        assert!(message.contains(part), "{part:?} in {message:?}");
    }
    assert!(refused(&memory(16, 4, true, 2)).contains("2 write ports; at most one"));
    let memrd = netlist(&[], &[("$memrd", json!({}), json!({}))]);
    assert!(refused(&memrd).contains("run `memory_collect`"));
    for (ty, advice) in [
        ("$macc", "maccmap -unmap"),
        ("$divfloor", "floored division"),
        ("$tribuf", "tri-state"),
    ] {
        let text = netlist(&[], &[(ty, json!({}), json!({}))]);
        let message = refused(&text);
        assert!(
            message.contains(&format!("{ty} is not supported")),
            "{message}"
        );
        assert!(message.contains(advice), "{message}");
    }
    // Inout ports stay refused, with the way around it.
    let inout = netlist(&[("a", "inout", bits(2, 1))], &[]);
    assert!(refused(&inout).contains("split the port into an input and an output"));
}
