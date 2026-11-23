# logi-sim architecture

`logi-sim` is the digital logic simulator behind GateQuest. It is a
synchronous, word-level, compiled simulator: a design is compiled into a
flat program of word operations sorted by rank, and every tick commits state
and then settles all combinational logic exactly once. It has no Bevy
dependency and runs the same code on native targets and on wasm.

The full design, with the reasons behind each decision and every point
where the implementation deviates from it, is `docs/sim-v2-design.md`
(section 13 lists the deviations). This file is the map of the crate.

```text
 netlist::Design  ──compile()──▶  compile::Program  ──Sim::new()──▶  sim::Sim
 (authoring IR:                   (flat slots, blocks,               (tick: commit,
  definitions,                     commits, memories,                 then settle;
  instances, nets)                 ports, debug map)                  probes, state)
        │                                                                 ▲
        └──────────── reference::Reference (oracle, tests only) ──────────┘
                                                                          │
                     engine::{EngineCore, Engine, LocalEngine} ───────────┘
                     (pacing, batches, probes, program swaps)
```

## Model

- **Synchronous.** A circuit is a combinational DAG plus explicit state
  elements (`Delay`, `Register`, `DLatch`, `SrLatch`, `Ram`). A
  combinational loop that does not pass through state is a compile error
  naming the nodes on it.
- **The tick.** `tick()` commits every state element (and RAM writes), then
  settles all combinational logic once, in rank order. After a tick every
  net is consistent with the inputs and state. There are no partial ticks
  and no dependence on node creation order. An input set between ticks
  reaches combinational logic at the next settle and state at the commit
  after that.
- **Word-level values.** Every net is 1 to 64 bits wide and stored in one
  `u64`. A classic gate is the 1-bit case. Values are two-state (no X/Z),
  with one driver per net; unconnected inputs read 0.
- **Shared definitions.** A subcircuit is a definition referenced by
  instances, not a copy.

## Modules

### `netlist`: authoring IR

A `Design` is a slotmap of `Definition`s plus a root. A definition holds
`Node`s (a `NodeKind` plus fixed-position input pins, each an
`Option<NetId>`) and `Net`s (each owned by the output pin that drives it).
Port order is the explicit `inputs`/`outputs` lists. Every mutation goes
through `Design` methods, which keep nets, drivers, instance pins and each
definition's `revision` consistent; `validate` checks widths, pin counts
and instance recursion. `Builder` adds composites (n-input gates, `mux_n`,
`decoder`, `demux`, `counter`, bus expand/collect, ...) built from the
primitives. Names live in a side table.

### `compile`: `Design -> Program`

`compile(&Design, &Options)` runs, all iteratively:

1. **Validate** the design.
2. **Elaborate** (`elaborate.rs`): flatten the instance tree, unifying port
   nets with the parent's by union-find, so ports cost nothing. Every flat
   node keeps its instance path (`NodePath`).
3. **Loop check** (`loops.rs`): iterative Tarjan SCC over combinational
   edges; each strongly connected component is one
   `CompileError::CombinationalLoop`.
4. **Function matching** (`func/`, `Options::func_match`): each
   subcircuit definition is fingerprinted by random simulation, matched
   against a catalogue of word functions (mux, add/sub with carries,
   compare, bitwise, constants, registers built from gate latches or
   flip-flop chips, grouped into word registers) and proven (exhaustively,
   per bit slice, or as a carry chain of proven adders). Instances of a proven definition lower to the word
   implementation; their internal nets stay readable through shadow slots
   (`Program::shadows`), evaluated from the instance's inputs and state on
   read. Results are cached by a BLAKE3 content key (`func::MatchCache`,
   held by the engine), so an edit re-analyses only what it changed.
5. **Lower and simplify** (`simplify.rs`): constant folding, alias
   elimination, CSE and dead-code elimination, each switchable in
   `Options`. DCE keeps root ports, state and `Options::probes`;
   `Options::debug()` turns it off so every net stays observable.
6. **Word lifting** (`lift.rs`, `Options::lift`): bus-aligned groups of
   1-bit gates and 1-bit state become word ops and state words; lifted
   nets stay readable as views (`Program::views`).
7. **Word-level cleanup** (`words.rs`, `Options::words`): the program is
   rebuilt as a hash-consed expression graph with exact word rewrites
   (slice and concatenation round trips, ops merged across bit ranges,
   complemented selects, double complements) and state words that are
   only read together (two 8-bit registers forming a 16-bit one, the
   master and slave latches of a word of flip-flops) are merged into one
   word. Nets and state elements that lose their slot become views; the
   result is kept only if it is cheaper.
8. **Levelize and emit** (`emit.rs`): rank every instruction, sort by
   `(rank, op)` into blocks, and number slots (`Options::slot_order`).
9. **Pack** (`pack.rs`, `Options::pack`): optionally pack runs of 1-bit
   instructions into the 64 lanes of a word.
10. **Partition** (`Options::partitions`): optionally lay blocks out per
   thread.
11. **Clock phases** (`phases.rs`, `Options::clock_phases`): a `Clock` is
   a function of the tick count alone, so when the clocks' common period
   `L` is at most 8, the program gets one variant per tick residue
   (`Program::phases`) with every clock folded to its constant there.
   Muxes on a constant select become their taken arm, a commit that would
   copy an unchanged value (a latch whose enable is 0 in that phase) is
   dropped, RAM writes whose enable is 0 are dropped, and what no exposed
   slot and no commit of the variant needs is not computed. Variants share
   every slot, the debug map, views, shadows and state keys with the
   unspecialized program: only the instruction and commit lists differ.

`Program` (`program.rs`, which documents the execution model in full) holds
the slot count, constants, blocks and instructions, packed words, commits,
memory descriptors, the root port map, the `DebugMap` from
`(instance path, NetId)` to slot or packed lane, and one `StateKey` per
state element.

### `sim`: runtime

`Sim` executes a `Program`. `Sim::try_new` checks every index once, so the
inner loops skip bounds checks, and lowers the program into its own
streaming encoding (`code.rs`), in which most blocks need no destination
operands. Dispatch happens once per block; each block is a branch-free pass
over one opcode. A tick allocates nothing.

- Inputs and outputs by port index or name (`set_input`, `set`, `output`,
  `get`); any observable net by `read_net(path, net)`.
- `export_state`/`import_state` carry register, latch and RAM contents
  across recompiles, keyed by `NodePath`; unmatched elements reset.
- `SimOptions::threads` (default 1) starts a persistent worker pool
  (`pool.rs`) for large ranks on native targets. With one thread, or on
  wasm, no pool exists and no atomics run.
- **Clock-phase variants.** Single-threaded, the settle of tick `t` (and
  the commit after it) runs variant `t % L` of `Program::phases`. With a
  pool, the unspecialized program runs.
- **Tick fusion** (`SimOptions::fuse`). Nothing can observe the slots
  between two ticks of one `Sim::run(n)` (inputs are constant within it,
  and probes, reads and memory copies happen between calls), so every
  tick but the last settles only the cone of the commits that follow it
  (next-state values and RAM write ports). State, memories, memory
  versions and the tick count evolve exactly as tick by tick; the last
  tick settles everything. `Sim::tick` always settles everything.

- **JIT** (`sim/jit/`, feature `jit`, native only; `SimOptions::jit`,
  off by default). Cranelift compiles each body the `Sim` runs (the
  program's own, each clock-phase variant, each fused-run body) into
  straight-line native settle chunks of at most `jit::CHUNK`
  instructions, `fn(slots, mems, tick)`, plus commit chunks. Within a
  chunk every slot is loaded once and every result stays in an SSA value;
  literal fields are immediates; results are stored to their slots (in a
  fused-run body only those its commits or a later chunk read). It runs
  single-threaded only (a pool runs the interpreter), skips bodies no tick
  uses or larger than `jit::MAX_BODY`, and leaves a body with a `MemClear`
  commit to the interpreter's commit. `Sim::compile_jit` builds the code
  without touching the `Sim`'s state and `Sim::install_jit` swaps it in at
  any time, which the threaded `Engine` uses to tier: an edit runs
  interpreted at once and native when the compile thread is done.
  `testkit::diff::check_jit` compares it with the interpreter every tick.

### `engine`: pacing, probes and edits

- `EngineCore` owns the `Sim`: tick-rate pacing (`Rate`), single steps,
  batches sized by a time budget, probe readback into a dense `ProbeFrame`,
  and program swaps that carry state and root inputs across an edit. Its
  clock is a parameter, so it is the same on native, on wasm and in tests.
- `Engine` (native) runs a core on a worker thread and compiles new design
  revisions on another thread, off the tick loop. Frames are published by
  swapping an `Arc`.
- `LocalEngine` runs a core in the caller's thread; the app's wasm build
  calls `run_frame` once per frame.
- A compile error keeps the previous program running and is reported as
  `EngineEvent::CompileFailed`.

### `reference`: the oracle

`Reference` interprets the hierarchical `Design` directly: commit, then
re-evaluate every node until nothing changes. It is slow, simple, and the
oracle every compiled result is tested against.

### `testkit`: test and benchmark inputs

A seeded RNG (`rng`), a random design generator (`random`: acyclic
combinational logic, state, nested instances, mixed widths, optional
injected loops), hand-built fixtures (`fixtures`: adders including a
four-level nested adder, mux trees, counters, a register file, a small CPU
with RAM that runs a program, gate-loop latches that must fail to compile),
a bit-blaster that rebuilds word-level designs from 1-bit gates
(`bitblast`), and the differential harness (`diff`).

## Testing

- `cargo test -p logi-sim` runs the unit tests of every module, including
  the differential sweep (`sim::tests::differential_random_designs`, about
  1,000 random designs, compiled `Sim` against `Reference` on every root
  output and every observable net, every tick).
- `cargo test -p logi-sim --release --lib -- --ignored differential_10k`
  is the acceptance sweep over more than 10,000 designs; `--ignored`
  also runs the other long sweeps.

## Benchmarks

- `cargo run -p logi-sim --release --bin sim_report` prints the full report
  (`-- --quick` for a short one). See `PROFILING.md` for profiling.
- `cargo bench -p logi-sim --bench sim` runs the Criterion benchmarks.

Every throughput case changes the root inputs every tick or runs real
sequential logic, and counts scalar instructions plus packed lanes.

### Final comparison with the legacy runners

Measured on 2026-11-20 on the 4-vCPU benchmark VM (Intel Xeon @ 2.10 GHz),
release build, with `sim_v2_report` at commit `d6c1bd7`, immediately
before the legacy runners were deleted. The legacy baseline is the legacy
SIMD runner (the fastest legacy runner on these designs) running the same
random 1-bit gate DAGs (`Not` and the 2-input bitwise gates), converted one
to one into the legacy `Circuit`. `Sim` is single-threaded with the default
layout and packing, simplify passes off.

| 1-bit gate DAG | Sim | Legacy SIMD | Speedup |
| ---: | ---: | ---: | ---: |
| 10,000 gates | 918 M gates/s | 99.4 M gates/s | 9.2x |
| 100,000 gates | 1,058 M gates/s | 71.1 M gates/s | 14.9x |
| 1,000,000 gates | 656 M gates/s | 62.2 M gates/s | 10.6x |

Acceptance gates (design doc section 9): gate 2, single-core throughput on
the 1M-gate DAG at least 3x the legacy SIMD runner, measured 10.55x; gate 3,
compiling 1M nodes in under 2 s, measured 1.08 s; gate 1, the 10k
differential sweep, passes.

Other results from the same run (single thread unless noted):

| Case | Result |
| --- | --- |
| 32-bit word-level DAG, ~10k / ~100k / ~1M nodes | 658 / 431 / 335 M instr/s |
| CPU fixture (looping program), word-level | 8.56 M cycles/s (37 instructions) |
| CPU fixture, bit-blasted | 1.42 M cycles/s (380 instructions) |
| Generated 32-bit stateful datapath, word-level / bit-blasted | 627 / 666 M instr/s |
| 1M-gate DAG, 4 threads | 946 M gates/s (1.40x one thread) |
| ~1M word-level DAG, 4 threads, partitioned | 623 M instr/s (1.95x one thread) |
| Compile 1M-gate DAG / ~2.3M-node word DAG (all passes) | 1.08 s / 2.46 s |
| Engine, unlimited rate, 100k gates | 13,443 ticks/s (`Sim::run`: 11,211) |
| Engine probe publish, 1,000 probes | 1.6 us per frame |

The legacy runners are gone, so `sim_report` no longer prints the legacy
column or checks gate 2.
