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

The framework of issue #110 wraps all of this: `cargo xtask perf
flamegraph <target>` builds a target, records it and draws the SVG with
`inferno`; see `docs/performance.md`. The quickest tick-loop target is
`perf-metrics ticks`, which builds one design and does nothing but tick:

```sh
cargo xtask perf flamegraph bin:perf-metrics -- ticks cpu16 300000
```

## Findings, 2026-11-22: first profiling round (#110)

### Method

- Machine: a 4-vCPU VM (Intel Xeon @ 2.10 GHz; L1d 48 KiB, L2 2 MiB per
  core, L3 260 MiB shared), shared with other build jobs: load average
  6 to 17 during the runs. Wall-clock numbers are indicative only; the
  single-threaded ones repeated within about 10%, the threaded ones did
  not (see below).
- `perf` 7.3.5 from the nix store, `perf record -F 997 --call-graph fp`
  (user space; `perf_event_paranoid` = 2). Hardware counters (cycles,
  instructions, cache misses) are `<not supported>` in this VM, so cache
  behaviour below is inferred from time per operation against the slot
  footprint, not measured with counters.
- Build: the `profiling` cargo profile (release, no LTO, 16 codegen units
  but one for logi-sim, line tables), so inlined kernels show up as their
  own frames.
- Targets: `perf-metrics ticks <case> <n> [default|raw] [threads]`, i.e.
  `Sim::tick` with root inputs changing every tick and nothing else.
  `default` is the compiler's defaults; `raw` turns the simplify passes off
  (every node an instruction, as the throughput benchmarks do) with the
  default layout and packing. Samples were attributed by inlined frame:
  `s1`/`s2`/`s3` (streaming kernels), `run1`..`run3` (kernels over full
  `Instr`s), `packed`/`operand` (lane-packed words), `commit`, and the rest
  of `code::exec` (block dispatch).
- Variant timings (streaming off, packing forced on or off, 2 and 4
  threads) came from a throwaway driver over the same designs; criterion
  numbers from `cargo xtask perf bench --quick`.

### The designs

| design | compiled (defaults unless `raw`) | blocks, median length | slots | time per tick | per op |
|---|---|---|---|---|---|
| `cpu16` (gates) | 12 294 instrs, 1 007 commits, no packing | 365, 11 | 104 KiB | 10.9 µs | 0.82 ns |
| `cpu16_rtl` (words) | 138 instrs, 15 commits | 53, 1 | 1.4 KiB | 0.35 µs | 2.3 ns |
| `gates1_1m` | 103 677 instrs (DCE keeps 10%) | 3 452, 24 | 819 KiB | 116 µs | 1.12 ns |
| `gates1_1m` raw | 2 685 instrs + 17 551 packed words (997 315 lanes) | 4 612, 3 | 172 KiB | 1.58 ms | 1.58 ns per lane |
| `gates1_1m` raw, packing off | 1 000 000 instrs | | 7.8 MiB | 1.68 ms | 1.68 ns |
| `dag32_1m` | 3 705 instrs | 885, 2 | 30 KiB | 9.5 µs | 2.56 ns |
| `dag32_1m` raw | 2 408 382 instrs | 4 066, 171 | 18.7 MiB | 6.58 ms | 2.73 ns |

(1 op = one instruction, packed word or commit; 1 ns is about 2.1 cycles
here.)

### Where the time goes

Share of the samples inside the tick loop:

| | `cpu16` | `cpu16_rtl` | `gates1_1m` | `gates1_1m` raw | `dag32_1m` raw |
|---|---|---|---|---|---|
| streaming kernels (`seq`) | 50% (And 25, Nor 12, Or 7) | 45% (Mux 32) | 84% | 1% | 41% |
| full-`Instr` kernels (`plain`) | 34% (Concat 17, Shr 16) | 17% (Shr 10) | 0 | 0 | 58% (Sext 25, Shr 21) |
| packed words | 0 | 0 | 0 | 98% (87% in `operand`) | 0 |
| commit | 8% | 3% | 0 | 0 | 0 |
| block dispatch (`exec`) | 6% | 27% | 13% | 1% | 1% |

- **Dispatch against work.** Dispatch is cheap where blocks are long
  (`cpu16`: 6%, median block 11 instructions) and dominant where they are
  not: `cpu16_rtl` has a median block of one instruction, spends 27% in
  `exec` itself and runs 2.3 ns per op against 0.82 ns for `cpu16`.
  `dag32_1m` with the defaults (median block 2) is the same story at 2.6
  ns per op.
- **Full-`Instr` kernels.** Ops with literal fields (`Shr`, `Concat`,
  `Sext`, `Clock`, `MemRead`) do not stream: they read a 16-byte `Instr`
  plus an 8-byte immediate. In `cpu16` they are 27% of the instructions
  and 34% of the time; in `dag32_1m` raw, 45% of the instructions and 58%
  of the time.
- **Streaming encoding.** Turning it off costs 1.40x on `cpu16` (16.6 µs),
  1.22x on `gates1_1m` and 1.17x on `dag32_1m` raw: it pays most where
  the slot array is small and the instruction stream is the traffic.
- **Commits.** `cpu16` commits 1 007 registers per tick, 8% of its time:
  one `match` per commit over a 24-byte enum, although all of them are
  plain `Copy`s.
- **Lane packing.** On `gates1_1m` raw, `PackMode::Auto` packs 99.7% of
  the instructions into 17 551 words and the slots shrink from 7.8 MiB to
  172 KiB, but the tick is only 6% faster than unpacked (1.58 against 1.68
  ms): 87% of the packed time is `operand`, assembling each word's lanes
  bit by bit from other words (gathers); the logic op itself is under 1%.
  Forcing `PackMode::All` where Auto declines is slower: 1.16x on `cpu16`,
  2.0x on `gates1_1m` with the defaults. Auto chooses right on these
  designs, and packing is limited by gathers, not by the op.
- **Cache behaviour.** Per op, 1-bit gates cost 0.82 ns with 104 KiB of
  slots (`cpu16`, in L2), 1.12 ns with 819 KiB (`gates1_1m`, L2) and 1.68
  ns with 7.8 MiB (raw, unpacked, beyond L2): about 2x from L1/L2 to L3
  for the same kernels. `dag32_1m` raw (18.7 MiB of slots) runs at 2.73
  ns per op, mostly in the `plain` kernels. No counters were available to
  split this into misses.
- **Threaded settle.** Every threaded run on this loaded VM was slower
  than one thread: `gates1_1m` raw 0.59x on 2 threads and 0.01x on 4
  (111 ms per tick); `dag32_1m` raw 0.82x and 0.06x. In the 4-thread
  profile 78% of the tick-loop samples are in the pool's waits
  (`wait_for`, spinning then yielding), 14% with 2 threads. With static
  partitions and a wait per phase (about 290 phases per settle here), a
  worker that the OS deschedules stalls every phase after it, and with 4
  threads on 4 busy vCPUs that happens constantly. Whatever the pool
  gains on an idle machine could not be measured here; under
  oversubscription the design falls apart, and a player's machine runs
  the renderer and the engine thread beside it. (With the defaults,
  `gates1_1m` and `dag32_1m` have no rank of 2 048 units, so no workers
  start at all.)
- **Per-tick engine overhead.** None measurable on the batch path:
  `EngineCore::step(1000)` + `run` takes 118 µs against 121 µs for
  `Sim::run(1000)` (testkit CPU). A probe frame costs about 2.4 ns per
  probe (118 ns for 48), a memory probe copy 2.2 µs for 256 words (29 ns
  when unchanged), a round trip to the worker thread 3.1 µs, a program
  swap with state carry-over 2.2 µs. The tick loop allocates nothing
  (`perf check` gates this: 0 allocations in 1 000 ticks and in 100
  engine frames on every case).
- **Outside the tick loop.** One edit of `cpu16` through the app
  (`bridge/edit`) takes 132 ms and an undo + redo 278 ms; flamegraphs of
  both show about 40% of the samples in wire routing's A* (`route_sink`),
  most of that in `BinaryHeap::pop` (`sift_down`), against 9-14% in
  compiling. Hashing (std's SipHash) is 3-5% and malloc/free under 2%.
  `routing/full/cpu16` is 63 ms and `routing/drag/cpu16` 18 ms.

### Candidates

| candidate | verdict | expected gain |
|---|---|---|
| Cranelift JIT of the settle | Most promising for the tick loop, native only (wasm cannot JIT this way; it would need a wasm-module backend). Removes dispatch and operand-index loads and keeps intermediate values in registers. Costs: a large dependency, compile time on every edit (now 11 ms for `cpu16`; JIT on top), a second execution path to keep identical. Prototype behind a feature on `cpu16_rtl`-like designs first. | `cpu16_rtl` and other word-level designs 3-5x (dispatch and index loads are most of their time); `cpu16` 1.5-2.5x; little for designs beyond L2, which are memory-bound. Estimate. |
| `std::simd` for packed ops | Not now. The op is under 1% of packed time; 87% is assembling operands. If anything, speed up gathers (BMI2 `pext`/`pdep`, or a lane assignment that needs fewer gathers). | Under 1% from SIMD ops; gathers bound the packed path. |
| mimalloc | No. The tick loop allocates nothing; malloc/free are under 2% of an edit. | Under 2% on edits, 0 on ticks. |
| foldhash / rustc-hash for app `HashMap`s | Low priority, but cheap: Bevy already re-exports a foldhash map (`bevy::platform::collections::HashMap`), so no new dependency is needed. | 2-4% of edit and undo latency. |
| rayon / a different barrier | Not rayon in the simulator (no threads on wasm, and a join costs about a microsecond against phases of about 5 µs here: 1.58 ms over 290 phases). Fix the pool first: cap threads at idle cores, fall back to one thread when a threaded settle measures slower, and hand out work dynamically within a phase (an atomic cursor) so a descheduled worker's share is taken by the others. | Turns the 0.01-0.6x seen here back into at least 1x; the gain on an idle machine needs measuring there. |

Other follow-ups, by expected value:

1. **Routing's priority queue** (app): the A* costs are small integers,
   so a bucket queue (Dial's algorithm) can replace `BinaryHeap`; and a
   change of part kind that keeps pins could keep every route. Expected:
   edit and undo latency on `cpu16` down by a third or more (40% of their
   samples are in the heap).
2. **Stream the literal-field ops** (`Shr`, `Concat`, `Sext`): put the
   shift or width in the operand stream or a uniform immediate, as for the
   others. Expected 10-15% on `cpu16` (34% of its time is in these
   kernels at about 1.9x the per-instruction cost of streamed ones) and
   more on word-level designs.
3. **Commit copies as a slice**: split `Commit::Copy` from the rest into
   a `(state, next)` array copied in one loop. Expected 4-6% on `cpu16`.
4. **Fewer, longer blocks for word-level designs**: designs whose ranks
   hold one instruction per op (`cpu16_rtl`) pay a dispatch per
   instruction. A mixed-op block with a per-instruction opcode (a small
   interpreter loop) for short runs would cut the 27% dispatch share.
   Expected 1.2-1.4x on `cpu16_rtl`.
5. **Re-measure threads and cache behaviour on an idle machine with
   hardware counters** (`cargo xtask perf stat`, `perf bless
   --instructions`), which this VM cannot provide.

Flamegraphs of these runs were not committed; they are reproduced by the
`cargo xtask perf flamegraph bin:perf-metrics -- ticks ...` commands
above (and `bench:app/bridge/edit`, `bench:app/history` for the edit
path).

## Findings, 2026-11-23: after #117 stages 1-4, and the JIT (stage 5)

### Fresh profile of the interpreter

Same machine and method as above (load average 1-4 during the runs),
`perf record -F 997 --call-graph fp` of `perf-metrics ticks <case>
3000000` (tick by tick, inputs changing), `profiling` build. The programs
are now small: `cpu16` 302 instructions, two clock-phase variants of
225/226 instructions plus 39 commits each (264.5 ops per tick, 154 in
fused batches); `cpu16_rtl` 135 instructions, 15 commits.

| share of samples | `cpu16` | `cpu16_rtl` |
|---|---|---|
| `code::exec` (all kernels, inlined) | 74% | 81% |
| of which block dispatch (`exec` prologue, form match, range set-up) | ~19% | ~22% |
| of which `Mux` kernel | ~15% | ~14% |
| of which other streaming kernels (operand index loads, stores) | ~20% | ~15% |
| of which full-`Instr` kernels (`Shr`, `MemRead`) | ~6% | ~8% |
| `Sim::evaluate` (block loop) | 8% | 9% |
| `Sim::commit` | 12% | 4% |
| `Sim::tick` and the rest | 6% | 6% |

So per tick about 0.30 µs (`cpu16`) and 0.23 µs (`cpu16_rtl`), i.e. 1.1
and 1.5 ns per op: dispatch per block (median block length 1 on
`cpu16_rtl`), operand index loads and the match per commit are most of
the time, not the operations. That is what a JIT removes.

### The JIT (`sim/jit/`, feature `jit`)

Each body (program, phase variant, fused-run body) becomes straight-line
Cranelift code in chunks of 64 instructions; within a chunk each slot is
loaded once and results stay in registers; literal fields are
immediates. Measured with `perf-metrics jit <case> <options> 6` (best of
3; "tick" is `Sim::tick` with inputs changing every tick, "run" is
`run(1000)` batches; the CPU columns run the program from reset to
`Halted`, as `cpi`). The binary has the `counters` feature, as the
numbers of #132.

| case | instrs | interp tick | JIT tick | interp run | JIT run | speed-up tick / run | JIT compile |
|---|---|---|---|---|---|---|---|
| `cpu16` default | 302 | 0.296 µs | 0.071 µs | 0.176 µs | 0.039 µs | 4.2x / 4.5x | 3.3 ms |
| `cpu16` editor | 333 | 0.337 µs | 0.083 µs | 0.175 µs | 0.040 µs | 4.1x / 4.3x | 3.6 ms |
| `cpu16_rtl` default | 135 | 0.213 µs | 0.044 µs | 0.211 µs | 0.042 µs | 4.9x / 5.1x | 0.7 ms |
| `cpu16_rtl` editor | 137 | 0.221 µs | 0.042 µs | 0.222 µs | 0.036 µs | 5.2x / 6.1x | 2.1 ms |
| `testkit_cpu` | 29 | 0.075 µs | 0.018 µs | 0.076 µs | 0.017 µs | 4.1x / 4.5x | 0.2 ms |
| `dag32_10k` | 251 | 0.608 µs | 0.086 µs | 0.013 µs | 0.015 µs | 7.1x / 1x | 1.2 ms |
| `dag32_1m` | 819 | 1.720 µs | 0.296 µs | 0.014 µs | 0.014 µs | 5.8x / 1x | 4.3 ms |
| `gates1_100k` | 23 009 | 18.9 µs | 11.7 µs | 0.031 µs | 0.025 µs | 1.6x / - | 68 ms |
| `gates1_1m` | 101 113 | 77.6 µs | (interpreted) | | | 1x | - |

Per CPU instruction (program run from reset to `Halted`):

| | interp tick | JIT tick | interp run | JIT run |
|---|---|---|---|---|
| `cpu16` | 1.66 µs | 0.40 µs | 1.00 µs | 0.22 µs |
| `cpu16_rtl` | 0.216 µs | 0.042 µs | 0.224 µs | 0.042 µs |

- The clockless DAGs have nothing to do inside a batch (fusion prunes
  every intermediate tick to 0 ops), so their "run" column is the call
  overhead either way.
- `gates1_1m` with the JIT was 432 µs/tick, 5.6x *slower*: about 2 MB of
  native code for 101k one-bit gates misses the instruction caches,
  while the interpreter streams 4-byte operands. Bodies over 32k
  instructions (`jit::MAX_BODY`) therefore stay interpreted.
- Chunk size: with chunks of 4096, `gates1_100k` compiled in 1.2-1.4 s and
  ticked *slower* than with 64 (17.5 against 11.9 µs): keeping every
  loaded slot in an SSA value across a long function makes long live
  ranges that spill, and register allocation is superlinear. 64 is as
  fast as larger chunks on `cpu16` in batches and faster tick by tick.
- Cranelift's optimizer (`opt_level = speed`) gave no measurable speed on
  these already simplified programs for about 25% more compile time; the
  IR verifier was another ~10% (on in debug builds only).
- Compile time is about 10 µs per instruction (all bodies: `cpu16`
  compiles its two variants and two fused bodies). Tiering in the engine
  keeps it off the tick loop anyway: an edit starts interpreted and
  switches to native code when the compile thread has it.

Cost of the dependencies (`cranelift-codegen`, `-frontend`, `-jit`,
`-module`, `-native` 0.138.0), measured with `sim_report` in the
`release` profile (fat LTO, one codegen unit), stripped: the binary grows
from 1.6 MB to 4.7 MB; a clean build of logi-sim and its dependencies
from 32 s to 123 s; rebuilding after touching logi-sim from 31 s to 76 s
(LTO runs over Cranelift too). Only with `--features jit`; without it
nothing of Cranelift is built.
