# Tick profiling

`profile_tick` can run either the selected compiled representation's tick loop
or the real background engine. The representation-only mode avoids Bevy, the
worker scheduler, channels, snapshots, and Criterion. Prefix the runner with
`engine-` to include command polling, batching, atomics, and snapshot publishing.

Install the frontend once if `cargo flamegraph` is unavailable:

```sh
cargo install flamegraph
```

The repository's Nix development shell also provides it. When not already in
that shell, prefix a command with `nix develop -c`, for example
`nix develop -c cargo flamegraph ...`.

For a flamegraph-optimized build with normal inlining, use the dedicated profile
(debug symbols + predictable symbolization):

```sh
nix develop -c cargo flamegraph --profile flamegraph -p logi-sim --features simd-profile --bin profile_tick -- \
  simd dense-fast-input
```

If you want less aggressive inlining boundaries for profiling *only*, use the
`flamegraph-noinline` profile. You can still keep a small workload and full
symbols:

```sh
RUSTFLAGS='-Cllvm-args=--inline-threshold=0 -Cforce-frame-pointers=yes' \
  nix develop -c cargo flamegraph --profile flamegraph-noinline -p logi-sim --features simd-profile --bin profile_tick -- \
  simd dense-fast-input
```

From the workspace root, profile an optimized representation and fixture:

```sh
cargo flamegraph --profile flamegraph -p logi-sim --bin profile_tick -- \
  engine-simd cpu 100000000
```

The runner may be `naive`, `flat`, `event`, or `simd`. Append `-raw` to bypass
the optimization pipeline, for example `flat-raw`. Prefix it with `engine-` to
profile the complete worker, for example `engine-flat` or `engine-simd-raw`.
Fixtures are `and`,
`adder`, `adder8`, `quiescent500`, `recursive16`, `cpu`, and `dense`. `dense-fast-input`
mirrors the SIMD fast-input throughput update path used by
`SimdRunner-optimized/large_dense_fast_input_throughput_tick`.
The final argument is the number of ticks. `dense-fast-input` defaults to
`2,000,000` ticks (instead of `100,000,000`) to keep profiles practical.

Useful comparisons:

```sh
cargo flamegraph --profile flamegraph -p logi-sim --bin profile_tick \
  -o flamegraph-flat.svg -- flat cpu 100000000
cargo flamegraph --profile flamegraph -p logi-sim --bin profile_tick \
  -o flamegraph-event.svg -- event cpu 100000000
cargo flamegraph --profile flamegraph -p logi-sim --bin profile_tick \
  -o flamegraph-simd.svg -- simd cpu 100000000
cargo flamegraph --profile flamegraph -p logi-sim --bin profile_tick \
  -o flamegraph-simd-dense.svg -- simd dense 100000000
cargo flamegraph --profile flamegraph -p logi-sim --bin profile_tick \
  -o flamegraph-simd-dense-fast-input.svg -- simd dense-fast-input 100000000

# If the default flamegraph capture is too large, run a smaller sample window first:
cargo flamegraph --profile flamegraph -p logi-sim --bin profile_tick \
  -o flamegraph-simd-dense-fast-input-short.svg -- simd dense-fast-input 2000000

# If you need a custom sampling frequency, capture with perf directly:
# (example: lower sample rate to reduce data volume)
# perf record -F 79 -g -m 1024 -o /tmp/perf.data -- target/release/profile_tick simd dense-fast-input 2000000
# perf script -i /tmp/perf.data | /tmp/stackcollapse-perf.pl | /tmp/flamegraph.pl > flamegraph-simd-dense-fast-input.svg

cargo flamegraph --profile flamegraph -p logi-sim --bin profile_tick \
  -o flamegraph-simd-dense-tick-only.svg -- simd-raw dense 100000000
cargo flamegraph --profile flamegraph -p logi-sim --bin profile_tick \
  -o flamegraph-engine-simd.svg -- engine-simd cpu 100000000
```

On Linux, `cargo flamegraph` uses `perf`. If sampling is denied, temporarily
adjust the machine's `kernel.perf_event_paranoid` setting according to local
security policy rather than running the game or profiler as root.
