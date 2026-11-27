//! Tests of the wasm backend itself (wasm32, feature `selftest`): what the
//! shared suite (`jit/tests.rs`, `jit/unit_tests.rs`) does not cover, the
//! encoder's deliberate bugs, the module bytes and the function table.

use std::sync::Arc;

use super::enc;
use super::rt;
use crate::compile::{Options, Program, compile};
use crate::netlist::Design;
use crate::sim::jit::compile_unit;
use crate::sim::{Sim, SimOptions};
use crate::testkit::diff::{check_batches, check_jit, check_jit_design};
use crate::testkit::{GenConfig, fixtures, generate};

fn program(d: &Design, options: &Options) -> Arc<Program> {
    Arc::new(compile(d, options).unwrap_or_else(|e| panic!("{e:?}")))
}

/// The designs the mutation test runs: every kind of op, state, memories,
/// clocks and lane-packed words.
fn designs() -> Vec<Design> {
    vec![
        fixtures::cpu(&fixtures::MULTIPLY_PROGRAM),
        fixtures::register_file_ram2(4, 64, &[u64::MAX, 1]),
        fixtures::fifo(4, 12),
        generate(&GenConfig {
            seed: 3,
            max_width: 64,
            nodes: 200,
            state_percent: 25,
            ..GenConfig::default()
        })
        .design,
    ]
}

/// The JIT runs wasm code: every body compiled, a superstep, and the
/// module validates.
fn the_jit_runs_generated_modules() {
    let p = program(&fixtures::cpu(&fixtures::LOOP_PROGRAM), &Options::default());
    let s = Sim::with_options(Arc::clone(&p), SimOptions::jit());
    let code = s.jit_code().expect("the wasm backend compiled the program");
    assert!(code.code_bytes > 0);
    assert!(code.superstep().is_some(), "a superstep");
    let sim = Sim::with_options(p, SimOptions::default());
    for b in sim.body_refs() {
        if !b.used {
            continue;
        }
        let mut builder = super::Builder::new(0).unwrap();
        let (mut n, mut bytes) = (0, 0);
        super::super::compile_body(&mut builder, &sim.program, &b, &mut n, &mut bytes).unwrap();
        let (module, _) = builder.bytes();
        let buf = js_sys::Uint8Array::from(&module[..]);
        assert_eq!(
            js_sys::WebAssembly::validate(&buf.into()).ok(),
            Some(true),
            "the module validates"
        );
    }
}

/// Deliberate encoder bugs (see `enc::MUTATION`) are caught by the checks
/// the suite runs: a wrong opcode by `check_jit`, a wrong constant by
/// `check_batches`.
fn encoder_mutations_are_caught() {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            enc::MUTATION.set(0);
        }
    }
    let _reset = Reset;
    let programs: Vec<_> = designs()
        .iter()
        .map(|d| program(d, &Options::default()))
        .collect();
    let check = |p: &Arc<Program>| {
        check_jit(p, 200, 1, &SimOptions::default()).and_then(|()| check_batches(p, 1))
    };
    for p in &programs {
        check(p).unwrap();
    }
    for m in 1..=2 {
        enc::MUTATION.set(m);
        let caught = programs.iter().any(|p| check(p).is_err());
        enc::MUTATION.set(0);
        assert!(caught, "mutation {m} is not caught");
    }
}

/// Units are really freed: thousands of compile/drop cycles reuse their
/// function-table entries (the table does not grow).
fn compile_and_free_cycles_reuse_the_table() {
    let p = program(
        &fixtures::cpu(&fixtures::MULTIPLY_PROGRAM),
        &Options::default(),
    );
    let sim = Sim::with_options(Arc::clone(&p), SimOptions::default());
    let cycle = || {
        let u = compile_unit(&p, sim.body_refs().remove(0)).unwrap();
        assert!(u.code_bytes() > 0);
    };
    for _ in 0..20 {
        cycle();
    }
    let before = rt::table_len();
    for _ in 0..2000 {
        cycle();
    }
    assert_eq!(rt::table_len(), before, "the table grew");
    // Programs too: a `Sim`'s code is freed with it.
    for _ in 0..200 {
        let s = Sim::with_options(Arc::clone(&p), SimOptions::jit());
        assert!(s.jit_active());
    }
    assert!(rt::table_len() <= before + 64, "the table grew");
}

/// Wide designs with long bodies and every superstep chunk size, guarded
/// ranges inside supersteps included: the locals and `if` blocks of long
/// functions.
fn long_functions_match_the_interpreter() {
    let d = generate(&GenConfig {
        seed: 11,
        max_width: 64,
        nodes: 3000,
        state_percent: 20,
        ..GenConfig::default()
    })
    .design;
    for options in [Options::default(), Options::debug()] {
        let p = program(&d, &options);
        for chunk in [1, 64, 100_000] {
            let o = SimOptions {
                superstep_chunk: chunk,
                guard_min: 1,
                ..SimOptions::default()
            };
            check_jit(&p, 120, chunk as u64, &o).unwrap_or_else(|e| panic!("chunk {chunk}: {e}"));
        }
    }
    check_jit_design(&fixtures::cpu(&fixtures::LOOP_PROGRAM), 2000, 5).unwrap();
}

/// The tests of this module, for the selftest runner.
pub fn all() -> Vec<(&'static str, fn())> {
    vec![
        (
            "the_jit_runs_generated_modules",
            the_jit_runs_generated_modules,
        ),
        ("encoder_mutations_are_caught", encoder_mutations_are_caught),
        (
            "compile_and_free_cycles_reuse_the_table",
            compile_and_free_cycles_reuse_the_table,
        ),
        (
            "long_functions_match_the_interpreter",
            long_functions_match_the_interpreter,
        ),
    ]
}
