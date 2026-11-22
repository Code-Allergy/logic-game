# Performance

How fast Gate Quest is, how to measure it, and how regressions are caught
(issue #110). Everything goes through one entry point:

```sh
cargo xtask perf <command>     # alias for: cargo run -q -p xtask -- perf <command>
```

| command | what it does |
|---|---|
| `perf list` | the benchmark suites |
| `perf bench [--quick] [--suite S] [--filter RE]` | run the criterion suites, write `<target>/perf/results/<label>.json` and `<target>/perf/latest.json` |
| `perf compare <base> [<head>]` | compare two result files or two git revisions: a table with % change and noise bounds |
| `perf check` | measure the deterministic metrics and fail if they regressed against `perf/baseline.json` |
| `perf bless [--instructions]` | measure and rewrite `perf/baseline.json` |
| `perf flamegraph <target> [-- args]` | `perf record -g` and an SVG flamegraph |
| `perf stat <target> [-- args]` | hardware counters with `perf stat` |

`<target>` is cargo's target directory: `$CARGO_TARGET_DIR` if set,
otherwise `target/`. Nothing here is a dependency of the game: the
benchmarks live in `logi-sim/benches` (dev-dependencies) and in the `perf`
and `perf-metrics` workspace crates, the orchestration in `xtask`; the game
links none of them.

For the simulator's tick loop itself (what the time goes into, and what
was tried), see [`logi-sim/PROFILING.md`](../logi-sim/PROFILING.md).

## Layout

```
logi-sim/benches/sim.rs     suite "sim": tick throughput, compile time, passes, threads
logi-sim/benches/engine.rs  suite "engine": engine overhead
perf/                       crate `perf`: shared fixtures + app-side suites
  src/lib.rs                catalog, gallery designs, headless Bevy app
  benches/gallery.rs        suite "gallery": cpu16 / cpu16_rtl ticks and compile
  benches/app.rs            suite "app": documents, design_of, routing, edits, undo, Check, Make chip
  benches/frame.rs          suite "frame": headless frame time
  metrics/                  crate `perf-metrics`: deterministic metrics (for perf check)
  baseline.json             the checked-in baseline
xtask/                      crate `xtask`: the `perf` commands
examples/cpu_bench.rs       one-shot CPU report (also a profiling target)
```

The `perf-metrics` crate is separate from `perf` so that only it turns on
logi-sim's `counters` feature: the benchmarks and the game build without
it. (A workspace-wide command such as `cargo test --workspace` builds
logi-sim once with the feature on for everyone; that only adds a few
integer additions per settle.)

## Benchmarks

| suite | group | what |
|---|---|---|
| sim | `sim/dag32/{10000,100000,1000000}` | one tick of a random word-level DAG, inputs changing every tick (instructions/s) |
| sim | `sim/gates1/{10000,…}` | the same, 1-bit gates |
| sim | `sim/sequential/{cpu,cpu_bitblasted,datapath32,…}` | the testkit CPU and a sequential datapath, word-level and bit-blasted |
| sim | `sim/compile/gates1/{100000,1000000}` | a full compile |
| sim | `sim/compile_pass/<design>/<pass>` | one compiler pass (validate, elaborate, loop_check, lower, simplify, levelize, pack, renumber), via `compile_observed` |
| sim | `sim/threads/<design>/{1t,2t,4t}` | one tick on 1, 2 and 4 threads |
| engine | `engine/batch/{sim_run_1000,core_step_1000}` | 1000 ticks directly and through `EngineCore` (chunking, clock reads) |
| engine | `engine/publish/{0,16,all}` | one probe frame |
| engine | `engine/mem_probe/{changed,unchanged}` | a frame with a memory probe |
| engine | `engine/reset`, `engine/install` | power-on reset, program swap with state carry-over |
| engine | `engine/threaded/step_{1,1000}_sync` | a round trip to the worker thread |
| gallery | `gallery/tick/{cpu16,cpu16_rtl}/{default,editor}` | one tick of the gallery CPUs, compiler defaults and the editor's options |
| gallery | `gallery/compile/{cpu16,cpu16_rtl}/{default,editor}` | compiling them |
| app | `doc/parse_v6/<id>`, `doc/expand_v6/<id>` | version 6 text → `SourceFile` → documents |
| app | `doc/write_v5/<id>`, `doc/read_v5/<id>` | the version 5 save, both ways |
| app | `design_of/<id>` | workspace → `logi_sim` design |
| app | `routing/full/<id>`, `routing/drag/<id>` | routing every wire, and the incremental route of one dragged part |
| app | `routing/rubber_band/<id>` | the rubber band while drawing a wire (`wiring::band_route`) |
| app | `bridge/edit/cpu16` | one design edit through ECS, bridge, inline compile and the undo snapshot |
| app | `history/undo_redo/cpu16` | undo + redo of one step |
| app | `challenges/check/adder8` | the Check of a challenge |
| app | `chip/plan/<id>` | `plan_chip` of every part of a root circuit |
| frame | `frame/idle/<id>` | one `App::update` of a large circuit, paused |
| frame | `frame/drag/cpu16` | a frame while dragging a part |
| frame | `frame/running/{cpu16,cpu16_rtl}` | a frame while the CPU runs unlimited on the worker thread |

`<id>` is `cpu16` and `cpu16_rtl`, plus `cpu_fibonacci`, `screen_text` and
`multiplier` outside quick mode. Every case is deterministic: fixed
seeds, fixed designs, fixed inputs.

Not covered yet: the circuit statistics of issue #107 (add `stats/<id>`
to `app.rs` and, if they are deterministic, their counts to
`perf-metrics`).

### Running

```sh
cargo xtask perf bench                      # everything (tens of minutes)
cargo xtask perf bench --quick              # criterion --quick, no 1M-node cases (a few minutes)
cargo xtask perf bench --suite gallery      # one suite
cargo xtask perf bench --filter 'gallery/tick/cpu16/'   # criterion's regex filter
```

`--quick` passes criterion's `--quick` (sample until significant, not
for a fixed time) and sets `PERF_QUICK=1`, which the suites read to leave
out their largest cases. Plain `cargo bench -p logi-sim` / `-p perf`
works too; results then land in criterion's usual `<target>/criterion`.

Each run writes one JSON file: per benchmark the typical time of one
iteration in ns with its 95% confidence interval and the declared
throughput, plus the deterministic metrics, the git revision and the
host.

### Comparing

```sh
cargo xtask perf compare HEAD~ HEAD          # two revisions
cargo xtask perf compare main                # a revision against the working tree
cargo xtask perf compare old.json new.json   # two result files
cargo xtask perf compare old.json            # a result file against latest.json
cargo xtask perf compare HEAD~ HEAD --quick --suite gallery
```

A revision is checked out with `git worktree add` under
`<target>/perf/worktrees/<sha>`, benchmarked there (with the suite list of
the checkout you run from; suites missing in that revision are skipped),
and the worktree is removed afterwards. Both sides build into the same
target directory, so dependencies are compiled once. Results of revisions
are kept in `<target>/perf/results/rev-<sha>.json` and reused by later
comparisons with the same settings.

The table shows, per benchmark, both times, the change, and its noise
bounds: from the base's upper to the head's lower confidence bound, and
from the base's lower to the head's upper one. A change is called
`faster`/`slower` only when the whole range is beyond `--noise` (2% by
default); otherwise `~`. Changed deterministic metrics are listed below.
Compare runs on the same, quiet machine: a laptop on battery or a busy CI
runner moves wall-clock times by more than 2%.

## The regression gate

Wall-clock times are too noisy to gate on. `perf check` gates on counts
that are the same on every machine and every run, measured by
`perf-metrics`:

| metric | kind | threshold |
|---|---|---|
| `nodes.<case>` | primitive nodes after flattening | +3% |
| `program.<case>.<opts>.{instrs,slots,packed_words,packed_lanes,commits,blocks}` | compiled program size | +3% |
| `tick.<case>.<opts>.ops` | operations executed per tick: instructions + packed words + commits (logi-sim feature `counters`) | +3% |
| `tick.<case>.<opts>.allocs` | heap allocations in 1000 ticks (counting global allocator in `perf-metrics`) | any increase |
| `engine.<case>.allocs` | heap allocations in 100 engine frames (100 ticks + a probe frame of every root net) | any increase |
| `instructions.<cpu>.default.per_tick` | instructions per tick from `perf stat` (optional) | +3% |

Cases: `cpu16` and `cpu16_rtl` (gallery; `<opts>` = `default` and
`editor`), `testkit_cpu`, `dag32_10k` and `gates1_100k` (`default`).
Thresholds are in `perf/baseline.json` (`thresholds`, per kind). A metric
that improved beyond its threshold passes with a note; bless it so it
cannot silently come back. A metric missing from the measurement fails.

```sh
cargo xtask perf check        # exit 1 on a regression
cargo xtask perf bless        # rewrite perf/baseline.json; review and commit it
```

Bless when a change is intended (a new compiler pass that trades size for
speed, a gallery circuit redesigned) and say why in the commit. The
`counters` feature is off by default and the counting code is compiled
out without it (`#[cfg(feature = "counters")]`); the allocation counter
exists only in the `perf-metrics` binary.

Instruction counts need hardware counters. Many VMs and containers do
not expose them (`perf stat` prints `<not supported>`), so the checked-in
baseline has none. On a machine that has them, `perf bless --instructions`
adds them; `perf check` then gates on them where they can be measured and
skips them, with a note, where they cannot.

CI: `.github/workflows/perf.yml` runs `cargo xtask perf check` (Linux) on
pull requests into `main`, pushes to `main` and manual runs
(`workflow_dispatch`), within the same runner budget as `ci.yml`. Run it
yourself before opening a PR into another branch.

## Profiling

`perf flamegraph` and `perf stat` need Linux `perf`. The xtask looks for
it on `PATH` (or `$PERF`) and explains how to get it when it is missing:

- **nix**: the dev shell has `perf`, `cargo-flamegraph` and `cargo-pgo`:
  `nix develop` in a normal checkout. Just perf:
  `nix shell --inputs-from . nixpkgs#perf`. Nix does not accept a git
  worktree as a flake reference; use the path form there:
  `nix shell --extra-experimental-features 'nix-command flakes' --inputs-from path:$PWD nixpkgs#perf -c cargo xtask perf flamegraph ...`
- **Debian/Ubuntu**: `sudo apt install linux-tools-common linux-tools-$(uname -r)`
- **Fedora**: `sudo dnf install perf`; **Arch**: `sudo pacman -S perf`

Sampling user space needs `kernel.perf_event_paranoid` ≤ 2 (the usual
default); kernel stacks need ≤ 1.

```sh
# The tick loop alone: perf-metrics' `ticks <case> <n> [default|editor|raw] [threads]`
# builds one design and runs n ticks, nothing else.
cargo xtask perf flamegraph bin:perf-metrics -- ticks cpu16 300000
cargo xtask perf flamegraph bin:perf-metrics -- ticks cpu16_rtl 20000000
cargo xtask perf flamegraph bin:perf-metrics -- ticks gates1_1m 300 raw 4
# The one-shot CPU report (includes compiling and the halt measurement):
cargo xtask perf flamegraph example:cpu_bench -- 500000 cpu16
# A criterion suite, run for 10 s without analysis (--profile-time). Its
# setup (building every design of the file) is in the profile too.
cargo xtask perf flamegraph bench:frame
# Hardware counters:
cargo xtask perf stat bin:perf-metrics -- ticks cpu16 100000
```

`ticks` cases: the gated ones (`cpu16`, `cpu16_rtl`, `testkit_cpu`,
`dag32_10k`, `gates1_100k`) and the profiling-only `gates1_1m` and
`dag32_1m`. Findings from profiling the tick loop are in
[`logi-sim/PROFILING.md`](../logi-sim/PROFILING.md).

Targets are built with the `profiling` profile (release optimizations without
LTO, 16 codegen units but one for logi-sim, line tables kept), which
`perf bench` uses too, so profiling reuses the benchmark build. (The
default `bench` profile's fat LTO with one codegen unit and full debug
info needs more memory than a 16 GB machine has for the Bevy crates.)

The SVG lands in `<target>/perf/profiles/<target>-<args>.svg` with the
folded stacks (`.folded`, for diffing with `inferno-diff-folded`) and the
raw `perf.data` (`perf report -i ...`; large, delete it when done).
Stacks are unwound through frame pointers by default (`--call-graph fp`):
every sample keeps its leaf function, and the simulator's kernels are
inlined into `logi_sim::sim::code::exec` anyway, but callers in code built
without frame pointers may be cut short. `--call-graph dwarf` unwinds
fully; on the 600 MB binaries that link Bevy it lost most samples in
testing, so use it with small targets (or rebuild with
`RUSTFLAGS='-Ctarget-cpu=native -Cforce-frame-pointers=yes'`, a full
rebuild). Sampling works in VMs and containers; hardware counters often
do not (see `perf stat`).

### Reading a flamegraph

Each box is a function; its width is the share of samples in which it was
on the stack, and the boxes above it are what it called. The x order is
alphabetical, not time. Look for wide plateaus: a wide box with nothing
above it is where the time is spent itself (for the simulator usually
`logi_sim::sim::code::exec` and its kernels `seq`, `plain`, `packed`). Hover
for the sample count, click to zoom, Ctrl+F to highlight a name. For the
tick loop, compare the gate and word CPUs: the gate CPU spends its time in
1-bit `seq` blocks, the word CPU in fewer, wider instructions.

## Adding a benchmark

1. Pick the suite: simulator-only code goes to `logi-sim/benches/` (no
   Bevy), anything that needs the app's types to `perf/benches/`. Shared
   inputs (catalog, gallery designs, the headless app) are in
   `perf/src/lib.rs`.
2. Write a criterion function in the file and add it to its
   `criterion_group!`. Name groups `<area>/<what>` and ids after the case
   (`BenchmarkId::new(case, variant)`), so filters and comparisons line
   up. Use fixed inputs and seeds; set `Throughput::Elements` where a rate
   means something.
3. Respect quick mode: leave the largest cases out when `perf::quick()`
   (or `PERF_QUICK`) is set.
4. A new bench file needs a `[[bench]]` entry (`harness = false`) in its
   crate's `Cargo.toml` and a `Suite` in `xtask/src/bench.rs`.
5. If the change has a count that is the same on every run (a size, an
   operation count, an allocation count), add it to
   `perf/metrics/src/main.rs` and bless the baseline, so `perf check`
   gates it.

## Updating the baseline

```sh
cargo xtask perf check     # see what moved
cargo xtask perf bless     # accept it
git diff perf/baseline.json
```

Commit the new baseline with the change that caused it, and say in the
message why the numbers moved.
