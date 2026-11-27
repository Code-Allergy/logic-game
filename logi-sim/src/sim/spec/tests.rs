//! State-keyed specialization at run time (P4 of `docs/adaptive-spec.md`):
//! batches with a plan equal single ticks of the plain runtime, on random
//! designs and hand-built ones, interpreted and JIT-compiled; `last`,
//! counters, the ROM-poke switch-off and supersteps; mutations caught.

use std::sync::Arc;

use super::{MISS, MUTATION, PhasePlan, PhaseSpec, SpecOptions, SpecPlan, Table};
use crate::compile::spec::{KeyField, KeyLayout};
use crate::compile::{Options, Program, compile};
use crate::netlist::{Design, NodeKind};
use crate::sim::{Sim, SimOptions};
use crate::testkit::diff::{
    self, JIT, check_batches_with, check_pair, observe, spec_options, spec_plan, standard_configs,
};
use crate::testkit::{GenConfig, Rng, generate};

pub(super) fn program(d: &Design, options: &Options) -> Arc<Program> {
    Arc::new(compile(d, options).unwrap_or_else(|e| panic!("{e:?}")))
}

/// Resets [`MUTATION`] when dropped (a failing assertion included).
pub(super) struct NoMutation;
impl Drop for NoMutation {
    fn drop(&mut self) {
        MUTATION.set(0);
    }
}

/// A small microcoded machine: a 4-bit program counter addresses a ROM
/// whose word selects the ALU operation (a ROM-word key, computed: it
/// needs a prefix); a 2-bit mode register loaded from an input picks
/// whether the result is inverted (a register key). With `clocked`, the
/// accumulator only loads on one phase of a 2-tick clock (two clock-phase
/// variants).
pub(super) fn microcoded(clocked: bool) -> Design {
    let mut d = Design::new("top");
    let r = d.root;
    let mut b = d.builder(r);
    let x = b.input("x", 8);
    let ld = b.input("ld", 1);
    let one = b.one();
    let zero = b.zero(1);
    let pc = b.counter(4, one, zero);
    let words: Vec<u64> = (0..16u64).map(|k| (k * 0x5b + 3) & 0xff).collect();
    let uw = b.rom(pc, 8, &words);
    let op = b.split(uw, 0, 2);
    let xm = b.split(x, 0, 2);
    let mode = b.register(ld, xm);
    let (acc_reg, acc) = b.state_placeholder(NodeKind::Register { w: 8 });
    let (sum, _) = b.add(acc, x, None);
    let xo = b.xor(acc, x);
    let an = b.and(acc, x);
    let alu = b.mux_n(op, &[sum, xo, an, acc]);
    let inv = b.eq_const(mode, 3);
    let nalu = b.not(alu);
    let next = b.mux(inv, alu, nalu);
    let en = if clocked { b.clock(2, 1, 0) } else { one };
    b.connect(en, (acc_reg, 0));
    b.connect(next, (acc_reg, 1));
    b.output("acc", acc);
    b.output("mode", mode);
    d
}

/// The slot of the ROM word and of the mode register in a program of
/// [`microcoded`]: the candidates of the first fused body, by class.
pub(super) fn microcoded_keys(p: &Program) -> (u32, u64, u32, u64) {
    use crate::compile::spec::KeyClass;
    let cxs = crate::sim::spec_contexts(p, &SimOptions::default());
    let cx = cxs
        .iter()
        .flatten()
        .find(|cx| {
            cx.candidates(p)
                .iter()
                .any(|c| c.class == KeyClass::RomWord)
        })
        .expect("a phase reads the ROM");
    let cands = cx.candidates(p);
    let rom = cands
        .iter()
        .find(|c| c.class == KeyClass::RomWord)
        .expect("the ROM word is a candidate");
    let reg = cands
        .iter()
        .find(|c| c.class == KeyClass::Register && c.support.count_ones() == 2)
        .expect("the mode register is a candidate");
    assert!(cx.computes(rom.slot) && !cx.computes(reg.slot));
    (rom.slot, rom.support, reg.slot, reg.support)
}

/// The plan keying every phase on the ROM word and the mode register, with
/// bodies for `keys` (all of them when `None`).
pub(super) fn microcoded_plan(p: &Program, keys: Option<Vec<u64>>) -> SpecPlan {
    let (rom, rm, reg, gm) = microcoded_keys(p);
    let layout = KeyLayout::new(vec![
        KeyField {
            slot: rom,
            mask: rm,
        },
        KeyField {
            slot: reg,
            mask: gm,
        },
    ]);
    let w = layout.width();
    let keys = keys.unwrap_or_else(|| (0..1u64 << w).collect());
    let period = p.phases.as_ref().map_or(1, |ph| ph.period as usize);
    SpecPlan {
        phases: vec![Some(PhasePlan { layout, keys }); period],
    }
}

pub(super) fn with_spec(o: SimOptions, plan: &SpecPlan) -> SimOptions {
    SimOptions {
        spec: Some(spec_options(plan.clone())),
        ..o
    }
}

/// Every random design of `standard_configs`, default and debug compile
/// options: with a plan from its candidates (`testkit::diff::spec_plan`),
/// every runtime variant of `check_batches` (interpreter and JIT, guards
/// on and off, phases and fusion off) equals single ticks of the plain
/// runtime, and the JIT equals the interpreter through pokes, `resettle`,
/// `reset` and `import_state`. Some designs are keyed, and keys hit.
#[test]
fn plans_on_random_designs_match_single_ticks() {
    let (mut keyed, mut hits, mut misses, mut n) = (0, 0, 0, 0);
    for (c, cfg) in standard_configs().iter().enumerate() {
        for seed in 0..5u64 {
            let g = generate(&GenConfig {
                seed: seed * 7919 + c as u64 * 31 + 1,
                ..cfg.clone()
            });
            for options in [Options::default(), Options::debug()] {
                let p = program(&g.design, &options);
                let plan = spec_plan(&p, seed, 64);
                let spec = spec_options(plan.clone());
                check_batches_with(&p, seed, Some(&spec))
                    .unwrap_or_else(|e| panic!("config {c} seed {seed}: {e}"));
                let on = with_spec(SimOptions::default(), &plan);
                check_pair(&p, 120, seed, &SimOptions::default(), &on)
                    .unwrap_or_else(|e| panic!("config {c} seed {seed}, spec on/off: {e}"));
                if JIT {
                    diff::check_jit(&p, 120, seed, &on)
                        .unwrap_or_else(|e| panic!("config {c} seed {seed}, JIT: {e}"));
                }
                let mut s = Sim::with_options(Arc::clone(&p), on);
                s.run(200);
                let st = s.spec_stats();
                keyed += st.keyed_phases;
                hits += st.hits;
                misses += st.misses;
                n += 1;
            }
        }
    }
    assert!(n >= 70);
    assert!(keyed >= n / 4, "{keyed} keyed phases in {n} programs");
    assert!(hits > 0 && misses > 0, "{hits} hits, {misses} misses");
}

/// The microcoded machine, with and without clock phases, under default
/// and debug compile options: a plan with every key, with some keys and
/// with no key at all, against single ticks (every `check_batches`
/// variant) and spec off (pokes, resettle, reset, import).
#[test]
fn microcoded_plans_match_single_ticks() {
    for clocked in [false, true] {
        for options in [Options::default(), Options::debug()] {
            let p = program(&microcoded(clocked), &options);
            for keys in [None, Some(vec![0, 5, 17, 33, 1000]), Some(vec![])] {
                let plan = microcoded_plan(&p, keys.clone());
                check_batches_with(&p, 3, Some(&spec_options(plan.clone())))
                    .unwrap_or_else(|e| panic!("clocked {clocked} keys {keys:?}: {e}"));
                for o in jit_options() {
                    check_pair(&p, 300, 4, &o, &with_spec(o.clone(), &plan))
                        .unwrap_or_else(|e| panic!("clocked {clocked} keys {keys:?}: {e}"));
                }
            }
        }
    }
}

pub(super) fn jit_options() -> Vec<SimOptions> {
    let mut v = vec![SimOptions::default()];
    if JIT {
        v.push(SimOptions::jit());
    }
    v
}

/// A 16-bit counter plus an input, a flag of the sum choosing between two
/// results. Returns the program, the counter's slot and the plan keying on
/// a computed candidate (a prefix) and the counter's bits `count_mask`,
/// with bodies for `keys` (the counter's part; the computed part 0).
pub(super) fn counting(count_mask: u64, keys: &[u64]) -> (Arc<Program>, SpecPlan) {
    let mut d = Design::new("top");
    let r = d.root;
    let mut b = d.builder(r);
    let x = b.input("x", 16);
    let one = b.one();
    let zero = b.zero(1);
    let cnt = b.counter(16, one, zero);
    let (sum, _) = b.add(cnt, x, None);
    let low = b.split(cnt, 0, 4);
    let is0 = b.eq_const(low, 0);
    let xo = b.xor(sum, x);
    let pick = b.mux(is0, sum, xo);
    let q = b.register(one, pick);
    b.output("q", q);
    let p = program(&d, &Options::default());
    let counter = p
        .state_keys
        .iter()
        .find_map(|k| match k.loc {
            crate::compile::StateLoc::Slot(s) if k.width == 16 && p.resolve(s).is_some() => {
                Some(p.resolve(s).unwrap().0)
            }
            _ => None,
        })
        .expect("a 16-bit state word");
    let cx = crate::sim::spec_contexts(&p, &SimOptions::default())
        .remove(0)
        .expect("a fused body");
    let cands = cx.candidates(&p);
    let flag = cands
        .iter()
        .find(|c| cx.computes(c.slot))
        .expect("a computed candidate");
    let at = flag.support.count_ones();
    let layout = KeyLayout::new(vec![
        KeyField {
            slot: flag.slot,
            mask: flag.support,
        },
        KeyField {
            slot: counter,
            mask: count_mask,
        },
    ]);
    let keys = keys.iter().map(|&k| k << at).collect();
    let plan = SpecPlan {
        phases: vec![Some(PhasePlan { layout, keys })],
    };
    (p, plan)
}

/// A key that changes every tick and never has a body (bodies only for
/// counter values past the test's ticks): every keyed tick misses, and the
/// runtime behaves exactly as with specialization off.
#[test]
fn a_key_that_always_misses_equals_spec_off() {
    let keys: Vec<u64> = (0..64u64).map(|k| 0x8000 + k).collect();
    let (p, plan) = counting(0xffff, &keys);
    for o in jit_options() {
        let mut off = Sim::with_options(Arc::clone(&p), o.clone());
        let mut on = Sim::with_options(Arc::clone(&p), with_spec(o.clone(), &plan));
        assert!(
            on.spec.phases[0]
                .as_ref()
                .is_some_and(|ps| ps.prefix.is_some())
        );
        for round in 0..40u64 {
            off.set_input(0, round * 977);
            on.set_input(0, round * 977);
            let n = 1 + round % 9;
            off.run(n);
            on.run(n);
            assert!(observe(&off) == observe(&on), "round {round}");
        }
        let st = on.spec_stats();
        assert!(st.bodies == 64 && st.hits == 0 && st.misses > 100, "{st:?}");
    }
    check_batches_with(&p, 2, Some(&spec_options(plan))).unwrap();
}

/// `last` is the body that settled the slots: the specialized body or the
/// fallback after a fused tick, the full body after `tick`, `resettle`,
/// `reset` and `import_state` (and the next commit is that body's).
#[test]
fn last_follows_the_settled_body() {
    let p = program(&microcoded(false), &Options::default());
    for keys in [None, Some(vec![])] {
        let plan = microcoded_plan(&p, keys.clone());
        for o in jit_options() {
            let mut want = Sim::with_options(Arc::clone(&p), SimOptions::baseline());
            let mut got = Sim::with_options(Arc::clone(&p), with_spec(o.clone(), &plan));
            let ps = got.spec.phases[0].clone().expect("phase 0 is keyed");
            let snap = want.export_state();
            let same = |want: &Sim, got: &Sim, at: &str| {
                assert!(observe(want) == observe(got), "{at} (keys {keys:?})");
            };
            for round in 0..6u64 {
                want.set_input(0, round * 37);
                got.set_input(0, round * 37);
                // A fused tick: the key's body (or the fallback) settled.
                got.fused_tick();
                let key = ps.key(&got.slots);
                assert_eq!(got.last, ps.lookup(key));
                assert_eq!(keys.is_none(), got.last != ps.fallback);
                want.tick();
                // A tick commits with it, and settles the full body.
                want.tick();
                got.tick();
                assert_eq!(got.last, got.full_run());
                same(&want, &got, "after a fused tick and a tick");
                got.run(5);
                want.run(5);
                assert_eq!(got.last, got.full_run());
                same(&want, &got, "after run(5)");
                match round % 3 {
                    0 => {
                        got.fused_tick();
                        want.tick();
                        got.set_state(0, round);
                        want.set_state(0, round);
                        got.resettle();
                        want.resettle();
                        assert_eq!(got.last, got.full_run());
                        same(&want, &got, "after resettle");
                    }
                    1 => {
                        got.fused_tick();
                        got.reset();
                        want.reset();
                        assert_eq!(got.last, got.full_run());
                        same(&want, &got, "after reset");
                    }
                    _ => {
                        got.fused_tick();
                        got.import_state(&snap);
                        want.import_state(&snap);
                        assert_eq!(got.last, got.full_run());
                        same(&want, &got, "after import_state");
                    }
                }
            }
        }
    }
}

/// Feature `counters`: on the microcoded machine with guards off, every
/// key hits, and `run(n)` counts what the bodies hold: the full settle's
/// commits, then per fused tick the prefix and the key's body (the key
/// follows the program counter, which is the tick count mod 16) and the
/// body's commits, then the full settle; `n` settles (the prefix is part
/// of one). Fewer ops than with specialization off.
#[cfg(feature = "counters")]
#[test]
fn counters_count_the_prefix_and_the_chosen_body() {
    let p = program(&microcoded(false), &Options::default());
    let plan = microcoded_plan(&p, None);
    let (_, rm, _, _) = microcoded_keys(&p);
    let words: Vec<u64> = (0..16u64).map(|k| (k * 0x5b + 3) & 0xff).collect();
    let rom_mem = (0..p.mems.len()).find(|&m| !p.mems[m].writable).unwrap();
    assert_eq!(&p.mems[rom_mem].init[..16], &words[..]);
    for o in jit_options() {
        let o = SimOptions { guard: false, ..o };
        let mut sim = Sim::with_options(Arc::clone(&p), with_spec(o.clone(), &plan));
        let ps = sim.spec.phases[0].clone().expect("keyed");
        let len = |sim: &Sim, run: u16| -> (u64, u64) {
            match run {
                0 => (p.instrs.len() as u64, p.commits.len() as u64),
                r => {
                    let b = sim.runs[r as usize - 1].body.as_ref().expect("a body");
                    (b.instrs.len() as u64, b.commits.len() as u64)
                }
            }
        };
        let prefix = len(&sim, ps.prefix.expect("the ROM word needs a prefix")).0;
        let full = len(&sim, sim.full[0]);
        for n in [1u64, 2, 7, 40] {
            sim.reset();
            sim.reset_counters();
            sim.run(n);
            let c = *sim.counters();
            let (mut instrs, mut commits) = (full.0, full.1);
            for t in 1..n {
                // The ROM word of tick `t`, the mode register 0.
                let key = crate::compile::spec::pext(words[(t % 16) as usize], rm);
                let (i, k) = len(&sim, ps.lookup(key));
                assert_ne!(ps.lookup(key), ps.fallback, "every key has a body");
                instrs += prefix + i;
                commits += k;
            }
            assert_eq!(c.instrs, instrs, "run({n}), {o:?}");
            assert_eq!(c.commits, commits, "run({n}), {o:?}");
            assert_eq!((c.ticks, c.settles), (n, n));
            if n == 40 {
                let mut off = Sim::with_options(Arc::clone(&p), o.clone());
                off.reset_counters();
                off.run(n);
                assert!(c.ops() < off.counters().ops(), "{c:?}");
            }
        }
    }
}

/// A ROM write turns specialization off with the known-bits folds (the
/// bodies fold ROM contents) and `reset` turns it back on; the batches
/// equal single ticks of a plain runtime that took the same pokes. With
/// `known_bits` off the plan alone makes the bodies folded.
#[test]
fn rom_pokes_switch_specialization_off_until_reset() {
    let p = program(&microcoded(true), &Options::default());
    let plan = microcoded_plan(&p, None);
    let rom = (0..p.mems.len() as u32)
        .find(|&m| !p.mems[m as usize].writable)
        .unwrap();
    for known_bits in [true, false] {
        for o in jit_options() {
            let o = with_spec(SimOptions { known_bits, ..o }, &plan);
            let mut want = Sim::with_options(Arc::clone(&p), SimOptions::baseline());
            let mut got = Sim::with_options(Arc::clone(&p), o.clone());
            assert!(
                got.folded && got.spec_stats().keyed_phases == 2,
                "{:?} {}",
                got.spec_stats(),
                got.folded
            );
            let jit = got.jit_active();
            got.run(30);
            want.run(30);
            let w = got.mem(rom)[3];
            got.set_mem_word(rom, 3, w ^ 0x40);
            want.set_mem_word(rom, 3, w ^ 0x40);
            assert!(
                !got.folded && got.spec.phases.is_empty() && got.spec_stats().keyed_phases == 0
            );
            assert_eq!(got.jit_active(), jit);
            for _ in 0..6 {
                got.run(13);
                for _ in 0..13 {
                    want.tick();
                }
                assert!(observe(&want) == observe(&got), "after a ROM poke");
            }
            got.reset();
            want.reset();
            let st = got.spec_stats();
            assert!(got.folded && st.keyed_phases == 2 && st.hits > 0, "{st:?}");
            assert_eq!(got.jit_active(), jit);
            same_batches(&mut want, &mut got);
        }
    }
}

pub(super) fn same_batches(want: &mut Sim, got: &mut Sim) {
    for n in [1, 2, 3, 7, 16, 5, 40] {
        for _ in 0..n {
            want.tick();
        }
        got.run(n);
        assert!(observe(want) == observe(got), "run({n})");
    }
}

/// With the JIT, a keyed program has no superstep (`run` goes tick by
/// tick through the keys); a plan that keys no phase keeps it, and a ROM
/// poke (specialization off) brings it back.
#[cfg(feature = "jit")]
#[test]
fn supersteps_only_without_keys() {
    let p = program(&microcoded(true), &Options::default());
    let sup = |s: &Sim| s.jit_code().and_then(|j| j.superstep()).is_some();
    let plan = microcoded_plan(&p, None);
    let mut keyed = Sim::with_options(Arc::clone(&p), with_spec(SimOptions::jit(), &plan));
    assert!(keyed.jit_active() && !sup(&keyed));
    let unkeyed = SpecPlan {
        phases: vec![None, None],
    };
    let plain = Sim::with_options(Arc::clone(&p), with_spec(SimOptions::jit(), &unkeyed));
    assert!(plain.spec.phases.is_empty() && sup(&plain));
    let rom = (0..p.mems.len() as u32)
        .find(|&m| !p.mems[m as usize].writable)
        .unwrap();
    let w = keyed.mem(rom)[0];
    keyed.set_mem_word(rom, 0, w ^ 1);
    assert!(sup(&keyed));
    keyed.reset();
    assert!(!sup(&keyed) && !keyed.spec.phases.is_empty());
}

/// Native code compiled for one plan is not installed into a `Sim` with
/// another (the run layouts differ), and is for the same plan.
#[cfg(feature = "jit")]
#[test]
fn native_code_follows_the_plan() {
    let p = program(&microcoded(false), &Options::default());
    let all = microcoded_plan(&p, None);
    let some = microcoded_plan(&p, Some(vec![1, 2, 3]));
    let a = Sim::with_options(Arc::clone(&p), with_spec(SimOptions::jit(), &all));
    let mut b = Sim::with_options(Arc::clone(&p), with_spec(SimOptions::jit(), &some));
    let mut c = Sim::with_options(
        Arc::clone(&p),
        with_spec(SimOptions::jit().tier_layout(), &all),
    );
    let code = Arc::new(a.compile_jit().unwrap());
    assert!(!b.install_jit(Some(Arc::clone(&code))));
    assert!(c.install_jit(Some(code)));
    let mut want = Sim::with_options(Arc::clone(&p), SimOptions::baseline());
    same_batches(&mut want, &mut c);
}

/// Whether every keyed phase's prefix packs its key in native code
/// (`jit::KeyFn`); `None` without native code.
#[cfg(feature = "jit")]
pub(super) fn prefixes_pack_keys(s: &Sim) -> Option<bool> {
    s.jit_active().then(|| {
        s.spec.phases.iter().flatten().all(|ps| {
            ps.prefix.is_none_or(|pre| {
                crate::sim::native(&s.jit, &s.spec, pre).is_some_and(|b| b.key.is_some())
            })
        })
    })
}

/// With the JIT, a plan's prefixes (built with the layout) pack their keys
/// in native code, and keyed batches still equal single ticks.
#[cfg(feature = "jit")]
#[test]
fn native_prefixes_pack_their_keys() {
    for keys in [&[][..], &[0, 1, 2, 3][..]] {
        let (p, plan) = counting(0xf, keys);
        let mut s = Sim::with_options(Arc::clone(&p), with_spec(SimOptions::jit(), &plan));
        assert!(s.spec.phases.iter().flatten().any(|ps| ps.prefix.is_some()));
        assert_eq!(prefixes_pack_keys(&s), Some(crate::sim::jit::KEY_ENTRY));
        let mut want = Sim::with_options(Arc::clone(&p), SimOptions::baseline());
        for x in [0, 3, 0xfff0] {
            want.set_input(0, x);
            s.set_input(0, x);
            same_batches(&mut want, &mut s);
        }
        assert!(s.spec_stats().hits > 0 || keys.is_empty());
        assert!(s.spec_stats().misses > 0);
    }
}

/// More than 255 runs: a body for each of 300 values of a counter key,
/// interpreted and JIT-compiled.
#[test]
fn hundreds_of_bodies() {
    let keys: Vec<u64> = (0..300).collect();
    let (p, plan) = counting(0x1ff, &keys);
    let s = Sim::with_options(Arc::clone(&p), with_spec(SimOptions::default(), &plan));
    assert!(s.runs.len() > 300 && s.spec_stats().bodies == 300);
    check_batches_with(&p, 7, Some(&spec_options(plan))).unwrap();
}

/// Deliberate runtime bugs (`MUTATION`) are caught by `check_batches`:
/// bodies built for the wrong key, a skipped prefix (the key read from
/// stale slots, the body run without the prefix's values), and keyed
/// fused ticks that never apply the inputs (`Sim::keyed_ticks` relies on
/// the tick before it having applied them).
#[test]
fn spec_mutations_are_caught() {
    let _reset = NoMutation;
    let programs: Vec<(Arc<Program>, SpecOptions)> = [false, true]
        .into_iter()
        .map(|clocked| {
            let p = program(&microcoded(clocked), &Options::default());
            let plan = microcoded_plan(&p, None);
            (p, spec_options(plan))
        })
        .collect();
    for (p, spec) in &programs {
        check_batches_with(p, 1, Some(spec)).unwrap();
    }
    for m in [1, 2, 6] {
        MUTATION.set(m);
        let caught = programs
            .iter()
            .any(|(p, spec)| check_batches_with(p, 1, Some(spec)).is_err());
        MUTATION.set(0);
        assert!(caught, "mutation {m} is not caught");
    }
}

/// Native code compiled for one plan is refused by a `Sim` whose plan has
/// the same keys over other bits (another mask): its bodies assume other
/// bits, so the run layouts differ.
#[cfg(feature = "jit")]
#[test]
fn native_code_for_other_key_bits_is_refused() {
    let keys: Vec<u64> = (0..16).collect();
    let (p, low) = counting(0xf, &keys);
    let mut high = low.clone();
    let pp = high.phases[0].as_mut().unwrap();
    let mut fields = pp.layout.fields.clone();
    fields[1].mask = 0xf0;
    pp.layout = KeyLayout::new(fields);
    let a = Sim::with_options(Arc::clone(&p), with_spec(SimOptions::jit(), &low));
    let mut b = Sim::with_options(
        Arc::clone(&p),
        with_spec(SimOptions::jit().tier_layout(), &high),
    );
    let code = Arc::new(a.compile_jit().unwrap());
    assert!(
        !b.install_jit(Some(code)),
        "code for bits 0..4 is not for bits 4..8"
    );
    let mut want = Sim::with_options(Arc::clone(&p), SimOptions::baseline());
    same_batches(&mut want, &mut b);
}

/// A plan with more key values than run indices on one phase of a
/// two-phase program: the `Sim` is built (bodies stop at the cap, leaving
/// room for the other phase's runs) and still equals single ticks.
#[test]
fn a_plan_past_the_run_index_cap_leaves_room_for_other_phases() {
    let p = program(&microcoded(true), &Options::default());
    let period = p.phases.as_ref().map_or(1, |ph| ph.period as usize);
    assert!(period >= 2, "two clock phases");
    // The 8-bit input `x` and the 8-bit accumulator: 65536 key values.
    let x = p
        .ports
        .inputs
        .iter()
        .find(|i| i.name.as_deref() == Some("x"))
        .expect("input x")
        .slot;
    let acc = p
        .state_keys
        .iter()
        .find_map(|k| match k.loc {
            crate::compile::StateLoc::Slot(s) if k.width == 8 => Some(p.resolve(s)?.0),
            _ => None,
        })
        .expect("the 8-bit accumulator");
    let layout = KeyLayout::new(vec![
        KeyField {
            slot: x,
            mask: 0xff,
        },
        KeyField {
            slot: acc,
            mask: 0xff,
        },
    ]);
    let keys: Vec<u64> = (0..1u64 << layout.width()).collect();
    assert!(keys.len() >= usize::from(u16::MAX));
    let mut phases = vec![None; period];
    phases[0] = Some(PhasePlan { layout, keys });
    let plan = SpecPlan { phases };
    let mut s = Sim::with_options(Arc::clone(&p), with_spec(SimOptions::default(), &plan));
    assert!(s.runs.len() <= usize::from(u16::MAX));
    let mut want = Sim::with_options(Arc::clone(&p), SimOptions::baseline());
    same_batches(&mut want, &mut s);
}

/// The key packed by segments (`KeyLayout::segments`) is `KeyLayout::pack`'s, on
/// random layouts: masks with gaps, fields of one slot (merged segments),
/// the full word, empty masks.
#[test]
fn keys_pack_as_the_layout_does() {
    let mut rng = Rng::new(0x6b65);
    let masks = |rng: &mut Rng| match rng.next_u64() % 6 {
        0 => 0,
        1 => u64::MAX,
        2 => rng.next_u64() & rng.next_u64() & rng.next_u64(),
        3 => (1 << (rng.next_u64() % 8)) - 1,
        4 => 0b1010_0110 << (rng.next_u64() % 40),
        _ => rng.next_u64() & 0xff,
    };
    let mut checked = 0;
    for _ in 0..4000 {
        let n = 1 + rng.next_u64() as usize % 6;
        let fields: Vec<KeyField> = (0..n)
            .map(|_| KeyField {
                slot: (rng.next_u64() % 4) as u32,
                mask: masks(&mut rng),
            })
            .collect();
        let layout = KeyLayout::new(fields);
        if !layout.fits() {
            continue;
        }
        let ps = PhaseSpec::new(&layout, None, 0);
        for _ in 0..8 {
            let slots: Vec<u64> = (0..4).map(|_| rng.next_u64()).collect();
            assert_eq!(ps.key(&slots), layout.pack(&slots), "{layout:?}");
        }
        checked += 1;
    }
    assert!(checked > 1000);
}

/// The table's memo and cache follow inserts and removes (a removal
/// moves the last entry into the hole), and agree with the map on many
/// keys (colliding in the cache).
#[test]
fn the_table_memo_follows_changes() {
    let mut t = Table::new();
    assert_eq!(t.find(0), MISS);
    t.insert(0, 10, 0);
    assert_eq!(t.find(0), 0);
    t.insert(5, 11, 0);
    t.insert(7, 12, 0);
    assert_eq!(t.find(7), 2);
    assert_eq!(t.remove(0).map(|e| e.run), Some(10));
    // 7 moved into slot 0, the memo with it.
    assert_eq!(t.memo, 0);
    assert_eq!(t.find(7), 0);
    assert_eq!(t.entries[0].run, 12);
    assert_eq!(t.find(5), 1);
    assert_eq!(t.remove(5).map(|e| e.run), Some(11));
    assert_eq!(t.memo, MISS);
    assert_eq!(t.find(5), MISS);
    t.insert(5, 13, 0);
    assert_eq!(t.memo, 1);
    assert_eq!(t.get(5).map(|e| e.run), Some(13));
    assert!(t.remove(9).is_none());
    assert_eq!(t.len(), 2);
    // A key installed again replaces its entry.
    t.insert(5, 14, 3);
    assert_eq!((t.len(), t.find(5)), (2, 1));
    assert_eq!(t.get(5).map(|e| (e.run, e.last)), Some((14, 3)));
    // Many keys, some with entries, looked up in a random order while
    // entries come and go: `find` is the map's answer.
    let mut rng = Rng::new(0x7ab1e);
    let mut t = Table::new();
    for round in 0..4000 {
        let key = rng.below(300) as u64;
        match rng.below(8) {
            0 if t.get(key).is_none() => t.insert(key, round as u16, 0),
            1 => {
                t.remove(key);
            }
            _ => {
                let want = t.index.get(&key).copied().unwrap_or(MISS);
                assert_eq!(t.find(key), want, "key {key}, round {round}");
                if want != MISS {
                    assert_eq!(t.entries[want as usize].key, key);
                }
            }
        }
    }
}
