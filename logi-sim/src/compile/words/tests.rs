//! Word-level cleanup: each rewrite against the reference and against the
//! same design compiled without the pass (every tick: outputs, state with
//! carry-over between the two programs, every net), near misses that must
//! not be rewritten, and random designs.

use crate::compile::func::tests::on_off_with;
use crate::compile::tests::check_program_invariants;
use crate::compile::{Commit, Op, Options, Program, compile};
use crate::netlist::{Builder, DefId, Design, NetId};
use crate::testkit::diff::{check_design, check_jit_design, standard_configs};
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
    // Function matching already sees one 16-bit register (two 8-bit
    // fields); without it, this pass merges the halves.
    assert_eq!(copies(&off), 1);
    let unmatched = |words| {
        compile(
            &d,
            &Options {
                func_match: false,
                words,
                ..Options::default()
            },
        )
        .unwrap()
    };
    assert_eq!(copies(&unmatched(false)), 2);
    for p in [&on, &unmatched(true)] {
        assert_eq!(copies(p), 1, "one 16-bit state word");
        // Both halves keep their own state keys (as bit fields of the word).
        assert_eq!(p.state_keys.len(), 2);
    }
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

/// The perf gate's 10k-node word DAG (`dag32_10k`), which the pass
/// shrinks most: exact against the pass off and the reference.
#[test]
fn large_word_dag_matches_with_and_without() {
    let d = generate(&GenConfig::dag(1, 10_000, 32)).design;
    for debug in [false, true] {
        on_off_with("dag32_10k", &d, debug, 12, |o| Options {
            words: false,
            ..o
        });
    }
    check_design(&d, 3, 12, &Options::default()).unwrap();
}

// ── Shifts and barrel shifters (`Options::shifts`, `Options::barrels`) ──

/// The options without the shift rewrites.
fn no_shifts(o: Options) -> Options {
    Options {
        shifts: false,
        barrels: false,
        ..o
    }
}

/// `check`, plus the same comparison against the shift rewrites off
/// (every net, every tick, both option sets) and the JIT. Returns the
/// default options' programs (with, without the shift rewrites).
fn check_shifts(name: &str, d: &Design) -> (Program, Program) {
    check(name, d);
    for debug in [false, true] {
        on_off_with(name, d, debug, 80, no_shifts);
        on_off_with(name, d, debug, 80, |o| Options {
            barrels: false,
            ..o
        });
    }
    check_jit_design(d, 80, 5).unwrap_or_else(|e| panic!("{name}: {e}"));
    let on = compile(d, &Options::default()).unwrap();
    let off = compile(d, &no_shifts(Options::default())).unwrap();
    for p in [&on, &off] {
        check_program_invariants(p);
    }
    (on, off)
}

/// `y << k` as a design writes it, `w` bits wide: the low `w - k` bits of
/// `y` (all of `y` when it is narrower) above `k` zero bits.
fn shl_lit(b: &mut Builder<'_>, y: NetId, k: u8, w: u8) -> NetId {
    let wy = b.width(y);
    let lo = b.zero(k);
    let keep = (w - k).min(wy);
    let hi = if keep == wy { y } else { b.split(y, 0, keep) };
    let r = b.merge(&[lo, hi]);
    if keep + k < w { b.zext(r, w) } else { r }
}

/// A barrel shifter of `stages` mux stages: `x << v[0..stages]`, `w` bits
/// wide (`x` may be narrower). Stage `i` muxes on bit `bits[i]` of `v`
/// and shifts by `shifts[i]`.
fn barrel(b: &mut Builder<'_>, x: NetId, v: NetId, w: u8, bits: &[u8], shifts: &[u8]) -> NetId {
    let mut y = x;
    for (&bit, &k) in bits.iter().zip(shifts) {
        let t = shl_lit(b, y, k, w);
        let s = b.bit(v, bit);
        if b.width(y) < w {
            y = b.zext(y, w);
        }
        y = b.mux(s, y, t);
    }
    y
}

fn ops(p: &Program) -> usize {
    p.instrs.len()
}

#[test]
fn zero_low_concatenations_are_shifts() {
    let (mut d, _, a, x) = root(16);
    let r = d.root;
    let mut b = d.builder(r);
    // {a[14:0], 1'b0}, {a[11:4], 8'b0}, and {x, 4'b0} into 20 bits.
    let y0 = shl_lit(&mut b, a, 1, 16);
    let mid = b.split(a, 4, 8);
    let y1 = shl_lit(&mut b, mid, 8, 16);
    let y2 = shl_lit(&mut b, x, 4, 20);
    // A shift of a shift: {{a[13:0], 1'b0}, 1'b0}.
    let s1 = shl_lit(&mut b, a, 1, 15);
    let y3 = shl_lit(&mut b, s1, 1, 16);
    // A slice of a shift: its operand's bits (no instruction), or zero.
    let y4 = b.split(y0, 4, 8);
    let y5 = b.split(y1, 0, 8);
    for (k, y) in [y0, y1, y2, y3, y4, y5].into_iter().enumerate() {
        b.output(format!("y{k}"), y);
    }
    let (on, off) = check_shifts("zero-low concatenations", &d);
    assert_eq!(count(&on, Op::Shl), 4, "{:?}", on.blocks);
    assert_eq!(count(&on, Op::Concat), 0);
    // y4 is a[3..11] (one slice), y5 a constant.
    assert_eq!(count(&on, Op::Shr), 1);
    assert_eq!(ops(&on), 5);
    assert_eq!(count(&off, Op::Shl), 0);
    assert!(ops(&off) > ops(&on), "{} > {}", ops(&off), ops(&on));
}

#[test]
fn barrel_shifters_become_one_shift() {
    for w in [4u8, 8, 13, 16, 32, 64] {
        for stages in 1..=6u8 {
            if 1u32 << (stages - 1) >= w as u32 {
                continue;
            }
            // Amount words exactly `stages` bits wide, and wider (the
            // high bits are not part of the shifter).
            for vw in [stages, stages + 2] {
                let mut d = Design::new("top");
                let r = d.root;
                let mut b = d.builder(r);
                let x = b.input("x", w);
                let v = b.input("v", vw);
                let bits: Vec<u8> = (0..stages).collect();
                let shifts: Vec<u8> = bits.iter().map(|&i| 1 << i).collect();
                let y = barrel(&mut b, x, v, w, &bits, &shifts);
                b.output("y", y);
                let name = format!("barrel w{w} stages{stages} v{vw}");
                let (on, off) = check_shifts(&name, &d);
                assert_eq!(count(&on, Op::ShlV), 1, "{name}: {:?}", on.blocks);
                assert_eq!(count(&on, Op::Mux), 0, "{name}");
                // The shift, and a slice of the amount when it is wider.
                assert_eq!(ops(&on), 1 + usize::from(vw > stages), "{name}");
                assert!(ops(&off) >= 2 * stages as usize, "{name}");
                assert_eq!(count(&off, Op::ShlV), 0, "{name}");
            }
        }
    }
}

#[test]
fn partial_width_barrels_are_exact() {
    // An 8-bit word shifted into 16 bits by a 4-bit amount (shifts up to
    // 15, past the operand's width), read whole and in parts.
    let mut d = Design::new("top");
    let r = d.root;
    let mut b = d.builder(r);
    let x = b.input("x", 8);
    let v = b.input("v", 4);
    let y = barrel(&mut b, x, v, 16, &[0, 1, 2, 3], &[1, 2, 4, 8]);
    let hi = b.split(y, 8, 8);
    let lo = b.split(y, 0, 4);
    b.output("y", y);
    b.output("hi", hi);
    b.output("lo", lo);
    // The same shifter over a 16-bit word, kept to its low 8 bits.
    let z = b.input("z", 16);
    let y = barrel(&mut b, z, v, 16, &[0, 1, 2], &[1, 2, 4]);
    let low = b.split(y, 0, 8);
    b.output("low", low);
    let (on, off) = check_shifts("partial width", &d);
    assert_eq!(count(&on, Op::ShlV), 2, "{:?}", on.blocks);
    assert_eq!(count(&on, Op::Mux), 0);
    assert!(ops(&on) < ops(&off));
}

#[test]
fn barrel_stages_read_elsewhere_stay_exact() {
    // Every stage is also an output: each is a shift by fewer bits.
    let mut d = Design::new("top");
    let r = d.root;
    let mut b = d.builder(r);
    let x = b.input("x", 16);
    let v = b.input("v", 4);
    let mut y = x;
    for i in 0..4u8 {
        let t = shl_lit(&mut b, y, 1 << i, 16);
        let s = b.bit(v, i);
        y = b.mux(s, y, t);
        b.name_net(y, format!("stage{i}"));
        b.output(format!("y{i}"), y);
    }
    let (on, off) = check_shifts("observed stages", &d);
    assert_eq!(count(&on, Op::ShlV), 4);
    assert_eq!(count(&on, Op::Mux), 0);
    assert!(ops(&on) < ops(&off));
    // Under the editor's options too (every net observable).
    let p = compile(&d, &Options::debug()).unwrap();
    assert_eq!(count(&p, Op::Mux), 0, "{:?}", p.blocks);
}

#[test]
fn near_miss_shifters_are_not_rewritten() {
    let mut d = Design::new("top");
    let r = d.root;
    let mut b = d.builder(r);
    let x = b.input("x", 16);
    let v = b.input("v", 4);
    let u = b.input("u", 4);
    // A stage shifting by 3 (not a power of two).
    let y0 = barrel(&mut b, x, v, 16, &[0, 1], &[1, 3]);
    // Stage 1 on the wrong bit of the amount.
    let y1 = barrel(&mut b, x, v, 16, &[0, 2], &[1, 2]);
    // The stages' selects from two words.
    let t = shl_lit(&mut b, x, 1, 16);
    let s = b.bit(v, 0);
    let m = b.mux(s, x, t);
    let t = shl_lit(&mut b, m, 2, 16);
    let s = b.bit(u, 1);
    let y2 = b.mux(s, m, t);
    // Stages in descending order.
    let y3 = barrel(&mut b, x, v, 16, &[1, 0], &[2, 1]);
    // Arms swapped: `v[0] ? x : x << 1`.
    let t = shl_lit(&mut b, x, 1, 16);
    let s = b.bit(v, 0);
    let y4 = b.mux(s, t, x);
    // A concatenation whose low part is not zero.
    let one = b.one();
    let hi = b.split(x, 0, 15);
    let y5 = b.merge(&[one, hi]);
    // Bit 0 shifting by 2; bit 1 shifting by 4.
    let y6 = barrel(&mut b, x, v, 16, &[0], &[2]);
    let y7 = barrel(&mut b, x, v, 16, &[0, 1], &[1, 4]);
    // A stage 8 bits wide, then one 16 bits wide: the bit the first
    // shifted out of 8 bits is gone.
    let x8 = b.split(x, 0, 8);
    let s0 = barrel(&mut b, x8, v, 8, &[0], &[1]);
    let s0 = b.zext(s0, 16);
    let y8 = barrel(&mut b, s0, v, 16, &[1], &[2]);
    for (k, y) in [y0, y1, y2, y3, y4, y5, y6, y7, y8].into_iter().enumerate() {
        b.output(format!("y{k}"), y);
    }
    let (on, off) = check_shifts("near misses", &d);
    // y0, y1, y2, y7: the first stage alone is a shift (one `ShlV` of `x`
    // by `v[0]`, shared), the second stays a mux. y3: the first stage
    // stays a mux, and the last (by `v[0]`) is a shift of it. y4 and y6
    // keep their mux. y8: an 8-bit shift and a mux.
    assert_eq!(count(&on, Op::Mux), 8, "{:?}", on.blocks);
    assert_eq!(count(&on, Op::ShlV), 3, "{:?}", on.blocks);
    assert_eq!(count(&on, Op::Concat), 1);
    assert!(ops(&on) <= ops(&off));
}

#[test]
fn shift_rewrites_off_leave_no_shift_ops() {
    let mut designs = vec![
        ("cpu", bitblast(&fixtures::cpu(&fixtures::MULTIPLY_PROGRAM))),
        ("mux tree", fixtures::mux_tree_bitblasted(3, 16)),
    ];
    let mut d = Design::new("top");
    let r = d.root;
    let mut b = d.builder(r);
    let x = b.input("x", 16);
    let v = b.input("v", 4);
    let y = barrel(&mut b, x, v, 16, &[0, 1, 2, 3], &[1, 2, 4, 8]);
    b.output("y", y);
    designs.push(("barrel", d));
    for (name, d) in &designs {
        for o in [Options::default(), Options::debug()] {
            let p = compile(d, &no_shifts(o)).unwrap();
            assert_eq!(count(&p, Op::Shl) + count(&p, Op::ShlV), 0, "{name}");
        }
        // `barrels` alone off: literal shifts only.
        let p = compile(
            d,
            &Options {
                barrels: false,
                ..Options::default()
            },
        )
        .unwrap();
        assert_eq!(count(&p, Op::ShlV), 0, "{name}");
        // `barrels` needs `shifts`.
        let p = compile(
            d,
            &Options {
                shifts: false,
                ..Options::default()
            },
        )
        .unwrap();
        assert_eq!(count(&p, Op::Shl) + count(&p, Op::ShlV), 0, "{name}");
    }
}

#[test]
fn random_shifters_match_the_reference() {
    // Random widths, stage counts, amounts and readers, with state
    // between them (a shifted register).
    let mut rng = crate::testkit::rng::Rng::new(4242);
    for n in 0..40 {
        let mut d = Design::new("top");
        let r = d.root;
        let mut b = d.builder(r);
        let w = 2 + rng.below(63) as u8;
        let wx = 1 + rng.below(w as usize) as u8;
        let x = b.input("x", wx);
        let max = (0..6).take_while(|&i| (1u32 << i) < w as u32).count() as u8;
        let stages = 1 + rng.below(max as usize) as u8;
        let v = b.input("v", stages + rng.below(3) as u8);
        let en = b.input("en", 1);
        let bits: Vec<u8> = (0..stages).collect();
        let shifts: Vec<u8> = bits.iter().map(|&i| 1 << i).collect();
        let y = barrel(&mut b, x, v, w, &bits, &shifts);
        let q = b.register(en, y);
        let k = 1 + rng.below(w as usize - 1) as u8;
        let z = shl_lit(&mut b, q, k, w);
        let lo = rng.below(w as usize) as u8;
        let len = 1 + rng.below((w - lo) as usize) as u8;
        let part = b.split(z, lo, len);
        b.output("y", y);
        b.output("z", z);
        b.output("part", part);
        let (on, off) = check_shifts(&format!("random shifter {n} (w{w} x{wx} s{stages})"), &d);
        assert!(ops(&on) <= ops(&off));
        assert_eq!(
            count(&on, Op::ShlV),
            1,
            "random shifter {n}: {:?}",
            on.blocks
        );
    }
}

// ── The rewrites on the graph itself ──────────────────────────────────────

use super::{Emission, Graph, Kind};
use crate::compile::{FastMap, Instr, simplify::fold};
use crate::netlist::mask;
use crate::testkit::rng::Rng;

fn graph(shifts: bool, barrels: bool) -> Graph<'static> {
    Graph {
        nodes: Vec::new(),
        cons: FastMap::default(),
        pieces: FastMap::default(),
        mems: &[],
        copy_state: &[],
        group_ends: FastMap::default(),
        pairs: Vec::new(),
        shifts,
        barrels,
    }
}

/// The value of node `n` with leaf `k` (slot `k`) holding `leaves[k]`.
fn eval(g: &Graph<'_>, n: u32, leaves: &[u64]) -> u64 {
    let mut memo = FastMap::default();
    eval_in(g, n, leaves, &mut memo)
}

fn eval_in(g: &Graph<'_>, n: u32, leaves: &[u64], memo: &mut FastMap<u32, u64>) -> u64 {
    if let Some(&v) = memo.get(&n) {
        return v;
    }
    let x = *g.node(n);
    let r = match x.kind {
        Kind::Leaf => leaves[x.a as usize],
        Kind::Const => x.imm,
        Kind::Op(op) => {
            let (sa, sb, sc) = op.slot_operands();
            let mut v = |is: bool, m: u32| is.then(|| eval_in(g, m, leaves, memo));
            let (va, vb, vc) = (v(sa, x.a), v(sb, x.b), v(sc, x.c));
            let ins = Instr {
                dst: 0,
                a: x.a,
                b: x.b,
                c: x.c,
            };
            let get = |s: u32| {
                if sa && s == x.a {
                    va
                } else if sb && s == x.b {
                    vb
                } else if sc && s == x.c {
                    vc
                } else {
                    None
                }
            };
            fold(op, &ins, x.imm, &get, &[]).expect("no clocks or memories")
        }
    };
    memo.insert(n, r);
    r
}

/// A node and the value it must have.
#[derive(Clone, Copy)]
struct Val {
    n: u32,
    v: u64,
}

/// Random expressions of slices, concatenations (with zero low parts),
/// muxes, `ShlV`s (with constant amounts too) and barrel-shifter chains
/// (with wrong stages mixed in), built through the graph's rewrites:
/// every node keeps the value of the expression it stands for, within
/// its width. Then `Emission::locate` on random bit fields: a located
/// field holds the same bits.
fn random_graph(seed: u64, shifts: bool, barrels: bool) -> usize {
    let mut rng = Rng::new(seed);
    let mut g = graph(shifts, barrels);
    let mut leaves = Vec::new();
    let mut pool: Vec<Val> = Vec::new();
    for k in 0..5u32 {
        let w = 1 + rng.below(64) as u8;
        let v = rng.next_u64() & mask(w);
        leaves.push(v);
        pool.push(Val { n: g.leaf(k, w), v });
    }
    let mut shl_ops = 0;
    for _ in 0..60 {
        let pick = |rng: &mut Rng, pool: &[Val]| pool[rng.below(pool.len())];
        let new = match rng.below(7) {
            0 => {
                let x = pick(&mut rng, &pool);
                let w = g.w(x.n) as u32;
                let off = rng.below(w as usize) as u32;
                let len = 1 + rng.below((w - off) as usize) as u32;
                Val {
                    n: g.slice(x.n, off, len),
                    v: (x.v >> off) & mask(len as u8),
                }
            }
            1 | 2 => {
                // `p | q << c`, `p` constant zero half of the time.
                let q = pick(&mut rng, &pool);
                let c = 1 + rng.below(63) as u32;
                let w = (c + g.w(q.n) as u32).min(64) as u8;
                let p = if rng.coin() {
                    let wp = 1 + rng.below(c as usize) as u8;
                    Val {
                        n: g.konst(0, wp),
                        v: 0,
                    }
                } else {
                    let p = pick(&mut rng, &pool);
                    if g.w(p.n) as u32 > c {
                        continue;
                    }
                    p
                };
                Val {
                    n: g.mk(Op::Concat, p.n, q.n, c, 0, w),
                    v: (p.v | q.v << c) & mask(w),
                }
            }
            3 => {
                let s = pick(&mut rng, &pool);
                let sel = g.slice(s.n, 0, 1);
                let (y, t) = (pick(&mut rng, &pool), pick(&mut rng, &pool));
                let w = g.w(y.n).max(g.w(t.n));
                Val {
                    n: g.mk(Op::Mux, sel, y.n, t.n, 0, w),
                    v: if s.v & 1 != 0 { t.v } else { y.v },
                }
            }
            4 => {
                let a = pick(&mut rng, &pool);
                let b = if rng.coin() {
                    pick(&mut rng, &pool)
                } else {
                    let k = rng.next_u64() & 127;
                    Val {
                        n: g.konst(k, 7),
                        v: k,
                    }
                };
                let imm = match rng.below(3) {
                    0 => mask(1 + rng.below(64) as u8),
                    1 => rng.next_u64(),
                    _ => mask(1 + rng.below(64) as u8) & (u64::MAX << rng.below(64)),
                };
                let w = (64 - imm.leading_zeros()).max(1) as u8;
                Val {
                    n: g.mk(Op::ShlV, a.n, b.n, 0, imm, w),
                    v: a.v.checked_shl((b.v % 64) as u32).unwrap_or(0) & imm,
                }
            }
            _ => {
                // A barrel shifter, stage by stage as a design writes it.
                let x = pick(&mut rng, &pool);
                let v = pick(&mut rng, &pool);
                let wv = g.w(v.n);
                let mut w = g.w(x.n).max(2 + rng.below(63) as u8).min(64);
                let mut y = x;
                for i in 0..rng.below(7) as u8 {
                    if rng.below(4) == 0 {
                        // A wider stage.
                        w = (w as u32 + rng.below(8) as u32).min(64) as u8;
                    }
                    let mut bit = i;
                    let mut k = 1u32 << i;
                    match rng.below(10) {
                        0 => bit = rng.below(7) as u8,
                        1 => k = 1 + rng.below(40) as u32,
                        _ => {}
                    }
                    if bit >= wv || k >= w as u32 {
                        break;
                    }
                    // Sometimes the stage shifts `y`'s bits from one up:
                    // the same `Shl` operand, a different bit field.
                    let j = u32::from(rng.below(8) == 0 && g.w(y.n) as u32 > 1);
                    let len = w as u32 - k - j;
                    let (base, yo, yl) = g.as_slice(y.n);
                    let (hi, hv) = if base != y.n && rng.below(4) == 0 {
                        // Or the bits of `y`'s word one above `y`: the
                        // same length, another offset.
                        let len = (yl as u32).min(w as u32 - k);
                        let bv = eval(&g, base, &leaves);
                        let off = yo as u32 + 1;
                        (g.slice(base, off, len), (bv >> off) & mask(len as u8))
                    } else {
                        (g.slice(y.n, j, len), (y.v >> j) & mask(len as u8))
                    };
                    let z = g.konst(0, k as u8);
                    let t = g.mk(Op::Concat, z, hi, k, 0, w);
                    let tv = (hv << k) & mask(w);
                    let sel = g.slice(v.n, bit as u32, 1);
                    let on = (v.v >> bit) & 1 != 0;
                    y = Val {
                        n: g.mk(Op::Mux, sel, y.n, t, 0, w),
                        v: if on { tv } else { y.v },
                    };
                }
                y
            }
        };
        let got = eval(&g, new.n, &leaves);
        assert_eq!(got, new.v, "seed {seed}: node {:?}", g.node(new.n));
        assert_eq!(got & !mask(g.w(new.n)), 0, "seed {seed}: within its width");
        pool.push(new);
    }
    for n in 0..g.nodes.len() as u32 {
        if matches!(g.node(n).kind, Kind::Op(Op::Shl | Op::ShlV)) {
            shl_ops += 1;
        }
    }
    // `locate`: some op nodes have slots.
    let mut e = Emission {
        home: FastMap::default(),
        redirect: FastMap::default(),
        stack: Vec::new(),
        zero: g.konst(0, 1),
    };
    for n in 0..g.nodes.len() as u32 {
        if matches!(g.node(n).kind, Kind::Op(_)) && rng.coin() {
            e.home.insert(n, 0);
        }
    }
    for _ in 0..200 {
        let x = pool[rng.below(pool.len())];
        let w = g.w(x.n);
        let off = rng.below(w as usize) as u8;
        let len = 1 + rng.below((w - off) as usize) as u8;
        if let Some((n2, off2, len2)) = e.locate(&g, x.n, off, len) {
            assert!(len2 >= 1 && len2 <= len);
            let k = g.node(n2).kind;
            assert!(matches!(k, Kind::Leaf | Kind::Const) || e.home.contains_key(&n2));
            let want = (x.v >> off) & mask(len2);
            let have = (eval(&g, n2, &leaves) >> off2) & mask(len2);
            assert_eq!(
                have,
                want,
                "seed {seed}: bits {off}+{len} of {:?}",
                g.node(x.n)
            );
        }
    }
    shl_ops
}

/// Barrel shifters whose operand, or amount, is 0 in one clock phase:
/// the variants fold them (a shift of 0 is 0; a shift by 0 is its
/// operand, not 0), exactly. A latch pair on the clock makes the
/// variants pay.
#[test]
fn clock_gated_shifters_specialize() {
    let mut d = Design::new("top");
    let r = d.root;
    let mut b = d.builder(r);
    let clk = b.clock(2, 1, 0);
    let nclk = b.not(clk);
    let x = b.input("x", 16);
    let v = b.input("v", 3);
    // `clk ? x : 0`, shifted: 0 in the low phase.
    let gx = b.switch(clk, x);
    let y0 = barrel(&mut b, gx, v, 16, &[0, 1, 2], &[1, 2, 4]);
    // `x` shifted by `clk ? v : 0`: `x` in the low phase.
    let gv = b.switch(clk, v);
    let y1 = barrel(&mut b, x, gv, 16, &[0, 1, 2], &[1, 2, 4]);
    let both = b.xor(y0, y1);
    let m = b.dlatch(nclk, both);
    let q = b.dlatch(clk, m);
    b.output("q", q);
    b.output("y0", y0);
    b.output("y1", y1);
    let (on, _) = check_shifts("clock-gated shifters", &d);
    for o in [Options::default(), Options::debug()] {
        on_off_with("clock-gated shifters", &d, o.debug, 60, |o| Options {
            clock_phases: false,
            ..o
        });
    }
    assert_eq!(count(&on, Op::ShlV), 2);
    let ph = on.phases.as_ref().expect("clock-phase variants");
    let shifts = |body: &crate::compile::Body| -> u32 {
        body.blocks
            .iter()
            .filter(|b| b.op == Op::ShlV)
            .map(|b| b.len)
            .sum()
    };
    let n: Vec<u32> = ph.variants.iter().map(shifts).collect();
    // The clock is high in variant 0; the low phase computes only the
    // shift by 0 (of `x`).
    assert_eq!(n, vec![2, 1], "{:?}", ph.variants[1].blocks);
}

#[test]
fn graph_rewrites_keep_every_value() {
    let mut shl_ops = 0;
    for seed in 0..400 {
        shl_ops += random_graph(seed, true, true);
        random_graph(seed, true, false);
        random_graph(seed, false, false);
    }
    assert!(shl_ops > 1000, "{shl_ops}");
}
