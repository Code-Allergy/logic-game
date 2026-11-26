#!/usr/bin/env bash
# The JIT test suite against the wasm backend (logi-sim feature `jit` on
# wasm32), in Node: builds `examples/wasm_selftest.rs` for
# wasm32-unknown-unknown (release, with debug assertions), binds it with
# wasm-bindgen for Node, and runs every test whose name contains $1 in a
# fresh instance. Needs the wasm32 target, Node and the wasm-bindgen CLI of
# the version in Cargo.lock (on PATH, or as Trunk caches it; or set
# WASM_BINDGEN). Exit status: the number of failed tests (capped at 255).
#
#   logi-sim/scripts/wasm-selftest.sh [filter] [-- extra cargo flags]
set -euo pipefail
cd "$(dirname "$0")/../.."
filter="${1:-}"
shift || true
[ "${1:-}" = "--" ] && shift
features="jit,selftest"
[ -n "${LOGI_SELFTEST_FEATURES:-}" ] && features="$features,$LOGI_SELFTEST_FEATURES"
CARGO_PROFILE_RELEASE_DEBUG_ASSERTIONS=true cargo build -q -p logi-sim --release \
  --target wasm32-unknown-unknown --example wasm_selftest --features "$features" "$@"
target="${CARGO_TARGET_DIR:-target}"
version=$(grep -A1 '^name = "wasm-bindgen"$' Cargo.lock | sed -n 's/^version = "\(.*\)"/\1/p')
wb="${WASM_BINDGEN:-$(command -v wasm-bindgen || echo "$HOME/.cache/trunk/wasm-bindgen-$version/wasm-bindgen")}"
out="$target/wasm-selftest"
"$wb" --target nodejs --out-dir "$out" \
  "$target/wasm32-unknown-unknown/release/examples/wasm_selftest.wasm"
node - "$out/wasm_selftest.js" "$filter" <<'JS'
const [js, filter] = process.argv.slice(2);
const load = () => {
  for (const k of Object.keys(require.cache)) delete require.cache[k];
  return require(require('path').resolve(js));
};
const names = load().tests().split('\n').filter((n) => n.includes(filter));
let failed = [];
for (const name of names) {
  const t = Date.now();
  try {
    load().run(name);
    console.log(`test ${name} ... ok (${((Date.now() - t) / 1000).toFixed(1)} s)`);
  } catch (e) {
    console.log(`test ${name} ... FAILED (${e})`);
    failed.push(name);
  }
}
console.log(`\n${names.length - failed.length} passed, ${failed.length} failed`);
for (const n of failed) console.log(`  failed: ${n}`);
process.exit(Math.min(failed.length, 255));
JS
