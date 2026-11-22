//! Built-in example circuits.
//!
//! The counters keep their gate-level master-slave T flip-flops. Simulation
//! v2 rejects combinational loops, so each latch's feedback passes through
//! one `Delay` (see `add_t_ff`). With every state element starting at 0 the
//! counters power up at 0 without a reset pulse.

use std::sync::Arc;

use bevy::prelude::Vec2;
use uuid::Uuid;

use super::{CircuitDocument, DocNode, GateKind, WorkspaceDocument};

const B1: u8 = 1;

pub fn fixture_workspace_catalog() -> Vec<(String, Arc<WorkspaceDocument>)> {
    vec![
        ("Half Adder".to_string(), build_half_adder()),
        ("T Flip-Flop".to_string(), build_t_ff_circuit()),
        ("8-Bit Counter".to_string(), build_8bit_counter()),
        (
            "8-Bit Counter Block".to_string(),
            build_8bit_counter_block(),
        ),
        (
            "8-Bit Counter Subcircuit".to_string(),
            build_8bit_counter_subcircuit(),
        ),
        (
            "64-Bit Counter (8 x 8-Bit Subcircuits)".to_string(),
            build_64bit_counter_subcircuits(),
        ),
        (
            "Nested 64-Bit Counter (Subcircuit)".to_string(),
            build_nested_64bit_counter(),
        ),
        (
            "Register Counter (Word Primitives)".to_string(),
            build_register_counter(),
        ),
    ]
    .into_iter()
    .map(|(name, ws)| (name, Arc::new(ws)))
    .collect()
}

fn gate(doc: &mut CircuitDocument, kind: GateKind, pos: Vec2) -> Uuid {
    doc.add(DocNode::new(kind, pos))
}

fn port(doc: &mut CircuitDocument, kind: GateKind, pos: Vec2, label: &str) -> Uuid {
    doc.add(DocNode::labeled(kind, pos, label))
}

/// Add a falling-edge master-slave T flip-flop with asynchronous reset,
/// built from gates. Returns the `(Q, Qn)` nodes (output pin 0 of each).
///
/// Each SR latch is two cross-coupled NORs. The latch's `q` reaches its own
/// `qn` gate, and the other latch, through a `Delay`, which breaks every
/// combinational loop (master, slave, and the T feedback through both).
fn add_t_ff(doc: &mut CircuitDocument, clk: Uuid, rst: Uuid, pos: Vec2) -> (Uuid, Uuid) {
    let mut g = |kind: GateKind, dx: f32, dy: f32| gate(doc, kind, pos + Vec2::new(dx, dy));
    let not_clk = g(GateKind::Not { w: B1 }, 40.0, -60.0);
    // Master latch.
    let s_m = g(GateKind::And { w: B1 }, -40.0, 20.0);
    let r_m = g(GateKind::And { w: B1 }, -40.0, -20.0);
    let rm_rst = g(GateKind::Or { w: B1 }, -10.0, -40.0);
    let q_m = g(GateKind::Nor { w: B1 }, 20.0, 20.0);
    let qn_m = g(GateKind::Nor { w: B1 }, 20.0, -20.0);
    let d_qm = g(GateKind::Delay { w: B1 }, 0.0, 50.0);
    // Slave latch.
    let s_s = g(GateKind::And { w: B1 }, 60.0, 20.0);
    let r_s = g(GateKind::And { w: B1 }, 60.0, -20.0);
    let rs_rst = g(GateKind::Or { w: B1 }, 90.0, -40.0);
    let q_s = g(GateKind::Nor { w: B1 }, 120.0, 20.0);
    let qn_s = g(GateKind::Nor { w: B1 }, 120.0, -20.0);
    let d_qs = g(GateKind::Delay { w: B1 }, 100.0, 50.0);

    let w = |doc: &mut CircuitDocument, from: Uuid, to: Uuid, pin: u8| {
        doc.connect(from, 0, to, pin);
    };
    w(doc, clk, not_clk, 0);
    // Master: s = qn_s & clk, r = q_s' & clk (+ reset).
    w(doc, qn_s, s_m, 0);
    w(doc, clk, s_m, 1);
    w(doc, d_qs, r_m, 0);
    w(doc, clk, r_m, 1);
    w(doc, r_m, rm_rst, 0);
    w(doc, rst, rm_rst, 1);
    w(doc, rm_rst, q_m, 0);
    w(doc, qn_m, q_m, 1);
    w(doc, s_m, qn_m, 0);
    w(doc, d_qm, qn_m, 1);
    w(doc, q_m, d_qm, 0);
    // Slave: s = q_m' & !clk, r = qn_m & !clk (+ reset).
    w(doc, d_qm, s_s, 0);
    w(doc, not_clk, s_s, 1);
    w(doc, qn_m, r_s, 0);
    w(doc, not_clk, r_s, 1);
    w(doc, r_s, rs_rst, 0);
    w(doc, rst, rs_rst, 1);
    w(doc, rs_rst, q_s, 0);
    w(doc, qn_s, q_s, 1);
    w(doc, s_s, qn_s, 0);
    w(doc, d_qs, qn_s, 1);
    w(doc, q_s, d_qs, 0);
    (q_s, qn_s)
}

/// A T flip-flop as a standalone circuit. Ports: inputs `Clock`, `Reset`;
/// outputs `Q`, `Qn`.
pub fn build_t_ff_circuit() -> WorkspaceDocument {
    WorkspaceDocument::new(t_ff_definition())
}

fn t_ff_definition() -> CircuitDocument {
    let mut doc = CircuitDocument::new("T Flip-Flop");
    let clk = port(
        &mut doc,
        GateKind::Input { w: B1 },
        Vec2::new(-100.0, 50.0),
        "Clock",
    );
    let rst = port(
        &mut doc,
        GateKind::Input { w: B1 },
        Vec2::new(-100.0, -50.0),
        "Reset",
    );
    let (q, qn) = add_t_ff(&mut doc, clk, rst, Vec2::ZERO);
    let out_q = port(
        &mut doc,
        GateKind::Output { w: B1 },
        Vec2::new(200.0, 20.0),
        "Q",
    );
    let out_qn = port(
        &mut doc,
        GateKind::Output { w: B1 },
        Vec2::new(200.0, -20.0),
        "Qn",
    );
    doc.connect(q, 0, out_q, 0);
    doc.connect(qn, 0, out_qn, 0);
    doc
}

/// Merge `bits` (1-bit nodes, low first) into one word and return the
/// merge node.
fn merge_bits(doc: &mut CircuitDocument, bits: &[(Uuid, u8)], pos: Vec2) -> Uuid {
    let merge = gate(
        doc,
        GateKind::Merge {
            widths: vec![1; bits.len()],
        },
        pos,
    );
    for (i, &(node, pin)) in bits.iter().enumerate() {
        doc.connect(node, pin, merge, i as u8);
    }
    merge
}

/// An 8-bit ripple counter of gate-level T flip-flops, clocked by a clock,
/// with one output per bit and an 8-bit display.
pub fn build_8bit_counter() -> WorkspaceDocument {
    let mut doc = CircuitDocument::new("8-Bit Counter");
    let clock = port(
        &mut doc,
        GateKind::Clock {
            period: 4,
            duty: 1,
            phase: 0,
        },
        Vec2::new(-400.0, 100.0),
        "Clock",
    );
    let reset = port(
        &mut doc,
        GateKind::Input { w: B1 },
        Vec2::new(-400.0, -100.0),
        "Reset",
    );
    let mut clk = clock;
    let mut bits = Vec::new();
    for i in 0..8 {
        let pos = Vec2::new(-200.0 + i as f32 * 200.0, 0.0);
        let (q, _) = add_t_ff(&mut doc, clk, reset, pos);
        let out = port(
            &mut doc,
            GateKind::Output { w: B1 },
            pos + Vec2::new(150.0, 100.0),
            &format!("Bit {i}"),
        );
        doc.connect(q, 0, out, 0);
        bits.push((q, 0));
        clk = q;
    }
    let merge = merge_bits(&mut doc, &bits, Vec2::new(600.0, -200.0));
    let display = port(
        &mut doc,
        GateKind::Display { w: 8 },
        Vec2::new(800.0, -200.0),
        "Count",
    );
    doc.connect(merge, 0, display, 0);
    WorkspaceDocument::new(doc)
}

/// The byte counter used by the 64-bit example. Ports: inputs `Increment`,
/// `Reset`; outputs `Bits` (8-bit word) and `Ripple` (bit 7, the next byte's
/// increment).
fn counter_block_definition() -> CircuitDocument {
    let mut doc = CircuitDocument::new("8-Bit Counter Block");
    let inc = port(
        &mut doc,
        GateKind::Input { w: B1 },
        Vec2::new(-300.0, 80.0),
        "Increment",
    );
    let reset = port(
        &mut doc,
        GateKind::Input { w: B1 },
        Vec2::new(-300.0, -80.0),
        "Reset",
    );
    let mut clk = inc;
    let mut bits = Vec::new();
    for bit in 0..8 {
        let pos = Vec2::new(-200.0 + bit as f32 * 200.0, 0.0);
        let (q, _) = add_t_ff(&mut doc, clk, reset, pos);
        bits.push((q, 0));
        clk = q;
    }
    let merge = merge_bits(&mut doc, &bits, Vec2::new(1500.0, 0.0));
    let out = port(
        &mut doc,
        GateKind::Output { w: 8 },
        Vec2::new(1650.0, 0.0),
        "Bits",
    );
    doc.connect(merge, 0, out, 0);
    let ripple = port(
        &mut doc,
        GateKind::Output { w: B1 },
        Vec2::new(1650.0, -100.0),
        "Ripple",
    );
    doc.connect(clk, 0, ripple, 0);
    doc
}

pub fn build_8bit_counter_block() -> WorkspaceDocument {
    WorkspaceDocument::new(counter_block_definition())
}

/// An 8-bit ripple counter of eight T flip-flop subcircuits sharing one
/// definition.
pub fn build_8bit_counter_subcircuit() -> WorkspaceDocument {
    let mut ws = WorkspaceDocument::new(CircuitDocument::new("8-Bit Counter (Subcircuits)"));
    let tff = ws.define(t_ff_definition());
    let doc = &mut ws.root;
    let inc = port(
        doc,
        GateKind::Input { w: B1 },
        Vec2::new(-400.0, 100.0),
        "Increment",
    );
    let reset = port(
        doc,
        GateKind::Input { w: B1 },
        Vec2::new(-400.0, -100.0),
        "Reset",
    );
    let mut clk = inc;
    let mut bits = Vec::new();
    for i in 0..8 {
        let mut node = DocNode::new(
            GateKind::SubCircuit { definition: tff },
            Vec2::new(-200.0 + i as f32 * 200.0, 0.0),
        );
        node.label = Some(format!("T-FF {i}"));
        let ff = doc.add(node);
        doc.connect(clk, 0, ff, 0); // Clock
        doc.connect(reset, 0, ff, 1); // Reset
        bits.push((ff, 0)); // Q
        clk = ff;
    }
    let merge = merge_bits(doc, &bits, Vec2::new(1450.0, 120.0));
    let out = port(
        doc,
        GateKind::Output { w: 8 },
        Vec2::new(1600.0, 120.0),
        "Count",
    );
    doc.connect(merge, 0, out, 0);
    ws
}

/// Eight byte-counter subcircuits chained into a 64-bit ripple counter,
/// shown on a 64-bit display.
pub fn build_64bit_counter_subcircuits() -> WorkspaceDocument {
    build_64bit_counter(GateKind::Clock {
        period: 4,
        duty: 2,
        phase: 0,
    })
}

fn build_64bit_counter(increment_kind: GateKind) -> WorkspaceDocument {
    let mut ws = WorkspaceDocument::new(CircuitDocument::new(
        "64-Bit Counter (8 x 8-Bit Subcircuits)",
    ));
    let block = ws.define(counter_block_definition());
    let doc = &mut ws.root;
    let inc = port(
        doc,
        increment_kind,
        Vec2::new(-650.0, 120.0),
        "Increment Clock",
    );
    let reset = port(
        doc,
        GateKind::Input { w: B1 },
        Vec2::new(-650.0, -120.0),
        "Reset",
    );
    let merge = gate(
        doc,
        GateKind::Merge { widths: vec![8; 8] },
        Vec2::new(550.0, 0.0),
    );
    let display = port(
        doc,
        GateKind::Display { w: 64 },
        Vec2::new(750.0, 0.0),
        "Display64",
    );
    let output = port(
        doc,
        GateKind::Output { w: 64 },
        Vec2::new(750.0, -180.0),
        "Output64",
    );
    doc.connect(merge, 0, display, 0);
    doc.connect(merge, 0, output, 0);
    let mut ripple = (inc, 0_u8);
    for byte in 0..8_u8 {
        let mut node = DocNode::new(
            GateKind::SubCircuit { definition: block },
            Vec2::new(-450.0 + f32::from(byte) * 125.0, 0.0),
        );
        node.label = Some(format!("Byte {byte:02}"));
        let counter = doc.add(node);
        doc.connect(ripple.0, ripple.1, counter, 0); // Increment
        doc.connect(reset, 0, counter, 1); // Reset
        doc.connect(counter, 0, merge, byte); // Bits
        ripple = (counter, 1); // Ripple
    }
    ws
}

/// The 64-bit counter wrapped in one more subcircuit level.
pub fn build_nested_64bit_counter() -> WorkspaceDocument {
    let inner = build_64bit_counter(GateKind::Input { w: B1 });
    let mut ws = WorkspaceDocument::new(CircuitDocument::new("Nested 64-Bit Counter (Subcircuit)"));
    ws.absorb(&inner);
    let counter_def = ws.define(inner.root);
    let doc = &mut ws.root;
    let inc = port(
        doc,
        GateKind::Clock {
            period: 4,
            duty: 2,
            phase: 0,
        },
        Vec2::new(-650.0, 120.0),
        "Increment Clock",
    );
    let reset = port(
        doc,
        GateKind::Input { w: B1 },
        Vec2::new(-650.0, -120.0),
        "Reset",
    );
    let mut node = DocNode::new(
        GateKind::SubCircuit {
            definition: counter_def,
        },
        Vec2::new(-50.0, 0.0),
    );
    node.label = Some("64-Bit Counter".into());
    let counter = doc.add(node);
    let display = port(
        doc,
        GateKind::Display { w: 64 },
        Vec2::new(650.0, 0.0),
        "Display64",
    );
    // Inner ports, sorted by label: inputs "Increment Clock", "Reset"; output "Output64".
    doc.connect(inc, 0, counter, 0);
    doc.connect(reset, 0, counter, 1);
    doc.connect(counter, 0, display, 0);
    ws
}

/// A word-level counter: `Register` + `Add`, incrementing while `Enable` is
/// high.
pub fn build_register_counter() -> WorkspaceDocument {
    let mut doc = CircuitDocument::new("Register Counter");
    let enable = port(
        &mut doc,
        GateKind::Input { w: B1 },
        Vec2::new(-300.0, 100.0),
        "Enable",
    );
    let reg = gate(&mut doc, GateKind::Register { w: 16 }, Vec2::new(0.0, 0.0));
    let one = gate(
        &mut doc,
        GateKind::Const { w: 16, value: 1 },
        Vec2::new(-300.0, -100.0),
    );
    let add = gate(&mut doc, GateKind::Add { w: 16 }, Vec2::new(-150.0, -60.0));
    let display = port(
        &mut doc,
        GateKind::Display { w: 16 },
        Vec2::new(250.0, 0.0),
        "Count",
    );
    doc.connect(enable, 0, reg, 0); // en
    doc.connect(add, 0, reg, 1); // d = sum
    doc.connect(reg, 0, add, 0); // a = q
    doc.connect(one, 0, add, 1); // b = 1
    doc.connect(reg, 0, display, 0);
    WorkspaceDocument::new(doc)
}

/// A half adder of two gates.
pub fn build_half_adder() -> WorkspaceDocument {
    let mut doc = CircuitDocument::new("Half Adder");
    let xor = gate(&mut doc, GateKind::Xor { w: B1 }, Vec2::new(0.0, 50.0));
    let and = gate(&mut doc, GateKind::And { w: B1 }, Vec2::new(0.0, -50.0));
    let a = port(
        &mut doc,
        GateKind::Input { w: B1 },
        Vec2::new(-200.0, 50.0),
        "A",
    );
    let b = port(
        &mut doc,
        GateKind::Input { w: B1 },
        Vec2::new(-200.0, -50.0),
        "B",
    );
    let sum = port(
        &mut doc,
        GateKind::Output { w: B1 },
        Vec2::new(200.0, 50.0),
        "Sum",
    );
    let carry = port(
        &mut doc,
        GateKind::Output { w: B1 },
        Vec2::new(200.0, -50.0),
        "Carry",
    );
    doc.connect(a, 0, xor, 0);
    doc.connect(b, 0, xor, 1);
    doc.connect(a, 0, and, 0);
    doc.connect(b, 0, and, 1);
    doc.connect(xor, 0, sum, 0);
    doc.connect(and, 0, carry, 0);
    WorkspaceDocument::new(doc)
}
