//! Adaptive tables (P5b of `docs/adaptive-spec.md`): hot keys get bodies,
//! bodies come and go under the caps, builds from another generation or
//! `Sim` are refused, builds on another thread, automatic plans; every
//! batch boundary equals single ticks (`check_batches_with`,
//! `check_pair`, `check_jit`); mutations caught.

use std::sync::Arc;

use super::tests::{
    NoMutation, counting, jit_options, microcoded, microcoded_plan, program, same_batches,
};
use super::{MUTATION, PhasePlan, SpecOptions, SpecPlan, SpecRequest, build_spec};
use crate::compile::{Options, Program};
use crate::sim::{Sim, SimOptions};
use crate::testkit::diff::{
    JIT, check_batches_with, check_pair, observe, spec_plan, standard_configs,
};
use crate::testkit::{GenConfig, generate};

/// `plan`'s layouts without key values: adaptive tables start empty.
fn layouts(plan: &SpecPlan) -> SpecPlan {
    SpecPlan {
        phases: plan
            .phases
            .iter()
            .map(|pp| {
                pp.as_ref().map(|pp| PhasePlan {
                    layout: pp.layout.clone(),
                    keys: Vec::new(),
                })
            })
            .collect(),
    }
}

/// Adaptive options that move fast on small designs: a key is hot at its
/// `hot`-th miss within a window of `window` fused ticks of its phase.
fn fast(plan: Option<SpecPlan>, per_phase: usize, window: u64, hot: u32) -> SpecOptions {
    SpecOptions {
        plan: plan.map(Arc::new),
        max_per_phase: per_phase,
        max_bodies: 3 * per_phase,
        window,
        hot,
        plan_window: window,
        samples: 8,
        min_loss: 1,
        ..SpecOptions::default()
    }
}

fn with(o: SimOptions, spec: &SpecOptions) -> SimOptions {
    SimOptions {
        spec: Some(spec.clone()),
        ..o
    }
}

/// Before any miss the table is empty; a key value seen `hot` times
/// within a window gets a body at the end of the `run` (inline builds),
/// and from then on hits. A threshold no key reaches builds nothing.
#[test]
fn hot_keys_get_bodies() {
    let p = program(&microcoded(false), &Options::default());
    let plan = layouts(&microcoded_plan(&p, None));
    for o in jit_options() {
        // The key follows the 4-bit program counter (the ROM word's bits
        // the logic reads, mode 0): each value recurs within 16 ticks, at
        // most 16 values.
        let spec = fast(Some(plan.clone()), 16, 1 << 10, 20);
        let mut s = Sim::with_options(Arc::clone(&p), with(o.clone(), &spec));
        let st = s.spec_stats();
        assert!(
            st.keyed_phases == 1 && st.bodies == 0 && st.requested == 0,
            "{st:?}"
        );
        // Fewer than 20 visits of any value: nothing is hot yet.
        s.run(19);
        let st = s.spec_stats();
        assert!(st.bodies == 0 && st.hits == 0 && st.misses == 18, "{st:?}");
        // 20 visits of every value: each turns hot, built after the run.
        s.run(320);
        let st = s.spec_stats();
        let distinct = s.spec.phases[0].as_ref().unwrap().bodies();
        assert!(st.bodies > 0 && st.bodies == distinct, "{st:?}");
        assert_eq!(st.installed, st.bodies as u64);
        assert_eq!(st.hits, 0);
        let misses = st.misses;
        s.run(64);
        let st = s.spec_stats();
        assert!(st.hits >= 60 && st.misses - misses <= 4, "{st:?}");
        if s.jit_active() {
            assert!(st.code_bytes > 0, "{st:?}");
        }
        let mut want = Sim::with_options(Arc::clone(&p), SimOptions::baseline());
        for _ in 0..s.tick_count() {
            want.tick();
        }
        assert!(observe(&want) == observe(&s));

        let cold = fast(Some(plan.clone()), 16, 1 << 10, 1000);
        let mut s = Sim::with_options(Arc::clone(&p), with(o.clone(), &cold));
        s.run(500);
        let st = s.spec_stats();
        assert!(
            st.bodies == 0 && st.requested == 0 && st.misses > 400,
            "{st:?}"
        );
    }
}

/// The key values of a fixed plan become wanted keys of an adaptive
/// table: built with the `Sim` (inline), they hit from the first tick.
#[test]
fn a_plans_keys_are_built_first() {
    let p = program(&microcoded(true), &Options::default());
    let plan = microcoded_plan(&p, Some(vec![0, 1, 2, 3, 4, 5, 6, 7]));
    for o in jit_options() {
        let s = Sim::with_options(
            Arc::clone(&p),
            with(o.clone(), &SpecOptions::with_plan(plan.clone())),
        );
        let st = s.spec_stats();
        assert!(st.keyed_phases == 2 && st.bodies == 16, "{st:?}");
    }
}

/// Random designs of `standard_configs`, default and debug compile
/// options: adaptive tables on the keys of `spec_plan` (empty at first),
/// with tiny caps and windows so that bodies are built, hit and evicted
/// within the test, match single ticks in every runtime variant
/// (`check_batches_with`), spec off (`check_pair`: pokes, ROM writes,
/// resets, imports) and the interpreter (`check_jit`). So do automatic
/// plans (sampled in the first window).
#[test]
fn adaptive_tables_on_random_designs_match_single_ticks() {
    let (mut installed, mut evicted, mut hits, mut planned, mut n) = (0, 0, 0, 0, 0);
    for (c, cfg) in standard_configs().iter().enumerate() {
        for seed in 0..4u64 {
            let g = generate(&GenConfig {
                seed: seed * 7919 + c as u64 * 31 + 1,
                ..cfg.clone()
            });
            for options in [Options::default(), Options::debug()] {
                let p = program(&g.design, &options);
                let keyed = layouts(&spec_plan(&p, seed, 64));
                let specs = [
                    fast(Some(keyed), 1, 2, 1),
                    fast(Some(layouts(&spec_plan(&p, seed + 1, 0))), 3, 16, 2),
                    fast(None, 2, 8, 1),
                ];
                for (i, spec) in specs.iter().enumerate() {
                    let at = format!("config {c} seed {seed} spec {i}");
                    check_batches_with(&p, seed, Some(spec))
                        .unwrap_or_else(|e| panic!("{at}: {e}"));
                    let on = with(SimOptions::default(), spec);
                    check_pair(&p, 150, seed, &SimOptions::default(), &on)
                        .unwrap_or_else(|e| panic!("{at}, spec on/off: {e}"));
                    if JIT {
                        crate::testkit::diff::check_jit(&p, 150, seed, &on)
                            .unwrap_or_else(|e| panic!("{at}, JIT: {e}"));
                    }
                    let mut s = Sim::with_options(Arc::clone(&p), on);
                    for _ in 0..20 {
                        s.run(13);
                    }
                    let st = s.spec_stats();
                    assert!(st.bodies <= spec.max_bodies, "{at}: {st:?}");
                    installed += st.installed;
                    evicted += st.evicted;
                    hits += st.hits;
                    planned += u64::from(i == 2 && st.keyed_phases > 0);
                    n += 1;
                }
            }
        }
    }
    assert!(n >= 150);
    assert!(
        installed > 100 && evicted > 0 && hits > 1000 && planned > 3,
        "{installed} installed, {evicted} evicted, {hits} hits, {planned} automatic plans"
    );
}

/// A key that changes every 16 ticks and cycles through 8 values every 64
/// (two bits of a counter, and a flag of its low bits) under a cap of 4
/// bodies and a window of 8: bodies are built, hit and evicted all the
/// time, never more than 4 live, and batches equal single ticks of the
/// plain runtime throughout; with the JIT, a code budget of 0 bytes keeps
/// every body out.
#[test]
fn eviction_under_a_tiny_cap_stays_exact() {
    let (p, plan) = counting(0x30, &[]);
    for o in jit_options() {
        for budget in [usize::MAX, 0] {
            let spec = SpecOptions {
                code_budget: budget,
                ..fast(Some(plan.clone()), 4, 8, 1)
            };
            let mut got = Sim::with_options(Arc::clone(&p), with(o.clone(), &spec));
            let mut want = Sim::with_options(Arc::clone(&p), SimOptions::baseline());
            let jit = got.jit_active();
            for round in 0..300u64 {
                got.set_input(0, round * 31);
                want.set_input(0, round * 31);
                let n = 1 + round % 11;
                got.run(n);
                for _ in 0..n {
                    want.tick();
                }
                assert!(observe(&want) == observe(&got), "round {round}");
                let st = got.spec_stats();
                assert!(st.bodies <= 4, "{st:?}");
                assert!(st.code_bytes <= budget, "{st:?}");
            }
            let st = got.spec_stats();
            if jit && budget == 0 {
                // No native body fits a budget of 0 bytes.
                assert_eq!(st.bodies, 0, "{st:?}");
                continue;
            }
            assert!(
                st.installed > 50 && st.evicted > 40 && st.hits > 0,
                "{st:?}"
            );
            // Freed runs are reused: the runs never outgrow the cap.
            assert!(
                got.runs.len() <= got.spec.base + 4 + 1,
                "{} runs, base {}",
                got.runs.len(),
                got.spec.base
            );
        }
    }
}

/// The caller drives the builds (no inline builds): requests are refused
/// once stale (a ROM write, the `reset` after it, an import that changes
/// a ROM, another `Sim`, another program), and accepted otherwise (a
/// plain `reset` changes nothing the bodies depend on).
#[test]
fn stale_builds_are_refused() {
    let p = program(&microcoded(false), &Options::default());
    let plan = layouts(&microcoded_plan(&p, None));
    let spec = SpecOptions {
        inline_build: false,
        ..fast(Some(plan.clone()), 16, 1 << 10, 2)
    };
    let rom = (0..p.mems.len() as u32)
        .find(|&m| !p.mems[m as usize].writable)
        .unwrap();
    for o in jit_options() {
        let o = with(o, &spec);
        let new = |p: &Arc<Program>| Sim::with_options(Arc::clone(p), o.clone());
        // Hot keys after 40 ticks, built (not installed).
        let builds = |s: &mut Sim| {
            s.run(40);
            let r = s.spec_requests();
            assert!(!r.is_empty());
            let prog = Arc::clone(s.program());
            r.iter().map(|r| build_spec(&prog, r)).collect::<Vec<_>>()
        };

        // Fresh: accepted.
        let mut s = new(&p);
        for b in builds(&mut s) {
            assert!(s.install_spec(b));
        }
        assert!(s.spec_stats().bodies > 0);

        // A plain reset: accepted.
        let mut s = new(&p);
        let b = builds(&mut s);
        s.reset();
        assert!(b.into_iter().all(|b| s.install_spec(b)));

        // A ROM write, then the reset that undoes it: refused both times.
        for reset in [false, true] {
            let mut s = new(&p);
            let b = builds(&mut s);
            let w = s.mem(rom)[1];
            s.set_mem_word(rom, 1, w ^ 4);
            if reset {
                s.reset();
            }
            let n = b.len() as u64;
            assert!(b.into_iter().all(|b| !s.install_spec(b)));
            let st = s.spec_stats();
            assert!(st.stale == n && st.bodies == 0, "{st:?}");
            if reset {
                // New requests are fine.
                for b in builds(&mut s) {
                    assert!(s.install_spec(b));
                }
            }
        }

        // An import (snapshots hold no ROM: the ROM keeps its contents):
        // accepted. An import that changes a ROM word goes through the
        // same switch-off as a ROM write.
        let mut other = new(&p);
        other.run(7);
        let snap = other.export_state();
        let mut s = new(&p);
        let b = builds(&mut s);
        s.import_state(&snap);
        assert!(b.into_iter().all(|b| s.install_spec(b)));

        // Another `Sim` of the same program, and of another program.
        let mut a = new(&p);
        let b = builds(&mut a);
        let mut s = new(&p);
        let q = program(&microcoded(false), &Options::default());
        let mut t = new(&q);
        for b in b {
            assert!(!s.install_spec(b));
        }
        let b = builds(&mut new(&p));
        for b in b {
            assert!(!t.install_spec(b));
        }
        assert!(s.spec_stats().bodies == 0 && t.spec_stats().bodies == 0);
        // Results stay exact through all this.
        let mut want = Sim::with_options(Arc::clone(&p), SimOptions::baseline());
        let mut s = new(&p);
        for b in builds(&mut s) {
            s.install_spec(b);
        }
        for _ in 0..s.tick_count() {
            want.tick();
        }
        same_batches(&mut want, &mut s);
    }
}

/// Builds on a thread of their own (as an engine's compile thread does):
/// after each batch the requests go out, built bodies come back and are
/// installed between batches; batches equal single ticks throughout, and
/// bodies arrive. With an automatic plan the plan is built there too.
#[test]
fn builds_on_another_thread() {
    use std::sync::mpsc;
    let p = program(&microcoded(true), &Options::default());
    let plan = layouts(&microcoded_plan(&p, None));
    for plan in [Some(plan), None] {
        for o in jit_options() {
            let spec = SpecOptions {
                inline_build: false,
                ..fast(plan.clone(), 6, 32, 2)
            };
            let mut got = Sim::with_options(Arc::clone(&p), with(o.clone(), &spec));
            let mut want = Sim::with_options(Arc::clone(&p), SimOptions::baseline());
            let (to_builder, requests) = mpsc::channel::<super::SpecRequest>();
            let (to_sim, built) = mpsc::channel();
            let prog = Arc::clone(&p);
            let builder = std::thread::spawn(move || {
                for r in requests {
                    if to_sim.send(build_spec(&prog, &r)).is_err() {
                        break;
                    }
                }
            });
            let mut flying = 0;
            for round in 0..200u64 {
                got.set_input(0, round * 7);
                want.set_input(0, round * 7);
                let n = 1 + round % 13;
                got.run(n);
                for _ in 0..n {
                    want.tick();
                }
                assert!(observe(&want) == observe(&got), "round {round}");
                for r in got.spec_requests() {
                    to_builder.send(r).unwrap();
                    flying += 1;
                }
                // Mostly whatever came back by now; every 16th round, wait
                // for everything in flight.
                loop {
                    let b = if round % 16 == 15 && flying > 0 {
                        built.recv().unwrap()
                    } else {
                        match built.try_recv() {
                            Ok(b) => b,
                            Err(_) => break,
                        }
                    };
                    flying -= 1;
                    got.install_spec(b);
                }
            }
            drop(to_builder);
            builder.join().unwrap();
            let st = got.spec_stats();
            assert!(
                st.installed > 0 && st.hits > 0 && st.keyed_phases > 0,
                "{plan:?}: {st:?}"
            );
        }
    }
}

/// An automatic plan: no key while the first window is sampled (no
/// superstep either), then the plan's keys and hot bodies; a ROM write
/// while sampling stops it, the reset brings sampling back.
#[test]
fn automatic_plans_key_the_microcoded_machine() {
    let p = program(&microcoded(true), &Options::default());
    let rom = (0..p.mems.len() as u32)
        .find(|&m| !p.mems[m as usize].writable)
        .unwrap();
    for o in jit_options() {
        let spec = SpecOptions {
            samples: 64,
            ..fast(None, 8, 256, 2)
        };
        let mut s = Sim::with_options(Arc::clone(&p), with(o.clone(), &spec));
        #[cfg(feature = "jit")]
        if s.jit_active() {
            assert!(s.jit_code().unwrap().superstep().is_none());
        }
        let st = s.spec_stats();
        assert!(st.awaiting_plan && st.keyed_phases == 0, "{st:?}");
        let mut want = Sim::with_options(Arc::clone(&p), SimOptions::baseline());
        same_batches(&mut want, &mut s);
        while s.spec_stats().awaiting_plan {
            s.run(50);
            for _ in 0..50 {
                want.tick();
            }
            assert!(observe(&want) == observe(&s));
            assert!(s.tick_count() < 5000, "no plan");
        }
        // Bodies for the plan's hot keys arrive after a run, then hit.
        for _ in 0..8 {
            s.run(50);
            for _ in 0..50 {
                want.tick();
            }
            assert!(observe(&want) == observe(&s));
        }
        let st = s.spec_stats();
        assert!(st.keyed_phases > 0 && st.hits > 0, "{st:?}");

        // A ROM write while sampling.
        let mut s = Sim::with_options(Arc::clone(&p), with(o.clone(), &spec));
        s.run(10);
        let w = s.mem(rom)[0];
        s.set_mem_word(rom, 0, w ^ 2);
        s.run(2000);
        let st = s.spec_stats();
        assert!(st.keyed_phases == 0 && st.awaiting_plan, "{st:?}");
        s.reset();
        s.run(2000);
        let st = s.spec_stats();
        assert!(st.keyed_phases > 0 && !st.awaiting_plan, "{st:?}");
    }
}

/// With the JIT: installed bodies run their own units; the program's
/// native code stays installed as it was (no recompile), and native code
/// compiled for the layout still installs after bodies came and went. An
/// automatic plan's layout refuses code of a plain one (which may hold a
/// superstep).
#[cfg(feature = "jit")]
#[test]
fn installed_bodies_keep_the_programs_native_code() {
    let (p, plan) = counting(0x30, &[]);
    let spec = fast(Some(plan), 4, 8, 1);
    let mut s = Sim::with_options(Arc::clone(&p), with(SimOptions::jit(), &spec));
    let code = s.jit_code().map(|c| c as *const _).unwrap();
    for _ in 0..100 {
        s.run(5);
    }
    let st = s.spec_stats();
    assert!(st.installed > 10 && st.evicted > 0 && st.hits > 0, "{st:?}");
    assert!(std::ptr::eq(code, s.jit_code().unwrap()));
    let ps = s.spec.phases[0].as_ref().unwrap();
    for (_, run) in ps.entries() {
        assert!(run as usize > s.spec.base && s.spec.unit(run).is_some());
    }
    let again = Arc::new(s.compile_jit().unwrap());
    assert!(s.install_jit(Some(again)));
    let mut want = Sim::with_options(Arc::clone(&p), SimOptions::baseline());
    for _ in 0..s.tick_count() {
        want.tick();
    }
    same_batches(&mut want, &mut s);
    // Interpreted again: the units stay but do not run.
    assert!(s.install_jit(None));
    same_batches(&mut want, &mut s);

    let q = program(&microcoded(true), &Options::default());
    let plain = Sim::with_options(Arc::clone(&q), SimOptions::jit());
    let mut auto = Sim::with_options(
        Arc::clone(&q),
        with(SimOptions::jit(), &fast(None, 4, 64, 2)),
    );
    let code = Arc::new(plain.compile_jit().unwrap());
    assert!(code.superstep().is_some());
    assert!(!auto.install_jit(Some(code)));
}

/// Thousands of build/evict cycles: the runs, the units and their code
/// bytes stay bounded by the cap (units are dropped on eviction; P5a
/// checks a dropped unit frees its memory).
#[test]
fn many_cycles_stay_bounded() {
    let (p, plan) = counting(0xff, &[]);
    for o in jit_options() {
        let spec = fast(Some(plan.clone()), 3, 4, 1);
        let mut s = Sim::with_options(Arc::clone(&p), with(o.clone(), &spec));
        let mut most = 0;
        for round in 0..1500u64 {
            s.set_input(0, round);
            s.run(3);
            let st = s.spec_stats();
            most = most.max(st.code_bytes);
            let live_units = s
                .spec
                .dynamic
                .iter()
                .filter(|d| d.as_ref().is_some_and(|d| d.unit.is_some()))
                .count();
            assert!(live_units <= 3 && s.runs.len() <= s.spec.base + 4);
        }
        let st = s.spec_stats();
        assert!(st.installed > 1000 && st.evicted > 990, "{st:?}");
        if s.jit_active() {
            assert!(most > 0 && most < 3 * (64 << 10), "{most} bytes");
        }
    }
}

/// Deliberate bugs in the adaptive tables (`MUTATION`) are caught by
/// `check_batches`: a body installed under the wrong key, and a body
/// evicted while its table entry still points at its run (which the next
/// install reuses for another key).
#[test]
fn adaptive_mutations_are_caught() {
    let _reset = NoMutation;
    let cases: Vec<(Arc<Program>, SpecOptions)> = [false, true]
        .into_iter()
        .map(|clocked| {
            let p = program(&microcoded(clocked), &Options::default());
            let plan = layouts(&microcoded_plan(&p, None));
            (p, fast(Some(plan), 3, 4, 1))
        })
        .chain(std::iter::once({
            let (p, plan) = counting(0x3f, &[]);
            (p, fast(Some(plan), 3, 4, 1))
        }))
        .collect();
    for (p, spec) in &cases {
        check_batches_with(p, 1, Some(spec)).unwrap();
    }
    for m in 3..=4 {
        MUTATION.set(m);
        let caught = cases
            .iter()
            .any(|(p, spec)| check_batches_with(p, 1, Some(spec)).is_err());
        MUTATION.set(0);
        assert!(caught, "mutation {m} is not caught");
    }
}

/// Run-time installs never push a run index past `u16::MAX`: with the
/// runs filled up to the last index, one more body installs and the next
/// is refused (not a panic), and the results stay exact.
#[test]
fn installs_at_the_run_index_cap_are_refused() {
    use crate::compile::Body;
    use crate::sim::Run;
    let (p, plan) = counting(0xff, &[]);
    let spec = SpecOptions {
        inline_build: false,
        max_bodies: usize::MAX,
        ..fast(Some(plan), 1 << 20, 1 << 20, 1)
    };
    let mut s = Sim::with_options(Arc::clone(&p), with(SimOptions::default(), &spec));
    // Fill every index but the last free one with placeholder runs (as
    // if that many bodies had been installed).
    while s.runs.len() < usize::from(u16::MAX) - 2 {
        s.runs.push(Run::new(&p, Some(Body::default()), false));
        s.spec.dynamic.push(None);
    }
    let mut want = Sim::with_options(Arc::clone(&p), SimOptions::baseline());
    let (mut ok, mut refused) = (0, 0);
    for round in 0..8 {
        s.run(3);
        for _ in 0..3 {
            want.tick();
        }
        let r = s.spec_requests();
        for r in &r {
            if s.install_spec(build_spec(&p, r)) {
                ok += 1;
            } else {
                refused += 1;
            }
        }
        assert!(s.runs.len() < usize::from(u16::MAX), "round {round}");
        assert!(observe(&want) == observe(&s), "round {round}");
    }
    assert!(ok == 1 && refused > 0, "{ok} installed, {refused} refused");
    same_batches(&mut want, &mut s);
}

/// The microcoded machine's program with its ROM contents changed (every
/// word `^ 3`): the same slots and memories, other folds.
fn other_rom(p: &Program) -> Arc<Program> {
    let mut q = p.clone();
    let m = q.mems.iter_mut().find(|m| !m.writable).expect("a ROM");
    m.init = m.init.iter().map(|w| w ^ 3).collect::<Vec<_>>().into();
    Arc::new(q)
}

/// Bodies built (and checked, and compiled) against another program than
/// the request's are refused: here the same design with other ROM
/// contents, whose folds would give wrong results.
#[test]
fn builds_against_another_program_are_refused() {
    let p = program(&microcoded(false), &Options::default());
    let q = other_rom(&p);
    let plan = layouts(&microcoded_plan(&p, None));
    let spec = SpecOptions {
        inline_build: false,
        ..fast(Some(plan), 16, 1 << 10, 2)
    };
    for o in jit_options() {
        let mut s = Sim::with_options(Arc::clone(&p), with(o, &spec));
        let mut want = Sim::with_options(Arc::clone(&p), SimOptions::baseline());
        for round in 0..20 {
            s.run(20);
            for _ in 0..20 {
                want.tick();
            }
            for r in s.spec_requests() {
                assert!(!s.install_spec(build_spec(&q, &r)), "round {round}");
            }
            assert!(observe(&want) == observe(&s), "round {round}");
        }
        let st = s.spec_stats();
        assert!(st.bodies == 0 && st.requested > 0, "{st:?}");
    }
}

/// A hot key is requested once (not again every window while it waits),
/// so every request installs.
#[test]
fn hot_keys_are_requested_once() {
    let p = program(&microcoded(false), &Options::default());
    let plan = layouts(&microcoded_plan(&p, None));
    let spec = SpecOptions {
        inline_build: false,
        ..fast(Some(plan), 16, 64, 2)
    };
    let mut s = Sim::with_options(Arc::clone(&p), with(SimOptions::default(), &spec));
    s.run(2000);
    let r = s.spec_requests();
    let mut keys: Vec<(usize, u64)> = r.iter().filter_map(SpecRequest::body).collect();
    let n = keys.len();
    keys.sort_unstable();
    keys.dedup();
    assert_eq!(n, keys.len(), "duplicate requests: {r:?}");
    for r in &r {
        assert!(s.install_spec(build_spec(&p, r)));
    }
    // Requested and installed: never again.
    s.run(2000);
    assert!(s.spec_requests().is_empty());
    let st = s.spec_stats();
    assert!(st.dropped == 0 && st.installed == n as u64, "{st:?}");
}

/// An automatic plan's request the caller drops (never builds) is asked
/// again a `plan_window` later; built and installed, it keys the machine.
#[test]
fn a_dropped_plan_request_is_asked_again() {
    let p = program(&microcoded(true), &Options::default());
    let spec = SpecOptions {
        inline_build: false,
        ..fast(None, 8, 64, 2)
    };
    let mut s = Sim::with_options(Arc::clone(&p), with(SimOptions::default(), &spec));
    let plans = |s: &mut Sim| {
        s.spec_requests()
            .into_iter()
            .filter(|r| r.body().is_none())
            .collect::<Vec<_>>()
    };
    let mut asked = 0;
    while plans(&mut s).is_empty() {
        s.run(50);
        asked += 1;
        assert!(asked < 100, "no plan request");
    }
    // Dropped. Not asked again at once, but a plan window later.
    s.run(10);
    assert!(plans(&mut s).is_empty(), "asked again at once");
    let mut again = Vec::new();
    for _ in 0..20 {
        s.run(50);
        again = plans(&mut s);
        if !again.is_empty() {
            break;
        }
    }
    assert_eq!(again.len(), 1);
    assert!(plans(&mut s).is_empty(), "asked again at once");
    assert!(s.install_spec(build_spec(&p, &again[0])));
    let st = s.spec_stats();
    assert!(!st.awaiting_plan && st.keyed_phases > 0, "{st:?}");
}

/// A plan that does not fit under the run-index cap is refused whole:
/// no pinned run of it is left behind.
#[test]
fn a_plan_past_the_run_cap_leaves_no_runs_behind() {
    use crate::compile::Body;
    use crate::sim::Run;
    let p = program(&microcoded(true), &Options::default());
    let spec = SpecOptions {
        inline_build: false,
        ..fast(None, 8, 64, 2)
    };
    let mut s = Sim::with_options(Arc::clone(&p), with(SimOptions::default(), &spec));
    let mut plan = None;
    while plan.is_none() {
        s.run(50);
        plan = s.spec_requests().into_iter().find(|r| r.body().is_none());
    }
    let built = build_spec(&p, &plan.unwrap());
    let runs_needed: usize = match &built.out {
        super::Out::Plan(ph, _) => ph.iter().flatten().filter(|x| x.prefix.is_some()).count() * 2,
        super::Out::Body { .. } => unreachable!(),
    };
    assert!(runs_needed >= 2, "the plan has a prefix");
    // One free index: the plan needs at least two.
    while s.runs.len() < usize::from(u16::MAX) - 2 {
        s.runs.push(Run::new(&p, Some(Body::default()), false));
        s.spec.dynamic.push(None);
    }
    let runs = s.runs.len();
    assert!(!s.install_spec(built));
    assert_eq!(s.runs.len(), runs);
    assert!(s.spec.dynamic.iter().all(Option::is_none));
    assert_eq!(s.spec.pinned_bytes, 0);
    assert!(s.spec_stats().awaiting_plan);
    let mut want = Sim::with_options(Arc::clone(&p), SimOptions::baseline());
    for _ in 0..s.tick_count() {
        want.tick();
    }
    same_batches(&mut want, &mut s);
}

/// With the JIT, a body larger than the code budget is refused before
/// anything is evicted for it (the key cycles through 32 values, so cold
/// bodies are there to evict).
#[cfg(feature = "jit")]
#[test]
fn a_body_that_cannot_fit_evicts_nothing() {
    let (p, plan) = counting(0xf0, &[]);
    let spec = fast(Some(plan), 64, 8, 1);
    let mut s = Sim::with_options(Arc::clone(&p), with(SimOptions::jit(), &spec));
    for _ in 0..40 {
        s.run(5);
    }
    let st = s.spec_stats();
    assert!(st.bodies > 2, "{st:?}");
    // Now every body is over budget.
    s.options.spec.as_mut().unwrap().code_budget = 1;
    for _ in 0..40 {
        s.run(5);
    }
    let after = s.spec_stats();
    assert!(
        after.evicted == st.evicted && after.bodies == st.bodies && after.dropped > st.dropped,
        "{st:?} then {after:?}"
    );
}

/// A body that fails its check (mutation 5: `finish` refuses every body)
/// is refused, and its key is wanted again (not left pending).
#[test]
fn a_body_that_fails_its_check_is_wanted_again() {
    let _reset = NoMutation;
    let p = program(&microcoded(false), &Options::default());
    let plan = layouts(&microcoded_plan(&p, None));
    let spec = SpecOptions {
        inline_build: false,
        ..fast(Some(plan), 16, 1 << 10, 2)
    };
    let mut s = Sim::with_options(Arc::clone(&p), with(SimOptions::default(), &spec));
    s.run(40);
    let r = s.spec_requests();
    assert!(!r.is_empty());
    MUTATION.set(5);
    let built: Vec<_> = r.iter().map(|r| build_spec(&p, r)).collect();
    MUTATION.set(0);
    for b in built {
        assert!(!s.install_spec(b));
    }
    s.run(40);
    let again = s.spec_requests();
    assert_eq!(again.len(), r.len(), "{again:?}");
}

/// Options whose automatic plan keys no phase (no key bit allowed),
/// sampling a short window.
fn keying_nothing() -> SpecOptions {
    SpecOptions {
        max_key_bits: 0,
        ..fast(None, 8, 16, 2)
    }
}

/// An automatic plan that keys no phase brings the superstep the layout's
/// native code lacks (compiled while the plan was awaited), built with
/// the plan (P8 (c)): `run` uses it while native code is installed, not
/// while interpreting, and a ROM write and the `reset` after it keep it
/// with the layout. Every batch boundary equals single ticks.
#[test]
fn a_plan_that_keys_nothing_brings_the_superstep() {
    let p = program(&microcoded(true), &Options::default());
    let spec = keying_nothing();
    for o in jit_options() {
        let mut s = Sim::with_options(Arc::clone(&p), with(o.clone(), &spec));
        let mut want = Sim::with_options(Arc::clone(&p), SimOptions::baseline());
        while s.spec_stats().awaiting_plan {
            assert!(s.spec.superstep.is_none());
            s.run(10);
            for _ in 0..10 {
                want.tick();
            }
            assert!(observe(&want) == observe(&s));
            assert!(s.tick_count() < 5000, "no plan");
        }
        let st = s.spec_stats();
        assert!(st.keyed_phases == 0 && st.installed == 1, "{st:?}");
        assert!(!s.spec.hooked);
        #[cfg(feature = "jit")]
        if s.jit_active() {
            assert!(s.jit_code().unwrap().superstep().is_none());
            let sup = s.spec.superstep.clone().expect("a superstep");
            assert_eq!(sup.code.info().ticks, 2);
            assert_eq!(s.super_fn().map(|f| f.1), Some(2));
            assert!(st.code_bytes >= sup.code.info().code_bytes && st.code_bytes > 0);
            same_batches(&mut want, &mut s);
            // Interpreted: the superstep stays but does not run.
            assert!(s.install_jit(None));
            assert!(s.super_fn().is_none());
            same_batches(&mut want, &mut s);
            let code = Arc::new(s.compile_jit().unwrap());
            assert!(s.install_jit(Some(code)));
            // A ROM write sets it aside with the layout (the unfolded
            // layout's native code has a superstep of its own); `reset`
            // brings it back.
            let rom = (0..p.mems.len() as u32)
                .find(|&m| !p.mems[m as usize].writable)
                .unwrap();
            let w = s.mem(rom)[1];
            s.set_mem_word(rom, 1, w ^ 1);
            want.set_mem_word(rom, 1, w ^ 1);
            assert!(s.spec.superstep.is_none() && s.super_fn().is_some());
            same_batches(&mut want, &mut s);
            s.reset();
            want.reset();
            assert!(s.spec.superstep.is_some() && s.super_fn().is_some());
            same_batches(&mut want, &mut s);
            continue;
        }
        assert!(s.spec.superstep.is_none());
        same_batches(&mut want, &mut s);
    }
    // Every runtime variant (supersteps of several periods, small chunks),
    // with the plan arriving mid-run; pokes, imports and resets.
    for seed in 1..=3 {
        check_batches_with(&p, seed, Some(&spec)).unwrap();
    }
    check_pair(
        &p,
        400,
        5,
        &SimOptions::baseline(),
        &with(SimOptions::jit(), &spec),
    )
    .unwrap();
    // Random designs: whatever the plan, exact.
    for (c, cfg) in standard_configs().iter().enumerate() {
        let g = generate(&GenConfig {
            seed: c as u64 * 131 + 7,
            ..cfg.clone()
        });
        for options in [Options::default(), Options::debug()] {
            let q = program(&g.design, &options);
            check_batches_with(&q, c as u64, Some(&spec))
                .unwrap_or_else(|e| panic!("config {c}: {e}"));
        }
    }
}

/// The superstep a plan brings is the one the mutations of
/// `jit::superstep` break: with each, `check_batches` fails on a program
/// whose only superstep is the plan's (so that superstep runs, and its
/// bugs are caught).
#[cfg(feature = "jit")]
#[test]
fn the_plans_superstep_is_checked() {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            crate::sim::jit::MUTATION.set(0);
        }
    }
    let _reset = Reset;
    let spec = keying_nothing();
    let cases: Vec<Arc<Program>> = [false, true]
        .into_iter()
        .map(|clocked| program(&microcoded(clocked), &Options::default()))
        .collect();
    for p in &cases {
        let mut s = Sim::with_options(Arc::clone(p), with(SimOptions::jit(), &spec));
        while s.spec_stats().awaiting_plan {
            s.run(10);
        }
        assert!(s.jit_code().unwrap().superstep().is_none() && s.super_fn().is_some());
        check_batches_with(p, 1, Some(&spec)).unwrap();
    }
    for m in 1..=3 {
        crate::sim::jit::MUTATION.set(m);
        let caught = cases
            .iter()
            .any(|p| check_batches_with(p, 1, Some(&spec)).is_err());
        crate::sim::jit::MUTATION.set(0);
        assert!(caught, "superstep mutation {m} is not caught");
    }
}

/// Work counters (feature `counters`) of the plan's superstep are those of
/// the program's own: a `Sim` whose plan keys nothing counts what a `Sim`
/// without specialization counts, batch for batch.
#[cfg(all(feature = "counters", feature = "jit"))]
#[test]
fn the_plans_superstep_counts_like_the_programs() {
    for clocked in [false, true] {
        let p = program(&microcoded(clocked), &Options::default());
        let mut s = Sim::with_options(Arc::clone(&p), with(SimOptions::jit(), &keying_nothing()));
        while s.spec_stats().awaiting_plan {
            s.run(10);
        }
        let mut plain = Sim::with_options(Arc::clone(&p), SimOptions::jit());
        plain.run(s.tick_count());
        assert!(s.super_fn().is_some() && plain.jit_code().unwrap().superstep().is_some());
        s.reset_counters();
        plain.reset_counters();
        for n in [1, 9, 2, 30, 7, 64] {
            s.run(n);
            plain.run(n);
            assert_eq!(s.counters(), plain.counters(), "run({n})");
        }
        assert!(observe(&s) == observe(&plain));
    }
}
