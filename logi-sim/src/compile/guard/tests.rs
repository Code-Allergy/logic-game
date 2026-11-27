//! Enable-guarded cones: which cones are guarded (and which near misses
//! are not), the layout invariants of a guarded body, and guards on
//! against off every tick under every runtime path.

use std::sync::Arc;

use super::*;
use crate::compile::{Options, Program, compile, phases};
use crate::netlist::{Builder, DefId, Design, NetId};
use crate::sim::{Sim, SimOptions};
use crate::testkit::diff::{check_batches, check_design_with, check_jit, check_pair};
use crate::testkit::{GenConfig, fixtures, generate};

fn own(p: &Program) -> Body {
    Body {
        blocks: p.blocks.clone(),
        instrs: p.instrs.clone(),
        imms: p.imms.clone(),
        commits: p.commits.clone(),
        guards: Vec::new(),
    }
}

/// The fused-run body of an unspecialized program, guarded with regions
/// of at least `min` instructions.
fn fused(p: &Program, min: usize) -> Option<Body> {
    let own = own(p);
    let pruned = phases::prune(p, &own).unwrap_or(own);
    guard_with(p.slot_count as usize, &pruned, None, min)
}

/// Instructions inside guards.
fn guarded_instrs(g: &Body) -> u32 {
    g.guards
        .iter()
        .flat_map(|gd| &g.blocks[gd.start as usize..gd.end as usize])
        .map(|b| b.len)
        .sum()
}

/// Layout invariants of a guarded body against the body it came from:
/// the same instructions and commits; every operand written before it is
/// read (or by no instruction), never by the same block; guards sorted and
/// disjoint, each condition written before its guard; no guarded slot
/// read by a commit or exposed.
fn check_layout(p: &Program, from: &Body, g: &Body, exposed: Option<&[bool]>) {
    let key = |b: &Body| {
        let mut v: Vec<(u32, u32, u32, u32, u64)> = b
            .instrs
            .iter()
            .zip(&b.imms)
            .map(|(i, &m)| (i.dst, i.a, i.b, i.c, m))
            .collect();
        v.sort_unstable();
        v
    };
    assert_eq!(key(from), key(g), "the same instructions");
    assert_eq!(from.commits, g.commits);
    let n = p.slot_count as usize;
    let mut written_in = vec![u32::MAX; n];
    let mut writer_any = vec![false; n];
    for i in &g.instrs {
        writer_any[i.dst as usize] = true;
    }
    let mut guarded = vec![false; n];
    let mut at = 0;
    for gd in &g.guards {
        assert!(gd.start >= at && gd.end > gd.start, "sorted, non-empty");
        assert!(!gd.conds.is_empty() || !gd.any.is_empty());
        at = gd.end;
    }
    for (bi, blk) in g.blocks.iter().enumerate() {
        let inside = g
            .guards
            .iter()
            .find(|x| x.start as usize <= bi && bi < x.end as usize);
        if let Some(gd) = inside
            && gd.start as usize == bi
        {
            for s in gd.slots() {
                assert!(
                    !writer_any[s as usize] || written_in[s as usize] < bi as u32,
                    "condition slot {s} written before its guard"
                );
            }
            assert!(gd.any.len() <= MAX_ANY, "{gd:?}");
        }
        let (sa, sb, sc) = blk.op.slot_operands();
        let range = blk.start as usize..(blk.start + blk.len) as usize;
        for i in &g.instrs[range.clone()] {
            for (is, s) in [(sa, i.a), (sb, i.b), (sc, i.c)] {
                if is && writer_any[s as usize] {
                    assert!(
                        written_in[s as usize] < bi as u32,
                        "slot {s} read before (or in the block) it is written"
                    );
                }
            }
        }
        for i in &g.instrs[range] {
            written_in[i.dst as usize] = bi as u32;
            guarded[i.dst as usize] = inside.is_some();
        }
    }
    for c in &g.commits {
        for s in c.reads() {
            assert!(!guarded[s as usize], "commit reads guarded slot {s}");
        }
    }
    if let Some(e) = exposed {
        for (s, &x) in e.iter().enumerate() {
            assert!(!(x && guarded[s]), "exposed slot {s} is guarded");
        }
    }
}

/// Guards on against off: tick by tick (everything observable after each
/// tick, batch, poke, reset and import), the JIT against the interpreter,
/// fused batches against single ticks, under the editor's options and the
/// defaults, and against the reference; and the layout invariants of
/// every body a `Sim` guards.
fn on_off(name: &str, d: &Design, ticks: u64) {
    for o in [Options::default(), Options::debug()] {
        let p = Arc::new(compile(d, &o).unwrap());
        let off = SimOptions {
            guard: false,
            ..guarded_options()
        };
        // Every region size down to single instructions.
        let small = SimOptions {
            guard_min: 1,
            ..guarded_options()
        };
        for on in [
            guarded_options(),
            small.clone(),
            SimOptions {
                guard_min: 4,
                ..guarded_options()
            },
            SimOptions {
                phases: false,
                ..small.clone()
            },
            SimOptions {
                stream: false,
                ..small.clone()
            },
        ] {
            check_pair(&p, ticks, 5, &off, &on).unwrap_or_else(|e| panic!("{name}: {e}"));
        }
        check_jit(&p, ticks, 6, &guarded_options()).unwrap_or_else(|e| panic!("{name}: {e}"));
        check_jit(&p, ticks, 8, &small).unwrap_or_else(|e| panic!("{name}: {e}"));
        check_batches(&p, 7).unwrap_or_else(|e| panic!("{name}: {e}"));
        check_design_with(d, 5, ticks.min(100), &o, &guarded_options())
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        let own = own(&p);
        let e = phases::exposed(&p);
        let mut bodies = vec![own];
        bodies.extend(p.phases.iter().flat_map(|ph| ph.variants.iter().cloned()));
        for b in &bodies {
            if let Some(g) = guard(&p, b, Some(&e)) {
                check_layout(&p, b, &g, Some(&e));
            }
            let pruned = phases::prune(&p, b).unwrap_or_else(|| b.clone());
            for min in [1, MIN_REGION] {
                if let Some(g) = guard_with(p.slot_count as usize, &pruned, None, min) {
                    check_layout(&p, &pruned, &g, None);
                }
            }
        }
    }
}

/// `(x + k) ^ (x << 1) ...`, twice over: a data cone of more than
/// [`MIN_REGION`] word ops.
fn cone(b: &mut Builder<'_>, x: NetId, k: u64) -> NetId {
    let mut v = x;
    for r in 0..2 {
        let c = b.konst(8, k + r);
        let (t1, _) = b.add(v, c, None);
        let lo = b.split(v, 0, 7);
        let z = b.zero(1);
        let sh = b.merge(&[z, lo]);
        let t2 = b.xor(t1, sh);
        let t3 = b.and(t2, x);
        let t4 = b.or(t3, c);
        let t5 = b.not(t4);
        v = b.xor(t5, t1);
    }
    v
}

/// The cone of a register's data input is guarded by its enable.
#[test]
fn register_data_cone_is_guarded() {
    let mut d = Design::new("top");
    let r = d.root;
    let mut b = d.builder(r);
    let x = b.input("x", 8);
    let en = b.input("en", 1);
    let dd = cone(&mut b, x, 3);
    let q = b.register(en, dd);
    b.output("q", q);
    let p = compile(&d, &Options::default()).unwrap();
    let g = fused(&p, MIN_REGION).expect("the data cone is guarded");
    assert_eq!(g.guards.len(), 1);
    let gd = &g.guards[0];
    assert_eq!(
        gd.conds,
        vec![Cond {
            slot: p.ports.inputs[1].slot,
            when: true
        }]
    );
    let n = guarded_instrs(&g);
    assert!(n >= 5, "{n} guarded instructions");
    on_off("register", &d, 300);
}

/// Near miss: a cone value with a second consumer (another register) is
/// not guarded; only the part feeding nothing else is.
#[test]
fn a_cone_with_a_second_consumer_is_not_guarded() {
    let mut d = Design::new("top");
    let r = d.root;
    let mut b = d.builder(r);
    let x = b.input("x", 8);
    let en = b.input("en", 1);
    let shared = cone(&mut b, x, 5);
    let one = b.one();
    let other = b.register(one, shared);
    let y = b.inc(shared);
    let q = b.register(en, y);
    b.output("q", q);
    b.output("o", other);
    let p = compile(&d, &Options::default()).unwrap();
    // Only the increment is exclusive to the enabled register: too small
    // to guard by default, and the only guarded instruction at size 1.
    assert!(fused(&p, MIN_REGION).is_none());
    let g = fused(&p, 1).expect("the increment alone");
    assert_eq!(guarded_instrs(&g), 1);
    on_off("shared cone", &d, 300);
}

/// Near miss: a cone value read by a root output is exposed in a full
/// settle (so not guarded there), but guarded in a fused-run body, which
/// nothing observes.
#[test]
fn an_observed_cone_is_guarded_only_in_fused_bodies() {
    let mut d = Design::new("top");
    let r = d.root;
    let mut b = d.builder(r);
    let x = b.input("x", 8);
    let en = b.input("en", 1);
    let dd = cone(&mut b, x, 9);
    let q = b.register(en, dd);
    b.output("q", q);
    b.output("d", dd);
    let p = compile(&d, &Options::default()).unwrap();
    let e = phases::exposed(&p);
    assert!(guard(&p, &own(&p), Some(&e)).is_none());
    assert!(fused(&p, MIN_REGION).is_some());
    on_off("observed cone", &d, 300);
}

/// Near miss: an enable computed late (deeper than the whole data cone)
/// places the region after it, not at the cone's own ranks.
#[test]
fn a_late_enable_is_computed_before_its_region() {
    let mut d = Design::new("top");
    let r = d.root;
    let mut b = d.builder(r);
    let x = b.input("x", 8);
    let y = b.input("y", 8);
    let dd = cone(&mut b, x, 7);
    let mut e = y;
    for k in 0..12 {
        let c = b.konst(8, k * 37 + 1);
        let (s, _) = b.add(e, c, None);
        e = b.xor(s, x);
    }
    let en = b.bit(e, 3);
    let q = b.register(en, dd);
    b.output("q", q);
    let p = compile(&d, &Options::default()).unwrap();
    let pr = phases::prune(&p, &own(&p)).unwrap_or_else(|| own(&p));
    let g = guard(&p, &pr, None).expect("guarded");
    check_layout(&p, &pr, &g, None);
    on_off("late enable", &d, 300);
}

/// Nested regions: a reset mux around an enable mux; the inner region's
/// conditions are the outer one's then its own, and registers sharing an
/// enable share one region.
#[test]
fn nested_and_shared_enables() {
    let mut d = Design::new("top");
    let r = d.root;
    let mut b = d.builder(r);
    let x = b.input("x", 8);
    let en = b.input("en", 1);
    let rst = b.input("rst", 1);
    let one = b.one();
    let z = b.zero(8);
    let mut outs = Vec::new();
    for k in 0..3 {
        let (node, q) = b.state_placeholder(crate::netlist::NodeKind::Register { w: 8 });
        let dd = cone(&mut b, x, 11 + k);
        let dd = b.xor(dd, q);
        let held = b.mux(en, q, dd);
        let next = b.mux(rst, held, z);
        b.connect(one, (node, 0));
        b.connect(next, (node, 1));
        outs.push(q);
    }
    for (k, q) in outs.into_iter().enumerate() {
        b.output(format!("q{k}"), q);
    }
    let p = compile(&d, &Options::default()).unwrap();
    let g = fused(&p, MIN_REGION).expect("guarded");
    let rst_slot = p.ports.inputs[2].slot;
    let en_slot = p.ports.inputs[1].slot;
    let inner: Vec<&Guard> = g.guards.iter().filter(|x| x.conds.len() == 2).collect();
    assert_eq!(inner.len(), 1, "one merged enable region: {:?}", g.guards);
    assert_eq!(
        inner[0].conds,
        vec![
            Cond {
                slot: rst_slot,
                when: false
            },
            Cond {
                slot: en_slot,
                when: true
            }
        ]
    );
    on_off("nested", &d, 400);
}

/// A net inside a guarded cone: under `Options::debug` full settles guard
/// nothing, and a probe keeps its cone exposed under the defaults; either
/// way `read_net` is exact every tick, single ticks and batches mixed.
#[test]
fn a_probed_net_inside_a_cone_reads_exactly() {
    let mut d = Design::new("top");
    let r = d.root;
    let mut b = d.builder(r);
    let x = b.input("x", 8);
    let en = b.input("en", 1);
    let c = b.konst(8, 13);
    let (t1, _) = b.add(x, c, None);
    let t2 = b.xor(t1, x);
    let t3 = b.and(t2, c);
    let t4 = b.or(t3, x);
    let t5 = b.not(t4);
    let t6 = cone(&mut b, t5, 3);
    let q = b.register(en, t6);
    b.output("q", q);
    for o in [
        Options::debug(),
        Options {
            probes: vec![(Vec::new(), t3)],
            ..Options::default()
        },
    ] {
        let p = Arc::new(compile(&d, &o).unwrap());
        let mut on = Sim::with_options(Arc::clone(&p), guarded_options());
        assert!(on.guarded().0 > 0, "the fused bodies are guarded");
        let mut off = Sim::with_options(
            Arc::clone(&p),
            SimOptions {
                guard: false,
                ..guarded_options()
            },
        );
        assert!(on.read_net(&[], t3).is_some());
        for t in 0..200u64 {
            for s in [&mut on, &mut off] {
                s.set("x", t * 7 % 256);
                s.set("en", u64::from(t % 9 == 0));
                if t % 3 == 0 {
                    s.run(4);
                } else {
                    s.tick();
                }
            }
            assert_eq!(on.read_net(&[], t3), off.read_net(&[], t3), "tick {t}");
            assert_eq!(on.get("q"), off.get("q"));
        }
    }
}

/// Gate-level flip-flops with an enable, from function matching and
/// lifting: word registers whose next value is a mux.
#[test]
fn lifted_registers_with_enables_are_guarded_exactly() {
    fn dffe(d: &mut Design) -> DefId {
        let def = d.add_definition("dffe");
        let mut b = Builder::new(d, def);
        let din = b.input("D", 1);
        let en = b.input("E", 1);
        let (node, q) = b.state_placeholder(crate::netlist::NodeKind::Register { w: 1 });
        let one = b.one();
        let next = b.mux(en, q, din);
        b.connect(one, (node, 0));
        b.connect(next, (node, 1));
        b.output("Q", q);
        def
    }
    let mut d = Design::new("top");
    let ff = dffe(&mut d);
    let r = d.root;
    let mut b = d.builder(r);
    let x = b.input("x", 8);
    let en = b.input("en", 1);
    let dd = cone(&mut b, x, 21);
    let bits: Vec<NetId> = (0..8).map(|k| b.bit(dd, k)).collect();
    let mut qs = Vec::new();
    for bit in bits {
        qs.push(b.instance(ff, &[Some(bit), Some(en)])[0]);
    }
    let q = b.merge(&qs);
    b.output("q", q);
    on_off("lifted dffe", &d, 300);
}

/// `t = (x + k) ^ y; !t & z`: four word ops, a cone too small to guard
/// on its own.
fn small(b: &mut Builder<'_>, x: NetId, y: NetId, z: NetId, k: u64) -> NetId {
    let c = b.konst(8, k);
    let (t, _) = b.add(x, c, None);
    let t = b.xor(t, y);
    let t = b.not(t);
    b.and(t, z)
}

/// Registers sharing an enable share the values of their data cones:
/// those values are guarded with them (one region for the enable), not
/// left to run every tick.
#[test]
fn values_shared_under_one_enable_are_guarded() {
    let mut d = Design::new("top");
    let r = d.root;
    let mut b = d.builder(r);
    let x = b.input("x", 8);
    let y = b.input("y", 8);
    let en = b.input("en", 1);
    let shared = cone(&mut b, x, 3);
    let d1 = b.xor(shared, y);
    let (d2, _) = b.add(shared, y, None);
    let q1 = b.register(en, d1);
    let q2 = b.register(en, d2);
    b.output("q1", q1);
    b.output("q2", q2);
    let p = compile(&d, &Options::default()).unwrap();
    let g = fused(&p, MIN_REGION).expect("guarded");
    let en_slot = p.ports.inputs[2].slot;
    assert_eq!(g.guards.len(), 1, "{:?}", g.guards);
    assert_eq!(
        g.guards[0].conds,
        vec![Cond {
            slot: en_slot,
            when: true
        }]
    );
    // The shared cone and both registers' own parts.
    let n = guarded_instrs(&g);
    assert!(n >= MIN_REGION as u32, "{n} guarded instructions");
    on_off("shared enable", &d, 300);
}

/// Near miss: arms of one enable where one arm reads the other register's
/// next value (a chain). One region cannot be ready before the first
/// mux, so the arms are split into a region each, both still guarded.
#[test]
fn a_chain_under_one_enable_is_split() {
    let mut d = Design::new("top");
    let r = d.root;
    let mut b = d.builder(r);
    let x = b.input("x", 8);
    let en = b.input("en", 1);
    let one = b.one();
    let (n1, q1) = b.state_placeholder(crate::netlist::NodeKind::Register { w: 8 });
    let (n2, q2) = b.state_placeholder(crate::netlist::NodeKind::Register { w: 8 });
    let f = cone(&mut b, x, 5);
    let f = b.xor(f, q1);
    let next1 = b.mux(en, q1, f);
    let g = cone(&mut b, next1, 9);
    let g = b.xor(g, q2);
    let next2 = b.mux(en, q2, g);
    b.connect(one, (n1, 0));
    b.connect(next1, (n1, 1));
    b.connect(one, (n2, 0));
    b.connect(next2, (n2, 1));
    b.output("q1", q1);
    b.output("q2", q2);
    let p = compile(&d, &Options::default()).unwrap();
    let pr = phases::prune(&p, &own(&p)).unwrap_or_else(|| own(&p));
    UNDONE.with(|u| u.set(0));
    let g = guard(&p, &pr, None).expect("guarded");
    assert_eq!(UNDONE.with(|u| u.get()), 1, "one merged region split");
    check_layout(&p, &pr, &g, None);
    assert_eq!(g.guards.len(), 2, "{:?}", g.guards);
    on_off("chained enable", &d, 300);
}

/// Registers with enables of their own and small data cones: grouped,
/// at most [`MAX_ANY`] enables to a group, each group run when any of
/// its enables is 1.
#[test]
fn small_cones_of_different_enables_are_grouped() {
    let mut d = Design::new("top");
    let r = d.root;
    let mut b = d.builder(r);
    let x = b.input("x", 8);
    let y = b.input("y", 8);
    let z = b.input("z", 8);
    let mut ens = Vec::new();
    for k in 0..8 {
        let en = b.input(format!("en{k}"), 1);
        let dd = small(&mut b, x, y, z, 3 + 7 * k);
        let q = b.register(en, dd);
        b.output(format!("q{k}"), q);
        ens.push(en);
    }
    let p = compile(&d, &Options::default()).unwrap();
    let en_slots: Vec<u32> = p.ports.inputs[3..].iter().map(|i| i.slot).collect();
    let g = fused(&p, MIN_REGION).expect("grouped");
    let groups: Vec<&Guard> = g.guards.iter().filter(|x| !x.any.is_empty()).collect();
    assert!(!groups.is_empty(), "{:?}", g.guards);
    for gd in &groups {
        assert!(gd.conds.is_empty());
        assert!((2..=MAX_ANY).contains(&gd.any.len()), "{gd:?}");
        for c in &gd.any {
            assert!(c.when && en_slots.contains(&c.slot), "{gd:?}");
        }
    }
    assert!(guarded_instrs(&g) >= MIN_REGION as u32);
    // Without groups, nothing is big enough.
    assert!(g.guards.iter().all(|x| !x.any.is_empty()));
    on_off("grouped enables", &d, 400);
    // Any one enable at a time, and none, against the unguarded body.
    let p = Arc::new(p);
    let off = SimOptions {
        guard: false,
        ..guarded_options()
    };
    for (o, jit) in [(guarded_options(), false), (SimOptions::jit(), true)] {
        let mut on = Sim::with_options(Arc::clone(&p), o);
        if jit && !on.jit_active() {
            continue;
        }
        let mut reference = Sim::with_options(Arc::clone(&p), off.clone());
        for t in 0..200u64 {
            for s in [&mut on, &mut reference] {
                s.set("x", t * 13 % 256);
                s.set("y", t * 5 % 256);
                s.set("z", 0xF0 ^ (t % 256));
                for k in 0..8u64 {
                    s.set(&format!("en{k}"), u64::from(t % 9 == k));
                }
                s.run(1 + t % 5);
            }
            for k in 0..8 {
                assert_eq!(
                    on.get(&format!("q{k}")),
                    reference.get(&format!("q{k}")),
                    "tick {t}"
                );
            }
        }
    }
}

/// Random registers with enables (inputs, decoded state, rarely 1) and
/// data of every size, reading inputs, other registers and other
/// registers' next values (chains through which groups and merged
/// regions can close a loop): guards on against off, and the layout
/// invariants, with groups and merged regions in some of them.
#[test]
fn random_enabled_registers_are_exact() {
    let mut grouped = 0;
    for seed in 0..40u64 {
        let mut rng = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
        let mut next = |n: u64| {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            rng % n
        };
        let mut d = Design::new("top");
        let r = d.root;
        let mut b = d.builder(r);
        let ins: Vec<NetId> = (0..3).map(|k| b.input(format!("x{k}"), 8)).collect();
        let ens: Vec<NetId> = (0..4).map(|k| b.input(format!("en{k}"), 1)).collect();
        let one = b.one();
        let n = 6 + next(10) as usize;
        let regs: Vec<(crate::netlist::NodeId, NetId)> = (0..n)
            .map(|_| b.state_placeholder(crate::netlist::NodeKind::Register { w: 8 }))
            .collect();
        let mut values: Vec<NetId> = ins.clone();
        values.extend(regs.iter().map(|x| x.1));
        let mut nexts = Vec::new();
        for (k, &(node, q)) in regs.iter().enumerate() {
            let en = match next(3) {
                0 => ens[next(4) as usize],
                1 => {
                    let s = values[next(values.len() as u64) as usize];
                    b.eq_const(s, next(256))
                }
                _ => {
                    let e = ens[next(4) as usize];
                    let s = values[next(values.len() as u64) as usize];
                    let c = b.eq_const(s, next(4));
                    b.and(e, c)
                }
            };
            let mut v = values[next(values.len() as u64) as usize];
            for _ in 0..next(14) {
                let o = values[next(values.len() as u64) as usize];
                v = match next(5) {
                    0 => b.add(v, o, None).0,
                    1 => b.xor(v, o),
                    2 => b.and(v, o),
                    3 => b.not(v),
                    _ => b.sub(v, o),
                };
            }
            let nx = if next(4) == 0 {
                let rst = ens[next(4) as usize];
                let held = b.mux(en, q, v);
                let z = b.zero(8);
                b.mux(rst, held, z)
            } else {
                b.mux(en, q, v)
            };
            b.connect(one, (node, 0));
            b.connect(nx, (node, 1));
            b.output(format!("q{k}"), q);
            nexts.push(nx);
            // Later registers may read this one's next value.
            values.push(nx);
        }
        let p = compile(&d, &Options::default()).unwrap();
        for min in [MIN_REGION, 4] {
            if let Some(g) = fused(&p, min) {
                grouped += u32::from(g.guards.iter().any(|x| !x.any.is_empty()));
            }
        }
        on_off(&format!("enabled registers {seed}"), &d, 60);
    }
    let undone = UNDONE.with(|u| u.get());
    eprintln!(
        "{grouped} bodies with groups; layouts undone: {} splits, {} ungroups",
        undone & 0xFFFF,
        undone >> 16
    );
    assert!(grouped >= 5, "only {grouped} bodies with groups");
    assert!(undone & 0xFFFF > 0 && undone >> 16 > 0, "both undo paths");
}

/// Literal operands (shift amounts, field offsets) larger than the
/// design's slot count are not slots: a small design with wide fields.
#[test]
fn literal_operands_are_not_slots() {
    let mut d = Design::new("top");
    let r = d.root;
    let mut b = d.builder(r);
    let x = b.input("x", 64);
    let en = b.input("en", 1);
    let hi = b.split(x, 60, 4);
    let lo = b.split(x, 40, 4);
    let t = b.xor(hi, lo);
    let (t, _) = b.add(t, lo, None);
    let q = b.register(en, t);
    b.output("q", q);
    let p = compile(&d, &Options::default()).unwrap();
    assert!(p.slot_count < 40, "{} slots", p.slot_count);
    for min in [1, MIN_REGION] {
        let _ = fused(&p, min);
    }
    on_off("wide fields", &d, 100);
}

#[test]
fn fixtures_are_exact_with_guards() {
    let cpu = fixtures::cpu(&fixtures::LOOP_PROGRAM);
    let blasted = crate::testkit::bitblast::bitblast(&cpu);
    for (name, d) in [
        ("cpu", cpu),
        ("counter", fixtures::counter(8)),
        ("cpu bit-blasted", blasted),
    ] {
        on_off(name, &d, 400);
    }
}

/// Random designs: guards on against off, and against the reference.
#[test]
fn random_designs_are_exact_with_guards() {
    let mut guarded = 0;
    for seed in 0..60 {
        let g = generate(&GenConfig {
            seed: seed * 11 + 5,
            state_percent: 30,
            ..GenConfig::default()
        });
        on_off(&format!("random {seed}"), &g.design, 40);
        let p = Arc::new(compile(&g.design, &Options::default()).unwrap());
        let s = Sim::with_options(
            Arc::clone(&p),
            SimOptions {
                guard_min: 1,
                ..guarded_options()
            },
        );
        guarded += (s.guarded().0 > 0) as u32;
    }
    assert!(guarded >= 10, "only {guarded} random designs had guards");
}

/// A body that is already guarded is left alone.
#[test]
fn guarded_bodies_are_not_guarded_again() {
    let d = fixtures::cpu(&fixtures::LOOP_PROGRAM);
    let p = compile(&d, &Options::default()).unwrap();
    let g = fused(&p, 1).expect("the CPU has enabled registers");
    assert!(guard_with(p.slot_count as usize, &g, None, 1).is_none());
}

/// The defaults with guards on in the interpreter as well as the JIT
/// (`SimOptions::guard_interpreter`), so both runtimes are tested guarded.
fn guarded_options() -> SimOptions {
    SimOptions {
        guard_interpreter: true,
        ..SimOptions::default()
    }
}
