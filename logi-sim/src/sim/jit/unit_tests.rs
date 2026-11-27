//! Units (`compile_unit`) against the interpreter and `JitProgram`.

use std::sync::Arc;

use super::{JitProgram, JitUnit, compile_unit};
use crate::compile::{Options, Program, compile};
use crate::netlist::{Design, mask};
use crate::sim::{Sim, SimOptions};
use crate::testkit::bitblast::bitblast;
use crate::testkit::diff::standard_configs;
use crate::testkit::{GenConfig, Rng, fixtures, generate};

fn program(d: &Design, options: &Options) -> Arc<Program> {
    Arc::new(compile(d, options).unwrap_or_else(|e| panic!("{e:?}")))
}

/// One unit per used body (`None` for the others).
fn units(sim: &Sim) -> Vec<Option<JitUnit>> {
    sim.body_refs()
        .into_iter()
        .map(|b| {
            if b.used {
                Some(compile_unit(&sim.program, b).unwrap_or_else(|e| panic!("{}", e.0)))
            } else {
                None
            }
        })
        .collect()
}

/// A `Sim`'s mutable state, driven by units instead of the interpreter.
struct Mirror {
    slots: Vec<u64>,
    mems: Vec<Vec<u64>>,
    writes: Vec<u64>,
    tick: u64,
    last: u16,
}

impl Mirror {
    fn of(sim: &Sim) -> Self {
        Mirror {
            slots: sim.slots.clone(),
            mems: sim.mems.clone(),
            writes: sim.mem_writes.clone(),
            tick: sim.tick,
            last: sim.last,
        }
    }

    /// One tick as `Sim::tick`/`fused_tick`: the last settled body's
    /// commit, the inputs, then the settle of `run`. `false`: the commit
    /// is the interpreter's (a `MemClear`), nothing done.
    fn step(&mut self, p: &Program, units: &[Option<JitUnit>], inputs: &[u64], run: u16) -> bool {
        let unit = |k: u16| units[k as usize].as_ref().expect("a used body");
        if !unit(self.last).commit(&mut self.slots, &mut self.mems, &mut self.writes) {
            return false;
        }
        self.tick += 1;
        for (port, &v) in p.ports.inputs.iter().zip(inputs) {
            self.slots[port.slot as usize] = v;
        }
        unit(run).settle(&mut self.slots, &mut self.mems, self.tick);
        self.last = run;
        true
    }

    fn same(&self, sim: &Sim) -> Result<(), String> {
        if self.tick != sim.tick || self.last != sim.last {
            return Err(format!("tick {} vs {}", self.tick, sim.tick));
        }
        if self.slots != sim.slots {
            let k = (0..self.slots.len())
                .find(|&k| self.slots[k] != sim.slots[k])
                .unwrap();
            return Err(format!(
                "slot {k}: unit {:#x}, interpreter {:#x} (tick {})",
                self.slots[k], sim.slots[k], sim.tick
            ));
        }
        if self.mems != sim.mems {
            return Err(format!("memories differ (tick {})", sim.tick));
        }
        if self.writes != sim.mem_writes {
            return Err(format!("memory versions differ (tick {})", sim.tick));
        }
        Ok(())
    }
}

/// Drive the interpreter and the units side by side, single ticks and
/// `run` batches (so the fused bodies run too), and compare every slot
/// and memory after each. `Ok(false)`: a commit the unit leaves to the
/// interpreter ended the comparison early.
fn check_units(p: &Arc<Program>, ticks: u64, seed: u64, o: &SimOptions) -> Result<bool, String> {
    let mut sim = Sim::with_options(
        Arc::clone(p),
        SimOptions {
            jit: false,
            ..o.clone()
        },
    );
    let us = units(&sim);
    let mut m = Mirror::of(&sim);
    m.same(&sim)?;
    let mut rng = Rng::new(seed);
    let mut inputs = vec![0; p.ports.inputs.len()];
    let mut t = 0;
    while t < ticks {
        for (port, q) in p.ports.inputs.iter().enumerate() {
            inputs[port] = rng.next_u64() & mask(q.width);
            sim.set_input(port, inputs[port]);
        }
        let n = if rng.below(3) == 0 {
            1
        } else {
            2 + rng.below(9) as u64
        };
        let at = |sim: &Sim, k: u64| ((sim.tick + k) % sim.period) as usize;
        let mut ok = true;
        if n == 1 {
            let run = sim.full[at(&sim, 1)];
            ok = m.step(p, &us, &inputs, run);
            sim.tick();
        } else {
            for i in 1..n {
                let run = sim.fused[at(&sim, i)];
                ok = ok && m.step(p, &us, &inputs, run);
            }
            let run = sim.full[at(&sim, n)];
            ok = ok && m.step(p, &us, &inputs, run);
            sim.run(n);
        }
        if !ok {
            return Ok(false);
        }
        m.same(&sim)?;
        t += n;
    }
    Ok(true)
}

fn designs() -> Vec<(String, Design)> {
    use fixtures::*;
    let mut out: Vec<(String, Design)> = [
        adder(8),
        ripple_adder(64),
        nested_adder16(),
        mux_tree(3, 8),
        counter(8),
        counter(64),
        counter_bitblasted(8),
        register_file(3, 16),
        register_file_ram2(4, 64, &[u64::MAX, 1]),
        fifo(4, 12),
        sr_latch_nor_delayed(),
        cpu(&MULTIPLY_PROGRAM),
        cpu(&LOOP_PROGRAM),
        bitblast(&cpu(&MULTIPLY_PROGRAM)),
        bitblast(&register_file(3, 16)),
        bitblast(&fifo(3, 8)),
    ]
    .into_iter()
    .enumerate()
    .map(|(i, d)| (format!("fixture {i} {}", d.root_def().name), d))
    .collect();
    for (c, cfg) in standard_configs().into_iter().enumerate() {
        for seed in 0..6 {
            let g = generate(&GenConfig {
                seed: seed * 7919 + c as u64,
                ..cfg.clone()
            });
            out.push((format!("random config {c} seed {seed}"), g.design));
        }
    }
    out
}

/// Every body of fixtures and random designs, as a unit, equals the
/// interpreter: guards on and off, with phase variants and fused runs.
#[cfg_attr(test, test)]
fn units_match_the_interpreter() {
    let mut full = 0;
    let mut total = 0;
    for (name, d) in designs() {
        let p = program(&d, &Options::default());
        for (i, o) in [
            SimOptions::default(),
            SimOptions {
                guard_min: 1,
                ..SimOptions::default()
            },
            SimOptions {
                guard: false,
                ..SimOptions::default()
            },
            SimOptions {
                fuse: false,
                ..SimOptions::default()
            },
        ]
        .iter()
        .enumerate()
        {
            total += 1;
            full += check_units(&p, 150, i as u64, o)
                .unwrap_or_else(|e| panic!("{name} options {i}: {e}")) as usize;
        }
    }
    assert!(total > 100);
    // Screens with clears leave the commit to the interpreter; the rest
    // are checked end to end.
    assert!(full * 10 >= total * 7, "{full} of {total}");
}

/// Unsimplified wide designs, and a body longer than one chunk.
#[cfg_attr(test, test)]
fn units_of_big_and_wide_bodies_match() {
    for seed in 0..8 {
        let g = generate(&GenConfig {
            seed,
            max_width: 64,
            nodes: 200,
            state_percent: 25,
            ..GenConfig::default()
        });
        let p = program(&g.design, &Options::unoptimized());
        check_units(&p, 80, seed, &SimOptions::default()).unwrap_or_else(|e| panic!("{seed}: {e}"));
    }
    let d = generate(&GenConfig {
        seed: 5,
        nodes: 40 * super::CHUNK,
        state_percent: 20,
        ..GenConfig::default()
    })
    .design;
    let p = program(&d, &Options::unoptimized());
    assert!(p.commits.len() > super::COMMIT_CHUNK);
    check_units(&p, 40, 5, &SimOptions::default()).unwrap();
}

/// A unit and the `JitProgram` body it was cut from compute the same
/// slots, memories and write counts from the same state.
#[cfg_attr(test, test)]
fn a_unit_equals_its_body_in_a_program() {
    for (name, d) in designs() {
        let p = program(&d, &Options::default());
        let mut sim = Sim::with_options(
            Arc::clone(&p),
            SimOptions {
                jit: false,
                guard_min: 1,
                ..SimOptions::default()
            },
        );
        let prog: JitProgram = sim.compile_jit().unwrap();
        let us = units(&sim);
        let mut rng = Rng::new(7);
        for round in 0..20 {
            for (port, q) in p.ports.inputs.iter().enumerate() {
                sim.set_input(port, rng.next_u64() & mask(q.width));
            }
            sim.run(1 + rng.below(6) as u64);
            for (k, u) in us.iter().enumerate() {
                let (Some(u), Some(body)) = (u, prog.body(k)) else {
                    continue;
                };
                assert_eq!(u.settle_chunks(), body.settle_chunks(), "{name}");
                assert_eq!(u.commit_chunks(), body.commit_chunks(), "{name}");
                let (mut a, mut b) = (Mirror::of(&sim), Mirror::of(&sim));
                u.settle(&mut a.slots, &mut a.mems, a.tick);
                let ptrs: Vec<*mut u64> = b.mems.iter_mut().map(|m| m.as_mut_ptr()).collect();
                for f in &body.settle {
                    // SAFETY: the buffers are `sim`'s sizes, the body passed `check`.
                    unsafe { f(b.slots.as_mut_ptr(), ptrs.as_ptr(), b.tick) };
                }
                assert_eq!(a.slots, b.slots, "{name} body {k} settle, round {round}");
                if let Some(fs) = &body.commit {
                    assert!(u.commit(&mut a.slots, &mut a.mems, &mut a.writes));
                    for f in fs {
                        // SAFETY: as above, with one counter per memory.
                        unsafe { f(b.slots.as_mut_ptr(), ptrs.as_ptr(), b.writes.as_mut_ptr()) };
                    }
                    assert_eq!(a.slots, b.slots, "{name} body {k} commit, round {round}");
                    assert_eq!(a.mems, b.mems, "{name} body {k} memories");
                    assert_eq!(a.writes, b.writes, "{name} body {k} versions");
                } else {
                    assert!(!u.commit(&mut a.slots, &mut a.mems, &mut a.writes));
                }
            }
        }
    }
}

/// A body compiled with a key ([`super::BodyRef::key`], as a prefix is)
/// gets a key entry with [`super::KEY_ENTRY`]: it settles as the body does
/// and returns the key `KeyLayout::pack` packs from the settled slots, on
/// random layouts (gaps, merged segments, the zero slot). A key slot past
/// the slots is refused.
#[cfg_attr(test, test)]
fn a_key_entry_settles_and_packs_the_key() {
    use crate::compile::spec::{KeyField, KeyLayout};
    let mut rng = Rng::new(0x6b6579);
    for (name, d) in designs().into_iter().step_by(3) {
        let p = program(&d, &Options::default());
        let mut sim = Sim::new(Arc::clone(&p));
        for _ in 0..4 {
            let fields: Vec<KeyField> = (0..1 + rng.below(5))
                .map(|_| KeyField {
                    slot: rng.below(p.slot_count as usize) as u32,
                    mask: rng.next_u64() & rng.next_u64() & mask(1 + rng.below(16) as u8),
                })
                .collect();
            let Some(layout) = KeyLayout::try_new(fields) else {
                continue;
            };
            let segs = layout.segments();
            if segs.is_empty() {
                continue;
            }
            let body = super::BodyRef {
                key: &segs,
                used: true,
                ..sim.body_refs().remove(0)
            };
            let u = compile_unit(&p, body).unwrap_or_else(|e| panic!("{name}: {}", e.0));
            assert_eq!(u.body().key.is_some(), super::KEY_ENTRY, "{name}");
            let Some(f) = u.body().key else {
                continue;
            };
            for (port, q) in p.ports.inputs.iter().enumerate() {
                sim.set_input(port, rng.next_u64() & mask(q.width));
            }
            sim.run(1 + rng.below(4) as u64);
            let (mut a, mut b) = (Mirror::of(&sim), Mirror::of(&sim));
            u.settle(&mut a.slots, &mut a.mems, a.tick);
            let ptrs: Vec<*mut u64> = b.mems.iter_mut().map(|m| m.as_mut_ptr()).collect();
            // SAFETY: the buffers are `sim`'s sizes, the body passed `check`.
            let key = unsafe { f(b.slots.as_mut_ptr(), ptrs.as_ptr(), b.tick) };
            assert_eq!(a.slots, b.slots, "{name}: the key entry's settle");
            assert_eq!(key, layout.pack(&b.slots), "{name}: {layout:?}");
        }
        let bad = [crate::compile::spec::KeySeg {
            slot: p.slot_count,
            mask: 1,
            rot: 0,
        }];
        let body = super::BodyRef {
            key: &bad,
            used: true,
            ..sim.body_refs().remove(0)
        };
        assert!(
            compile_unit(&p, body).is_err(),
            "{name}: a key slot past the slots"
        );
    }
}

/// A body that is unused (or too large) is refused, not compiled empty.
#[cfg_attr(test, test)]
fn an_unused_body_is_refused() {
    let p = program(&fixtures::counter(8), &Options::default());
    let sim = Sim::with_options(
        Arc::clone(&p),
        SimOptions {
            jit: false,
            ..SimOptions::default()
        },
    );
    let mut b = sim.body_refs().remove(0);
    b.used = false;
    assert!(compile_unit(&p, b).is_err());
}

/// Resident memory, in bytes.
#[cfg(target_os = "linux")]
fn rss() -> usize {
    let s = std::fs::read_to_string("/proc/self/statm").expect("Linux");
    let pages: usize = s.split_whitespace().nth(1).unwrap().parse().unwrap();
    pages * 4096
}

/// Units are really freed: thousands of compile/drop cycles do not grow
/// the process (a leaked module costs at least a page of code and one of
/// data each).
///
/// The process's resident size is only meaningful when no other test runs
/// beside it, so the parent re-runs this binary on the child test alone.
#[cfg_attr(test, test)]
#[cfg(target_os = "linux")]
fn compile_and_free_cycles_do_not_leak() {
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "sim::jit::unit_tests::leak_child",
            "--test-threads=1",
            "--nocapture",
        ])
        .env("LOGI_LEAK_CHILD", "1")
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success() && text.contains("1 passed"),
        "{text}{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[cfg_attr(test, test)]
#[cfg(target_os = "linux")]
fn leak_child() {
    if std::env::var_os("LOGI_LEAK_CHILD").is_none() {
        return;
    }
    let d = fixtures::cpu(&fixtures::MULTIPLY_PROGRAM);
    let p = program(&d, &Options::default());
    let sim = Sim::with_options(
        Arc::clone(&p),
        SimOptions {
            jit: false,
            ..SimOptions::default()
        },
    );
    let cycle = |sim: &Sim| {
        let u = compile_unit(&sim.program, sim.body_refs().remove(0)).unwrap();
        assert!(u.code_bytes() > 0);
        let mut m = Mirror::of(sim);
        u.settle(&mut m.slots, &mut m.mems, m.tick);
    };
    for _ in 0..200 {
        cycle(&sim);
    }
    let before = rss();
    for _ in 0..2000 {
        cycle(&sim);
    }
    let grown = rss().saturating_sub(before);
    // A leak is >= 8 KiB per cycle, 16 MiB here.
    assert!(grown < 4 << 20, "RSS grew by {} KiB", grown >> 10);
}

/// Numbers for the doc's code budget: native bytes and compile time of
/// each body of a fixture CPU (printed with `--nocapture`).
#[cfg_attr(test, test)]
#[cfg(not(target_arch = "wasm32"))] // (no clock on wasm32-unknown-unknown)
fn unit_sizes_are_reported() {
    let d = fixtures::cpu(&fixtures::MULTIPLY_PROGRAM);
    let p = program(&d, &Options::default());
    let sim = Sim::new(Arc::clone(&p));
    for (k, b) in sim.body_refs().into_iter().enumerate() {
        if !b.used {
            continue;
        }
        let t = std::time::Instant::now();
        let u = compile_unit(&p, b).unwrap();
        println!(
            "body {k}: {} instrs, {} bytes, {:?}",
            u.instrs(),
            u.code_bytes(),
            t.elapsed()
        );
        assert!(u.code_bytes() >= u.instrs());
    }
}

/// `Sim::compile_unit_of` compiles every used body, and only those.
#[cfg_attr(test, test)]
fn compile_unit_of_covers_the_used_bodies() {
    let d = fixtures::cpu(&fixtures::LOOP_PROGRAM);
    let p = program(&d, &Options::default());
    let sim = Sim::new(Arc::clone(&p));
    let refs = sim.body_refs();
    assert_eq!(refs.len(), sim.body_count());
    for (k, b) in refs.iter().enumerate() {
        match sim.compile_unit_of(k) {
            Some(u) => assert!(b.used && u.is_ok()),
            None => assert!(!b.used),
        }
    }
    assert!(sim.compile_unit_of(sim.body_count()).is_none());
}

/// `compile_unit` is public and takes any body: one whose indices are out
/// of range for the program (a write to slot 10^9, a commit past the
/// slots, a memory that does not exist) is refused, as `Sim` refuses it,
/// so that the safe `settle`/`commit` never touch memory out of bounds.
#[cfg_attr(test, test)]
fn compile_unit_refuses_invalid_bodies() {
    let d = fixtures::cpu(&fixtures::MULTIPLY_PROGRAM);
    let p = program(&d, &Options::default());
    let sim = Sim::new(Arc::clone(&p));
    let b = sim.body_refs().remove(0);
    assert!(compile_unit(&p, sim.body_refs().remove(0)).is_ok());
    let mut instrs = b.instrs.to_vec();
    instrs[0].dst = 1_000_000_000;
    let bad = super::BodyRef {
        instrs: &instrs,
        ..sim.body_refs().remove(0)
    };
    assert!(compile_unit(&p, bad).is_err(), "a write past the slots");
    let mut commits = b.commits.to_vec();
    if let Some(crate::compile::Commit::Copy { state, .. }) = commits.first_mut() {
        *state = 1_000_000_000;
        let bad = super::BodyRef {
            commits: &commits,
            ..sim.body_refs().remove(0)
        };
        assert!(compile_unit(&p, bad).is_err(), "a commit past the slots");
    }
}

/// The tests of this module, for a runner without `cargo test` (wasm32,
/// feature `selftest`; see `examples/wasm_selftest.rs`).
#[cfg(any(test, feature = "selftest"))]
pub fn all() -> Vec<(&'static str, fn())> {
    #[allow(unused_mut)]
    let mut v: Vec<(&'static str, fn())> = vec![
        ("units_match_the_interpreter", units_match_the_interpreter),
        (
            "units_of_big_and_wide_bodies_match",
            units_of_big_and_wide_bodies_match,
        ),
        (
            "a_unit_equals_its_body_in_a_program",
            a_unit_equals_its_body_in_a_program,
        ),
        ("an_unused_body_is_refused", an_unused_body_is_refused),
        (
            "a_key_entry_settles_and_packs_the_key",
            a_key_entry_settles_and_packs_the_key,
        ),
        (
            "compile_unit_of_covers_the_used_bodies",
            compile_unit_of_covers_the_used_bodies,
        ),
        (
            "compile_unit_refuses_invalid_bodies",
            compile_unit_refuses_invalid_bodies,
        ),
    ];
    #[cfg(target_os = "linux")]
    v.push((
        "compile_and_free_cycles_do_not_leak",
        compile_and_free_cycles_do_not_leak,
    ));
    #[cfg(target_os = "linux")]
    v.push(("leak_child", leak_child));
    #[cfg(not(target_arch = "wasm32"))]
    v.push(("unit_sizes_are_reported", unit_sizes_are_reported));
    v
}

/// Every test of this file is in [`all`].
#[test]
fn all_lists_every_test() {
    crate::testkit::selftest::check_list(include_str!("unit_tests.rs"), &all());
}
