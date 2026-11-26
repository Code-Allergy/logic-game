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
