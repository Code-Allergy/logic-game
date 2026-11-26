//! The JIT test suite on wasm32 (feature `selftest`): the tests the
//! Cranelift backend runs under `cargo test`, run against the wasm backend
//! in Node. Build and run it with `logi-sim/scripts/wasm-selftest.sh`
//! (optionally a filter: test names containing it).
//!
//! Exports `tests()` (the names, newline-separated), `run(name)`, and the
//! two halves of the asynchronous tiering test (`tiering_start`, then
//! `tiering_check` after yielding to the event loop). A
//! failing test panics, which aborts the instance on wasm32 (the message is
//! logged first): the script starts a fresh instance for each test.

#[cfg(target_arch = "wasm32")]
mod runner {
    use wasm_bindgen::prelude::*;

    #[wasm_bindgen]
    extern "C" {
        #[wasm_bindgen(js_namespace = console)]
        fn log(s: &str);
    }

    /// The names of every test, one per line.
    #[wasm_bindgen]
    pub fn tests() -> String {
        logi_sim::testkit::selftest::tests()
            .into_iter()
            .map(|(n, _)| n)
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Run test `name` (panics, so traps, if it fails).
    #[wasm_bindgen]
    pub fn run(name: &str) {
        std::panic::set_hook(Box::new(|info| log(&format!("panicked: {info}"))));
        let (_, f) = logi_sim::testkit::selftest::tests()
            .into_iter()
            .find(|(n, _)| n == name)
            .unwrap_or_else(|| panic!("no test {name}"));
        f();
    }

    /// JIT tiering in `LocalEngine`, first half (see
    /// `testkit::selftest::tiering`); yield to the event loop, then call
    /// `tiering_check`.
    #[wasm_bindgen]
    pub fn tiering_start() {
        std::panic::set_hook(Box::new(|info| log(&format!("panicked: {info}"))));
        logi_sim::testkit::selftest::tiering::start();
    }

    #[wasm_bindgen]
    pub fn tiering_check() {
        logi_sim::testkit::selftest::tiering::check();
    }

    /// Registry-slot reuse after a finished instantiation (see
    /// `testkit::selftest::slot_reuse`): phases 0, 1 and 2, with the event
    /// loop run in between.
    #[wasm_bindgen]
    pub fn slot_reuse_phase(phase: u32) {
        std::panic::set_hook(Box::new(|info| log(&format!("panicked: {info}"))));
        use logi_sim::testkit::selftest::slot_reuse;
        match phase {
            0 => slot_reuse::start(),
            1 => slot_reuse::reuse(),
            _ => slot_reuse::check(),
        }
    }
}

fn main() {
    #[cfg(not(target_arch = "wasm32"))]
    eprintln!("wasm_selftest runs on wasm32: see logi-sim/scripts/wasm-selftest.sh");
}
