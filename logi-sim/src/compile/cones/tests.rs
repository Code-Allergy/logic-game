//! Cone tables: decoders and random designs with the pass on against the
//! reference and against the same design compiled without it (every tick:
//! outputs, state carried across, every net), exhaustively on small
//! combinational designs, under the JIT and fused batches, with probes
//! and the editor's options, and the bit-blasted CPU.

use std::sync::Arc;

use crate::compile::func::tests::on_off_with;
use crate::compile::tests::check_program_invariants;
use crate::compile::{Op, Options, Program, compile, compile_cached, func};
use crate::netlist::{Design, NetId, mask};
use crate::reference::Reference;
use crate::sim::{Sim, SimOptions};
use crate::testkit::diff::{
    check_batches, check_design, check_design_with, check_jit, standard_configs, sweep,
};
use crate::testkit::{GenConfig, bitblast::bitblast, fixtures, generate};

fn on(o: Options) -> Options {
    Options {
        cone_tables: true,
        ..o
    }
}

fn reads(p: &Program) -> usize {
    p.blocks
        .iter()
        .filter(|b| b.op == Op::MemRead)
        .map(|b| b.len as usize)
        .sum()
}

/// With the pass and without, under the editor's options and the
/// defaults, every tick; both against the reference; the JIT against the
/// interpreter and fused batches against single ticks. Returns the
/// default options' programs (on, off).
fn check(name: &str, d: &Design, ticks: u64) -> (Program, Program) {
    for debug in [false, true] {
        on_off_with(name, d, debug, ticks, on);
    }
    for o in [on(Options::default()), on(Options::debug())] {
        check_design(d, 3, ticks, &o).unwrap_or_else(|e| panic!("{name}: {e}"));
        let p = Arc::new(compile(d, &o).unwrap());
        check_program_invariants(&p);
        check_jit(&p, ticks, 5, &SimOptions::default()).unwrap_or_else(|e| panic!("{name}: {e}"));
        check_batches(&p, 9).unwrap_or_else(|e| panic!("{name}: {e}"));
    }
    (
        compile(d, &on(Options::default())).unwrap(),
        compile(d, &Options::default()).unwrap(),
    )
}

/// An instruction decoder: an 8-bit opcode and a 3-bit cycle count drive
/// control signals through comparisons and gates, as a CPU's control
/// unit does; a 16-bit datapath (too wide for a table) uses them.
fn decoder() -> (Design, NetId) {
    let mut d = Design::new("top");
    let r = d.root;
    let mut b = d.builder(r);
    let op = b.input("op", 8);
    let cyc = b.input("cyc", 3);
    let x = b.input("x", 16);
    let lo = b.split(op, 0, 4);
    let hi = b.split(op, 4, 4);
    let mut ctrl = Vec::new();
    let mut probe = x;
    for k in 0..6u64 {
        let e1 = b.eq_const(lo, k * 3 % 16);
        let e2 = b.eq_const(hi, 15 - k);
        let c = b.eq_const(cyc, k % 8);
        let t = b.and(e1, e2);
        if k == 1 {
            probe = t;
        }
        let u = b.or(t, c);
        let bit = b.bit(op, k as u8);
        let v = b.xor(u, bit);
        let n = b.not(e1);
        let w = b.and(v, n);
        ctrl.push(w);
    }
    let all = b.merge(&ctrl);
    b.output("ctrl", all);
    let mut y = x;
    for (k, &c) in ctrl.iter().enumerate() {
        let n = b.not(y);
        let (s, _) = b.add(y, x, None);
        let alt = if k % 2 == 0 { n } else { s };
        y = b.mux(c, y, alt);
    }
    let q = b.delay(y);
    b.output("y", q);
    (d, probe)
}

#[test]
fn a_decoder_runs_as_tables() {
    let (d, _) = decoder();
    let (p_on, p_off) = check("decoder", &d, 200);
    assert_eq!(reads(&p_off), 0);
    assert!(reads(&p_on) > 0, "the decoder is tabled");
    assert!(
        p_on.instrs.len() < p_off.instrs.len(),
        "{} instructions with tables, {} without",
        p_on.instrs.len(),
        p_off.instrs.len()
    );
    // The editor's options keep every net computed: a full settle runs
    // more, never less, but what a fused tick runs is the tables.
    let dbg = compile(&d, &on(Options::debug())).unwrap();
    assert!(reads(&dbg) > 0);
}

/// Nets inside a table leave the debug map without `debug` (as dead code
/// does), except probed ones, which read exactly; with `debug` every net
/// stays readable (`check` compares every net with the reference).
#[test]
fn probes_inside_a_table_read_exactly() {
    let (d, probe) = decoder();
    let plain = compile(&d, &on(Options::default())).unwrap();
    assert_eq!(
        plain.debug.slot(&[], probe),
        None,
        "a tabled net is not computed"
    );
    let o = Options {
        probes: vec![(Vec::new(), probe)],
        ..on(Options::default())
    };
    let p = compile(&d, &o).unwrap();
    assert!(p.debug.slot(&[], probe).is_some());
    check_design(&d, 4, 100, &o).unwrap();
    let dbg = compile(&d, &on(Options::debug())).unwrap();
    assert!(dbg.debug.slot(&[], probe).is_some());
}

/// Every input combination of small combinational designs, outputs
/// against the reference.
#[test]
fn small_combinational_designs_are_exact_exhaustively() {
    let mut tabled = 0;
    for seed in 0..60u64 {
        let cfg = GenConfig {
            seed,
            nodes: 40,
            child_defs: 2,
            child_nodes: 10,
            max_depth: 2,
            max_width: 4,
            inputs: 3,
            outputs: 3,
            state_percent: 0,
            ..GenConfig::default()
        };
        let d = generate(&cfg).design;
        let p = Arc::new(compile(&d, &on(Options::default())).unwrap());
        tabled += (reads(&p) > 0) as usize;
        let widths: Vec<u8> = p.ports.inputs.iter().map(|q| q.width).collect();
        let bits: u32 = widths.iter().map(|&w| w as u32).sum();
        assert!(bits <= 12, "seed {seed}: {bits} input bits");
        let mut sim = Sim::new(Arc::clone(&p));
        let mut reference = Reference::new(&d).unwrap();
        for x in 0..1u64 << bits {
            let mut at = 0;
            for (port, &w) in widths.iter().enumerate() {
                let v = (x >> at) & mask(w);
                sim.set_input(port, v);
                reference.set_input(port, v);
                at += w as u32;
            }
            sim.tick();
            reference.tick().unwrap();
            for port in 0..p.ports.outputs.len() {
                assert_eq!(
                    sim.output(port),
                    reference.output(port),
                    "seed {seed}: input {x:#x}: output {port}"
                );
            }
        }
    }
    assert!(tabled >= 10, "only {tabled} designs have a table");
}

#[test]
fn random_designs_match_with_and_without() {
    for o in [on(Options::default()), on(Options::debug())] {
        sweep(&standard_configs(), 40, 40, &o);
    }
    for seed in 0..25 {
        let d = generate(&GenConfig {
            seed,
            max_width: 6,
            ..GenConfig::default()
        })
        .design;
        for debug in [false, true] {
            on_off_with(&format!("seed {seed}"), &d, debug, 60, on);
        }
    }
}

#[test]
fn bitblasted_fixtures_match_with_and_without() {
    let program = [0x1101, 0x1201, 0x2312, 0x4F03, 0x0000];
    for (name, d) in [
        ("cpu", bitblast(&fixtures::cpu(&program))),
        ("mux tree", fixtures::mux_tree_bitblasted(3, 4)),
        ("counter", fixtures::counter_bitblasted(6)),
        ("register file", bitblast(&fixtures::register_file(2, 4))),
    ] {
        check(name, &d, 150);
    }
}

/// The same program from a warm match cache, and an edit recompiled with
/// it, as from a cold one.
#[test]
fn recompiles_are_deterministic() {
    let (mut d, _) = decoder();
    let o = on(Options::default());
    let mut cache = func::MatchCache::new();
    let a = compile_cached(&d, &o, &mut cache, |_| {}).unwrap();
    let b = compile_cached(&d, &o, &mut cache, |_| {}).unwrap();
    assert_eq!(a.instrs, b.instrs);
    assert_eq!(a.mems, b.mems);
    // One gate edit: a new output of the opcode.
    let r = d.root;
    let mut bl = d.builder(r);
    let op = bl.input("op2", 8);
    let z = bl.reduce_xor(op);
    bl.output("z", z);
    let warm = compile_cached(&d, &o, &mut cache, |_| {}).unwrap();
    let cold = compile(&d, &o).unwrap();
    assert_eq!(warm.instrs, cold.instrs);
    assert_eq!(warm.mems, cold.mems);
    check_design_with(&d, 2, 60, &o, &SimOptions::default()).unwrap();
}

/// Index fields that are parts of wider values: low bits of two words
/// (only the top field may keep the bits above its own, which the read's
/// address mask drops), a middle field, and a whole narrow value. Every
/// input combination against the reference.
#[test]
fn partial_fields_are_sliced() {
    let mut d = Design::new("top");
    let r = d.root;
    let mut b = d.builder(r);
    let x = b.input("x", 8);
    let y = b.input("y", 8);
    let z = b.input("z", 2);
    let xl = b.split(x, 0, 3);
    let yl = b.split(y, 0, 3);
    let ym = b.split(y, 5, 2);
    let mut outs = Vec::new();
    for k in 0..4u64 {
        let e1 = b.eq_const(xl, k + 1);
        let e2 = b.eq_const(yl, 6 - k);
        let e3 = b.eq_const(ym, k);
        let e4 = b.eq_const(z, 3 - k);
        let t = b.and(e1, e2);
        let u = b.or(t, e3);
        let v = b.xor(u, e4);
        let w = b.nand(v, e1);
        outs.push(w);
    }
    let all = b.merge(&outs);
    b.output("o", all);
    let p = Arc::new(compile(&d, &on(Options::default())).unwrap());
    assert!(reads(&p) > 0, "tabled");
    let mut sim = Sim::new(Arc::clone(&p));
    let mut reference = Reference::new(&d).unwrap();
    for v in 0..1u64 << 18 {
        let ins = [v & 0xFF, (v >> 8) & 0xFF, v >> 16];
        for (port, &x) in ins.iter().enumerate() {
            sim.set_input(port, x);
            reference.set_input(port, x);
        }
        sim.tick();
        reference.tick().unwrap();
        assert_eq!(sim.output(0), reference.output(0), "inputs {ins:?}");
    }
}
