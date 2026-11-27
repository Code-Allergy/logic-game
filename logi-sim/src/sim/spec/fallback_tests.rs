//! The fallback from keys to supersteps ([`super::FallbackOptions`]):
//! switching modes at any `run` boundary equals single ticks (forced
//! switches, and the wall clock with tiny windows); with a virtual clock,
//! a key that always misses falls back, one that hits stays on, and a
//! cost that changes moves the runtime both ways; mutations caught.

use std::sync::Arc;

#[cfg(feature = "jit")]
use super::SpecPlan;
use super::tests::{counting, microcoded, microcoded_plan, program, same_batches};
use super::{FORCE, FallbackOptions, MUTATION, SpecOptions, VIRTUAL};
use crate::compile::{Options, Program};
use crate::sim::{Sim, SimOptions};
#[cfg(feature = "jit")]
use crate::testkit::diff::observe;
use crate::testkit::diff::{JIT, check_batches_with, check_pair, standard_configs};
use crate::testkit::{GenConfig, generate};

/// Resets the test hooks when dropped (a failing assertion included).
struct Hooks;
impl Drop for Hooks {
    fn drop(&mut self) {
        FORCE.set(0);
        VIRTUAL.set(None);
        MUTATION.set(0);
    }
}

/// Small windows: every few runs a decision.
fn small() -> FallbackOptions {
    FallbackOptions {
        window: 64,
        sample: 32,
        resample: 4,
        margin: 5,
        patience: 2,
        probe_min: 256,
        probe_max: 1024,
        min_run: 1,
    }
}

fn with_fallback(spec: SpecOptions, f: FallbackOptions) -> SpecOptions {
    SpecOptions {
        fallback: Some(f),
        ..spec
    }
}

fn jit_with(spec: &SpecOptions) -> SimOptions {
    SimOptions {
        spec: Some(spec.clone()),
        ..SimOptions::jit()
    }
}

/// The specializations the switching tests run under: a fixed plan with
/// every key value, one with some, adaptive tables on the plan's keys,
/// and an automatic plan (whose superstep comes with the plan).
fn specs(p: &Program) -> Vec<SpecOptions> {
    let all = microcoded_plan(p, None);
    let some = microcoded_plan(p, Some(vec![0, 3, 5]));
    let f = small();
    vec![
        with_fallback(SpecOptions::fixed(all.clone()), f.clone()),
        with_fallback(SpecOptions::fixed(some), f.clone()),
        with_fallback(
            SpecOptions {
                window: 8,
                hot: 2,
                max_per_phase: 2,
                max_bodies: 4,
                ..SpecOptions::with_plan(all)
            },
            f.clone(),
        ),
        with_fallback(
            SpecOptions {
                window: 16,
                hot: 2,
                plan_window: 16,
                samples: 8,
                min_loss: 1,
                max_per_phase: 4,
                ..SpecOptions::default()
            },
            f,
        ),
    ]
}

fn machines() -> Vec<Arc<Program>> {
    [false, true]
        .into_iter()
        .map(|clocked| program(&microcoded(clocked), &Options::default()))
        .collect()
}

/// With the fallback on, the layout's native code carries the superstep
/// beside the keys (a fixed plan), or the automatic plan brings it; off,
/// neither does.
#[cfg(feature = "jit")]
#[test]
fn the_fallback_brings_the_superstep() {
    for p in machines() {
        let plan = microcoded_plan(&p, None);
        let on = Sim::with_options(
            Arc::clone(&p),
            jit_with(&with_fallback(SpecOptions::fixed(plan.clone()), small())),
        );
        assert!(!on.spec.phases.is_empty());
        assert!(on.jit_code().unwrap().superstep().is_some());
        let off = Sim::with_options(Arc::clone(&p), jit_with(&SpecOptions::fixed(plan)));
        assert!(off.jit_code().unwrap().superstep().is_none());
        let auto = &specs(&p)[3];
        let mut s = Sim::with_options(Arc::clone(&p), jit_with(auto));
        while s.spec_stats().awaiting_plan {
            assert!(s.super_fn().is_none());
            s.run(10);
            assert!(s.tick_count() < 5000, "no plan");
        }
        assert!(!s.spec.phases.is_empty(), "the automatic plan keys a phase");
        assert!(s.spec.superstep.is_some() && s.super_fn().is_some());
        let mut s = Sim::with_options(
            Arc::clone(&p),
            jit_with(&SpecOptions {
                fallback: None,
                ..auto.clone()
            }),
        );
        while s.spec_stats().awaiting_plan {
            s.run(10);
        }
        assert!(!s.spec.phases.is_empty() && s.super_fn().is_none());
    }
}

/// Modes forced to alternate at every `run`, or supersteps always: every
/// batch boundary of every runtime variant equals single ticks, through
/// pokes, imports and resets; supersteps did run on the keyed `Sim`s.
#[test]
fn forced_switches_match_single_ticks() {
    let _hooks = Hooks;
    for force in [1, 2] {
        FORCE.set(force);
        for p in machines() {
            for spec in specs(&p) {
                for seed in 1..=3 {
                    check_batches_with(&p, seed, Some(&spec))
                        .unwrap_or_else(|e| panic!("force {force}: {e}"));
                }
                check_pair(&p, 300, 4, &SimOptions::baseline(), &jit_with(&spec))
                    .unwrap_or_else(|e| panic!("force {force}: {e}"));
                if JIT {
                    let mut want = Sim::with_options(Arc::clone(&p), SimOptions::baseline());
                    let mut s = Sim::with_options(Arc::clone(&p), jit_with(&spec));
                    for _ in 0..30 {
                        same_batches(&mut want, &mut s);
                    }
                    let st = s.spec_stats();
                    assert!(st.keyed_phases > 0 && st.super_ticks > 100, "{st:?}");
                    if force == 1 {
                        assert!(st.hits + st.misses > 100, "{st:?}");
                    }
                }
            }
        }
    }
    // Random designs, with a plan from their candidates.
    FORCE.set(1);
    for (c, cfg) in standard_configs().iter().enumerate() {
        let g = generate(&GenConfig {
            seed: c as u64 * 131 + 7,
            ..cfg.clone()
        });
        for options in [Options::default(), Options::debug()] {
            let q = program(&g.design, &options);
            let plan = crate::testkit::diff::spec_plan(&q, c as u64 + 1, 64);
            for spec in [
                SpecOptions::fixed(plan.clone()),
                SpecOptions {
                    window: 4,
                    hot: 2,
                    ..SpecOptions::with_plan(plan)
                },
            ] {
                let spec = with_fallback(spec, small());
                check_batches_with(&q, c as u64, Some(&spec))
                    .unwrap_or_else(|e| panic!("config {c}: {e}"));
            }
        }
    }
}

/// The wall clock with tiny windows: modes flip as the machine's noise
/// says, and every batch boundary still equals single ticks.
#[test]
fn wall_clock_switches_match_single_ticks() {
    let tiny = FallbackOptions {
        window: 4,
        sample: 4,
        resample: 2,
        margin: 0,
        patience: 1,
        probe_min: 4,
        probe_max: 16,
        min_run: 1,
    };
    for p in machines() {
        for spec in specs(&p) {
            let spec = with_fallback(spec, tiny.clone());
            for seed in 1..=2 {
                check_batches_with(&p, seed, Some(&spec)).unwrap();
            }
        }
    }
}

/// The ticks before a superstep settled by the keyed path instead of the
/// general fused body (whose commit the superstep's first part is): caught.
#[cfg(feature = "jit")]
#[test]
fn a_keyed_settle_before_a_superstep_is_caught() {
    let _hooks = Hooks;
    FORCE.set(1);
    let cases: Vec<(Arc<Program>, SpecOptions)> = machines()
        .into_iter()
        .flat_map(|p| specs(&p).into_iter().map(move |s| (Arc::clone(&p), s)))
        .collect();
    for (p, spec) in &cases {
        check_batches_with(p, 1, Some(spec)).unwrap();
    }
    MUTATION.set(7);
    let caught = cases
        .iter()
        .any(|(p, spec)| (1..=3).any(|seed| check_batches_with(p, seed, Some(spec)).is_err()));
    MUTATION.set(0);
    assert!(caught, "mutation 7 is not caught");
}

/// Runs of `n` ticks of `s` against single ticks of `want`, with input
/// changes; equal at every boundary.
#[cfg(feature = "jit")]
fn batches(want: &mut Sim, s: &mut Sim, rounds: u64, n: u64) {
    for round in 0..rounds {
        let x = round.wrapping_mul(977) & 0xff;
        want.set_input(0, x);
        s.set_input(0, x);
        for _ in 0..n {
            want.tick();
        }
        s.run(n);
        assert!(observe(want) == observe(s), "round {round}");
    }
}

/// A key that always misses (a counter key with no body for its values):
/// with misses dearer than superstep ticks the runtime falls back after
/// two confirmed windows, and mostly runs supersteps from then on (probes
/// only); exact throughout.
#[cfg(feature = "jit")]
#[test]
fn a_key_that_always_misses_falls_back() {
    let _hooks = Hooks;
    VIRTUAL.set(Some((10, 5, 15)));
    let keys: Vec<u64> = (0..64u64).map(|k| 0x8000 + k).collect();
    let (p, plan) = counting(0xffff, &keys);
    let spec = with_fallback(SpecOptions::fixed(plan), small());
    let mut want = Sim::with_options(Arc::clone(&p), SimOptions::baseline());
    let mut s = Sim::with_options(Arc::clone(&p), jit_with(&spec));
    batches(&mut want, &mut s, 4, 16);
    let st = s.spec_stats();
    assert!(st.fallbacks == 0 && st.misses > 0 && st.hits == 0, "{st:?}");
    batches(&mut want, &mut s, 200, 16);
    let st = s.spec_stats();
    assert!(st.fallbacks >= 1 && st.probes >= 1, "{st:?}");
    // 3200 ticks: the first 192 or so keyed, then probes of 64 after
    // 256, 512, 1024, 1024 ... superstep ticks.
    assert!(st.super_ticks > 2400, "{st:?}");
    assert!(st.misses < 800, "{st:?}");
    // The same with the fallback off: keys all the way.
    let mut off = Sim::with_options(
        Arc::clone(&p),
        jit_with(&SpecOptions {
            fallback: None,
            ..spec
        }),
    );
    off.run(3264);
    assert!(off.spec_stats().misses > 3000 && off.spec_stats().super_ticks == 0);
}

/// Keys that always hit (a body for every value) with hits cheaper than
/// superstep ticks: specialization stays on, supersteps only for the
/// baseline samples.
#[cfg(feature = "jit")]
#[test]
fn keys_that_hit_stay_on() {
    let _hooks = Hooks;
    VIRTUAL.set(Some((10, 5, 15)));
    for p in machines() {
        let spec = with_fallback(SpecOptions::fixed(microcoded_plan(&p, None)), small());
        let mut want = Sim::with_options(Arc::clone(&p), SimOptions::baseline());
        let mut s = Sim::with_options(Arc::clone(&p), jit_with(&spec));
        batches(&mut want, &mut s, 200, 16);
        let st = s.spec_stats();
        assert!(st.fallbacks == 0 && !st.fallen && st.misses == 0, "{st:?}");
        // One sample of 32 per 4 windows of 64 (the first window too).
        assert!(
            st.super_ticks > 0 && st.super_ticks <= 3200 / 8 + 64,
            "{st:?}"
        );
    }
}

/// The cost moves: within the margin specialization stays on; past it,
/// the runtime falls back; once keys pay again a probe brings them back
/// (and the probe interval had grown meanwhile). Exact throughout.
#[cfg(feature = "jit")]
#[test]
fn the_runtime_follows_the_cost_both_ways() {
    let _hooks = Hooks;
    let keys: Vec<u64> = (0..64u64).map(|k| 0x8000 + k).collect();
    let (p, plan) = counting(0xffff, &keys);
    let spec = with_fallback(SpecOptions::fixed(plan), small());
    let mut want = Sim::with_options(Arc::clone(&p), SimOptions::baseline());
    let mut s = Sim::with_options(Arc::clone(&p), jit_with(&spec));
    // 4% dearer: stays on.
    VIRTUAL.set(Some((100, 104, 104)));
    batches(&mut want, &mut s, 100, 16);
    let st = s.spec_stats();
    assert!(st.fallbacks == 0 && !st.fallen, "{st:?}");
    // 20% dearer: falls back, and probes lose.
    VIRTUAL.set(Some((100, 120, 120)));
    batches(&mut want, &mut s, 300, 16);
    let st = s.spec_stats();
    assert!(st.fallen && st.fallbacks >= 2 && st.probes >= 1, "{st:?}");
    // 4% dearer again: a probe within the margin does not come back.
    VIRTUAL.set(Some((100, 104, 104)));
    batches(&mut want, &mut s, 300, 16);
    let before = s.spec_stats();
    assert!(before.fallen && before.probes > st.probes, "{before:?}");
    // Cheaper: the next probe comes back, and it stays.
    VIRTUAL.set(Some((100, 80, 80)));
    batches(&mut want, &mut s, 300, 16);
    let st = s.spec_stats();
    assert!(!st.fallen && st.fallbacks == before.fallbacks, "{st:?}");
    let misses = st.misses;
    batches(&mut want, &mut s, 100, 16);
    let st = s.spec_stats();
    assert!(st.misses - misses > 1200, "{st:?}");
}

/// Without native code (no superstep) there is nothing to fall back to:
/// no measurement, keys all the way; runs shorter than `min_run` are not
/// measured either.
#[test]
fn no_superstep_no_fallback() {
    let _hooks = Hooks;
    VIRTUAL.set(Some((10, 5, 15)));
    let keys: Vec<u64> = (0..64u64).map(|k| 0x8000 + k).collect();
    let (p, plan) = counting(0xffff, &keys);
    let spec = with_fallback(SpecOptions::fixed(plan.clone()), small());
    let mut s = Sim::with_options(
        Arc::clone(&p),
        SimOptions {
            spec: Some(spec.clone()),
            ..SimOptions::default()
        },
    );
    s.run(5000);
    let st = s.spec_stats();
    assert!(
        st.super_ticks == 0 && st.fallbacks == 0 && st.misses > 4000,
        "{st:?}"
    );
    if JIT {
        let short = with_fallback(
            SpecOptions::fixed(plan),
            FallbackOptions {
                min_run: 17,
                ..small()
            },
        );
        let mut s = Sim::with_options(Arc::clone(&p), jit_with(&short));
        for _ in 0..500 {
            s.run(16);
        }
        let st = s.spec_stats();
        assert!(st.super_ticks == 0 && st.fallbacks == 0, "{st:?}");
    }
}

/// A plan that keys nothing has no keys to fall back from: the superstep
/// runs as before, and the fallback stays out of it.
#[cfg(feature = "jit")]
#[test]
fn unkeyed_plans_are_left_alone() {
    let _hooks = Hooks;
    VIRTUAL.set(Some((10, 5, 15)));
    let p = program(&microcoded(true), &Options::default());
    let spec = with_fallback(
        SpecOptions::fixed(SpecPlan {
            phases: vec![None, None],
        }),
        small(),
    );
    let mut s = Sim::with_options(Arc::clone(&p), jit_with(&spec));
    assert!(s.spec.phases.is_empty() && s.super_fn().is_some());
    s.run(5000);
    assert_eq!(s.spec_stats().super_ticks, 0);
}

/// The supersteps a keyed `Sim` falls back to do run: each of the
/// superstep mutations of `jit::superstep` is caught by `check_batches`
/// on keyed `Sim`s forced to supersteps (a fixed plan, whose superstep is
/// the program's, and an automatic one, whose superstep is the plan's).
#[cfg(feature = "jit")]
#[test]
fn the_supersteps_fallen_back_to_are_checked() {
    let _hooks = Hooks;
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            crate::sim::jit::MUTATION.set(0);
        }
    }
    let _reset = Reset;
    FORCE.set(2);
    for (i, kind) in [0, 3].into_iter().enumerate() {
        let cases: Vec<(Arc<Program>, SpecOptions)> = machines()
            .into_iter()
            .map(|p| {
                let s = specs(&p).swap_remove(kind);
                (p, s)
            })
            .collect();
        for (p, spec) in &cases {
            check_batches_with(p, 1, Some(spec)).unwrap();
        }
        for m in 1..=3 {
            crate::sim::jit::MUTATION.set(m);
            let caught = cases
                .iter()
                .any(|(p, spec)| check_batches_with(p, 1, Some(spec)).is_err());
            crate::sim::jit::MUTATION.set(0);
            assert!(caught, "superstep mutation {m} is not caught ({i})");
        }
    }
}
