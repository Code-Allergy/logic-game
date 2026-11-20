# Simulation v2: synchronous, word-level, compiled

Status: proposed design. Supersedes the timing model described in
`logi-sim/ARCHITECTURE.md` and the bridge/cache model in
`docs/simulation-model-and-cache.md` once milestone M6 lands.

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

Combinational dependency edges for loop detection and ranking are: every
input→output of a combinational node, `DLatch` en/d→q, and `Ram`/`Rom`
addr→data. `Delay`, `Register` and `SrLatch` outputs have no combinational
dependency on their inputs.

`Counter8`/`Counter64`/`BusExpand`/`BusReduceOr`/`Demux` are not primitives;
the builder provides them as helpers composed from the table above.
Later additions (`Sub`, shifts, `Mul`, n-way mux, dual-port RAM, bus-conflict
detection for `Switch`) are additive and do not change the model.

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

`Sim` is single-threaded, allocation-free per tick, and has no locks or
atomics. It is the same code on native and wasm.

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

## 12. Deferred

Considered and deliberately left out of v2; each is additive later.

- Per-definition compile memoization and incremental recompilation.
- Packing many 1-bit gates of one op into the lanes of a single `u64`.
- Activity gating (skipping regions whose inputs did not change).
- Partitioned multithreading.
- Native JIT (not available on wasm).
- Multi-driver buses with conflict detection; X/Z values.
