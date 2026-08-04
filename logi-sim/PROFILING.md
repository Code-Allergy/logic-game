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

From the workspace root, profile an optimized representation and fixture:

```sh
cargo flamegraph --profile flamegraph -p logi-sim --bin profile_tick -- \
  engine-simd cpu 100000000
```

The runner may be `naive`, `flat`, `event`, or `simd`. Append `-raw` to bypass
the optimization pipeline, for example `flat-raw`. Prefix it with `engine-` to
profile the complete worker, for example `engine-flat` or `engine-simd-raw`.
Fixtures are `and`,
`adder`, `adder8`, `quiescent500`, `recursive16`, and `cpu`. The final argument
is the number of ticks and defaults to 100 million.

Useful comparisons:

```sh
cargo flamegraph --profile flamegraph -p logi-sim --bin profile_tick \
  -o flamegraph-flat.svg -- flat cpu 100000000
cargo flamegraph --profile flamegraph -p logi-sim --bin profile_tick \
  -o flamegraph-event.svg -- event cpu 100000000
cargo flamegraph --profile flamegraph -p logi-sim --bin profile_tick \
  -o flamegraph-simd.svg -- simd cpu 100000000
cargo flamegraph --profile flamegraph -p logi-sim --bin profile_tick \
  -o flamegraph-engine-simd.svg -- engine-simd cpu 100000000
```

On Linux, `cargo flamegraph` uses `perf`. If sampling is denied, temporarily
adjust the machine's `kernel.perf_event_paranoid` setting according to local
security policy rather than running the game or profiler as root.
