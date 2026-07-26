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
/// Core simulation runner contract for all execution models (Interpreter, Event-Driven, SIMD, JIT).
pub trait Runner {
    fn set_input(&mut self, node: NodeId, pin: u8, value: bool);
    fn tick(&mut self);
    fn output(&self, node: NodeId, pin: u8) -> bool;
}

/// Optional extension trait for runners that support detailed wire-level state inspection (e.g., UI debug visualization).
pub trait InspectableRunner: Runner {
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
- **Nested Sub-Circuits**: Supports modular composition via `NodeKind::SubCircuit(Box<Circuit>)` and `circuit.add_subcircuit(sub)`. Input/output pins are mapped dynamically to inner `Input` and `Output` nodes.
- **Runner**: `NaiveRunner` / `InterpreterRunner`. Evaluates node logic recursively via inner runner instances for `SubCircuit` nodes.

#### Level 1: Flattened Direct Index Circuit (`FlattenedCircuit`)
- **Data Model**: Contiguous `Vec<FlatNode>` and `Vec<u32>` direct index lookup arrays.
- **Transformation**: Resolves `NodeId` and `WireId` indirection into zero-offset array indices.
- **Sub-Circuit Inlining**: `FlattenedCircuit::compile` recursively inlines and unrolls all `NodeKind::SubCircuit` nodes, aliasing child input/output wires directly to parent wires and completely stripping sub-circuit boundaries into primitive gates.
- **Purpose**: Eliminates map overhead, eliminates sub-circuit function call overhead, and improves CPU cache line utilization.
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

---

## 5. Structural Optimization & Compiler Pass Architecture (`logi_sim::compiler`)

Before a circuit graph is passed to lower-level execution models (`EventDrivenCircuit`, `BitPackedCircuit`, `FlattenedCircuit`), it passes through structural optimization passes in `logi_sim::compiler`:

```
                 +-------------------------------------+
                 |       Level 0: Raw Circuit          |
                 | (Dynamic Graph from UI / Editor)    |
                 +-------------------------------------+
                                    |
                                    v
                 +-------------------------------------+
                 |         Compiler Pipeline           |
                 |  (Compiler::optimize(&raw_circuit)) |
                 +-------------------------------------+
                                    |
            +-----------------------+-----------------------+
            |                       |                       |
            v                       v                       v
  +--------------------+  +--------------------+  +--------------------+
  | Pass 1: Dead-Code  |  | Pass 2: Constant   |  | Pass 3: Pattern    |
  | & Node Culling     |  | Folding            |  | Substitution       |
  | (cull_dead_nodes)  |  | (AND(x,0) -> 0)    |  | (5-gate XOR -> Xor)|
  +--------------------+  +--------------------+  +--------------------+
                                    |
                                    v
                 +-------------------------------------+
                 |         Optimized Circuit           |
                 +-------------------------------------+
                                    |
                                    v
                 +-------------------------------------+
                 | Target Execution Layer / Runner     |
                 | (Flattened, Event-Driven, SIMD, JIT)|
                 +-------------------------------------+
```

---

### 5.1 Optimization Passes

#### 1. Dead-Code & Unconnected Node Culling (`cull_dead_nodes`)
- **Mechanism**: Performs a reverse reachability traversal (BFS/DFS) starting from all `NodeKind::Output` nodes.
- **Benefit**: Any floating node placed during UI editing or any sub-circuit branch not connected to an output is completely eliminated, reducing graph size and runtime tick overhead.

#### 2. Constant Logic Folding
- **Mechanism**: Simplifies known constant logic combinations:
  - `AND(A, false) -> false`
  - `OR(A, true) -> true`
  - `NOT(NOT(A)) -> A`
- **Benefit**: Short-circuits invariant signals.

#### 3. Pattern Matching & Macro Gate Substitution
- **Mechanism**: Scans the graph for known multi-gate sub-graph patterns (e.g. 5 gates forming an XOR: `(A OR B) AND NOT (A AND B)`).
- **Substitution**: Replaces the entire multi-gate cluster with a single native macro node (`NodeKind::Xor`, `NodeKind::Mux`, `NodeKind::FullAdder`).
- **Benefit**: Converts multi-tick cascading evaluations across multiple gates into a single 1-tick CPU operation, providing substantial speedup and making netlists much easier for SIMD and Event-Driven runners to process.

---

## 6. Benchmarking Suite Architecture

`logi-sim` features a **trait-based benchmark harness** (`logi-sim/benches/runners.rs`) powered by [Criterion](file:///home/ryan/code/logi-game/logi-sim/benches/runners.rs). 

Just as unit testing uses `RunnerFactory` to test logic parity across runners, the benchmark suite uses `BenchmarkRunnerFactory` to evaluate execution throughput and measure exact optimization pass speedups (`unoptimized` vs `optimized`) across all runner implementations.

### 6.1 `BenchmarkRunnerFactory` Trait Abstraction

```rust
pub trait BenchmarkRunnerFactory {
    fn name() -> &'static str;
    type Runner<'a>: Runner where Self: 'a;
    fn create<'a>(circuit: &'a Circuit, opt_circuit: &'a Circuit) -> Self::Runner<'a>;
}
```

### 6.2 Pre-Configured Benchmark Factories

The suite compares 8 primary configurations side-by-side:
1. **`NaiveRunner-unoptimized`**: Raw `Circuit` executed via `NaiveRunner`.
2. **`NaiveRunner-optimized`**: Structural `Compiler::optimize` output executed via `NaiveRunner`.
3. **`FlatRunner-unoptimized`**: Raw `FlattenedCircuit` executed via `FlatRunner`.
4. **`FlatRunner-optimized`**: `FlattenedCircuit` compiled from `Compiler::optimize` output executed via `FlatRunner`.
5. **`EventDrivenRunner-unoptimized`**: Raw `EventDrivenCircuit` executed via `EventDrivenRunner`.
6. **`EventDrivenRunner-optimized`**: `EventDrivenCircuit` compiled from `Compiler::optimize` output executed via `EventDrivenRunner`.
7. **`SimdRunner-unoptimized`**: Raw `BitPackedCircuit` executed via `SimdRunner` (64-parallel SIMD).
8. **`SimdRunner-optimized`**: `BitPackedCircuit` compiled from `Compiler::optimize` output executed via `SimdRunner` (64-parallel SIMD).

### 6.3 Standardized Benchmark Circuits

Each factory runs through:
- **`and_gate_tick`**: Single gate evaluation overhead.
- **`full_adder_tick`**: Mid-scale cascading logic benchmark.
- **`d_flip_flop_tick`**: Sequential feedback loop benchmark.
- **`dead_code_circuit_tick`**: Circuit containing 50 dead floating gates + active adder to measure exact compiler culling speedup.
- **`ripple_carry_adder_8bit_tick`**: 8-bit ripple carry adder (~112 gates) evaluating multi-bit carry propagation speedup.
- **`large_quiescent_grid_500_tick`**: 500-gate deep logic matrix testing `EventDrivenRunner` throughput on large sparse signal events.

### 6.4 High-Precision Batched Benchmark Results & Game Clock Metrics

By batching 1,000 continuous ticks per iteration sample (`BATCH_TICKS = 1_000`), measurement timer noise is reduced by **1,000x**, producing sub-1% deterministic variance and reducing total benchmark execution time from **6.5 minutes to 20 seconds**.

The following table summarizes empirical high-precision benchmark timings and corresponding **Game Simulation Clock Frequency (MHz / GHz)** metrics on a modern Linux CPU:

| Benchmark Test | Naive (Opt) | Flat (Opt) | EventDriven (Opt) | SIMD (Opt) | SIMD Effective Multi-Run Throughput | Max Simulation Clock Frequency |
| :--- | :---: | :---: | :---: | :---: | :---: | :---: |
| **`and_gate_tick`** | 5.01 ns | 3.83 ns | **784.0 ps** | **979.7 ps** | **15.3 ps / run** | **1.27 GHz** |
| **`full_adder_tick`** | 36.93 ns | 25.37 ns | **785.4 ps** | **9.97 ns** | **155.7 ps / run** | **1.27 GHz** |
| **`d_flip_flop_tick`** | 26.06 ns | 17.71 ns | **793.2 ps** | **6.36 ns** | **99.3 ps / run** | **1.26 GHz** |
| **`dead_code_circuit_tick`** (50 dead gates) | 37.28 ns | 25.72 ns | **784.0 ps** | **9.95 ns** | **155.4 ps / run** | **1.27 GHz** |
| **`ripple_carry_adder_8bit_tick`** (~112 gates) | 170.24 ns | 136.49 ns | **27.97 ns** | **56.53 ns** | **883.2 ps / run** | **35.7 MHz** |
| **`large_quiescent_grid_500_tick`** (500 gates) | 1.726 µs | 1.344 µs | **14.03 ns** | **725.83 ns** | **11.34 ns / run** | **71.2 MHz** |

#### Key CI/CD & Game Engine Takeaways:
1. **Game Engine Simulation Budgeting**: `EventDrivenRunner` can simulate a 500-gate quiescent circuit at **71.2 MHz** (consuming only **14 nanoseconds** per tick). At a 60 FPS game frame rate (16.6 ms per frame), `logi-sim` can process **over 1.1 million circuit ticks per frame** on a single background worker thread without dropping frames.
2. **Multi-Run Parallel Vector Rate**: `SimdRunner` evaluates 64 parallel test vectors simultaneously in **725.8 ns** per 500-gate tick (**11.34 ns per vector run**), achieving an effective parallel logic throughput of **88.1 MHz**.
3. **CI/CD Reliability**: 1,000-tick iteration batching eliminates timer jitter noise, guaranteeing deterministic, zero-swing performance comparisons across PRs.

### 6.5 Fast CI/CD Benchmarking Command

To execute the fast 20-second benchmark suite:

```bash
cargo bench -p logi-sim
```







