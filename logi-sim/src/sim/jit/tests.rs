//! The JIT backend against the interpreter (`testkit::diff::check_jit`):
//! bit-exact on every observable value, every tick.

use std::sync::Arc;

use super::{BodyRef, CHUNK, COMMIT_CHUNK, JitProgram};
use crate::compile::{Options, PackMode, compile};
use crate::netlist::Design;
use crate::sim::{Sim, SimOptions};
use crate::testkit::bitblast::bitblast;
use crate::testkit::diff::{check_batches, check_jit, check_jit_design, standard_configs};
use crate::testkit::{GenConfig, fixtures, generate};

fn program(d: &Design, options: &Options) -> Arc<Program> {
    Arc::new(compile(d, options).unwrap_or_else(|e| panic!("{e:?}")))
}

use crate::compile::Program;

#[test]
fn every_fixture_matches_the_interpreter() {
    use fixtures::*;
    for (i, d) in [
        adder(8),
        ripple_adder(8),
        adder(64),
        ripple_adder(64),
        nested_adder16(),
        mux_tree(3, 8),
        mux_tree_bitblasted(3, 8),
        counter(8),
        counter_bitblasted(8),
        counter(64),
        counter_bitblasted(64),
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
    .iter()
    .enumerate()
    {
        check_jit_design(d, 300, i as u64).unwrap_or_else(|e| panic!("{}: {e}", d.root_def().name));
    }
}

/// Random designs of every shape the differential sweep uses (wide words
/// up to 64 bits, state, nested instances, screens with clears), every
/// combinational primitive.
#[test]
fn random_designs_match_the_interpreter() {
    let mut n = 0;
    for (c, cfg) in standard_configs().into_iter().enumerate() {
        for seed in 0..12 {
            let g = generate(&GenConfig {
                seed: seed * 7919 + c as u64,
                ..cfg.clone()
            });
            check_jit_design(&g.design, 120, seed)
                .unwrap_or_else(|e| panic!("config {c} seed {seed}: {e}"));
            n += 1;
        }
    }
    assert!(n >= 80);
}

/// Wide random words with lots of state, without simplification: every
/// instruction the generator draws reaches the JIT, at widths 1..=64.
#[test]
fn unsimplified_wide_designs_match_the_interpreter() {
    for seed in 0..24 {
        let g = generate(&GenConfig {
            seed,
            max_width: 64,
            nodes: 200,
            state_percent: 25,
            ..GenConfig::default()
        });
        let p = program(&g.design, &Options::unoptimized());
        check_jit(&p, 150, seed, &SimOptions::default())
            .unwrap_or_else(|e| panic!("seed {seed}: {e}"));
        check_batches(&p, seed).unwrap_or_else(|e| panic!("seed {seed}: {e}"));
    }
}

/// Lane-packed programs (`Pack` words with base, splat, gathers and
/// scatters).
#[test]
fn packed_programs_match_the_interpreter() {
    for seed in 0..10 {
        let d = generate(&GenConfig {
            seed,
            nodes: 600,
            state_percent: 10,
            ..GenConfig::gates(seed, 600, 1)
        })
        .design;
        for pack in [PackMode::All, PackMode::Auto] {
            let options = Options {
                pack,
                ..Options::unoptimized()
            };
            let p = program(&d, &options);
            check_jit(&p, 80, seed, &SimOptions::default())
                .unwrap_or_else(|e| panic!("seed {seed} {pack:?}: {e}"));
        }
    }
    // The bit-blasted CPU, packed.
    let d = bitblast(&fixtures::cpu(&fixtures::MULTIPLY_PROGRAM));
    let p = program(
        &d,
        &Options {
            pack: PackMode::All,
            ..Options::default()
        },
    );
    assert!(!p.packs.is_empty());
    check_jit(&p, 400, 1, &SimOptions::default()).unwrap();
}

/// A body longer than one chunk runs as several functions, with values
/// crossing chunk boundaries through the slots (and, in fused-run bodies,
/// stored because a later chunk reads them).
#[test]
fn bodies_longer_than_a_chunk_match_the_interpreter() {
    let d = generate(&GenConfig::dag(3, 40 * CHUNK, 16)).design;
    let p = program(&d, &Options::unoptimized());
    assert!(p.instrs.len() > 2 * CHUNK);
    check_jit(&p, 40, 3, &SimOptions::default()).unwrap();
    let d = generate(&GenConfig {
        seed: 5,
        nodes: 40 * CHUNK,
        state_percent: 20,
        ..GenConfig::default()
    })
    .design;
    let p = program(&d, &Options::unoptimized());
    assert!(p.commits.len() > COMMIT_CHUNK);
    check_jit(&p, 60, 5, &SimOptions::default()).unwrap();
    check_batches(&p, 5).unwrap();
}

/// Every runtime switch the JIT combines with: streaming off, fusion off,
/// phase variants off; and threads, where the JIT stays off and the pool
/// runs.
#[test]
fn runtime_options_match_the_interpreter() {
    let d = fixtures::cpu(&fixtures::MULTIPLY_PROGRAM);
    let p = program(&d, &Options::default());
    for o in [
        SimOptions::baseline(),
        SimOptions {
            fuse: false,
            ..SimOptions::default()
        },
        SimOptions {
            phases: false,
            ..SimOptions::default()
        },
    ] {
        check_jit(&p, 300, 2, &o).unwrap();
    }
    let big = generate(&GenConfig::dag(4, 20_000, 8)).design;
    let options = Options::unoptimized().for_threads(2);
    let p = program(&big, &options);
    let o = SimOptions {
        par_min: 64,
        ..SimOptions::threaded(2)
    };
    let s = Sim::with_options(
        Arc::clone(&p),
        SimOptions {
            jit: true,
            ..o.clone()
        },
    );
    if s.threads() > 1 {
        assert!(!s.jit_active(), "a pool runs the interpreter");
    }
    check_jit(&p, 10, 4, &o).unwrap();
}

/// The JIT can be compiled off the `Sim` and installed later (tiering):
/// results are the same before, during and after.
#[test]
fn compiled_code_can_be_installed_and_removed() {
    let d = fixtures::cpu(&fixtures::LOOP_PROGRAM);
    let p = program(&d, &Options::default());
    let mut want = Sim::new(Arc::clone(&p));
    let mut got = Sim::new(Arc::clone(&p));
    let code = Arc::new(got.compile_jit().unwrap());
    for t in 0..600 {
        match t {
            100 => assert!(got.install_jit(Some(Arc::clone(&code)))),
            400 => assert!(got.install_jit(None)),
            500 => assert!(got.install_jit(Some(Arc::clone(&code)))),
            _ => {}
        }
        assert_eq!(got.jit_active(), (100..400).contains(&t) || t >= 500);
        if t % 7 == 0 {
            want.run(5);
            got.run(5);
        } else {
            want.tick();
            got.tick();
        }
        assert_eq!(want.export_state(), got.export_state(), "tick {t}");
        for k in 0..p.ports.outputs.len() {
            assert_eq!(want.output(k), got.output(k));
        }
    }
    // Code for another program, or another run layout, is refused.
    let other = program(&d, &Options::debug());
    let mut s = Sim::new(Arc::clone(&other));
    assert!(!s.install_jit(Some(Arc::clone(&code))));
    let same = Arc::new((*p).clone());
    let mut s = Sim::new(Arc::clone(&same));
    assert!(
        !s.install_jit(Some(Arc::clone(&code))),
        "an equal program is not the same one"
    );
    assert!(!s.jit_active());
    // Compiling bodies directly: one chunk per `CHUNK` instructions.
    let body = BodyRef {
        blocks: &p.blocks,
        instrs: &p.instrs,
        imms: &p.imms,
        commits: &p.commits,
        guards: &[],
        used: true,
        fused_only: false,
    };
    let j = JitProgram::compile(&p, &[body]).unwrap();
    assert_eq!(j.bodies.len(), 1);
    let b = j.bodies[0].as_ref().unwrap();
    assert_eq!(b.settle.len(), p.instrs.len().div_ceil(CHUNK).max(1));
    assert_eq!(
        b.commit.as_ref().unwrap().len(),
        p.commits.len().div_ceil(COMMIT_CHUNK)
    );
}

/// Interpreter and JIT agree on every root output of the fixture CPU
/// for thousands of ticks, and the CPU computes its result.
#[test]
fn the_fixture_cpu_runs_the_same_for_thousands_of_ticks() {
    let d = fixtures::cpu(&fixtures::LOOP_PROGRAM);
    for options in [Options::default(), Options::debug()] {
        let p = program(&d, &options);
        check_jit(&p, 5000, 9, &SimOptions::default()).unwrap();
    }
    let p = program(
        &fixtures::cpu(&fixtures::MULTIPLY_PROGRAM),
        &Options::default(),
    );
    let mut s = Sim::with_options(p, SimOptions::jit());
    assert!(s.jit_active());
    let mut ticks = 0;
    while s.get("halted") == 0 {
        s.tick();
        ticks += 1;
        assert!(ticks < 500);
    }
    assert_eq!(s.get("acc"), fixtures::MULTIPLY_RESULT);
}

/// Work counters (feature `counters`) count the same with the JIT: the
/// same blocks, instructions, commits and memory writes per settle.
#[cfg(feature = "counters")]
#[test]
fn counters_are_the_same_with_the_jit() {
    for d in [
        fixtures::cpu(&fixtures::MULTIPLY_PROGRAM),
        fixtures::register_file_ram2(4, 64, &[u64::MAX, 1]),
        fixtures::fifo(4, 12),
        // Clock phases and guards inside supersteps.
        clocked(&[3]),
        clocked(&[2, 4]),
        bitblast(&fixtures::cpu(&fixtures::MULTIPLY_PROGRAM)),
    ] {
        let p = program(&d, &Options::default());
        // The interpreter guarded like the JIT (`guard_interpreter`), so
        // both skip the same work.
        let mut want = Sim::with_options(
            Arc::clone(&p),
            SimOptions {
                guard_interpreter: true,
                ..SimOptions::default()
            },
        );
        let mut got = Sim::with_options(Arc::clone(&p), SimOptions::jit());
        assert!(got.jit_active());
        for k in 0..200u64 {
            if k % 50 == 0 {
                // Several supersteps in one run.
                want.run(40);
                got.run(40);
            }
            for port in 0..p.ports.inputs.len() {
                want.set_input(port, (k * 2654435761) >> 3);
                got.set_input(port, (k * 2654435761) >> 3);
            }
            if k % 5 == 0 {
                want.run(7);
                got.run(7);
            } else {
                want.tick();
                got.tick();
            }
        }
        assert_eq!(want.counters(), got.counters());
        assert!(want.counters().mem_writes > 0 || p.mems.is_empty());
    }
}

/// A body over `MAX_BODY` instructions stays interpreted (its native code
/// would be slower), while the rest of the `Sim` runs native; results are
/// the same.
#[test]
fn oversized_bodies_stay_interpreted() {
    let d = generate(&GenConfig::gates(6, super::MAX_BODY + 4000, 1)).design;
    let options = Options {
        pack: PackMode::Off,
        ..Options::unoptimized()
    };
    let p = program(&d, &options);
    assert!(p.instrs.len() > super::MAX_BODY);
    let s = Sim::with_options(Arc::clone(&p), SimOptions::jit());
    let code = s.jit.as_ref().expect("the JIT ran");
    assert!(code.bodies[0].is_none(), "the full body is interpreted");
    check_jit(&p, 6, 6, &SimOptions::default()).unwrap();
}

/// The JIT's `Shl` and `ShlV` against their definitions (the interpreter
/// tests in `sim/tests.rs` run the same cases).
#[test]
fn shift_ops_match_their_definitions() {
    crate::sim::tests::shl_semantics(true);
    crate::sim::tests::shlv_semantics(true);
}

/// Master/slave registers, a latch and a RAM written on clock high, per
/// clock of `periods` (common period: their least common multiple).
fn clocked(periods: &[u32]) -> Design {
    use crate::netlist::NodeKind;
    let mut d = Design::new("top");
    let r = d.root;
    let mut b = d.builder(r);
    let x = b.input("x", 8);
    let we = b.input("we", 1);
    let mut acc = x;
    for (k, &period) in periods.iter().enumerate() {
        let clk = b.clock(period, 1.max(period / 2), k as u32 % period);
        let nclk = b.not(clk);
        let (slave, q) = b.state_placeholder(NodeKind::Register { w: 8 });
        let step = b.xor(q, acc);
        let (sum, _) = b.add(q, step, None);
        let master = b.register(nclk, sum);
        b.connect(clk, (slave, 0));
        b.connect(master, (slave, 1));
        let held = b.dlatch(clk, sum);
        let wen = b.and(we, clk);
        let addr = b.split(q, 0, 3);
        let m = b.ram(addr, wen, held, &[]);
        acc = b.xor(m, held);
        b.output(format!("q{k}"), q);
    }
    b.output("y", acc);
    d
}

/// Supersteps over every common clock period up to 8 (and none):
/// against single unfused ticks at every batch boundary, and against the
/// interpreter tick by tick, with chunks of every size and supersteps of
/// several periods.
#[test]
fn supersteps_match_single_ticks_over_every_common_period() {
    for (periods, l) in [
        (&[][..], 1),
        (&[2][..], 2),
        (&[3][..], 3),
        (&[4, 2][..], 4),
        (&[2, 3][..], 6),
        (&[8, 4][..], 8),
    ] {
        let d = clocked(periods);
        for options in [Options::default(), Options::debug()] {
            let p = program(&d, &options);
            let s = Sim::with_options(Arc::clone(&p), SimOptions::jit());
            let sup = s
                .jit_code()
                .and_then(|j| j.superstep())
                .expect("a superstep");
            assert_eq!(sup.info.ticks, l, "periods {periods:?}");
            check_batches(&p, l).unwrap_or_else(|e| panic!("periods {periods:?}: {e}"));
            for (ticks, chunk) in [(1, 1), (1, 3), (5, 7), (16, 64), (9, 100_000)] {
                let o = SimOptions {
                    superstep_ticks: ticks,
                    superstep_chunk: chunk,
                    guard_min: 1,
                    ..SimOptions::default()
                };
                check_jit(&p, 300, l + ticks, &o)
                    .unwrap_or_else(|e| panic!("periods {periods:?} {ticks}/{chunk}: {e}"));
            }
        }
    }
}

/// Deliberate superstep bugs (see `superstep::MUTATION`) are caught by
/// `check_batches`.
#[test]
fn superstep_mutations_are_caught() {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            super::MUTATION.set(0);
        }
    }
    let _reset = Reset;
    let designs = [
        clocked(&[3]),
        clocked(&[2, 4]),
        fixtures::cpu(&fixtures::LOOP_PROGRAM),
    ];
    let programs: Vec<_> = designs
        .iter()
        .map(|d| program(d, &Options::default()))
        .collect();
    for p in &programs {
        check_batches(p, 1).unwrap();
    }
    for m in 1..=3 {
        super::MUTATION.set(m);
        let caught = programs.iter().any(|p| check_batches(p, 1).is_err());
        super::MUTATION.set(0);
        assert!(caught, "mutation {m} is not caught");
    }
}

/// `Shl` and `ShlV` inside supersteps: a barrel shifter and a shifted
/// register on a period-3 clock (master/slave registers), against single
/// unfused ticks and the interpreter.
#[test]
fn shift_ops_run_in_supersteps() {
    use crate::compile::Op;
    use crate::netlist::NodeKind;
    let mut d = Design::new("top");
    let r = d.root;
    let mut b = d.builder(r);
    let clk = b.clock(3, 1, 0);
    let nclk = b.not(clk);
    let x = b.input("x", 16);
    let v = b.input("v", 5);
    // `x << v[0..4]` as four mux stages of `{y[15-k:0], k'b0}`.
    let mut y = x;
    for i in 0..4u8 {
        let k = 1u8 << i;
        let lo = b.zero(k);
        let hi = b.split(y, 0, 16 - k);
        let t = b.merge(&[lo, hi]);
        let s = b.bit(v, i);
        y = b.mux(s, y, t);
    }
    // A register shifted up by one each period, mixed with the barrel.
    let (slave, q) = b.state_placeholder(NodeKind::Register { w: 16 });
    let lo = b.zero(1);
    let hi = b.split(q, 0, 15);
    let shifted = b.merge(&[lo, hi]);
    let next = b.xor(shifted, y);
    let master = b.register(nclk, next);
    b.connect(clk, (slave, 0));
    b.connect(master, (slave, 1));
    b.output("q", q);
    b.output("y", y);
    for options in [Options::default(), Options::debug()] {
        let p = program(&d, &options);
        for op in [Op::Shl, Op::ShlV] {
            assert!(
                p.blocks.iter().any(|b| b.op == op),
                "{op:?}: {:?}",
                p.blocks
            );
        }
        let s = Sim::with_options(Arc::clone(&p), SimOptions::jit());
        let sup = s
            .jit_code()
            .and_then(|j| j.superstep())
            .expect("a superstep");
        assert_eq!(sup.info.ticks, 3);
        check_batches(&p, 3).unwrap();
        for (ticks, chunk) in [(1, 1), (1, 3), (5, 7), (16, 64)] {
            let o = SimOptions {
                superstep_ticks: ticks,
                superstep_chunk: chunk,
                guard_min: 1,
                ..SimOptions::default()
            };
            check_jit(&p, 300, 3 + ticks, &o).unwrap_or_else(|e| panic!("{ticks}/{chunk}: {e}"));
        }
    }
}
