//! Function matching: per-catalogue matches, near misses, alternative
//! player-style implementations, shadow reads and the analysis cache.

use std::sync::Arc;

use super::*;
use crate::compile::{Options, Program, compile};
use crate::netlist::{Builder, DefId, Design, NetId, NodeKind};
use crate::sim::Sim;
use crate::testkit::diff::check_design;
use crate::testkit::fixtures::full_adder_def;

/// The analysis of the definition named `name` in `d`.
fn info(d: &Design, name: &str) -> Arc<DefInfo> {
    let (id, _) = d.defs.iter().find(|(_, x)| x.name == name).unwrap();
    let w = wrap(d.clone(), id, 1);
    let a = analyze(&w, &mut MatchCache::new());
    Arc::clone(a.get(id).unwrap_or_else(|| panic!("{name} not analysed")))
}

/// A root with `copies` instances of `child`, each fed from its own root
/// inputs (same widths as the child's) and driving root outputs.
fn wrap(mut d: Design, child: DefId, copies: usize) -> Design {
    let (ins, _) = d.defs[child].signature();
    let r = d.root;
    let mut b = Builder::new(&mut d, r);
    for c in 0..copies {
        let nets: Vec<Option<NetId>> = ins
            .iter()
            .enumerate()
            .map(|(k, &w)| Some(b.input(format!("i{c}_{k}"), w)))
            .collect();
        for (k, o) in b.instance(child, &nets).into_iter().enumerate() {
            b.output(format!("o{c}_{k}"), o);
        }
    }
    d
}

/// Compile with and without function matching and compare, every tick,
/// outputs, state and every net observable without matching; also both
/// against the reference interpreter. Halfway, state crosses over.
/// Returns the instruction counts (matched, unmatched).
pub(crate) fn on_off(name: &str, d: &Design, debug: bool, ticks: u64) -> (usize, usize) {
    on_off_with(name, d, debug, ticks, |o| Options {
        func_match: false,
        ..o
    })
}

/// [`on_off`] for any switch: the editor's (`debug`) or the default
/// options against the same with `off` applied.
pub(crate) fn on_off_with(
    name: &str,
    d: &Design,
    debug: bool,
    ticks: u64,
    off: impl Fn(Options) -> Options,
) -> (usize, usize) {
    let on_opts = if debug {
        Options::debug()
    } else {
        Options::default()
    };
    let off_opts = off(on_opts.clone());
    for o in [&on_opts, &off_opts] {
        if let Err(e) = check_design(d, 7, ticks.min(200), o) {
            panic!("{name}: {e}");
        }
    }
    let p_on: Arc<Program> = Arc::new(compile(d, &on_opts).unwrap());
    let p_off: Arc<Program> = Arc::new(compile(d, &off_opts).unwrap());
    let (mut on, mut off) = (Sim::new(Arc::clone(&p_on)), Sim::new(Arc::clone(&p_off)));
    let mut x = 0x9E37_79B9_7F4A_7C15u64 ^ ticks;
    for t in 0..=ticks {
        if t > 0 {
            for (port, p) in p_on.ports.inputs.iter().enumerate() {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                let v = x & crate::netlist::mask(p.width);
                on.set_input(port, v);
                off.set_input(port, v);
            }
            on.tick();
            off.tick();
        }
        if t == ticks / 2 {
            let (a, b) = (on.export_state(), off.export_state());
            assert_eq!(a.words, b.words, "{name}: tick {t}: state");
            on.import_state(&b);
            off.import_state(&a);
        }
        for port in 0..p_on.ports.outputs.len() {
            assert_eq!(
                on.output(port),
                off.output(port),
                "{name}: tick {t}: output {port}"
            );
        }
        assert_eq!(
            on.export_state().words,
            off.export_state().words,
            "{name}: tick {t}: state"
        );
        for inst in 0..p_off.debug.instances.len() {
            let path = p_off.debug.path(inst as u32);
            for (net, _) in &p_off.debug.net_slots[inst] {
                let (got, want) = (on.read_net(&path, net), off.read_net(&path, net));
                if debug {
                    assert_eq!(got, want, "{name}: tick {t}: net {path:?}/{net:?}");
                } else if let (Some(g), Some(w)) = (got, want) {
                    assert_eq!(g, w, "{name}: tick {t}: net {path:?}/{net:?}");
                }
            }
        }
    }
    (p_on.instrs.len(), p_off.instrs.len())
}

fn check_all(name: &str, d: &Design) -> (usize, usize) {
    on_off(name, d, false, 60);
    on_off(name, d, true, 60)
}

// ── Builders ─────────────────────────────────────────────────────────────

/// `y = s ? b : a` from gates.
fn mux2_def(d: &mut Design, nand: bool) -> DefId {
    let def = d.add_definition("mux2");
    let mut b = Builder::new(d, def);
    let a = b.input("A", 1);
    let x = b.input("B", 1);
    let s = b.input("S", 1);
    let ns = b.not(s);
    let y = if nand {
        let p = b.nand(a, ns);
        let q = b.nand(x, s);
        b.nand(p, q)
    } else {
        let p = b.and(a, ns);
        let q = b.and(x, s);
        b.or(p, q)
    };
    b.output("Y", y);
    def
}

/// A `w`-bit ripple-carry adder from full-adder instances; `wrong_bit`
/// replaces one sum bit's XOR with XNOR (a near miss).
fn ripple_def(d: &mut Design, w: u8, wrong_bit: Option<u8>) -> DefId {
    let fa = full_adder_def(d);
    let def = d.add_definition("ripple");
    let mut b = Builder::new(d, def);
    let a = b.input("A", w);
    let x = b.input("B", w);
    let mut c = b.input("Cin", 1);
    let ab = b.bus_expand(a);
    let bb = b.bus_expand(x);
    let mut sums = Vec::new();
    for i in 0..w as usize {
        let o = b.instance(fa, &[Some(ab[i]), Some(bb[i]), Some(c)]);
        let s = if wrong_bit == Some(i as u8) {
            b.not(o[0])
        } else {
            o[0]
        };
        sums.push(s);
        c = o[1];
    }
    let sum = b.bus_collect(&sums);
    b.output("Sum", sum);
    b.output("Cout", c);
    def
}

/// A 4-bit carry-lookahead adder from gates (the library's `cla4` shape).
fn cla4_def(d: &mut Design) -> DefId {
    let def = d.add_definition("cla4");
    let mut b = Builder::new(d, def);
    let a = b.input("A", 4);
    let x = b.input("B", 4);
    let c0 = b.input("Cin", 1);
    let ab = b.bus_expand(a);
    let bb = b.bus_expand(x);
    let g: Vec<NetId> = (0..4).map(|i| b.and(ab[i], bb[i])).collect();
    let p: Vec<NetId> = (0..4).map(|i| b.xor(ab[i], bb[i])).collect();
    let mut cs = vec![c0];
    for i in 0..4 {
        // c[i+1] = g[i] | p[i]g[i-1] | ... | p[i]..p[0]c0, flattened.
        let mut terms = vec![g[i]];
        for j in (0..i).rev() {
            let mut t = g[j];
            for &pk in &p[j + 1..=i] {
                t = b.and(t, pk);
            }
            terms.push(t);
        }
        let mut t = c0;
        for &pk in &p[..=i] {
            t = b.and(t, pk);
        }
        terms.push(t);
        cs.push(b.or_n(&terms));
    }
    let sums: Vec<NetId> = (0..4).map(|i| b.xor(p[i], cs[i])).collect();
    let sum = b.bus_collect(&sums);
    b.output("Sum", sum);
    b.output("Cout", cs[4]);
    def
}

/// A gated D latch with tick semantics (the state is a `Delay` on the
/// feedback; the output is the state), from NOR SR logic or NANDs.
fn d_latch_def(d: &mut Design, nand: bool) -> DefId {
    let def = d.add_definition("d_latch");
    let mut b = Builder::new(d, def);
    let dd = b.input("D", 1);
    let e = b.input("E", 1);
    let delay = b.node(NodeKind::Delay { w: 1 }, &[None]);
    let q = b.out(delay, 0);
    let next = if nand {
        let s = b.nand(dd, e);
        let nd = b.not(dd);
        let r = b.nand(nd, e);
        let qn = b.nand(r, q);
        b.nand(s, qn)
    } else {
        let nd = b.not(dd);
        let s = b.and(dd, e);
        let r = b.and(nd, e);
        let qn = b.nor(s, q);
        b.nor(r, qn)
    };
    b.connect(next, (delay, 0));
    b.output("Q", q);
    def
}

/// A master-slave flip-flop from two latch instances.
fn dff_def(d: &mut Design, latch: DefId) -> DefId {
    let def = d.add_definition("dff");
    let mut b = Builder::new(d, def);
    let dd = b.input("D", 1);
    let clk = b.input("Clk", 1);
    let nclk = b.not(clk);
    let m = b.instance(latch, &[Some(dd), Some(nclk)]);
    let s = b.instance(latch, &[Some(m[0]), Some(clk)]);
    b.output("Q", s[0]);
    def
}

// ── Catalogue ────────────────────────────────────────────────────────────

#[test]
fn mux_from_gates_matches_mux() {
    for nand in [false, true] {
        let mut d = Design::new("root");
        let m = mux2_def(&mut d, nand);
        let i = info(&d, "mux2");
        assert!(i.matched(), "{}", i.note);
        assert!(i.note.contains("Mux"), "{}", i.note);
        let d = wrap(d, m, 2);
        let (on, off) = check_all("mux2", &d);
        assert!(on < off, "{on} vs {off}");
    }
}

#[test]
fn ripple_and_lookahead_adders_match_the_same_add() {
    let mut d1 = Design::new("root");
    let r = ripple_def(&mut d1, 4, None);
    let mut d2 = Design::new("root");
    let c = cla4_def(&mut d2);
    let (i1, i2) = (info(&d1, "ripple"), info(&d2, "cla4"));
    assert!(i1.matched() && i2.matched(), "{} / {}", i1.note, i2.note);
    assert_eq!(i1.note, i2.note, "the same function");
    assert!(i1.note.contains("Add"), "{}", i1.note);
    assert!(i1.proof.is_some() && i2.proof.is_some());
    check_all("ripple", &wrap(d1, r, 2));
    check_all("cla4", &wrap(d2, c, 1));
}

#[test]
fn subtractor_matches_add_with_inverted_operand() {
    let mut d = Design::new("root");
    let def = d.add_definition("sub3");
    let mut b = Builder::new(&mut d, def);
    let a = b.input("A", 3);
    let x = b.input("B", 3);
    let nx = b.not(x);
    let one = b.one();
    let (s, c) = b.add(a, nx, Some(one));
    // Rebuilt from gates through a bus round trip so it is not already
    // the primitive alone.
    let bits = b.bus_expand(s);
    let s2 = b.bus_collect(&bits);
    b.output("D", s2);
    b.output("NoBorrow", c);
    let i = info(&d, "sub3");
    assert!(i.matched(), "{}", i.note);
    assert!(i.note.contains("inv_b: true"), "{}", i.note);
    check_all("sub3", &wrap(d, def, 1));
}

#[test]
fn comparators_match_eq_and_ltu() {
    let mut d = Design::new("root");
    let def = d.add_definition("cmp2");
    let mut b = Builder::new(&mut d, def);
    let a = b.input("A", 2);
    let x = b.input("B", 2);
    let ab = b.bus_expand(a);
    let bb = b.bus_expand(x);
    let e0 = b.xnor(ab[0], bb[0]);
    let e1 = b.xnor(ab[1], bb[1]);
    let eq = b.and(e0, e1);
    // a < b: !a1 & b1 | (a1 == b1) & !a0 & b0
    let na1 = b.not(ab[1]);
    let hi = b.and(na1, bb[1]);
    let na0 = b.not(ab[0]);
    let lo0 = b.and(na0, bb[0]);
    let lo = b.and(e1, lo0);
    let lt = b.or(hi, lo);
    b.output("Eq", eq);
    b.output("Lt", lt);
    let i = info(&d, "cmp2");
    assert!(i.matched(), "{}", i.note);
    assert!(
        i.note.contains("Eq") && i.note.contains("LtU"),
        "{}",
        i.note
    );
    check_all("cmp2", &wrap(d, def, 1));
}

#[test]
fn constants_and_pass_through_match() {
    let mut d = Design::new("root");
    let def = d.add_definition("silly");
    let mut b = Builder::new(&mut d, def);
    let a = b.input("A", 4);
    let na = b.not(a);
    let nna = b.not(na);
    let z = b.xor(a, a);
    b.output("Same", nna);
    b.output("Zero", z);
    let i = info(&d, "silly");
    assert!(i.matched(), "{}", i.note);
    check_all("silly", &wrap(d, def, 1));
}

#[test]
fn wide_mux_is_proven_by_bit_slices() {
    let mut d = Design::new("root");
    let m = mux2_def(&mut d, false);
    let def = d.add_definition("mux2x16");
    let mut b = Builder::new(&mut d, def);
    let a = b.input("A", 16);
    let x = b.input("B", 16);
    let s = b.input("S", 1);
    let ab = b.bus_expand(a);
    let bb = b.bus_expand(x);
    let ys: Vec<NetId> = (0..16)
        .map(|i| b.instance(m, &[Some(ab[i]), Some(bb[i]), Some(s)])[0])
        .collect();
    let y = b.bus_collect(&ys);
    b.output("Y", y);
    let i = info(&d, "mux2x16");
    assert!(i.matched(), "{}", i.note);
    assert!(
        matches!(i.proof, Some(Proof::Sliced { .. })),
        "{:?}",
        i.proof
    );
    check_all("mux2x16", &wrap(d, def, 1));
}

// ── Near misses ──────────────────────────────────────────────────────────

#[test]
fn an_adder_with_one_wrong_gate_does_not_match() {
    for bit in [0u8, 3] {
        let mut d = Design::new("root");
        let r = ripple_def(&mut d, 4, Some(bit));
        let i = info(&d, "ripple");
        // The carry out is still an adder's; the sum is not anything in
        // the catalogue. With 9 input bits it is small enough to run as
        // its own truth table (exact by construction), and nothing else.
        assert!(i.is_table(), "bit {bit}: matched as {}", i.note);
        assert_eq!(i.proof, Some(Proof::Table { bits: 9 }));
        check_all("near-miss", &wrap(d, r, 1));
    }
}

#[test]
fn a_latch_that_is_not_a_register_does_not_match() {
    // An SR latch's complementary output depends on the set input
    // combinationally: no register with outputs from state alone.
    let mut d = Design::new("root");
    let def = d.add_definition("sr");
    let mut b = Builder::new(&mut d, def);
    let s = b.input("S", 1);
    let r = b.input("R", 1);
    let delay = b.node(NodeKind::Delay { w: 1 }, &[None]);
    let q = b.out(delay, 0);
    let nor_s = b.nor(s, q);
    let nor_r = b.nor(r, nor_s);
    b.connect(nor_r, (delay, 0));
    b.output("Q", q);
    b.output("Qn", nor_s);
    let i = info(&d, "sr");
    assert!(!i.matched(), "{}", i.note);
    check_all("sr", &wrap(d, def, 1));
}

// ── Sequential ───────────────────────────────────────────────────────────

#[test]
fn nor_and_nand_latches_match_the_same_register() {
    let mut notes = Vec::new();
    for nand in [false, true] {
        let mut d = Design::new("root");
        let l = d_latch_def(&mut d, nand);
        let i = info(&d, "d_latch");
        assert!(i.matched(), "nand {nand}: {}", i.note);
        notes.push(i.note.clone());
        let (on, off) = check_all("d_latch", &wrap(d, l, 2));
        assert!(on < off, "{on} vs {off}");
    }
    assert_eq!(notes[0], notes[1]);
}

#[test]
fn flip_flops_from_either_latch_match_alike() {
    let mut notes = Vec::new();
    for nand in [false, true] {
        let mut d = Design::new("root");
        let l = d_latch_def(&mut d, nand);
        let f = dff_def(&mut d, l);
        let i = info(&d, "dff");
        assert!(i.matched(), "nand {nand}: {}", i.note);
        notes.push(i.note.clone());
        check_all("dff", &wrap(d, f, 2));
    }
    assert_eq!(notes[0], notes[1]);
}

// ── Cache ────────────────────────────────────────────────────────────────

#[test]
fn the_cache_reanalyses_only_changed_definitions() {
    let mut d = Design::new("root");
    let m = mux2_def(&mut d, false);
    let l = d_latch_def(&mut d, false);
    let f = dff_def(&mut d, l);
    let mut d = wrap(d, m, 1);
    let r = d.root;
    let mut b = Builder::new(&mut d, r);
    let x = b.input("x", 1);
    let y = b.input("y", 1);
    let q = b.instance(f, &[Some(x), Some(y)]);
    b.output("q", q[0]);
    let mut cache = MatchCache::new();
    let opts = Options::debug();
    crate::compile::compile_cached(&d, &opts, &mut cache, |_| {}).unwrap();
    assert_eq!((cache.analysed, cache.reused), (3, 0));
    // An edit in the root: nothing re-analysed.
    let mut b = Builder::new(&mut d, r);
    let n = b.not(x);
    b.output("n", n);
    crate::compile::compile_cached(&d, &opts, &mut cache, |_| {}).unwrap();
    assert_eq!((cache.analysed, cache.reused), (3, 3));
    // An edit in the latch: it and the flip-flop above it, not the mux.
    let mut b = Builder::new(&mut d, l);
    let e = b.input("unused", 1);
    let _ = b.not(e);
    let p = crate::compile::compile_cached(&d, &opts, &mut cache, |_| {}).unwrap();
    assert_eq!((cache.analysed, cache.reused), (5, 4));
    drop(p);
}

#[test]
fn random_designs_match_with_and_without_matching() {
    // Random designs, matched and not, against each other and the
    // reference: outputs, state and every net, every tick.
    use crate::testkit::{GenConfig, generate};
    // Small children with few, narrow ports: many match something.
    let mut matched = 0;
    for seed in 0..150 {
        let g = generate(&GenConfig {
            seed,
            nodes: 30,
            child_defs: 4,
            child_nodes: 6,
            max_width: 4,
            inputs: 3,
            outputs: 2,
            ..GenConfig::default()
        });
        let d = &g.design;
        if crate::compile::compile(d, &Options::default()).is_err() {
            continue;
        }
        matched += analyze(d, &mut MatchCache::new())
            .values()
            .filter(|i| i.matched())
            .count();
        on_off(&format!("random {seed}"), d, true, 20);
        on_off(&format!("random {seed}"), d, false, 20);
    }
    assert!(matched > 20, "only {matched} matched definitions");
}

#[test]
fn chained_adders_compose_to_a_wide_add() {
    // Four matched 4-bit adders chained by carries over slices of two
    // 16-bit buses (the library's `cla16`): one 16-bit Add.
    let mut d = Design::new("root");
    let c4 = cla4_def(&mut d);
    let def = d.add_definition("cla16");
    let mut b = Builder::new(&mut d, def);
    let a = b.input("A", 16);
    let x = b.input("B", 16);
    let mut c = b.input("Cin", 1);
    let mut sums = Vec::new();
    for k in 0..4 {
        let ak = b.split(a, 4 * k, 4);
        let bk = b.split(x, 4 * k, 4);
        let o = b.instance(c4, &[Some(ak), Some(bk), Some(c)]);
        sums.push(o[0]);
        c = o[1];
    }
    let s = b.merge(&sums);
    b.output("Sum", s);
    b.output("Cout", c);
    let i = info(&d, "cla16");
    assert!(i.matched(), "{}", i.note);
    assert_eq!(i.proof, Some(Proof::Chained { adders: 8 }), "{}", i.note);
    let (on, off) = check_all("cla16", &wrap(d, def, 1));
    assert!(on < off, "{on} vs {off}");
}

#[test]
fn a_chain_with_a_crossed_slice_is_not_composed() {
    // Slices 1 and 2 of B swapped: not an adder (and not proven).
    let mut d = Design::new("root");
    let c4 = cla4_def(&mut d);
    let def = d.add_definition("bad16");
    let mut b = Builder::new(&mut d, def);
    let a = b.input("A", 16);
    let x = b.input("B", 16);
    let mut c = b.input("Cin", 1);
    let mut sums = Vec::new();
    for k in 0..4u8 {
        let kb = [0u8, 2, 1, 3][k as usize];
        let ak = b.split(a, 4 * k, 4);
        let bk = b.split(x, 4 * kb, 4);
        let o = b.instance(c4, &[Some(ak), Some(bk), Some(c)]);
        sums.push(o[0]);
        c = o[1];
    }
    let s = b.merge(&sums);
    b.output("Sum", s);
    b.output("Cout", c);
    let i = info(&d, "bad16");
    assert!(!i.matched(), "{}", i.note);
    check_all("bad16", &wrap(d, def, 1));
}

// ── Truth tables (stage 4) ───────────────────────────────────────────────

/// Tables on against off (and both against the reference), every tick,
/// outputs, state and every net including the table's internal nets
/// (shadow slots), under the editor's options and the defaults.
fn check_tables(name: &str, d: &Design) -> (usize, usize) {
    on_off_with(name, d, false, 60, |o| Options { tables: false, ..o });
    on_off_with(name, d, true, 60, |o| Options { tables: false, ..o })
}

/// A `n`-to-`2^n` decoder with enable from gates: `Y` (one bit per output
/// port).
fn decoder_def(d: &mut Design, n: u8) -> DefId {
    let def = d.add_definition("decoder");
    let mut b = Builder::new(d, def);
    let sel = b.input("S", n);
    let en = b.input("E", 1);
    let bits: Vec<NetId> = (0..n).map(|k| b.bit(sel, k)).collect();
    let nots: Vec<NetId> = bits.iter().map(|&x| b.not(x)).collect();
    for v in 0..1u32 << n {
        let mut y = en;
        for k in 0..n as usize {
            let t = if v >> k & 1 == 1 { bits[k] } else { nots[k] };
            y = b.and(y, t);
        }
        b.output(format!("Y{v}"), y);
    }
    def
}

/// Irregular logic over two inputs of `wa` and `wb` bits, with no
/// catalogue match: `Y = parity(A & B) ^ (A0 & B0) | (A1 ^ B1)`.
fn irregular_def(d: &mut Design, wa: u8, wb: u8) -> DefId {
    let def = d.add_definition("irregular");
    let mut b = Builder::new(d, def);
    let a = b.input("A", wa);
    let x = b.input("B", wb);
    let w = wa.min(wb);
    let (al, xl) = (b.split(a, 0, w), b.split(x, 0, w));
    let m = b.and(al, xl);
    let p = b.reduce_xor(m);
    let (a0, b0, a1, b1) = (b.bit(a, 0), b.bit(x, 0), b.bit(a, 1), b.bit(x, 1));
    let q = b.and(a0, b0);
    let r = b.xor(a1, b1);
    let s = b.xor(p, q);
    let y = b.or(s, r);
    let z = b.nand(y, a1);
    b.output("Y", y);
    b.output("Z", z);
    def
}

#[test]
fn decoders_run_as_truth_tables() {
    for n in 1..=5u8 {
        let mut d = Design::new("root");
        let def = decoder_def(&mut d, n);
        let i = info(&d, "decoder");
        if n + 1 < matcher::MIN_TABLE_BITS as u8 {
            // Small decoders stay gates (see `MIN_TABLE_BITS`).
            assert!(!i.is_table(), "{}", i.note);
        } else {
            assert!(i.is_table(), "n {n}: {}", i.note);
            assert_eq!(i.proof, Some(Proof::Table { bits: n as u32 + 1 }));
        }
        let (on, off) = check_tables("decoder", &wrap(d, def, 2));
        assert!(on <= off, "n {n}: {off} -> {on}");
    }
}

#[test]
fn twelve_input_bits_are_tabled_and_thirteen_are_not() {
    let mut d = Design::new("root");
    let def = irregular_def(&mut d, 6, 6);
    let i = info(&d, "irregular");
    assert!(i.is_table(), "{}", i.note);
    assert_eq!(i.proof, Some(Proof::Table { bits: 12 }));
    let (on, off) = check_tables("12 bits", &wrap(d, def, 1));
    assert!(on < off, "{off} -> {on}");

    let mut d = Design::new("root");
    let def = irregular_def(&mut d, 7, 6);
    let i = info(&d, "irregular");
    assert!(!i.matched(), "13 bits: {}", i.note);
    let (on, off) = check_tables("13 bits", &wrap(d, def, 1));
    assert_eq!(on, off);
}

/// A table is built once per definition and cached under its content key:
/// a root edit analyses nothing again, and the instance keeps its table.
#[test]
fn tables_are_cached_per_definition() {
    let mut d = Design::new("root");
    let def = decoder_def(&mut d, 4);
    let mut d = wrap(d, def, 3);
    let mut cache = MatchCache::new();
    let p = crate::compile::compile_cached(&d, &Options::default(), &mut cache, |_| {}).unwrap();
    let analysed = cache.analysed;
    assert_eq!(p.mems.len(), 3, "one ROM per instance");
    let r = d.root;
    let net = d.defs[r]
        .nets
        .iter()
        .find(|(_, n)| n.width == 1)
        .map(|(id, _)| id)
        .unwrap();
    let not = d.add_node(r, NodeKind::Not { w: 1 }).unwrap();
    d.connect(r, net, (not, 0)).unwrap();
    let q = crate::compile::compile_cached(&d, &Options::default(), &mut cache, |_| {}).unwrap();
    assert_eq!(cache.analysed, analysed, "nothing analysed again");
    assert_eq!(q.mems.len(), 3);
}

// ── Registers of flip-flop chips ─────────────────────────────────────────

/// How a `dffe` (a 1-bit register with enable, `Q' = En ? D : Q`) is
/// built.
#[derive(Clone, Copy, Debug)]
enum Dffe {
    /// AND/OR/NOT gates.
    Gates,
    /// NAND gates only.
    Nand,
    /// A 2-to-1 multiplexer chip (itself matched as a `Mux`).
    Mux,
}

/// A 1-bit register with enable as a player builds it: a `Delay` holding
/// the bit, fed back through an enable from gates or a mux chip.
fn dffe_def(d: &mut Design, how: Dffe) -> DefId {
    let mux = matches!(how, Dffe::Mux).then(|| mux2_def(d, false));
    let def = d.add_definition("dffe");
    let mut b = Builder::new(d, def);
    let dd = b.input("D", 1);
    let en = b.input("En", 1);
    let delay = b.node(NodeKind::Delay { w: 1 }, &[None]);
    let q = b.out(delay, 0);
    let next = match how {
        Dffe::Gates => {
            let ne = b.not(en);
            let load = b.and(dd, en);
            let keep = b.and(q, ne);
            b.or(load, keep)
        }
        Dffe::Nand => {
            let ne = b.nand(en, en);
            let load = b.nand(dd, en);
            let keep = b.nand(q, ne);
            b.nand(load, keep)
        }
        Dffe::Mux => b.instance(mux.unwrap(), &[Some(q), Some(dd), Some(en)])[0],
    };
    b.connect(next, (delay, 0));
    b.output("Q", q);
    def
}

/// A deliberate defect in a register of `dffe` chips (a near miss).
#[derive(Clone, Copy, Debug, PartialEq)]
enum Defect {
    None,
    /// Bit 5 on a second enable input.
    OtherEnable,
    /// Bit 3 loads `D[4]` and bit 4 loads `D[3]`.
    Crossed,
    /// `Q` in the reverse order of the bits.
    Reversed,
    /// `Q[3]` inverted.
    Inverted,
    /// Bit 3 loads `D[3] & D[4]`.
    ExtraGate,
}

/// `name`: a `w`-bit register of `dffe` instances sharing one enable,
/// with an optional defect.
fn reg_def(d: &mut Design, name: &str, dffe: DefId, w: u8, defect: Defect) -> DefId {
    let def = d.add_definition(name);
    let mut b = Builder::new(d, def);
    let dd = b.input("D", w);
    let en = b.input("Load", 1);
    let en2 = (defect == Defect::OtherEnable).then(|| b.input("Load2", 1));
    let bits = b.bus_expand(dd);
    let mut q = Vec::new();
    for k in 0..w as usize {
        let data = match (defect, k) {
            (Defect::Crossed, 3) => bits[4],
            (Defect::Crossed, 4) => bits[3],
            (Defect::ExtraGate, 3) => b.and(bits[3], bits[4]),
            _ => bits[k],
        };
        let e = match (en2, k) {
            (Some(e2), 5) => e2,
            _ => en,
        };
        let o = b.instance(dffe, &[Some(data), Some(e)])[0];
        q.push(if defect == Defect::Inverted && k == 3 {
            b.not(o)
        } else {
            o
        });
    }
    if defect == Defect::Reversed {
        q.reverse();
    }
    let y = b.merge(&q);
    b.output("Q", y);
    def
}

/// The state words `p` commits.
fn copies(p: &crate::compile::Program) -> usize {
    p.commits
        .iter()
        .filter(|c| matches!(c, crate::compile::Commit::Copy { .. }))
        .count()
}

#[test]
fn a_register_of_dffe_chips_matches_one_register() {
    // The report of issue #117: `reg8` of eight `dffe` chips was "not a
    // register" (its state bits load bits of the `D` port).
    let mut notes = Vec::new();
    for how in [Dffe::Gates, Dffe::Nand, Dffe::Mux] {
        let mut d = Design::new("root");
        let f = dffe_def(&mut d, how);
        let r = reg_def(&mut d, "reg8", f, 8, Defect::None);
        let i = info(&d, "dffe");
        assert!(i.matched(), "{how:?}: {}", i.note);
        let i = info(&d, "reg8");
        assert!(i.matched(), "{how:?}: {}", i.note);
        assert_eq!(
            i.note,
            "state [8-bit Register { en: In(1), d: In(0) }], outputs [r0]"
        );
        notes.push(i.note.clone());
        let d = wrap(d, r, 2);
        let (on, off) = check_all(&format!("reg8 {how:?}"), &d);
        assert!(on <= off, "{how:?}: {off} -> {on}");
        // One 8-bit state word per instance, even without lifting and the
        // word cleanup; each bit keeps its own state key.
        for opts in [
            Options::default(),
            Options {
                lift: false,
                words: false,
                ..Options::default()
            },
        ] {
            let p = compile(&d, &opts).unwrap();
            assert_eq!(p.state_keys.len(), 16);
            assert_eq!(copies(&p), 2, "{how:?}");
        }
        on_off_with(&format!("reg8 {how:?} unlifted"), &d, true, 40, |o| {
            Options {
                func_match: false,
                lift: false,
                words: false,
                ..o
            }
        });
    }
    assert!(notes.windows(2).all(|w| w[0] == w[1]));
}

#[test]
fn wide_registers_beyond_the_enumeration_cap_match() {
    // 16 and 32 bits of `dffe` chips: 33 and 65 input and state bits,
    // proven per bit (three support bits each).
    for w in [16u8, 32] {
        let mut d = Design::new("root");
        let f = dffe_def(&mut d, Dffe::Nand);
        let r = reg_def(&mut d, "reg", f, w, Defect::None);
        let i = info(&d, "reg");
        assert!(i.matched(), "{w}: {}", i.note);
        assert!(i.note.contains(&format!("{w}-bit Register")), "{}", i.note);
        assert_eq!(i.proof, Some(Proof::Sliced { max_support: 3 }));
        check_all(&format!("reg{w}"), &wrap(d, r, 1));
    }
}

#[test]
fn a_register_of_master_slave_flip_flops_matches_two_words() {
    // 16 master-slave flip-flops (two latches each): the masters form one
    // 16-bit register loading `D`, the slaves one loading the masters.
    let mut d = Design::new("root");
    let l = d_latch_def(&mut d, true);
    let f = dff_def(&mut d, l);
    let def = d.add_definition("reg16");
    let mut b = Builder::new(&mut d, def);
    let dd = b.input("D", 16);
    let clk = b.input("Clk", 1);
    let bits = b.bus_expand(dd);
    let q: Vec<NetId> = bits
        .iter()
        .map(|&x| b.instance(f, &[Some(x), Some(clk)])[0])
        .collect();
    let y = b.merge(&q);
    b.output("Q", y);
    let i = info(&d, "reg16");
    assert!(i.matched(), "{}", i.note);
    assert_eq!(
        i.note,
        "state [16-bit Register { en: !In(1), d: In(0) }, \
         16-bit Register { en: In(1), d: r0 }], outputs [r1]"
    );
    check_all("reg16 of flip-flops", &wrap(d, def, 1));
}

/// A register file of `n` `w`-bit registers of `dffe` chips: a shared `D`
/// bus, a one-hot `En` bus (one enable per register) and one output per
/// register.
fn regfile_def(d: &mut Design, n: u8, w: u8) -> DefId {
    let f = dffe_def(d, Dffe::Gates);
    let reg = reg_def(d, "reg", f, w, Defect::None);
    let def = d.add_definition("regfile");
    let mut b = Builder::new(d, def);
    let dd = b.input("D", w);
    let en = b.input("En", n);
    let ens = b.bus_expand(en);
    for (r, &e) in ens.iter().enumerate() {
        let q = b.instance(reg, &[Some(dd), Some(e)])[0];
        b.output(format!("Q{r}"), q);
    }
    def
}

#[test]
fn a_register_file_with_decoded_enables_matches() {
    for (n, w) in [(8u8, 8u8), (8, 16)] {
        let mut d = Design::new("root");
        let def = regfile_def(&mut d, n, w);
        let i = info(&d, "regfile");
        assert!(i.matched(), "{n} x {w}: {}", i.note);
        let regs = (0..n)
            .map(|r| format!("{w}-bit Register {{ en: In(1)[{r}], d: In(0) }}"))
            .collect::<Vec<_>>()
            .join(", ");
        let outs = (0..n)
            .map(|r| format!("r{r}"))
            .collect::<Vec<_>>()
            .join(", ");
        assert_eq!(i.note, format!("state [{regs}], outputs [{outs}]"));
        // The enables come from a decoder of an address and a write enable
        // in the root.
        let r = d.root;
        let mut b = Builder::new(&mut d, r);
        let x = b.input("d", w);
        let addr = b.input("addr", 3);
        let we = b.input("we", 1);
        let dec = b.decoder(addr);
        let ens: Vec<NetId> = dec.iter().map(|&e| b.and(e, we)).collect();
        let en = b.merge(&ens);
        for (k, q) in b
            .instance(def, &[Some(x), Some(en)])
            .into_iter()
            .enumerate()
        {
            b.output(format!("q{k}"), q);
        }
        check_all(&format!("regfile {n} x {w}"), &d);
        let p = compile(&d, &Options::default()).unwrap();
        assert_eq!(p.state_keys.len(), n as usize * w as usize);
    }
}

#[test]
fn near_miss_registers_are_not_one_register() {
    // Each defect keeps the chip from being one 8-bit register; whatever
    // the matcher makes of it (unmatched, or a few smaller registers) is
    // exact.
    for defect in [
        Defect::OtherEnable,
        Defect::Crossed,
        Defect::Reversed,
        Defect::Inverted,
        Defect::ExtraGate,
    ] {
        let mut d = Design::new("root");
        let f = dffe_def(&mut d, Dffe::Gates);
        let r = reg_def(&mut d, "reg8", f, 8, defect);
        let i = info(&d, "reg8");
        assert!(!i.note.contains("8-bit Register"), "{defect:?}: {}", i.note);
        let expect = match defect {
            // Bits 0-4 and 6-7 on one enable, bit 5 on the other.
            Defect::OtherEnable => Some(
                "state [5-bit Register { en: In(1), d: In(0)[0..5] }, \
                 2-bit Register { en: In(1), d: In(0)[6..8] }, \
                 1-bit Register { en: In(2), d: In(0)[5] }], \
                 outputs [r0 ++ r2 ++ r1]",
            ),
            // Bits 3 and 4 are read in the order opposite to their data.
            Defect::Crossed => Some(
                "state [3-bit Register { en: In(1), d: In(0)[0..3] }, \
                 1-bit Register { en: In(1), d: In(0)[3] }, \
                 1-bit Register { en: In(1), d: In(0)[4] }, \
                 3-bit Register { en: In(1), d: In(0)[5..8] }], \
                 outputs [r0 ++ r2 ++ r1 ++ r3]",
            ),
            Defect::Reversed => None,
            Defect::Inverted => Some(
                "state [3-bit Register { en: In(1), d: In(0)[0..3] }, \
                 1-bit Register { en: In(1), d: In(0)[3] }, \
                 4-bit Register { en: In(1), d: In(0)[4..8] }], \
                 outputs [r0 ++ !r1 ++ r2]",
            ),
            Defect::ExtraGate => None,
            Defect::None => unreachable!(),
        };
        match (defect, expect) {
            (_, Some(text)) => assert_eq!(i.note, text, "{defect:?}"),
            (Defect::Reversed, None) => {
                // Eight 1-bit registers, read in reverse.
                assert!(i.matched(), "{}", i.note);
                assert_eq!(i.note.matches("1-bit Register").count(), 8, "{}", i.note);
            }
            (_, None) => {
                assert!(!i.matched(), "{defect:?}: {}", i.note);
                assert!(
                    i.note
                        .starts_with("state element 3 (1-bit Delay): not a register"),
                    "{}",
                    i.note
                );
            }
        }
        check_all(&format!("reg8 {defect:?}"), &wrap(d, r, 1));
    }
}

/// Two unrelated functions in one chip: `Y0 = &A`, `Y1 = &B`, of gates.
fn two_ands_def(d: &mut Design) -> DefId {
    let def = d.add_definition("two_ands");
    let mut b = Builder::new(d, def);
    for (port, out) in [("A", "Y0"), ("B", "Y1")] {
        let x = b.input(port, 4);
        let bits: Vec<NetId> = (0..4).map(|k| b.bit(x, k)).collect();
        let lo = b.and(bits[0], bits[1]);
        let hi = b.and(bits[2], bits[3]);
        let y = b.and(lo, hi);
        b.output(out, y);
    }
    def
}

/// An output of a chip may feed an input that this output does not read:
/// no loop, in gates. A truth table reads every input for every output,
/// so such a chip must not become one (it was a cycle of instructions,
/// and the compiler panicked).
#[test]
fn a_chip_fed_from_its_own_unrelated_output_compiles() {
    let mut d = Design::new("root");
    let def = two_ands_def(&mut d);
    let r = d.root;
    let mut b = Builder::new(&mut d, r);
    let p = b.input("P", 4);
    let inst = b.node(NodeKind::Instance(def), &[Some(p), None]);
    let (y0, y1) = (b.out(inst, 0), b.out(inst, 1));
    let back = b.merge(&[y0, y0, y0, y0]);
    b.connect(back, (inst, 1));
    b.output("Y0", y0);
    b.output("Y1", y1);
    check_tables("fed back", &d);
}
