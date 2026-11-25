//! `choose_plan`: determinism, caps, the obvious key on designs where it
//! is known by construction, exactness of the plan's bodies on every key
//! of the samples, prefix cost, and mutations the checks must catch.

use std::sync::Arc;

use super::tests::{State, controlled, force, fused_bodies, observed, random_state, step};
use super::*;
use crate::compile::{Options, Program, compile};
use crate::netlist::Design;
use crate::sim::Sim;
use crate::testkit::{GenConfig, Rng, generate};

/// Sampled rows per phase, and the memories at each.
type Rows = Vec<Vec<Vec<u64>>>;
type Mems = Vec<Vec<u64>>;
/// Sets a sim's inputs before a tick.
type Drive = Box<dyn FnMut(&mut Sim, u64)>;

/// One context per phase of `p`.
fn contexts(p: &Program) -> Vec<Option<SpecContext>> {
    let possible = possible_bits(p);
    fused_bodies(p)
        .into_iter()
        .map(|(b, ph)| SpecContext::new(p, &b, &possible, ph))
        .collect()
}

/// Rows of settled slots per phase: `ticks` ticks after `warm`, `drive`
/// setting the inputs before each (given the tick number).
fn sample(
    p: &Arc<Program>,
    warm: u64,
    ticks: u64,
    drive: &mut dyn FnMut(&mut Sim, u64),
) -> (Rows, Vec<Vec<Mems>>) {
    let period = p.phases.as_ref().map_or(1, |x| u64::from(x.period));
    let mut sim = Sim::new(Arc::clone(p));
    let mut rows = vec![Vec::new(); period as usize];
    let mut mems = vec![Vec::new(); period as usize];
    for t in 0..warm + ticks {
        drive(&mut sim, t);
        sim.tick();
        if t >= warm {
            let k = (sim.tick_count() % period) as usize;
            rows[k].push((0..p.slot_count).map(|s| sim.read(s)).collect());
            mems[k].push(
                (0..p.mems.len() as u32)
                    .map(|m| sim.mem(m).to_vec())
                    .collect(),
            );
        }
    }
    (rows, mems)
}

fn port(p: &Program, name: &str) -> usize {
    p.ports.input(name).unwrap()
}

/// A register `sel` masks a large cone of `x` and `y` (all of it, or
/// nothing); a register `h`, loaded with a fresh random byte on half
/// the ticks, selects (by `h == 77`) a second, smaller cone. The best key is
/// `sel`; `h` (eight bits) takes a new value nearly every sample.
fn big_or_small() -> Design {
    let mut d = Design::new("top");
    let r = d.root;
    let mut b = d.builder(r);
    let we = b.input("we", 1);
    let din = b.input("din", 1);
    let x = b.input("x", 8);
    let y = b.input("y", 8);
    let hin = b.input("hin", 8);
    let hen = b.input("hen", 1);
    let one = b.one();
    let sel = b.register(we, din);
    let mut acc = x;
    for i in 0..12 {
        let q = if i % 2 == 0 { y } else { x };
        acc = b.add(acc, q, None).0;
        acc = b.xor(acc, y);
    }
    // `sel` masks the cone: no mux arm to guard, but a known bit kills it.
    let m1 = b.sext(sel, 8);
    let pick = b.and(acc, m1);
    let out = b.register(one, pick);
    let h = b.register(hen, hin);
    let hit = b.eq_const(h, 77);
    let mut acc2 = y;
    for _ in 0..3 {
        acc2 = b.add(acc2, x, None).0;
        acc2 = b.xor(acc2, x);
    }
    let m2 = b.sext(hit, 8);
    let pick2 = b.and(acc2, m2);
    let out2 = b.register(one, pick2);
    b.output("out", out);
    b.output("out2", out2);
    d
}

fn drive_big(p: &Program, rng: &mut Rng) -> Drive {
    let (we, din, x, y, hin, hen) = (
        port(p, "we"),
        port(p, "din"),
        port(p, "x"),
        port(p, "y"),
        port(p, "hin"),
        port(p, "hen"),
    );
    let mut rng = Rng::new(rng.next_u64());
    Box::new(move |sim: &mut Sim, t: u64| {
        sim.set_input(we, u64::from(t % 50 == 0));
        sim.set_input(din, (t / 50) % 2);
        sim.set_input(x, rng.next_u64() & 0xff);
        sim.set_input(y, rng.next_u64() & 0xff);
        sim.set_input(hin, rng.next_u64() & 0xff);
        sim.set_input(hen, rng.next_u64() & 1);
    })
}

struct Setup {
    p: Arc<Program>,
    cx: Vec<Option<SpecContext>>,
    rows: Vec<Vec<Vec<u64>>>,
    /// Memories at each sampled row.
    mems: Vec<Vec<Vec<Vec<u64>>>>,
}

fn setup(d: &Design, drive: impl FnOnce(&Program, &mut Rng) -> Drive) -> Setup {
    let p = Arc::new(compile(d, &Options::default()).unwrap());
    let mut rng = Rng::new(7);
    let mut f = drive(&p, &mut rng);
    let (rows, mems) = sample(&p, 20, 600, &mut *f);
    let cx = contexts(&p);
    Setup { p, cx, rows, mems }
}

fn big_setup() -> Setup {
    setup(&big_or_small(), |p, rng| drive_big(p, rng))
}

fn plan_of(s: &Setup, o: &PlanOptions) -> SpecPlan {
    let cx: Vec<Option<&SpecContext>> = s.cx.iter().map(Option::as_ref).collect();
    let rows: Vec<&[Vec<u64>]> = s.rows.iter().map(Vec::as_slice).collect();
    choose_plan(&s.p, &cx, &rows, o)
}

/// Whether the plan's bodies are exact on every key value of the samples:
/// the prefix and the specialized body commit what the general body does,
/// on the sampled states themselves.
fn exact_on_samples(s: &Setup, plan: &SpecPlan) -> usize {
    let mut checked = 0;
    for (k, pp) in plan.phases.iter().enumerate() {
        let (Some(pp), Some(cx)) = (pp, s.cx[k].as_ref()) else {
            continue;
        };
        let slots: Vec<u32> = pp.layout.fields.iter().map(|f| f.slot).collect();
        let (prefix, pre) = cx.prefix(&slots);
        let mut seen: Vec<u64> = Vec::new();
        for (row, mems) in s.rows[k].iter().zip(&s.mems[k]) {
            let key = pp.layout.pack(row);
            let st = State {
                slots: row.clone(),
                mems: mems.clone(),
            };
            let spec = cx.specialize(&s.p, &pp.layout.assume(key), &pre);
            let fb = cx.fallback(&s.p, &pre);
            let want = observed(&s.p, &step(&s.p, &[cx.body()], &st, k as u64));
            for (what, body) in [("body", &spec), ("fallback", &fb)] {
                let got = observed(&s.p, &step(&s.p, &[&prefix, body], &st, k as u64));
                assert_eq!(got, want, "phase {k}, key {key:#x}: the {what} differs");
            }
            if !seen.contains(&key) {
                seen.push(key);
            }
            checked += 1;
        }
        assert!(!seen.is_empty());
    }
    checked
}

fn width(plan: &SpecPlan) -> u32 {
    plan.phases
        .iter()
        .flatten()
        .map(|p| p.layout.width())
        .max()
        .unwrap_or(0)
}

#[test]
fn the_plan_is_deterministic() {
    let s = big_setup();
    let o = PlanOptions::default();
    let a = plan_of(&s, &o);
    assert!(a.phases.iter().any(Option::is_some));
    assert_eq!(a, plan_of(&s, &o));
    // Rebuilt contexts and candidates give the same plan.
    let again = Setup {
        p: Arc::clone(&s.p),
        cx: contexts(&s.p),
        rows: s.rows.clone(),
        mems: s.mems.clone(),
    };
    assert_eq!(a, plan_of(&again, &o));
    // And for a design with several phases and many candidates.
    let c = setup(&controlled(), |p, rng| {
        let (we, din, x) = (port(p, "we"), port(p, "din"), port(p, "x"));
        let mut rng = Rng::new(rng.next_u64());
        Box::new(move |sim: &mut Sim, t: u64| {
            sim.set_input(we, u64::from(t % 9 == 0));
            sim.set_input(din, rng.next_u64() & 0x3f);
            sim.set_input(x, rng.next_u64() & 0xff);
        })
    });
    let first = plan_of(&c, &o);
    assert_eq!(first, plan_of(&c, &o));
    assert_eq!(first, plan_of(&c, &o));
}

#[test]
fn the_key_that_selects_the_big_cone_is_chosen() {
    let s = big_setup();
    let plan = plan_of(&s, &PlanOptions::default());
    let pp = plan.phases[0].as_ref().expect("a plan");
    let cx = s.cx[0].as_ref().unwrap();
    // `sel`, the one-bit register, is the first field.
    let f = pp.layout.fields[0];
    assert_eq!(f.mask, 1, "{plan:?}");
    let c = cx
        .candidates(&s.p)
        .into_iter()
        .find(|c| c.slot == f.slot)
        .unwrap();
    assert_eq!(c.class, KeyClass::Register);
    assert_eq!(c.prefix_ops, 0);
    // Alone it beats every other candidate alone.
    let alone = |slot, mask| {
        let l = KeyLayout::new(vec![KeyField { slot, mask }]);
        estimate(&s.p, cx, &l, &s.rows[0], 24).saved
    };
    let sel = alone(f.slot, 1);
    for o in cx.candidates(&s.p) {
        if o.slot != f.slot {
            assert!(alone(o.slot, o.support) < sel, "{o:?}");
        }
    }
    // It folds the cone: most of the body.
    let general = executed_ops(&guarded(&s.p, cx.body().clone()), &s.rows[0][0]) as f64;
    assert!(pp.saved > general / 3.0, "{} of {general}", pp.saved);
    assert!(pp.saved >= sel, "{} < {sel}", pp.saved);
    assert!(pp.distinct <= 4, "{}", pp.distinct);
    assert!(pp.hit > 0.99);
    exact_on_samples(&s, &plan);
}

#[test]
fn a_key_that_changes_every_sample_is_not_chosen() {
    let s = big_setup();
    let cx = s.cx[0].as_ref().unwrap();
    // The ROM address register is a candidate whose values never repeat.
    let h = cx
        .candidates(&s.p)
        .into_iter()
        .filter(|c| c.class == KeyClass::Register && c.support == 0xff)
        .map(|c| c.slot)
        .collect::<Vec<_>>();
    assert!(!h.is_empty(), "the address register is a candidate");
    for o in [
        PlanOptions::default(),
        PlanOptions {
            max_distinct: 1 << 20,
            ..PlanOptions::default()
        },
    ] {
        let plan = plan_of(&s, &o);
        for f in plan.phases[0].iter().flat_map(|p| &p.layout.fields) {
            assert!(!h.contains(&f.slot), "{plan:?}");
        }
    }
    // Alone it saves less than the stable key (24 bodies of ~250 values).
    let alone = KeyLayout::new(vec![KeyField {
        slot: h[0],
        mask: 0xff,
    }]);
    let e = estimate(&s.p, cx, &alone, &s.rows[0], 24);
    assert!(e.distinct > 100, "{}", e.distinct);
    let plan = plan_of(&s, &PlanOptions::default());
    assert!(e.saved < plan.phases[0].as_ref().unwrap().saved / 4.0);
    // A distinct-value cap below its cardinality rejects it outright.
    let capped = plan_of(
        &s,
        &PlanOptions {
            max_distinct: e.distinct - 1,
            ..PlanOptions::default()
        },
    );
    for f in capped.phases[0].iter().flat_map(|p| &p.layout.fields) {
        assert!(!h.contains(&f.slot));
    }
}

#[test]
fn caps_are_respected_and_pruned_bits_are_gone() {
    let s = setup(&controlled(), |p, rng| {
        let (we, din, x) = (port(p, "we"), port(p, "din"), port(p, "x"));
        let mut rng = Rng::new(rng.next_u64());
        Box::new(move |sim: &mut Sim, t: u64| {
            sim.set_input(we, u64::from(t % 5 == 0));
            sim.set_input(din, rng.next_u64() & 0x3f);
            sim.set_input(x, rng.next_u64() & 0xff);
        })
    });
    let keep_all = PlanOptions {
        min_loss: -1e9,
        min_gain: 0.0,
        ..PlanOptions::default()
    };
    let unpruned = plan_of(&s, &keep_all);
    let wide = width(&unpruned);
    assert!(wide >= 4, "{unpruned:?}");
    for cap in 0..=wide {
        let plan = plan_of(
            &s,
            &PlanOptions {
                max_key_bits: cap,
                ..keep_all.clone()
            },
        );
        assert!(width(&plan) <= cap, "cap {cap}: {plan:?}");
        exact_on_samples(&s, &plan);
    }
    // A threshold of two ops drops bits, and every bit it kept pays: its
    // loss, over the sampled keys, is at least `min_loss`.
    let o = PlanOptions {
        min_gain: 0.0,
        min_loss: 2.0,
        ..PlanOptions::default()
    };
    let pruned = plan_of(&s, &o);
    assert!(
        width(&pruned) < wide,
        "{} !< {wide}: {pruned:?}",
        width(&pruned)
    );
    let pp = pruned.phases[0].as_ref().expect("a plan");
    let cx = s.cx[0].as_ref().unwrap();
    let slots: Vec<u32> = pp.layout.fields.iter().map(|f| f.slot).collect();
    let pre = cx.prefix(&slots).1;
    let mean = |fields: &[KeyField]| -> f64 {
        let l = KeyLayout::new(fields.to_vec());
        let mut sum = 0.0;
        for row in &s.rows[0] {
            let a: Vec<(u32, Known)> = l.assume(l.pack(row)).into_iter().collect();
            sum += cx.specialize(&s.p, &a, &pre).ops() as f64;
        }
        sum / s.rows[0].len() as f64
    };
    let base = mean(&pp.layout.fields);
    for (i, f) in pp.layout.fields.iter().enumerate() {
        let mut m = f.mask;
        while m != 0 {
            let bit = m.isolate_lowest_one();
            m &= m - 1;
            let mut t = pp.layout.fields.clone();
            t[i].mask &= !bit;
            assert!(
                mean(&t) - base >= o.min_loss - 1e-9,
                "bit {bit:#x} of slot {} is not worth {} ops",
                f.slot,
                o.min_loss
            );
        }
    }
    exact_on_samples(&s, &pruned);
}

/// A flag whose prefix (nine adds and a compare of three registers)
/// feeds only the data of a register that is enabled one tick in sixteen:
/// the guarded general body skips the whole cone on the other fifteen,
/// but a key's prefix is unguarded and runs it every tick, to fold a mux
/// and a not.
fn costly_flag() -> Design {
    let mut d = Design::new("top");
    let r = d.root;
    let mut b = d.builder(r);
    let we = b.input("we", 1);
    let en = b.input("en", 1);
    let a = b.input("a", 8);
    let c = b.input("c", 8);
    let e = b.input("e", 8);
    let x = b.input("x", 8);
    let ra = b.register(we, a);
    let rc = b.register(we, c);
    let re = b.register(we, e);
    let mut acc = ra;
    for q in [rc, re, ra, rc, re, ra, rc, re, ra] {
        acc = b.add(acc, q, None).0;
    }
    let flag = b.eq_const(acc, 13);
    let nx = b.not(x);
    let pick = b.mux(flag, x, nx);
    let out = b.register(en, pick);
    b.output("out", out);
    d
}

fn costly_setup() -> Setup {
    setup(&costly_flag(), |p, rng| {
        let (we, en, a, c, e, x) = (
            port(p, "we"),
            port(p, "en"),
            port(p, "a"),
            port(p, "c"),
            port(p, "e"),
            port(p, "x"),
        );
        let mut rng = Rng::new(rng.next_u64());
        Box::new(move |sim: &mut Sim, t: u64| {
            sim.set_input(we, u64::from(t % 40 == 0));
            sim.set_input(en, u64::from(t % 16 == 0));
            // The flag alternates between loads.
            sim.set_input(a, if (t / 40) % 2 == 0 { 1 } else { 2 });
            sim.set_input(c, 2);
            sim.set_input(e, 1);
            sim.set_input(x, rng.next_u64() & 0xff);
        })
    })
}

/// The estimate by hand: per sampled state, the guarded general body's
/// executed ops, minus the prefix's, minus the specialized body's (the
/// fallback where `bodies` has no key); with or without the prefix.
fn by_hand(s: &Setup, layout: &KeyLayout, bodies: &[u64], prefix_cost: bool) -> f64 {
    let cx = s.cx[0].as_ref().unwrap();
    let slots: Vec<u32> = layout.fields.iter().map(|f| f.slot).collect();
    let (prefix, pre) = cx.prefix(&slots);
    let general = guarded(&s.p, cx.body().clone());
    let fallback = guarded(&s.p, cx.fallback(&s.p, &pre));
    let mut sum = 0.0;
    for row in &s.rows[0] {
        let key = layout.pack(row);
        let body = if bodies.contains(&key) {
            guarded(&s.p, cx.specialize(&s.p, &layout.assume(key), &pre))
        } else {
            fallback.clone()
        };
        sum += executed_ops(&general, row) as f64 - executed_ops(&body, row) as f64;
        if prefix_cost {
            sum -= prefix.ops() as f64;
        }
    }
    sum / s.rows[0].len() as f64
}

fn flag_layout(s: &Setup) -> (Candidate, KeyLayout) {
    let cx = s.cx[0].as_ref().unwrap();
    let flag = cx
        .candidates(&s.p)
        .into_iter()
        .find(|c| c.class == KeyClass::Flag)
        .expect("the flag is a candidate");
    let layout = KeyLayout::new(vec![KeyField {
        slot: flag.slot,
        mask: flag.support,
    }]);
    (flag, layout)
}

#[test]
fn a_candidate_whose_prefix_costs_more_than_it_folds_is_rejected() {
    let s = costly_setup();
    let cx = s.cx[0].as_ref().unwrap();
    let (flag, layout) = flag_layout(&s);
    assert!(flag.prefix_ops >= 9, "{flag:?}");
    let e = estimate(&s.p, cx, &layout, &s.rows[0], 24);
    assert_eq!(e.distinct, 2);
    // Without the prefix charge the key looks good; with it, it loses.
    let free = by_hand(&s, &layout, &e.keys, false);
    let paid = by_hand(&s, &layout, &e.keys, true);
    assert!(free > 0.0, "{free}");
    assert!(paid < free - 5.0 && paid < 0.0, "{paid} against {free}");
    assert!((e.saved - paid).abs() < 1e-9, "{} != {paid}", e.saved);
    let plan = plan_of(&s, &PlanOptions::default());
    assert_eq!(plan.phases, vec![None], "{plan:?}");
    // Nor does it get through with every threshold off.
    let eager = plan_of(
        &s,
        &PlanOptions {
            min_gain: 0.0,
            min_loss: 0.0,
            ..PlanOptions::default()
        },
    );
    assert!(
        eager.phases[0]
            .iter()
            .flat_map(|p| &p.layout.fields)
            .all(|f| f.slot != flag.slot),
        "{eager:?}"
    );
}

/// The estimate equals the hand-computed sum over the samples (prefix
/// charged on every tick, hits and misses alike) for several body counts
/// and layouts. A planner that ignored the prefix, or that costed a miss
/// as free, would differ.
#[test]
fn the_estimate_charges_the_prefix() {
    for s in [costly_setup(), big_setup()] {
        let cx = s.cx[0].as_ref().unwrap();
        let mut layouts: Vec<KeyLayout> = cx
            .candidates(&s.p)
            .iter()
            .map(|c| {
                KeyLayout::new(vec![KeyField {
                    slot: c.slot,
                    mask: c.support,
                }])
            })
            .collect();
        layouts.truncate(4);
        assert!(!layouts.is_empty());
        for layout in &layouts {
            for bodies in [0, 1, 24] {
                let e = estimate(&s.p, cx, layout, &s.rows[0], bodies);
                let want = by_hand(&s, layout, &e.keys, true);
                assert!(
                    (e.saved - want).abs() < 1e-9,
                    "{bodies} bodies: {} != {want}",
                    e.saved
                );
            }
        }
    }
    // No bodies at all: every tick runs the fallback after the prefix.
    let s = costly_setup();
    let (_, layout) = flag_layout(&s);
    let none = estimate(&s.p, s.cx[0].as_ref().unwrap(), &layout, &s.rows[0], 0);
    assert!(none.saved < 0.0 && none.hit == 0.0, "{none:?}");
}

#[test]
fn plans_are_exact_on_random_designs() {
    let mut planned = 0;
    for seed in 0..40 {
        let g = generate(&GenConfig {
            seed: seed * 11 + 5,
            max_width: if seed % 2 == 0 { 16 } else { 64 },
            state_percent: 30,
            ..GenConfig::default()
        });
        let p = Arc::new(compile(&g.design, &Options::default()).unwrap());
        let mut rng = Rng::new(seed);
        let ports = p.ports.inputs.len();
        let mut drive = move |sim: &mut Sim, t: u64| {
            for i in 0..ports {
                // Inputs hold for a few ticks so keys repeat.
                if t % 8 == i as u64 % 8 {
                    sim.set_input(i, rng.next_u64());
                }
            }
        };
        let (rows, mems) = sample(&p, 10, 300, &mut drive);
        let s = Setup {
            cx: contexts(&p),
            p,
            rows,
            mems,
        };
        let o = PlanOptions {
            min_gain: 0.5,
            ..PlanOptions::default()
        };
        let plan = plan_of(&s, &o);
        assert_eq!(plan, plan_of(&s, &o));
        planned += plan.phases.iter().flatten().count();
        exact_on_samples(&s, &plan);
        // Also on random states that satisfy a plan's key.
        let possible = possible_bits(&s.p);
        let mut sim = Sim::new(Arc::clone(&s.p));
        let mut rng = Rng::new(99 + seed);
        for (k, pp) in plan.phases.iter().enumerate() {
            let (Some(pp), Some(cx)) = (pp, s.cx[k].as_ref()) else {
                continue;
            };
            let slots: Vec<u32> = pp.layout.fields.iter().map(|f| f.slot).collect();
            let (prefix, pre) = cx.prefix(&slots);
            if slots.iter().any(|&x| cx.computes(x)) {
                continue;
            }
            for key in [0, 1, u64::MAX, rng.next_u64()] {
                let key = key & mask(pp.layout.width() as u8);
                let spec = cx.specialize(&s.p, &pp.layout.assume(key), &pre);
                for _ in 0..4 {
                    let st = random_state(&s.p, &mut sim, &possible, &mut rng);
                    let st = force(&pp.layout, key, &st);
                    let want = observed(&s.p, &step(&s.p, &[cx.body()], &st, k as u64));
                    let got = observed(&s.p, &step(&s.p, &[&prefix, &spec], &st, k as u64));
                    assert_eq!(got, want, "seed {seed} phase {k} key {key:#x}");
                }
            }
        }
    }
    assert!(planned > 0);
}

#[test]
fn every_phase_has_its_own_plan_or_none() {
    // Two clocks of periods 2 and 4 and a mode register: phases differ,
    // and some phase may have no key. Exact either way.
    let mut d = Design::new("top");
    let r = d.root;
    let mut b = d.builder(r);
    let x = b.input("x", 8);
    let y = b.input("y", 8);
    let we = b.input("we", 1);
    let m = b.input("m", 1);
    let mode = b.register(we, m);
    let c2 = b.clock(2, 1, 0);
    let c4 = b.clock(4, 1, 1);
    let e0 = b.and(c2, mode);
    let (s1, _) = b.add(x, y, None);
    let (s2, _) = b.add(s1, x, None);
    let t = b.xor(s2, y);
    let held = b.register(e0, t);
    let e1 = b.or(c4, mode);
    let h2 = b.register(e1, x);
    let z = b.xor(held, h2);
    b.output("z", z);
    let s = setup(&d, |p, rng| {
        let (x, y, we, m) = (port(p, "x"), port(p, "y"), port(p, "we"), port(p, "m"));
        let mut rng = Rng::new(rng.next_u64());
        Box::new(move |sim: &mut Sim, t: u64| {
            sim.set_input(we, u64::from(t % 64 == 0));
            sim.set_input(m, (t / 64) % 2);
            sim.set_input(x, rng.next_u64() & 0xff);
            sim.set_input(y, rng.next_u64() & 0xff);
        })
    });
    assert!(s.p.phases.is_some());
    let plan = plan_of(&s, &PlanOptions::default());
    assert_eq!(plan.phases.len(), s.cx.len());
    assert!(plan.phases.iter().any(Option::is_some), "{plan:?}");
    exact_on_samples(&s, &plan);
    // No samples, no plan.
    let cx: Vec<Option<&SpecContext>> = s.cx.iter().map(Option::as_ref).collect();
    let none = choose_plan(&s.p, &cx, &[], &PlanOptions::default());
    assert!(none.phases.iter().all(Option::is_none));
}

/// A plan with a wrong layout (a bit dropped that the body reads, then
/// specialized on a value the sample does not have) is caught by the
/// exactness check: the harness is not vacuous.
#[test]
fn a_wrong_body_fails_the_exactness_check() {
    let s = big_setup();
    let plan = plan_of(&s, &PlanOptions::default());
    let pp = plan.phases[0].as_ref().unwrap().clone();
    let cx = s.cx[0].as_ref().unwrap();
    let slots: Vec<u32> = pp.layout.fields.iter().map(|f| f.slot).collect();
    let (prefix, pre) = cx.prefix(&slots);
    let mut caught = false;
    for (row, mems) in s.rows[0].iter().zip(&s.mems[0]) {
        let key = pp.layout.pack(row);
        let wrong = cx.specialize(&s.p, &pp.layout.assume(key ^ 1), &pre);
        let st = State {
            slots: row.clone(),
            mems: mems.clone(),
        };
        let want = observed(&s.p, &step(&s.p, &[cx.body()], &st, 0));
        let got = observed(&s.p, &step(&s.p, &[&prefix, &wrong], &st, 0));
        caught |= want != got;
    }
    assert!(caught);
}
