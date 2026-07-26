# Logi-Sim Architecture & Design Specification

`logi-sim` is a modular, high-performance digital logic simulation library designed to run standalone or directly inside game engines like **Bevy**.

---

## 1. High-Level Architectural Overview

```
                      +----------------------------------+
                      |       Bevy / Main Thread         |
                      | (UI, Systems, Input, Rendering)  |
                      +----------------------------------+
                               |                ^
                set_input(...) |                | get_output(...) / get_wire(...)
             (MPSC Cmd Channel)|                | (Atomic / Lock-free Read)
                               v                |
                      +----------------------------------+
                      |         SharedState              |
                      | (Atomic Ticks, RwLock Wire Map)  |
                      +----------------------------------+
                               ^
                               | Writes State Updates
                      +----------------------------------+
                      |    Background Worker Thread      |
                      |  (Continuous Simulation Loop)    |
                      |                                  |
                      |   +--------------------------+   |
                      |   |     Runner Trait         |   |
                      |   |   (e.g., NaiveRunner)    |   |
                      |   +--------------------------+   |
                      |                |                 |
                      |   +--------------------------+   |
                      |   |    Circuit Graph Data    |   |
                      |   +--------------------------+   |
                      +----------------------------------+
```

---

## 2. Core Subsystems

### 2.1 Graph Layer (`logi_sim::circuit`)
The circuit graph represents the topology of digital components and connections.

- **`Circuit`**: The graph container holding nodes and wires stored in generational `slotmap::SlotMap`s.
- **`Node` / `NodeKind`**: Represents logic elements (`Input`, `Output`, `And`, `Or`, `Not`). Indexed by `NodeId`.
- **`Wire`**: Directed connection between a source `OutputRef` (node + pin) and target `InputRef` (node + pin) sinks. Indexed by `WireId`.
- **`InputPin` / `OutputPin`**: Pin connections on nodes.

### 2.2 Runner Abstraction Layer (`logi_sim::runner`)
The `Runner` trait abstracts simulation execution models from graph topology:

```rust
pub trait Runner {
    fn set_input(&mut self, node: NodeId, pin: u8, value: bool);
    fn tick(&mut self);
    fn output(&self, node: NodeId, pin: u8) -> bool;
    fn wire_value(&self, wire: WireId) -> bool;
    fn for_each_wire(&self, f: &mut dyn FnMut(WireId, bool));
}
```

#### Implementations:
- **`NaiveRunner`**: Evaluates circuit nodes iteratively by checking input wire states and propagating boolean results across wires.
- **Extensibility**: Alternative runners (e.g., compiled evaluation graphs, event-driven queue runners, GPU/SIMD runners) implement the same `Runner` trait, allowing `SimulationEngine` to work across different evaluation models.

### 2.3 Threaded Simulation Engine (`logi_sim::engine`)
To ensure game engines like Bevy remain at 60+ FPS while simulation runs as fast as possible, `logi-sim` decouples execution onto a background thread:

- **`SimulationEngine`**: High-level owned engine struct containing `Circuit`, `SharedState`, and background worker thread handle.
- **`SharedState`**: Thread-safe snapshot container containing:
  - `tick_count: AtomicU64`: Total simulation ticks performed.
  - `is_running: AtomicBool`: Active execution status.
  - `wire_states: RwLock<SecondaryMap<WireId, bool>>`: Thread-safe wire states.
- **Multi-Threaded Execution**:
  - Worker thread runs in an uncapped loop, executing `runner.tick()` continuously.
  - Commands (`SetInput`, `Pause`, `Resume`, `Step`, `Stop`) are sent from main thread to worker thread via an `mpsc` channel.
  - Reads from main thread (`get_output`, `get_wire`) are atomic and non-blocking.

### 2.4 Bevy Engine Integration (`logi_sim::bevy_integration` / Feature `"bevy"`)
Optional integration via `bevy = { version = "0.19.0", optional = true }` in `Cargo.toml`.

- **`Resource` derive**: `SimulationEngine` derives `bevy::prelude::Resource`.
- **`LogiSimPlugin`**: Bevy plugin that manages engine startup and ensures background worker threads are cleanly joined on `AppExit`.
- **Usage in Bevy Frame Update**:
  ```rust
  fn update_circuit_system(mut sim: ResMut<SimulationEngine>) {
      // Send input update to worker thread
      sim.set_input(input_node, 0, true);
      
      // Read state atomically without blocking worker loop
      let result = sim.get_output(output_node, 0);
      let tick_count = sim.tick_count();
  }
  ```

---

---

## 4. Layered Circuit Transformation Pipeline & Execution Models

To achieve maximum performance while maintaining clean debugging and dynamic UI editing, `logi-sim` uses a **multi-layer transformation pipeline**. 

Each layer transforms the circuit data model into a lower-level, more execution-optimized representation. **Crucially, every layer exposes a corresponding `Runner` implementation**, allowing all representations to be run, tested, debugged, and validated identically using the exact same test harness and `SimulationEngine`.

```
                    +-------------------------------------+
                    |       Level 0: Dynamic Graph        |
                    | (Circuit: SlotMap Nodes & Wires)    |
                    +-------------------------------------+
                                       |
                                       | Pass 1: Topological Flatten & Direct Indexing
                                       v
                    +-------------------------------------+
                    |     Level 1: Flattened Circuit      |
                    | (Flat Direct Array & Index Mapping) |
                    +-------------------------------------+
                                       |
                   +-------------------+-------------------+
                   |                   |                   |
                   v                   v                   v
      +---------------------+ +-----------------+ +-----------------+
      | Level 2: Event-     | | Level 3: Bit-   | | Level 4: JIT /  |
      | Driven Netlist      | | Packed SIMD     | | Machine Code    |
      +---------------------+ +-----------------+ +-----------------+
                   |                   |                   |
                   +-------------------+-------------------+
                                       |
                                       v
                       +----------------------------------+
                       |    Unified `Runner` Trait        |
                       | (set_input, tick, get_output)    |
                       +----------------------------------+
```

---

### 4.1 Layer Breakdown

#### Level 0: Dynamic Graph (`Circuit`)
- **Data Model**: `SlotMap<NodeId, Node>` & `SlotMap<WireId, Wire>`.
- **Purpose**: Interactive UI editing in Bevy, real-time node insertion/deletion, dynamic rewiring.
- **Runner**: `NaiveRunner` / `InterpreterRunner`. Interprets node logic directly via wire slot map lookups.

#### Level 1: Flattened Direct Index Circuit (`FlattenedCircuit`)
- **Data Model**: Contiguous `Vec<FlatNode>` and `Vec<u32>` direct index lookup arrays.
- **Transformation**: Resolves `NodeId` and `WireId` indirection into zero-offset array indices.
- **Purpose**: Eliminates map overhead and improves CPU cache line utilization.
- **Runner**: `FlatInterpreterRunner`. Evaluates nodes using tight array loops.

#### Level 2: Event-Driven Netlist (`EventDrivenCircuit`)
- **Data Model**: Topological dependency graph with an active evaluation queue (`VecDeque<NodeIdx>`).
- **Transformation**: Extracts signal propagation paths into an active netlist.
- **Purpose**: Only re-evaluates nodes connected to wires whose state changed on the previous tick. Extremely fast for large, mostly quiescent logic circuits (e.g. CPUs, RAM blocks).
- **Runner**: `EventDrivenRunner`. Executes only queued dirty nodes per tick.

#### Level 3: Bit-Packed SIMD Circuit (`BitPackedCircuit`)
- **Data Model**: Circuit gates operating on `u64` bit vectors representing 64 parallel logic runs simultaneously.
- **Transformation**: Maps gate inputs/outputs to bitwise bitmask operations (`AND`, `OR`, `XOR`, `NOT`).
- **Purpose**: Evaluates 64 circuit test vectors concurrently per CPU instruction using AVX2/NEON SIMD vectorization.
- **Runner**: `SimdRunner`. Executes 64-parallel logic evaluations in single CPU cycles.

#### Level 4: JIT / Native Code Compilation (`JitCircuit`)
- **Data Model**: Executable memory buffer containing native x86_64 or AArch64 machine instructions (generated via Cranelift or custom code emitter).
- **Transformation**: Compiles the evaluated boolean expressions directly into native assembly.
- **Purpose**: Bare-metal execution speed for sub-circuits and fixed logic blocks.
- **Runner**: `JitRunner`. Calls generated native function pointers.

---

### 4.2 Multi-Threaded Execution Across Layers (`ParallelWorkerRunner`)

For multi-core execution:
- Large circuits are partitioned into decoupled topological sub-graphs.
- Independent sub-graphs run concurrently across worker threads, synchronizing boundary wire states at tick boundaries.

---

### 4.3 Unified Testing & Verification

Because every execution model implements the `Runner` trait:

```rust
pub trait Runner {
    fn set_input(&mut self, node: NodeId, pin: u8, value: bool);
    fn tick(&mut self);
    fn output(&self, node: NodeId, pin: u8) -> bool;
    fn wire_value(&self, wire: WireId) -> bool;
    fn for_each_wire(&self, f: &mut dyn FnMut(WireId, bool));
}
```

Any circuit transformation can be verified for 100% equivalence against `NaiveRunner`:

```rust
fn verify_runner_equivalence<R: Runner>(circuit: &Circuit, runner: &mut R) {
    // Both NaiveRunner and target R execute identical input vectors
    // Assert outputs and wire states match exactly per tick
}
```

---

## 5. Directory Layout

```
logi-sim/
├── Cargo.toml          # Workspace crate manifest (with optional "bevy" feature)
├── ARCHITECTURE.md     # Architecture & Multi-Layer Pipeline documentation
├── src/
│   ├── lib.rs          # Module exports & prelude
│   ├── circuit/        # Level 0 Circuit graph, Node, Wire, SlotMap keys
│   ├── compiler/       # Circuit transformation passes (Level 0 -> Level 1 -> Level N)
│   ├── runner/         # Runner trait, NaiveRunner, EventDriven, SIMD, JIT runners
│   ├── engine/         # SimulationEngine & SharedState atomic snapshot
│   ├── bevy_integration.rs # Bevy Resource & LogiSimPlugin (feature = "bevy")
│   ├── graphviz/       # DOT format exporter
│   └── fixtures/       # Helper test circuit builders
└── tests/
    ├── engine.rs       # Multi-threaded engine & atomic state tests
    └── runner/         # Logic tests for adders, gates, complex circuits
```

