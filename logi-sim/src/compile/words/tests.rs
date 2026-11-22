//! Word-level cleanup: each rewrite against the reference and against the
//! same design compiled without the pass (every tick: outputs, state with
//! carry-over between the two programs, every net), near misses that must
//! not be rewritten, and random designs.

use crate::compile::func::tests::on_off_with;
use crate::compile::tests::check_program_invariants;
use crate::compile::{Commit, Op, Options, Program, compile};
use crate::netlist::{Builder, DefId, Design, NetId};
use crate::testkit::diff::{check_design, standard_configs};
use crate::testkit::{GenConfig, bitblast::bitblast, fixtures, generate};

/// With and without the pass, under the editor's options and the
/// defaults: equal every tick, and both equal to the reference. Returns
/// the default options' programs (with, without).
fn check(name: &str, d: &Design) -> (Program, Program) {
    for debug in [false, true] {
        on_off_with(name, d, debug, 80, |o| Options { words: false, ..o });
    }
    for o in [Options::default(), Options::debug()] {
        check_design(d, 11, 80, &o).unwrap_or_else(|e| panic!("{name}: {e}"));
    }
    let on = compile(d, &Options::default()).unwrap();
    let off = compile(
        d,
        &Options {
            words: false,
            ..Options::default()
        },
    )
    .unwrap();
    for p in [&on, &off, &compile(d, &Options::debug()).unwrap()] {
        check_program_invariants(p);
    }
    (on, off)
}

fn copies(p: &Program) -> usize {
    p.commits
        .iter()
        .filter(|c| matches!(c, Commit::Copy { .. }))
        .count()
}

fn count(p: &Program, op: Op) -> u32 {
    p.blocks.iter().filter(|b| b.op == op).map(|b| b.len).sum()
}

/// A root with a select and two `w`-bit words.
fn root(w: u8) -> (Design, NetId, NetId, NetId) {
    let mut d = Design::new("top");
    let r = d.root;
    let mut b = d.builder(r);
    let s = b.input("s", 1);
    let a = b.input("a", w);
    let x = b.input("b", w);
    (d, s, a, x)
}

#[test]
fn split_merge_round_trips_fold() {
    let (mut d, _, a, x) = root(16);
    let r = d.root;
    let mut b = d.builder(r);
    // Slice of a slice: a[6..10].
    let mid = b.split(a, 4, 8);
    let y0 = b.split(mid, 2, 4);
    // Split into nibbles and merged again: a itself.
    let parts: Vec<NetId> = (0..4).map(|k| b.split(a, 4 * k, 4)).collect();
    let y1 = b.merge(&parts);
    // Slices of a concatenation: the parts they select.
    let lo = b.split(x, 0, 8);
    let hi = b.split(a, 8, 8);
    let m = b.merge(&[lo, hi]);
    let y2 = b.split(m, 0, 8);
    let y3 = b.split(m, 8, 8);
    // Consecutive slices of x, merged: x[4..12].
    let p = b.split(x, 4, 4);
    let q = b.split(x, 8, 4);
    let y4 = b.merge(&[p, q]);
    for (k, y) in [y0, y1, y2, y3, y4].into_iter().enumerate() {
        b.output(format!("y{k}"), y);
    }
    let (on, off) = check("round trips", &d);
    // Four slices of the inputs and no concatenation.
    assert_eq!(on.instrs.len(), 4);
    assert_eq!(count(&on, Op::Concat), 0);
    assert!(off.instrs.len() >= on.instrs.len());
}

#[test]
fn muxes_over_adjacent_halves_merge() {
    let (mut d, s, a, x) = root(16);
    let r = d.root;
    let mut b = d.builder(r);
    let (al, ah) = (b.split(a, 0, 8), b.split(a, 8, 8));
    let (xl, xh) = (b.split(x, 0, 8), b.split(x, 8, 8));
    let lo = b.mux(s, al, xl);
    let hi = b.mux(s, ah, xh);
    let y = b.merge(&[lo, hi]);
    b.output("y", y);
    let (on, off) = check("mux halves", &d);
    assert_eq!(on.instrs.len(), 1, "one 16-bit Mux");
    assert!(off.instrs.len() >= 3);
    // Under the editor's options the halves are bit fields of the wide
    // mux (`check` compared every net).
    let p = compile(&d, &Options::debug()).unwrap();
    assert_eq!(p.instrs.len(), 1);
}

#[test]
fn bitwise_ops_over_adjacent_halves_merge() {
    let (mut d, _, a, x) = root(16);
    let r = d.root;
    let mut b = d.builder(r);
    let (al, ah) = (b.split(a, 0, 8), b.split(a, 8, 8));
    let (xl, xh) = (b.split(x, 0, 8), b.split(x, 8, 8));
    let and = [b.and(al, xl), b.and(ah, xh)];
    let xor = [b.xor(al, xl), b.xor(ah, xh)];
    let not = [b.not(al), b.not(ah)];
    let nand = [b.nand(al, xl), b.nand(ah, xh)];
    for (k, pair) in [and, xor, not, nand].into_iter().enumerate() {
        let y = b.merge(&pair);
        b.output(format!("y{k}"), y);
    }
    let (on, _) = check("bitwise halves", &d);
    assert_eq!(on.instrs.len(), 4, "one 16-bit op each");
}

#[test]
fn a_complemented_select_swaps_the_arms() {
    let (mut d, s, a, x) = root(8);
    let r = d.root;
    let mut b = d.builder(r);
    let ns = b.not(s);
    let y = b.mux(ns, a, x);
    let z = b.mux(s, x, a);
    b.output("y", y);
    b.output("z", z);
    let (on, _) = check("complemented select", &d);
    // Both are the same mux, and the complement is gone.
    assert_eq!(on.instrs.len(), 1);
}

#[test]
fn near_misses_are_not_merged() {
    let (mut d, s, a, x) = root(16);
    let r = d.root;
    let mut b = d.builder(r);
    let t = b.input("t", 1);
    let (al, ah) = (b.split(a, 0, 8), b.split(a, 8, 8));
    let (xl, xh) = (b.split(x, 0, 8), b.split(x, 8, 8));
    // Different selects.
    let lo = b.mux(s, al, xl);
    let hi = b.mux(t, ah, xh);
    let y0 = b.merge(&[lo, hi]);
    // Crossed halves: the arms' concatenations are not slices of one word.
    let lo = b.mux(s, ah, xl);
    let hi = b.mux(s, al, xh);
    let y1 = b.merge(&[lo, hi]);
    // Different ops.
    let lo = b.and(al, xl);
    let hi = b.or(ah, xh);
    let y2 = b.merge(&[lo, hi]);
    // Non-consecutive slices.
    let p = b.split(a, 0, 4);
    let q = b.split(a, 5, 4);
    let y3 = b.merge(&[p, q]);
    for (k, y) in [y0, y1, y2, y3].into_iter().enumerate() {
        b.output(format!("y{k}"), y);
    }
    let (on, off) = check("near misses", &d);
    assert_eq!(count(&on, Op::Concat), 4, "one concatenation per output");
    assert!(on.instrs.len() <= off.instrs.len());
}

/// A 16-bit register built as two 8-bit registers on the halves.
fn split_register(d: &mut Design) -> DefId {
    let def = d.add_definition("reg16");
    let mut b = Builder::new(d, def);
    let en = b.input("en", 1);
    let x = b.input("d", 16);
    let (lo, hi) = (b.split(x, 0, 8), b.split(x, 8, 8));
    let qlo = b.register(en, lo);
    let qhi = b.register(en, hi);
    let q = b.merge(&[qlo, qhi]);
    b.output("q", q);
    def
}

#[test]
fn registers_on_the_halves_of_a_word_merge() {
    let mut d = Design::new("top");
    let reg = split_register(&mut d);
    let r = d.root;
    let mut b = d.builder(r);
    let en = b.input("en", 1);
    let x = b.input("d", 16);
    let q = b.instance(reg, &[Some(en), Some(x)])[0];
    b.output("q", q);
    // A separate reader of one half.
    let lo = b.split(q, 0, 8);
    let y = b.not(lo);
    b.output("y", y);
    let (on, off) = check("split register", &d);
    assert_eq!(copies(&off), 2);
    assert_eq!(copies(&on), 1, "one 16-bit state word");
    // Both halves keep their own state keys (as bit fields of the word).
    assert_eq!(on.state_keys.len(), 2);
}

#[test]
fn master_slave_latch_pairs_on_halves_merge() {
    // Master/slave flip-flops of D latches on each half of a word: the
    // masters merge with each other as the slaves do.
    let mut d = Design::new("top");
    let r = d.root;
    let mut b = d.builder(r);
    let clk = b.clock(2, 1, 0);
    let nclk = b.not(clk);
    let x = b.input("d", 16);
    let mut q = Vec::new();
    for k in 0..2 {
        let h = b.split(x, 8 * k, 8);
        let m = b.dlatch(nclk, h);
        q.push(b.dlatch(clk, m));
    }
    let y = b.merge(&q);
    b.output("q", y);
    let (on, off) = check("latch pairs", &d);
    assert_eq!(copies(&off), 4);
    assert_eq!(copies(&on), 2, "a 16-bit master and a 16-bit slave");
}

#[test]
fn state_read_in_both_orders_is_exact() {
    // Concat(A, B) and Concat(B, A): at most one order merges.
    let mut d = Design::new("top");
    let r = d.root;
    let mut b = d.builder(r);
    let en = b.input("en", 1);
    let x = b.input("x", 8);
    let z = b.input("z", 8);
    let qa = b.register(en, x);
    let qb = b.register(en, z);
    let y0 = b.merge(&[qa, qb]);
    let y1 = b.merge(&[qb, qa]);
    b.output("y0", y0);
    b.output("y1", y1);
    check("both orders", &d);
}

#[test]
fn bitblasted_fixtures_match_with_and_without() {
    for (name, d) in [
        ("cpu", bitblast(&fixtures::cpu(&fixtures::MULTIPLY_PROGRAM))),
        ("counter", fixtures::counter_bitblasted(16)),
        ("mux tree", fixtures::mux_tree_bitblasted(3, 16)),
        ("register file", bitblast(&fixtures::register_file(3, 16))),
        ("fifo", bitblast(&fixtures::fifo(2, 8))),
        ("ripple adder", fixtures::ripple_adder(16)),
    ] {
        let (on, off) = check(name, &d);
        assert!(
            on.instrs.len() + on.commits.len() <= off.instrs.len() + off.commits.len(),
            "{name}"
        );
    }
}

#[test]
fn random_designs_match_with_and_without() {
    let configs = standard_configs();
    for seed in 0..60u64 {
        let mut cfg = GenConfig {
            seed: 7_000 + seed,
            ..configs[seed as usize % configs.len()].clone()
        };
        cfg.nodes = cfg.nodes.min(120);
        cfg.inject_loop = false;
        let d = generate(&cfg).design;
        check(&format!("random {seed}"), &d);
        // Bit-blasted: full of slices, concatenations and 1-bit state.
        if seed % 3 == 0 {
            cfg.instance_percent = 0;
            cfg.screens = false;
            let d = generate(&cfg).design;
            check(&format!("random {seed} bit-blasted"), &bitblast(&d));
        }
    }
}
