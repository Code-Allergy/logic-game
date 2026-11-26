//! The JIT test suite as plain functions (feature `selftest`), for a
//! runner without `cargo test`: on wasm32, `examples/wasm_selftest.rs`
//! runs the wasm backend through the same tests the Cranelift backend runs
//! natively (see `scripts/wasm-selftest.sh`).

/// Every test of the JIT suite: `jit/tests.rs`, `jit/unit_tests.rs` and
/// (on wasm32) the wasm backend's own, `jit/wasm/tests.rs`.
#[cfg(feature = "jit")]
pub fn tests() -> Vec<(String, fn())> {
    let mut v = Vec::new();
    for (suite, tests) in [
        ("jit::tests", crate::sim::jit::tests::all()),
        ("jit::unit_tests", crate::sim::jit::unit_tests::all()),
        #[cfg(target_arch = "wasm32")]
        ("jit::wasm::tests", crate::sim::jit::wasm_tests()),
    ] {
        for (name, f) in tests {
            v.push((format!("{suite}::{name}"), f));
        }
    }
    v
}

/// Check that `all` lists every test function of `src` (a test module's
/// source): each `fn` marked `#[cfg_attr(test, test)]`, except those with
/// a `#[cfg(...)]` of their own (listed under the same condition).
pub fn check_list(src: &str, all: &[(&str, fn())]) {
    let lines: Vec<&str> = src.lines().collect();
    let mut found = Vec::new();
    for (i, l) in lines.iter().enumerate() {
        if *l != "#[cfg_attr(test, test)]" {
            continue;
        }
        let mut cfg = false;
        let mut j = i;
        while j > 0 && lines[j - 1].starts_with("#[") {
            j -= 1;
            cfg |= lines[j].starts_with("#[cfg(");
        }
        let mut k = i + 1;
        while lines[k].starts_with("#[") {
            cfg |= lines[k].starts_with("#[cfg(");
            k += 1;
        }
        let name = lines[k]
            .strip_prefix("fn ")
            .and_then(|r| r.split('(').next())
            .unwrap_or_else(|| panic!("a test function: {}", lines[k]));
        if !cfg {
            found.push(name);
        }
    }
    for name in &found {
        assert!(
            all.iter().any(|(n, _)| n == name),
            "test `{name}` is missing from `all()`"
        );
    }
    let mut names: Vec<&str> = all.iter().map(|(n, _)| *n).collect();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), all.len(), "a test listed twice");
}

/// JIT tiering in [`crate::engine::LocalEngine`] on wasm32, in two halves
/// with the host's event loop in between (the module is instantiated
/// asynchronously): [`tiering::start`] loads a program, which starts
/// interpreted; after the runner yields, [`tiering::check`] finds the wasm
/// code installed by the next frame, and the state equal to the
/// interpreter's.
#[cfg(all(feature = "jit", target_arch = "wasm32"))]
pub mod tiering {
    use std::cell::RefCell;
    use std::sync::Arc;
    use std::time::Duration;

    use crate::compile::{Options, compile};
    use crate::engine::LocalEngine;
    use crate::sim::{Sim, SimOptions};
    use crate::testkit::fixtures;

    thread_local! {
        static ENGINE: RefCell<Option<LocalEngine>> = const { RefCell::new(None) };
    }

    const STEPS: [u64; 2] = [300, 2000];

    fn frame(e: &mut LocalEngine, steps: u64) {
        e.core.step(steps);
        let t = std::cell::Cell::new(0u64);
        // A clock that never runs out of budget (each read is 1 ns on).
        let clock = move || {
            t.set(t.get() + 1);
            Duration::from_nanos(t.get())
        };
        e.run_frame(Duration::from_secs(3600), &clock);
    }

    pub fn start() {
        let mut e = LocalEngine::with_sim_options(Options::default(), SimOptions::interactive());
        e.load(&fixtures::cpu(&fixtures::LOOP_PROGRAM), 1);
        assert!(e.jit_pending(), "the code is on its way");
        frame(&mut e, STEPS[0]);
        let sim = e.core.sim().expect("loaded");
        assert!(!sim.jit_active(), "interpreted until the module is ready");
        ENGINE.set(Some(e));
    }

    pub fn check() {
        let mut e = ENGINE.take().expect("started");
        frame(&mut e, STEPS[1]);
        assert!(!e.jit_pending());
        let sim = e.core.sim().expect("loaded");
        assert!(sim.jit_active(), "the wasm code was installed");
        let p = Arc::new(
            compile(&fixtures::cpu(&fixtures::LOOP_PROGRAM), &Options::default()).unwrap(),
        );
        let mut want = Sim::new(p);
        want.run(STEPS[0]);
        want.run(STEPS[1]);
        assert_eq!(want.tick_count(), sim.tick_count());
        assert_eq!(want.export_state(), sim.export_state());
    }
}
