//! The JIT test suite on wasm32 (feature `selftest`): the tests the
//! Cranelift backend runs under `cargo test`, run against the wasm backend
//! in Node. Build and run it with `logi-sim/scripts/wasm-selftest.sh`
//! (optionally a filter: test names containing it).
//!
//! Exports `tests()` (the names, newline-separated) and `run(name)`. A
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
}

fn main() {
    #[cfg(not(target_arch = "wasm32"))]
    eprintln!("wasm_selftest runs on wasm32: see logi-sim/scripts/wasm-selftest.sh");
}
