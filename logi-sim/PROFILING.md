# Profiling the simulator

The hot path is `Sim::tick` (`logi-sim/src/sim/`): one commit, then one
settle that runs the program's blocks in order. Two entry points exercise
it without Bevy, the engine thread or channels:

- `cargo run -p logi-sim --release --bin sim_report` prints throughput for
  every benchmark case (random DAGs, the CPU fixture, the generated
  datapath and their bit-blasted versions), the effect of each compile and
  runtime option, compile times and engine overhead. `-- --quick` runs a
  smaller set; `-- --runs N` sets how many runs the per-step table takes the
  median of.
- `cargo bench -p logi-sim --bench sim` runs the same cases under Criterion
  (`sim/dag32`, `sim/gates1`, `sim/sequential`, `sim/compile`). Pass a
  filter to run one group, for example `cargo bench -p logi-sim --bench sim
  -- sim/gates1/1000000`.

Both count throughput in work items: scalar instructions plus packed lanes
(`Program::work`), so cases compiled with different options stay
comparable. Throughput cases compile with `Options::unoptimized()` plus the
default slot order and packing, so every instruction counted is work done;
the report's simplify section shows what the simplify passes remove.

## Flamegraphs

Install the frontend once if `cargo flamegraph` is unavailable:

```sh
cargo install flamegraph
```

The repository's Nix development shell also provides it. When not already
in that shell, prefix a command with `nix develop -c`.

The workspace defines two profiles for this:

- `flamegraph`: release optimizations, no LTO, debug symbols kept.
- `flamegraph-noinline`: the same with more codegen units, for use with
  `RUSTFLAGS='-Cllvm-args=--inline-threshold=0 -Cforce-frame-pointers=yes'`
  when inlining hides the function you care about.

Profile the report (use `--quick` to keep the capture small):

```sh
cargo flamegraph --profile flamegraph -p logi-sim --bin sim_report \
  -o flamegraph-sim.svg -- --quick
```

Or one Criterion benchmark, in profiling mode so Criterion does not
analyse the samples:

```sh
cargo flamegraph --profile flamegraph -p logi-sim --bench sim \
  -o flamegraph-gates1.svg -- --bench --profile-time 10 sim/gates1/1000000
```

To capture with `perf` directly, for example at a lower sample rate:

```sh
cargo build --profile flamegraph -p logi-sim --bin sim_report
perf record -F 99 -g -o /tmp/perf.data -- target/flamegraph/sim_report --quick
perf report -i /tmp/perf.data
```

Settles that use the worker pool (`SimOptions::threads > 1`) spend their
waiting time spinning in `sim/pool.rs`; profile with one thread first.

On Linux, `cargo flamegraph` uses `perf`. If sampling is denied, temporarily
adjust the machine's `kernel.perf_event_paranoid` setting according to local
security policy rather than running the game or profiler as root.
