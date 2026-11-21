# Simulation v2: synchronous, word-level, compiled

Status: implemented; all milestones (M0 to M6) are done. M6 (#31) deleted
the legacy circuit, compiler, runners and engine. `logi-sim/ARCHITECTURE.md`
describes the crate as built and records the final benchmark comparison
with the legacy runners; `docs/app-simulation-bridge.md` describes how the
app maps onto the simulator. Sections 1 to 12 are the design as proposed;
section 13 lists where the implementation deviates from it.

## 1. Why

An audit of the current simulator (November 2026) found that the inner loops
are not the main problem. The model is:

- **"Tick" has no single meaning.** Naive/Flat/Threaded sweep gates in creation
  order, so a gate has zero or one tick of delay depending on when it was
  placed. EventDriven drains an unranked FIFO and stops after 10,000
  evaluations (`logi-sim/src/runner/event_driven.rs`, `drain_queue`), so its
  tick is an arbitrary slice of work. Only the SIMD runner orders gates
  topologically, and only for acyclic logic.
- **Sequential circuits cannot reach the fast path.** `build_runner` forces
  EventDriven for anything with a feedback cycle or counter. State is built
  from raw gate loops; there is no register, RAM or ROM primitive.
- **Everything is one bit.** A 64-bit add is hundreds of gate evaluations.
- **The compiler does not levelize**, detect loops, fold constants or unify
  nets, and its adder/counter lowering matches only exact fixture shapes.
- **Every edit recompiles the whole circuit** and subcircuits are owned by
  value per instance.

Measured on a 4-vCPU VM, release build: Flat 25–34M gate-evals/s and SIMD
67–108M on mixed-op random DAGs; EventDriven needed 6,687 ticks to settle one
input change on a 1M-gate DAG.

## 2. Decisions

| # | Decision |
|---|---|
| D1 | **Synchronous model.** A circuit is a combinational DAG plus explicit state elements. |
| D2 | **Combinational loops are a compile error**, reported with the nodes on the cycle. A loop is legal only if it passes through a state element. |
| D3 | **Word-level values.** Every net has a width of 1–64 bits, stored in one `u64`. A classic gate is the 1-bit case. |
| D4 | **Two-state.** No X or Z. One driver per net. |
| D5 | **Shared definitions.** A subcircuit is a definition referenced by instances, not a copy. |
| D6 | **One compiled runner.** Naive, Flat, ThreadedFlat, EventDriven and the current SIMD runner are deleted once v2 passes its gates (section 9). |

### Game-design consequence of D2

Players can no longer build an SR latch from two cross-coupled NORs directly.
Two things soften this:

- `Delay` is a primitive. A feedback loop that passes through a `Delay` is
  legal, so "build memory out of gates" survives as "gates plus one delay".
- `DLatch`, `SrLatch` and `Register` are provided as primitives.

## 3. Semantics

### 3.1 Values

A value is a `u64` masked to the net's width. Unconnected inputs read 0.
Width mismatches between a driver and a sink are a validation error; widths
are converted only by explicit `Split`, `Merge`, `Extend` nodes.

### 3.2 The tick

There is one implicit global clock. `tick()` does exactly this:

1. **Commit.** Every state element takes the next-state value computed by the
   previous settle. RAM writes are applied.
2. **Settle.** All combinational logic is evaluated once, in rank order, from
   the current inputs and the just-committed state.
3. `tick_count += 1`.

After a tick, every net is consistent with the inputs and state, so the editor
never displays a half-propagated circuit. Building a `Sim` performs one settle
with all state at its reset value (tick 0). An input changed between ticks is
visible to combinational logic at the next settle and to state at the commit
after that.

There are no partial ticks, no evaluation caps and no dependence on node
creation order.

### 3.3 Primitives

Combinational (bitwise ops apply across the full width):

| Kind | Pins | Notes |
|---|---|---|
| `Input{w}`, `Output{w}` | | definition ports |
| `Const{w, value}` | → y | |
| `Not`, `And`, `Or`, `Xor`, `Nand`, `Nor`, `Xnor` `{w}` | a, b → y | 2-input; wider gates are lowered by the builder |
| `Mux{w}` | sel(1), a, b → y | `sel ? b : a` |
| `Switch{w}` | en(1), d → y | `en ? d : 0`; replaces `TriStateBuffer` |
| `Add{w}` | a, b, cin(1) → sum, cout(1) | |
| `Eq{w}`, `LtU{w}` | a, b → y(1) | |
| `ReduceOr/And/Xor{w}` | a → y(1) | |
| `Split{w, lo, len}` | a → y(len) | bit slice |
| `Merge{widths}` | a0.. → y | concatenation, low first |
| `Extend{from, to, signed}` | a → y | |
| `Clock{period, duty, phase}` | → y(1) | pure function of `tick_count` |
| `Rom{addr_bits, w}` | addr → data | combinational read |
| `Instance(DefId)` | per definition ports | |

State (output depends on committed state; next state is computed at settle):

| Kind | Pins | Next state | Output |
|---|---|---|---|
| `Delay{w}` | d → q | `d` | state |
| `Register{w}` | en(1), d → q | `en ? d : state` | state |
| `DLatch{w}` | en(1), d → q | `en ? d : state` | `en ? d : state` (transparent) |
| `SrLatch` | s, r → q | `r ? 0 : s ? 1 : state` | state |
| `Ram{addr_bits, w}` | addr, we(1), wdata → rdata | write at commit | `mem[addr]` (combinational read) |
| `Ram2{addr_bits, w, reads, mask}` | waddr, we(1 or w), wdata, raddr0.. → rdata0.. | write at commit (`mask`: the bits set in `we`) | `rdata_k = mem[raddr_k]` (combinational read) |

Combinational dependency edges for loop detection and ranking are: every
input→output of a combinational node, `DLatch` en/d→q, `Ram`/`Rom`
addr→data, and `Ram2` raddr_k→rdata_k (each read address feeds only its
own read data). `Delay`, `Register` and `SrLatch` outputs have no
combinational dependency on their inputs.

`Counter8`/`Counter64`/`BusExpand`/`BusReduceOr`/`Demux` are not primitives;
the builder provides them as helpers composed from the table above.
Later additions (`Sub`, shifts, `Mul`, n-way mux, bus-conflict detection
for `Switch`) are additive and do not change the model; the dual-port RAM
was added this way (`Ram2`, section 13 item 70).

## 4. Authoring IR (`logi_sim::netlist`)

```rust
pub struct Design {
    pub defs: SlotMap<DefId, Definition>,
    pub root: DefId,
}

pub struct Definition {
    pub name: String,
    pub nodes: SlotMap<NodeId, Node>,
    pub nets: SlotMap<NetId, Net>,
    pub inputs: Vec<NodeId>,   // explicit, ordered port lists
    pub outputs: Vec<NodeId>,
    pub revision: u64,         // bumped on every mutation
}

pub struct Node {
    pub kind: NodeKind,
    pub inputs: SmallVec<[Option<NetId>; 4]>,  // None = unconnected = 0
    pub outputs: SmallVec<[NetId; 2]>,         // each output pin owns a net
}

pub struct Net { pub width: u8, pub driver: (NodeId, u8) }
```

Rules:

- A net is created with its driving output pin and lives as long as it does.
  Fan-out is many input pins naming the same `NetId`. An editor "wire" is
  identified by its sink `(NodeId, pin)`.
- Port order is the explicit `inputs`/`outputs` vectors, never slotmap
  iteration order.
- Pins are fixed-position; an unconnected pin never shifts its neighbours.
- No per-pin `String`s. Names live in an optional side table.
- Instance recursion (a definition that contains itself, directly or not) is a
  validation error.
- Mutations go through methods that keep nets, drivers and `revision`
  consistent, including node removal (which disconnects its sinks).

## 5. Compiler (`logi_sim::compile`)

`compile(&Design, &Options) -> Result<Program, Vec<CompileError>>`

1. **Validate.** Widths, pin counts, instance recursion.
2. **Elaborate.** Flatten instances to a single node list. Each flat node
   records its instance path (`[NodeId]` of enclosing instances) for
   diagnostics, probes and state carry-over. Port nets are unified with the
   parent's nets by union-find; no copy instructions are emitted for ports.
3. **Simplify.** Constant propagation and folding, `Buffer`/identity/alias
   elimination, common-subexpression merging by structural hash, dead-code
   elimination. Under `Options::debug` DCE is off and every authored net stays
   observable; otherwise the live set is root ports, state, and registered
   probes.
4. **Loop check.** Iterative Tarjan SCC over combinational edges. Any SCC with
   more than one node, or a self-edge, yields
   `CompileError::CombinationalLoop { nodes: Vec<NodePath> }`.
5. **Levelize.** Rank = 1 + max rank of combinational predecessors; inputs,
   constants and non-transparent state outputs are rank 0.
6. **Emit.** Instructions sorted by `(rank, opcode)` and grouped into blocks.

All passes are iterative (no recursion on circuit depth) and near-linear.

```rust
pub struct Program {
    pub slot_count: u32,            // one u64 per surviving net
    pub blocks: Vec<Block>,         // { op: Op, start: u32, len: u32 }
    pub instrs: Vec<Instr>,         // { dst: u32, a: u32, b: u32, c: u32 } + per-op mask
    pub commits: Vec<Commit>,       // state slot <- next slot, per kind
    pub mems: Vec<MemDesc>,         // RAM/ROM geometry and initial contents
    pub ports: PortMap,             // root inputs/outputs -> slots
    pub debug: DebugMap,            // (instance path, NetId) <-> slot
    pub state_keys: Vec<StateKey>,  // (instance path, NodeId) per state element
}
```

Dispatch happens once per block, so the inner loop for each opcode is a
branch-free pass over a contiguous instruction slice. Results are masked only
for ops that can set bits above the width.

## 6. Runtime (`logi_sim::sim`)

```rust
pub struct Sim { /* program, values: Vec<u64>, mems, tick */ }

impl Sim {
    pub fn new(program: Arc<Program>) -> Self;        // performs the tick-0 settle
    pub fn set_input(&mut self, port: PortId, value: u64);
    pub fn tick(&mut self);
    pub fn run(&mut self, ticks: u64);
    pub fn output(&self, port: PortId) -> u64;
    pub fn read(&self, slot: Slot) -> u64;            // via Program::debug
    pub fn tick_count(&self) -> u64;
    pub fn export_state(&self) -> StateSnapshot;      // keyed by StateKey
    pub fn import_state(&mut self, s: &StateSnapshot);// unmatched keys reset
}
```

`Sim` is allocation-free per tick and the same code on native and wasm.
By default it is single-threaded and has no locks or atomics;
`SimOptions::threads` (#41, section 13 item 34) opts into a worker pool
on native targets.

## 7. Engine (`logi_sim::engine`, rewritten)

- A worker thread owns the `Sim`. It runs batches sized by a time budget
  (about 1 ms), draining commands between batches. Pacing never resets on a
  redundant `SetTickRate`.
- **Probes instead of publish-everything.** The app registers a list of slots
  it wants to see (the nets in the open view). After a batch the worker copies
  those slots into a dense `Vec<u64>` and swaps it into a shared buffer. No
  per-wire hashing, no per-wire locking, and cost scales with what is on
  screen.
- **Edits.** The app sends a new `Design` revision; the engine compiles off
  the tick loop, then swaps `Program`s and carries state across with
  `export_state`/`import_state`. On a compile error the old program keeps
  running and the errors are sent back as an event.
- The wasm path calls the same `Sim` in-frame with a time budget.

## 8. Reference interpreter and tests (`logi_sim::reference`)

A deliberately simple interpreter over the hierarchical `Design`, with no
flattening and no optimization: commit state, then re-evaluate every node
until nothing changes, bounded by the node count (exceeding the bound is the
loop error). It is the oracle.

- **Differential tests:** a seeded random design generator (acyclic
  combinational logic, state elements, nested instances, mixed widths) drives
  random inputs for N ticks; compiled `Sim` and reference must agree on every
  root output and every debug-observable net, every tick.
- **Fixtures** are ported to the new builder: adders, mux trees, counters, a
  register file, and a small CPU with RAM that runs a program.
- **Loop diagnostics:** the old gate-loop latch fixtures become tests that
  expect `CombinationalLoop` naming the right nodes.
- **Regression tests** for the bugs found in the audit: unconnected pin does
  not shift later pins; passthrough subcircuit; port order after deletion.

## 9. Benchmarks and acceptance gates

Every benchmark changes inputs each tick or runs real sequential logic.

- Random mixed-op DAGs at 10k, 100k and 1M instructions.
- A generated word-level CPU-like design and its bit-blasted equivalent.
- The small CPU fixture, reported in clock cycles per second.
- Compile time for 100k and 1M nodes.

Gates before the legacy runners are deleted (M6):

1. Differential tests pass on at least 10,000 random designs.
2. Single-core throughput on the 1M mixed-op DAG is at least 3× the current
   SIMD runner on the same machine. (The longer-term goal is on the order of
   1e9 simple ops per second per core.)
3. Compiling 1M nodes takes under 2 seconds.

All three held when M6 deleted the legacy runners. The final measurement
(gate 2 at 10.55x, gate 3 at 1.08 s) is recorded with the date and machine
in `logi-sim/ARCHITECTURE.md`, "Benchmarks"; the legacy baseline can no
longer be re-run.

## 10. App integration (root crate)

- The ECS authoring components become the single source of truth and map
  one-to-one onto a `Design`. `GameCircuit` as a parallel snapshot, the legacy
  `GateComponent`/`ConnectionLine` data, and three of the four kind enums go.
- Subcircuits are stored once in a definition table and referenced by id.
  Opening a subcircuit is a view into a live instance (probe slots resolved
  through its instance path), not a separate simulation.
- Readback uses engine probes for visible nets. Wire and gate visuals update
  only when their value or geometry changed.
- Loop errors highlight the offending nodes and wires in the editor.
- Node and wire deletion are wired through.

## 11. Milestones

| | Scope | Depends on |
|---|---|---|
| M0 | Quick wins in the current code: tick-rate resend, default rate, `opt-level` | – |
| M1 | `netlist` IR, builder, reference interpreter, random generator, ported fixtures | – |
| M2 | Compiler: elaborate, loop check, levelize, emit; then simplify passes | M1 |
| M3 | `Sim` runtime, differential tests, benchmark suite | M2 |
| M4 | Engine rewrite: batching, probes, state carry-over, wasm path | M3 |
| M5 | App: bridge to v2, single source of truth, definition table, change-driven visuals, loop UX, deletion | M4 |
| M6 | Delete legacy circuit/compiler/runners, rewrite `ARCHITECTURE.md` | M3 gates, M5 |

v2 is built beside the existing modules so the app keeps running until M5.
Bugs in the legacy runners are not fixed, since M6 deletes them.

All milestones are complete. M6 (#31) deleted the legacy `circuit`,
`compiler`, `runner`, `engine`, `fixtures` and `graphviz` modules with
their tests, benches and the `profile_tick` binary, and ported the legacy
tests whose behaviour had no v2 test (item 36).

## 12. Deferred

Considered and deliberately left out of v2; each is additive later.

- Per-definition compile memoization and incremental recompilation.
- Packing many 1-bit gates of one op into the lanes of a single `u64`
  (done in #41, section 13 item 33).
- Activity gating (skipping regions whose inputs did not change).
- Partitioned multithreading (done in #41, section 13 items 34 and 35).
- Native JIT (not available on wasm).
- Multi-driver buses with conflict detection; X/Z values.

## 13. Deviations

Choices made while implementing M1 where this document was silent or
ambiguous. Each is small and can be revisited.

1. **Clock timing.** Section 3.2 lists `tick_count += 1` after the settle, but
   `Clock` is a function of `tick_count`. The implementation fixes it as: the
   settle of a tick sees the *new* count, so whenever `tick_count() == t` the
   clock reads `((t + phase) % period) < duty`. The tick-0 settle uses `t = 0`.
2. **`Not` has one input** (`a → y`). The table groups it with the 2-input gates.
3. **Memory contents live in the node kind.** `Rom { addr_bits, w, contents }`
   and `Ram { addr_bits, w, init }` hold an `Arc<[u64]>`. Missing words are 0,
   stored words are masked to `w`, and `addr_bits` is limited to 1..=20.
   Address pins must be exactly `addr_bits` wide.
4. **`SrLatch` with both inputs high resets** (`r ? 0 : s ? 1 : state`, as in
   the table). Reset values of all state elements are 0, or `init` for RAM.
5. **The mutation API is on `Design`, not `Definition`.** Adding or removing a
   port node has to update every instance of that definition, and those
   instances live in other definitions. Removing a port drops that pin from
   every instance. The other pins keep their connections, so they stay on the
   same ports, and later pins shift down by one. Adding a port appends an
   unconnected pin. The struct fields stay `pub` for reading. Code that writes
   them directly should call `Design::validate`.
6. **Names.** Each definition has a `names: SecondaryMap<NodeId, String>` side
   table. Port names are the names of the port nodes.
7. **The reference interpreter also checks loops structurally.** Settling by
   value cannot see every combinational loop. For example, cross-coupled NORs
   reach a fixed point under in-order evaluation. So `Reference::new` runs a
   per-definition cycle check over the combinational edges in 3.3. Each
   instance contributes its definition's input→output paths. A cycle is
   `CombinationalLoop { def, nodes }`, naming the nodes on the cycle in the
   definition where it closes. A full `NodePath` is left to the M2 compiler.
   The bounded settle is kept and reports `DidNotSettle`, which the structural
   check should make unreachable.
8. **Reference net reads take `(instance path, NetId)`.** `NetId`s are
   per-definition slotmap keys, so the caller must pass a `NetId` that
   belongs to the definition at that path. A key from another definition can
   resolve to an unrelated net.
9. **Test support lives in `logi_sim::testkit`** (`rng`, `random`, `fixtures`).
   It is a public module so that M3 benchmarks can use it. The legacy gate-loop
   latch fixtures are `LoopFixture`s that record the nodes expected on the
   loop.
10. **Program execution model (M2).** `logi_sim::compile::program` documents
    it in full. Points the doc leaves open:
    - **Fixed slots.** Slot 0 is always zero. Unconnected pins and undriven
      nets read it. Constants live in `Program::consts`, are loaded once and
      are never written.
    - **Next state is computed during the settle.** Each state element gets
      a `next` slot that only commits read. A RAM gets snapshot slots for
      `we`, `addr` and `wdata`, also read only by commits. This keeps the
      rule in 3.2: an input change reaches state at the commit after the
      next settle. Root-input writes are staged and applied after the
      commit, at the start of the settle.
    - **Instruction encoding.** `Instr { dst, a, b, c, imm }`; each op
      documents which fields are slots and which are literals.
    - **Lowering.** `Add` becomes `AddSum` plus `AddCarry`. `Merge` becomes
      a chain of `Concat`s through temporary slots. `DLatch` gets a hidden
      state slot, and its output is a `Mux`.
    - **Blocks** also record their rank.
11. **Loop check runs before unification.** It runs on the per-instance net
    graph, where port crossings keep their direction. There is one
    `CombinationalLoop` error per strongly connected component, so a loop
    inside a definition instanced *k* times is reported *k* times, each with
    full `NodePath`s. The nodes listed are the primitives in the component
    (the component can contain more than one simple cycle). A loop made only
    of wires through instance ports reports the instance nodes it crosses.
12. **`compile` returns every validation error** (wrapped as
    `CompileError::Invalid`) or every loop. `Options::debug` exists but has
    no effect until the simplify passes (#26) land; until then every
    authored net is observable.
13. **State carry-over (M3).**
    - **Snapshot contents.** `StateSnapshot` carries the tick count, one word
      per state element keyed by `NodePath`, and each RAM's contents with its
      geometry.
    - **Import.** `import_state` restores the tick count and masks words to
      the current width. Unmatched elements reset, and so does a RAM whose
      `addr_bits` or width changed. It then re-settles, so nets are
      consistent immediately.
    - **Root inputs** are not part of the snapshot; the engine (M4) reapplies
      them.
    - **`StateKey` has a `width` field**, which the import needs for
      masking.
14. **`Sim::try_new` validates every index in the program.** This lets the
    per-block loops skip bounds checks. `Sim::new` panics on a malformed
    program; programs from `compile` never fail this check.
15. **Benchmark gate 2 runs on the legacy gate set.** The legacy runners
    simulate only 1-bit gates, so the "3× the SIMD runner on the 1M mixed-op
    DAG" gate is measured on a 1M-gate random DAG of `Not` and the 2-input
    bitwise gates (`GenConfig::gates`), converted one-to-one into the legacy
    `Circuit`. Word-level mixed-op DAGs are reported alongside, with no
    legacy baseline. `cargo run -p logi-sim --release --bin sim_v2_report`
    printed every case and the two gates. Since M6 the binary is
    `sim_report` and has no legacy column (item 36).
16. **Program layout for throughput (M3).**
    - **`imm` moved out of `Instr`.** `Instr` is four `u32`s (16 bytes);
      `imm` lives in the parallel `Program::imms`, read only by ops that use
      it.
    - **Slots are renumbered densely after emission:** zero, inputs,
      constants, state, then destinations in execution order.
    - **Effect.** Together these took the 1M-gate DAG from 2.7× to about 4×
      the legacy SIMD runner.
17. **Simplify passes (#26).**
    - **Where they run.** One pass in topological order over the lowered
      instructions: constant folding and constant identities, then aliases,
      then CSE, then DCE. Each is switchable in `Options`.
    - **Defaults.** `Options::default()` enables all of them;
      `Options::unoptimized()` and `Options::debug()` (DCE off) are presets.
    - **Probes.** `Options::probes` lists `(instance path, NetId)` pairs that
      DCE must keep.
    - **Commit safety.** A slot read by a commit is never aliased to a slot
      written by a commit; such an instruction stays as a copy. Commits run
      in sequence, so without this a shift register would read
      already-committed state.
    - **Folding scope.** ROM reads at a constant address fold. Clocks and RAM
      reads never fold.
    - **Benchmarks.** Throughput benchmarks compile with
      `Options::unoptimized()`, so the legacy comparison runs identical
      logic. The report's simplify section shows the reduction.
18. **Engine v2 (M4).**
    - **Location.** It was built as `logi_sim::engine_v2` beside the
      legacy `engine`; M6 deleted the legacy module and renamed it to
      `logi_sim::engine` (item 36).
    - **`EngineCore`** takes its clock as a parameter, so pacing works the
      same on native, on wasm (no `std::time::Instant` there) and under test.
      Both the threaded `Engine` and the in-frame `LocalEngine` wrap it.
    - **Inputs are addressed by root port node.** `Port` gains `node`.
      Values persist across edits and are reapplied to the new program.
    - **Compile options.** The engine compiles with `Options::debug()` by
      default, so any net can be probed without a recompile.
    - **Paced rates and stalls.** A paced rate drops debt beyond a quarter
      second instead of bursting after a stall.
    - **Probe frames.** They are published by swapping an `Arc` under one
      mutex per batch, and their buffers are reused once readers drop them.
19. **App mapping (M5, #28).** The bridge keeps one persistent `Design`
    and edits it through the mutation API as ECS components change
    (spawn, despawn, `GateType` or `WireEndpoints` change), so `NodeId`s
    are stable and the engine carries state across edits. A gate maps to
    one root node; changing its kind or width replaces that node, which
    resets its own state only. A wire maps to one connected input pin.
    Opening another workspace despawns every gate and wire and spawns new
    ones; the new nodes get fresh slotmap keys, so no state is carried
    between workspaces.
20. **`Display` is an `Output` port in the root** (it needs a sink node),
    and is left out of subcircuit definitions, where it would otherwise add
    an output pin to every instance. Subcircuit port order is the port
    nodes sorted by label, then id.
21. **Probes.** The app probes every output net of every root gate (the
    nets of the open view); probe indices are stored on the entities
    (`GateProbes`, `WireState::probe`). After a probe-list change the
    threaded bridge calls `Engine::sync` once, so a frame is never read
    with a stale probe layout. Restricting probes to the on-screen
    viewport is left to #30.
22. **Built-in examples** (superseded by the demos of #40, items 37 to
    42) keep their gate-level master-slave T flip-flops.
    Each latch's `q` reaches its own `qn` gate and the other latch
    through one `Delay`, which breaks every combinational loop (master,
    slave, and the T feedback through both). With all state starting at
    0 the counters power up at 0, so the old power-on reset pulse is gone.
23. **Instance views (M5, #29).** Opening a subcircuit does not start a
    second simulation. The view stack (`CircuitView`) holds the opened
    instance gates by their stable ids; the bridge resolves them to the
    instance's `NodeId` path and probes every output net of the definition
    at that path. Changing the view only replaces the probe list; the
    design is not recompiled. The root's gate and wire entities are parked
    with Bevy's `Disabled` marker (children included) while a view is open,
    so the editor's default queries skip them and the bridge keeps their
    nodes; closing the view unparks them, with no respawn and no state
    loss. The definition's entities are spawned from the library and
    marked `InstanceViewItem`. Inputs in a view are driven by the parent
    and refuse `SetInputValue` with a message. A `Display` inside a
    definition has no node; it shows the value of the net wired into it.
    Opening a definition from the definition table opens the first
    instance found from the root (depth first, gates in id order); if
    nothing instances it, the bare definition opens with no live values.
24. **Editing a definition (M5, #29).** Edits made in a view are written
    back to the definition in the `CircuitLibrary` (same id), so they apply
    to every instance. The bridge updates the built definition in place,
    matching nodes by document id: unchanged nodes keep their `NodeId`
    (and so their state through the engine's carry-over), a node whose
    kind changed is replaced and resets. Position-only edits do not touch
    the design. Port nodes hold no state, so when the port list changes
    (added, removed, relabelled, re-kinded) all ports of that definition
    are rebuilt; that renumbers instance pins, so every wire to an instance
    of it, in the root and in other definitions, is renumbered to stay on
    the same port (by port node id). Wires to a removed port are deleted.
25. **Definition identity and registration (M5, #29).** A definition is its
    `Uuid`; there is no structural matching. Placing from the catalog or a
    saved workspace registers the workspace root under the root's id, plus
    its definitions. A registration never replaces a definition already in
    the library (it may have been edited); only new ids are added. Loading
    a workspace replaces the library entries it contains with the file's.
    Built definitions that the root no longer reaches are removed from the
    design, so a stale one cannot fail validation.
26. **Recursion (M5, #29).** Placing definition `X` is refused, with a
    message, when `X` is the open circuit or any circuit around it in the
    view stack, or contains one of them. Registering definitions that would
    form a cycle is refused, and loading a file whose definitions form a
    cycle is refused with `WorkspaceError::Invalid`. The bridge still
    guards against cycles when building, and `Design::add_instance`
    rejects them.
27. **Change-driven visuals (M5, #30).** Gate and wire visuals live in
    `SimulationVisualsPlugin` (`src/sim/visuals.rs`), which needs no window
    or renderer, so headless tests run them. Each system reacts to change
    detection and writes a component only when the value differs:
    - **Wire geometry** is recomputed for wires whose `WireEndpoints`
      changed and for the wires of gates whose `Transform` changed. The
      gate-to-wire index is rebuilt only after structural changes (spawn,
      despawn, endpoint change, view change). A drag therefore touches the
      dragged gate's wires only, and never the `Design`: position-only
      edits create no revision (tested at the root and in an instance
      view, where the move is written back to the definition but the
      structure key is unchanged).
    - **Display text** remembers what it shows (`DisplayedValue`: value,
      width, format) and is formatted only when that changes.
    - **Idle cost.** `project_probes_system` already skips frames whose
      probe frame (revision, tick, binding generation) is unchanged. With
      the simulation paused and no input, no simulation or visual system
      touches an entity. `EntityWorkCounter`, a resource the app never
      inserts, counts touched entities for the acceptance test.
    - **Editor definition table** is rebuilt only when the root's gates or
      the library change, instead of every frame.
28. **Multi-bit wires (M5, #30)** are drawn thicker (5 px against 2 px) and
    in blue (brighter when non-zero), with a hex label (`0x` and one digit
    per 4 bits) as a child of the wire, offset from the line and kept
    upright. The label is spawned when the wire is connected and wider
    than 1 bit, and its text changes only when the value does.
29. **Loop errors (M5, #30).** `CompileFailure::loops` holds, per reported
    loop, its nodes as `LoopNode { path, gate }`: the instance gate ids from
    the root and the gate id, resolved from the compiler's `NodePath`s
    through the bridge's node maps. A loop node is shown in the open view
    when its path starts with the view's instance path: as itself when the
    path is equal, otherwise as the instance gate one level down (so at
    the root, a loop inside a subcircuit marks the instance gate). A bare
    definition view (not live) shows none. Gates get `LoopHighlight` and a
    red outline child; a wire is marked when both its ends are on the same
    loop, since every edge inside a strongly connected component lies on a
    cycle (the compiler reports components, not single cycles). The marks
    follow `SimDiagnostics`, so they clear when a later revision loads.
    The message names up to 8 gates (`cell1 / NOT "inv"`), then "and N
    more", and says the loop needs a Delay (or a Register, DLatch, SrLatch
    or Ram); the editor adds that the previous circuit keeps running.
30. **Cargo features (#30).** The default build has neither
    `bevy/dynamic_linking` nor the world inspector. `dev` enables both
    (`inspector` alone enables the inspector); `cargo dev` is an alias for
    `cargo run -p gate_quest --features dev`. The unused
    `bevy_mod_debugdump` dependency was removed.
31. **Slot order (#41).** `Options::slot_order` picks how instructions
    are ordered inside a `(rank, op)` block and how slots are numbered.
    - **`FirstUse`** (the default) lays the ranks out last to first and
      sorts each block by the position of its instructions' first readers;
      slots are still numbered in execution order. Every block keeps
      writing one contiguous run of slots, and values read together sit
      together.
    - **`Dfs`** numbers slots in depth-first post-order from the sinks
      (root outputs, then commit inputs). It measured slower everywhere
      (the 1M-gate DAG lost 40%), because blocks then write scattered
      slots. It is kept behind the switch only.
    - **`Production`** is the M3 layout (item 16). `Options::unoptimized()`
      keeps it, so older benchmarks stay comparable.
    - **Effect** (alone, single thread): 1M-gate DAG +7 to 25%, 1M
      word-level DAG +12%, bit-blasted datapath +14%, small designs
      within noise.
32. **Streaming runtime (#41).** `Sim::try_new` lowers the checked
    `Program` into its own encoding (`sim/code.rs`). In a block whose
    destinations are consecutive, which is every block under `FirstUse`
    and `Production`, `dst` is implicit and only the slot operands are
    streamed (4 bytes each, instead of a 16-byte `Instr` plus an 8-byte
    immediate). A block whose immediates are all equal stores one.
    Blocks with literal fields (`Shr`, `Concat`, `Sext`, `Clock`,
    `MemRead`) or out-of-order destinations run from the program's
    instructions as before. `SimOptions::stream` switches it off
    (`SimOptions::baseline()` is the M3 runtime). Together with
    `FirstUse` it took the 1M-gate DAG from about 265 to 460 M gates/s.
33. **Lane packing (#41).** `Options::pack` (`PackMode`) and `pack_min`.
    - **What packs.** 1-bit `Copy`, `Not`, `And`, `Or`, `Xor`, `Nand`,
      `Nor`, `Xnor`, `Mux` and `Switch` whose operands are all 1 bit. Per
      `(rank, op)` run they become `ceil(n / 64)` dense words (`Pack`),
      each with a new 64-bit slot. Lanes that read one source word are
      placed in the lanes they already occupy when at least four do.
    - **Operands.** Each operand word is
      `(slots[base] & keep) | splat | gathered bits`: lanes already in
      place in one packed word, one bit shared by many lanes (a register
      enable, a constant, a sign bit), then single bits (`Gather`). Reads
      of the zero slot are dropped.
    - **Boundaries.** A packed net that something reads as a plain slot
      (a scalar instruction, a commit, a root output) keeps its slot, and
      its word scatters that lane there (`Scatter`). So commits, ports and
      state keys still name real slots, and the commit rule (commits read
      only slots no commit writes) is unchanged.
    - **Debug reads.** A packed net with no plain slot is named in the
      `DebugMap` by a lane slot, `slot_count + 64 * k + lane`, meaning bit
      `lane` of `Program::lane_words[k]` (`Program::locate`). `Sim::read`,
      `read_net` and the `engine` probes resolve both kinds, so every
      net stays observable.
    - **Validation.** `Sim::try_new` checks every pack, gather and scatter
      slot, bit and lane, and every gather and scatter range.
    - **Modes.** `Off`; `All` packs every packable word with at least
      `pack_min` (default 8) lanes; `Aligned` packs a word only when a cost
      model says it is cheaper than its scalar instructions with everything
      in cache (about 0.9 ns per scalar 1-bit instruction or certain
      scatter, 3 ns per word, 0.3 ns per base or splat term, 1 ns per
      gathered lane); `Auto`, the default, is `All` when the slots would
      exceed `AUTO_PACK_BYTES` (4 MiB) and at least half the instructions
      are packable, and `Off` otherwise. `Options::unoptimized()` is `Off`.
    - **Effect.** A gathered lane costs about as much as a scalar
      instruction, so packing pays only when operands are in place or the
      scalar slot array no longer fits a core's L2 cache (2 MiB on the
      benchmark VM). The 1M-gate DAG gains 40 to 50% (8 MB of slots against
      128 KB of words). Everything that fits in cache loses with `All`
      (10k gates: -45%, bit-blasted CPU: -25%); `Aligned` measured neutral
      on every benchmark, because the bit-blasted ones are dominated by
      splats and by scatters to state commits. Hence the `Auto` rule.
34. **Threads (#41).** `SimOptions::threads` (default 1) and `par_min`
    (default 2048 work units, about 2 us).
    - **Pool.** `Sim::with_options` starts `threads - 1` workers once and
      keeps them for its lifetime. None start when no rank reaches
      `par_min` or on wasm, so small circuits and the default stay
      single-threaded with no behaviour change. Waiting threads spin
      about 20 us before yielding, and idle workers about 90 us before
      they sleep on a condition variable (`spin_loop` costs about 11 ns on
      the benchmark VM).
      `EngineCore::set_sim_options` passes runtime options to the engine;
      the engine and the app keep the single-threaded default unless they
      call it.
    - **One thread and wasm.** With `threads == 1` no schedule is built
      and no worker spawned; the settle checks one `Option` and takes the
      plain single-threaded loop, with no atomics. On wasm the pool module
      is replaced at compile time by an uninhabited stub, so no thread or
      synchronization code is compiled in and a threaded `SimOptions`
      runs on one thread.
    - **Schedule.** Ranks with at least `par_min` work (one unit per
      streamed instruction, three per instruction run from the full
      `Instr`, four plus gathers and scatters per packed word) become
      parallel phases, cut into equal-work pieces (blocks may be split).
      Runs of smaller ranks become serial phases on the calling thread.
    - **Synchronization.** There is no global barrier. Each thread
      publishes a progress counter after each phase in which it wrote, and
      before a phase waits only for the threads and phases whose writes it
      reads (wait lists computed with the schedule). Workers skip serial
      phases they do not depend on. A centralized barrier measured 0.5 to
      1.3 us per phase on the benchmark VM; the counters 0.4 to 0.8 us.
    - **Soundness.** The schedule is used only after checking over the
      whole settle that each slot has one writer, that no parallel phase
      reads its own writes, and that nothing reads a slot another thread
      writes in the same or a later phase. Every compiled program passes;
      any other program runs single-threaded. A cross-thread read is
      ordered after its write by the release/acquire pair on the writer's
      counter. The caller blocks until every worker's final counter, so
      the slot and memory buffers it lends out stay valid, and commits and
      input writes happen only between settles.
    - **Effect** (4 threads against 1, same program): the 1M word-level
      DAG 1.7 to 1.9x, the 1M-gate DAG about 1.3x, and the bit-blasted
      datapath about 1.0x. Ranks in the random gate DAG are narrow (median
      about 700 gates), so each synchronization guards little work; that
      choice of `par_min` came from a sweep of 256 to 8192.
35. **Partitions (#41).** `Options::partitions` (default 1;
    `Options::for_threads(n)`) lays the program out for `n` threads. Rank
    by rank, an instruction joins the partition that produced most of its
    operands, unless that partition already has its share of the rank's
    estimated cost. Each `(rank, op)` run becomes one block per partition
    (`Program::block_parts`), packed blocks first, and a runtime with that
    many threads gives thread `t` the blocks of partition `t` whole. It
    cuts cross-core reads of just-written values, which are slow on the
    benchmark VM. With 4 threads it helped the word-level DAG (up to 2.1x
    over 1 thread) but slowed single-thread runs (more and smaller blocks,
    interleaved slots) and the gate DAGs, so it is off by default.
36. **M6 (#31).**
    - **Rename.** `engine_v2` took the name `logi_sim::engine` once the
      legacy engine was deleted (item 18). The app imports it from there.
    - **Report and benches.** `sim_v2_report` became `sim_report` and
      `benches/sim_v2.rs` became `benches/sim.rs` (Criterion groups
      `sim/*`). Both lost the legacy SIMD baseline along with
      `testkit::legacy`, so gate 2 is no longer re-measured; its final value
      is in `logi-sim/ARCHITECTURE.md`.
    - **Prelude.** The crate prelude only re-exported legacy types and had
      no users, so it was removed rather than replaced.
    - **Ported tests.** Legacy tests whose behaviour no v2 test covered
      became v2 tests: exhaustive 1-bit primitive and demux truth tables on
      the compiled `Sim`, a four-level nested adder fixture
      (`testkit::fixtures::nested_adder16`, from the legacy
      `recursive_16bit_adder`, with floating gates that DCE removes), and
      CSE inside and across instances with every merged net still readable.
      Tests of legacy-only machinery (the fixture-shaped adder and counter
      lowering, SIMD record layouts, runner equivalence) were dropped.
    - **Dependencies.** `wide` and `log` and the `simd-profile` feature
      were removed. `slotmap` and `smallvec` remain (used by `netlist`).
    - **Docs.** `docs/simulation-model-and-cache.md`, which M5 had already
      rewritten for the ECS bridge, was renamed
      `docs/app-simulation-bridge.md`.
37. **Save format version 6 (#40).** Version 5 stays the expanded,
    in-memory form (`WorkspaceDocument`) and the format the editor writes.
    Version 6 is an authoring layer over it (`src/sim/source.rs`): names
    instead of ids, `use` (also across files), node arrays (`count`,
    `step`) and bus/bit-range wiring. It keeps the `gatequest-workspace`
    tag and is expanded into version 5 on load. The reader accepts 5 and 6
    (5 is the expanded subset, so existing workspaces keep loading);
    versions 1 to 4, or none, are still refused with the old message
    (which now says it reads 5 and 6), never migrated. Why not make the
    editor write version 6: the ECS and the bridge work on expanded
    circuits, so the editor has no arrays or buses to write back;
    inferring them from an edited circuit is out of scope. The guarantees
    are therefore: the version 6 text round-trips byte for byte when
    canonical (`to_text`), expansion is deterministic, and version 5
    load/save is lossless; an editor save of a version 6 circuit keeps
    definitions deduplicated, positions and labels, but not the compact
    spelling (`docs/save-format.md`, "Round trips"). The syntax tree keeps
    a primitive's parameters as written (a JSON map), so defaults (`w` is 1)
    and hex strings (`"0x1F"`) survive load and save. `serde_json` now
    has `preserve_order`, so maps keep their field order.
38. **Derived ids (#40).** Every id of an expanded version 6 file is a UUID
    v5: circuit `c` of a file with key `K` is `v5(NS, "K/c")` (`K` is
    `demos/<id>` or `workspaces/<folder>`), node `n` of a circuit `C` is
    `v5(C, "node/n")` (array elements `n[i]`), and the wire into input pin
    `p` of node `n` is `v5(C, "wire/n/p")`. Reloading a demo therefore
    gives the same ids, two demos using one definition share one library
    entry, and nothing collides with editor ids (UUID v4). The generated
    adapters are named `~split/…` and `~merge/…` and follow the same rule.
39. **Bus wiring (#40).** A wire entry is `[source, sink, …]`. The source
    pins are concatenated (element 0 lowest) into `W` bits; a sink whose
    pins each take `W` bits gets a copy (fan-out), otherwise its pins must
    take `W` bits together and get them in order (a bus); anything else is
    an error. Bits are collected per input pin and then emitted: one wire
    when a pin gets one whole source pin, otherwise one shared `Split` per
    distinct source field and a `Merge` per sink pin (neighbouring bits of
    one source pin are joined first). Each bit is driven at most once, and
    a partly driven pin is an error. Adapters are placed at fixed offsets
    from their node (70 units beside it, stacked 30 apart), since there is
    no auto-layout yet (#42).
40. **Demos (#40).** The hard-coded examples (`src/sim/fixtures.rs`) are
    gone. Sixteen demos in ten levels live in `assets/demos/` as version 6
    files plus `manifest.json` (order, level, title, description, uses),
    loaded by `src/sim/demos.rs`. The manifest's `uses` must equal the
    file's cross-file references, and each must name a demo listed
    earlier at the same or a lower level: a level's "then" step (decoder
    and mux, then mux tree; D latch, then flip-flop; register, then
    counter) builds on the step before it at the same level. The files
    are embedded with `include_str!` for wasm; native builds read
    `assets/demos/` from the working directory when it is there and fall
    back to the embedded copies on error. A demo opens read-only
    (`LoadWorkspaceEvent::read_only`, `ActiveCircuit::read_only`): gate,
    wire, kind, label, move and layer edits and saves are refused with
    `READ_ONLY_MESSAGE`, dragging is off, and it is not saved on exit;
    inputs, display settings and instance views work. "Copy to my
    workspace" saves and opens a version 5 copy whose root and definitions
    get fresh UUID v4s, so editing it never touches a demo's shared
    definitions. The app opens the counter demo at startup (it used to
    open the 64-bit counter); "New empty workspace" opens a blank one.
41. **Gate-level state in the demos (#40).** The SR latch is two NORs and a
    `Delay` that holds the state: `qn = NOR(S, Q)`, `Q' = NOR(R, qn)`,
    `Q = Delay(Q')`. Its output is the Delay, so a latch, and every
    flip-flop and register built from it, has no combinational path from
    its inputs to `Q`. That matters: a counter or a CPU feeds a register's
    output back to its input through logic, and with a transparent path
    through the master and slave latches that loop would be combinational
    even though the latches are never open together. The cost is timing:
    `Q` changes one tick after `S`, `R`, or (for the D latch) `D` while
    enabled, and a flip-flop's `Q` one tick after the rising clock edge.
    The `SrLatch` primitive has the same timing; `DLatch` is transparent
    in the same tick; the demos say so. Everything powers up at 0 with no
    reset pulse. The flip-flop is a master-slave pair of D latches, rising
    edge; the register puts a 2-to-1 mux before each flip-flop to hold its
    bit unless `Load` is set.
42. **CPU demo (#40).** A register machine rather than the testkit's
    accumulator CPU, so it uses the register file and the ALU of the levels
    below: 16-bit instructions `op | rd | rs | imm8` (rt is `imm[1:0]`),
    ops LDI, JMP, JZ, ADD, SUB, AND, OR, OUT, HLT. A 16-word microcode ROM
    turns the opcode into control lines; `ReduceOr`, `Rom` and `Const` are
    used as primitives where the gate version would add nothing new. One
    instruction runs per clock period (4 ticks). The program multiplies 5
    by 7; the test checks the result against an interpreter of the ISA run
    on the ROM contents read from the file.
43. **Tests replaced (#40).** Tests that loaded code-built examples now load
    demos from the files: the register-carry-over test uses the counter
    demo (both its gate-built and its word-level counters), the counter
    test checks the counter demo against its word-level twin every tick,
    the save/load round trip uses the register demo (nine definitions
    stored once), the counter-block interface test checks `counter8`,
    "every example compiles" became "every demo compiles", and the
    instance-view and idle-visuals tests load the half adder and counter
    demos. `src/sim/tests/demos.rs` adds the file, format, pyramid and
    per-demo behaviour tests.
44. **RTL import (#42).** The format is the Yosys JSON netlist
    (`write_json`), read with serde_json, so there is no new dependency;
    BLIF and a direct Verilog subset are not read. `src/sim/rtl.rs` maps
    each module reachable from the top to a circuit (instances become
    `use` instances) and the cells listed in `docs/rtl-import.md` to
    primitives, keeping widths; any other cell type, an `inout` port, a
    word wider than 64 bits, a black box or a net with two drivers is an
    error that names the module and cell. Flip-flops (`$dff`, `$dffe` and
    the gate-level `$_DFF_*`) become `Delay`/`Register` clocked by the
    global tick: the clock pin and its polarity are ignored (a warning
    says so), because the simulator has one implicit clock (D2). `x`, `z`
    and undriven bits read as 0, also with a warning. Bit-level wiring is
    turned into whole-pin wires with explicit `Split`, `Const` and `Merge`
    nodes that the layout places, instead of relying on the version 6
    expander's fixed-offset adapters (deviation 39). The importer builds
    a version 6 `SourceFile` and expands it with the key
    `workspaces/<name>`, so the in-memory workspace is the same as loading
    that file from a workspace folder.
45. **Auto-layout (#42).** `src/sim/layout.rs` is a simple Sugiyama-style
    layered layout: feedback cut at the outputs of `Delay`, `Register` and
    `SrLatch` (then any remaining cycle broken by DFS back edges), ranks by
    longest path from the inputs with constants pulled next to their
    consumer and outputs in the last column, six rounds of barycenter
    sweeps keeping the order with the fewest adjacent-rank crossings, and
    columns on the 20-unit grid with no overlapping boxes. It does not use
    the compiler's levelized ranks (the compiler works on the flattened
    `Design`, while the layout works per definition on documents, before
    anything is compiled); the ranking rule is the same. Long edges get no
    dummy nodes and wires are not routed. The clustering, chunk regions and
    incremental re-layout asked for in the issue's follow-up comment (#59)
    are not done: module boundaries are the only blocks. Version 6 still
    requires `at`; the layout is not yet applied to files that omit it.
    The editor's "Import RTL…" field is native only (no file system on
    wasm). `save_workspace_in` now uses the new `workspace_to_json`, which
    the `rtl_import` example also uses for `--v5`.
46. **Gallery.** 27 circuits beside the curriculum live in
    `assets/gallery/` with their own manifest (`"gatequest-gallery"`:
    categories and items instead of levels and demos), loaded after the
    demos into the same `DemoCatalog` (`gallery`, `categories`; `get` and
    `by_root` search both; a gallery item has level 0 and a category).
    Chosen over a `gallery` section in the demo manifest so that the
    curriculum's level rules stay as they are; the pyramid rule carries
    over: an item uses curriculum demos and items listed before it, and 10
    of them build on earlier items (the edge detector, the BCD digit
    counter, the LFSR, the comparators, the ripple adder). The editor
    lists it as a second group under the demos. No new primitive or I/O
    part was added: lights are `Output` nodes (the 7-segment digit and the
    traffic lights are arrangements of them), readouts are `Display`
    nodes, buttons are `Input` switches pressed and released (an edge
    detector turns a press into a one-tick pulse), and there is no speaker.
47. **Array parameters (`{i}`).** Version 6 gains one authoring feature: in
    an array, `{i}` in a string parameter is the element's index (`"lo":
    "{i}"` cuts a word into bits, `"value": "{i}"` numbers constants);
    elements must keep the same pins. Files without it read as before.
    It exists because hand placement of bit slices was otherwise only
    possible one node at a time.
48. **Adapter placement.** Generated `Split`/`Merge` adapters used to stand
    70 beside their node, stacked 30 apart, so a word cut into bits gave a
    pile of overlapping 50-unit boxes. They now stand in a column at a
    distance that leaves 30 between boxes (80 for an ordinary node,
    rounded up to the grid for wider ones such as a `Display`), 60 apart,
    splits right and merges left with separate stacks. Only generated
    positions change: ids, wiring and behaviour are the same, so existing
    files load the same circuits. Some curriculum demos still have
    overlapping boxes (two word inputs whose split columns meet in
    `ripple_adder/adder8` and `mux/mux2x8`, and the AND/OR arrays spaced 40
    apart in `alu/alu8`); the no-overlap test covers the gallery only.
49. **Preview renderer.** `examples/render_svg.rs` writes SVG previews of
    circuits (see `docs/save-format.md`, "Previews") and reports
    overlapping boxes and wires drawn through boxes. It is a development
    tool only; the app does not use it. `layout.rs` is unchanged: the
    gallery is placed by hand, not by `layout_circuit`.
60. **More RTL cells, by composition.** The importer (`src/sim/rtl.rs`
    and `src/sim/rtl/`) maps arithmetic (`$mul`, `$div`, `$mod`, `$neg`,
    `$alu`), every shift, the ordered comparisons, `$pmux`/`$bmux`/
    `$demux`, `$lut`, flip-flops with synchronous and asynchronous
    resets, sets, clears and loads (word and gate level), latches and
    memories (`docs/rtl-import.md` has the table). No primitive was added
    to `logi_sim`: a multiplier is shift and add (`Switch`, `Add`), a
    divider restoring division (`Add`, `Or`, `Mux`), a variable shift a
    barrel of `Mux` stages, `$lt` an `LtU` with the sign bits inverted
    when signed. A `Mul` or `Shift` primitive would need the IR, the
    reference interpreter, the compiler's lowering and simplification,
    the runtime, the editor's `GateKind`, the palette and the save format
    to agree, for cells that composition already computes exactly; the
    cost is node count (a 32-bit multiplier is 63 primitives plus
    adapters). Asynchronous controls cannot act between ticks (D2): the
    output shows the reset value at once through a `Mux` (or `Or`/`And`
    for set and clear) after the state element, and the state element
    stores it at the next tick, with a warning; a design whose
    asynchronous reset depends on its own outputs within a tick becomes
    a combinational loop. Undefined results (`$pmux` with several selects,
    division by 0, `$shiftx` out of range) take a fixed value. `inout`
    ports stay refused rather than read as outputs. Generated adapters
    gain `Extend` for a repeated bit (sign extension), and helper nodes
    nobody reads are dropped.
61. **RTL memories.** (Superseded by item 71: a memory with one write
    port is now a `Ram2`.) `$mem_v2` and `$mem` (one write port at most)
    became a `Rom` per read port when nothing writes them, a `Ram` when
    every read port reads at the write address, and otherwise a
    `Register` per word (up to 64 words) with a `Mux` tree per read port,
    because the `Ram` primitive has a single address. The editor's `Ram`
    has no initial contents (`GateKind::Ram` drops the netlist's `init`),
    so initial contents are kept without changing the save format: the
    state element holds `word ^ init[addr]`, with the initial word from a
    `Rom` at the same address (or a constant per register), which is exact
    and powers up reading `init`. A clocked read port is a `Delay` or
    `Register` after the read, reading the old word or, when transparent,
    the word being written. Building this exposed a compiler bug, fixed in
    `logi_sim::compile::simplify`: constant identities looked up
    immediates (a concatenation's shift) as slots, which panicked when a
    shift was past the slot count (a small design with a constant in the
    high bits of a `Merge`).
70. **Dual-port RAM (`Ram2`).** `NodeKind::Ram2 { addr_bits, w, reads,
    mask, init }` is a memory with one write port and `reads` independent
    read ports (1 to `MAX_READ_PORTS` = 8). Pins: `waddr(addr_bits)`,
    `we`, `wdata(w)`, then `raddr0 .. raddr{reads-1}` (each
    `addr_bits`); outputs `rdata0 .. rdata{reads-1}` (each `w`). With
    `mask` false `we` is 1 bit; with `mask` true it is `w` bits, a per-bit
    write enable: the commit stores `(old & !we) | (wdata & we)`. The
    timing is `Ram`'s exactly: every read is combinational from committed
    memory, the write port is sampled at the end of the settle and
    applied at the next commit, so a read at the address being written
    returns the old word during that tick and the new one after the next
    (a `Ram2` with one read port at the write address behaves as a `Ram`,
    which a test checks tick by tick). Only `raddr_k → rdata_k` is
    combinational (`CombDeps::Diagonal`), so a read port may be addressed
    by another port's data (pointer chasing) and any read data may feed
    the write port. In every layer it is a `Ram` with more read ports: the
    interpreter keeps the pending write as `(addr, data, mask)` (a `Ram`'s
    mask is all ones); the compiler emits one memory, one `MemRead` per
    read port, snapshot copies of the write port and one commit
    (`Commit::RamWrite`, or the new `Commit::RamWriteMasked`); the runtime
    checks and applies it like `RamWrite`. `MemRead` is never packed, so
    lane packing needs nothing. State carry-over is the `Ram`'s: one
    `StateKey` with `StateLoc::Mem` per node, contents kept across edits
    while the geometry `(addr_bits, w)` is unchanged; a new read port
    count or `mask` keeps them, and so does a new `init` (the contents
    are state; `init` is only the reset value), while a new geometry
    resets them to `init`. The random generator draws it (one to three read ports, with
    or without a mask; its write port is wired late, like other state
    inputs), and bit-blasting keeps it whole. In the editor it is
    `GateKind::Ram2` ("DUAL-PORT RAM" in the State group of the palette,
    a violet tile taller per read port), saved as `"type": "Ram2"` in
    versions 5 and 6 (`"DualPortRam"` is accepted when reading), with
    `mask` and `init` optional.
71. **RTL memories on `Ram2`.** `$mem_v2`/`$mem` with one write port (at
    any size up to 2^20 words, any read addresses) become one `Ram2`
    holding `INIT` directly, with a read port per memory read port (a
    memory with more than eight becomes several copies written alike).
    Byte enables use the per-bit write enable. This replaces the `Ram`
    with an `Xor`-ed `Rom` of initial contents and the register file of
    item 61, which was limited to 64 words. Only memories with several
    write ports are still built from a `Register` per word (up to 64
    words, a later port winning at the same address); larger ones are
    refused.
