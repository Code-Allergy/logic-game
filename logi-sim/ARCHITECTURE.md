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

## 3. Directory Layout

```
logi-sim/
├── Cargo.toml          # Workspace crate manifest (with optional "bevy" feature)
├── ARCHITECTURE.md     # Architecture documentation
├── src/
│   ├── lib.rs          # Module exports & prelude
│   ├── circuit/        # Circuit graph, Node, Wire, SlotMap keys
│   ├── runner/         # Runner trait, NaiveRunner implementation
│   ├── engine/         # SimulationEngine & SharedState atomic snapshot
│   ├── bevy_integration.rs # Bevy Resource & LogiSimPlugin (feature = "bevy")
│   ├── graphviz/       # DOT format exporter
│   └── fixtures/       # Helper test circuit builders
└── tests/
    ├── engine.rs       # Multi-threaded engine & atomic state tests
    └── runner/         # Logic tests for adders, gates, complex circuits
```
