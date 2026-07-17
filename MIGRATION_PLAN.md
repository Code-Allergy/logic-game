# Migration Plan: Decoupling Simulation from Bevy

## Overview
The goal of this migration is to decouple the internal logic simulation (`sim_core`) from the Bevy game engine. We will replace `sim_core` with a heavily layered, separate crate called `logi-sim`. The new architecture separates the high-performance simulation logic from the visual representation, running the simulation on a dedicated thread and communicating with the Bevy ECS frontend via cross-thread channels.

This new architecture enables several key optimizations:
1. **Separation of Concerns:** Bevy will only be responsible for rendering (UI, visual elements) and taking user inputs.
2. **Concurrency:** The simulation logic will run entirely on its own thread, unblocking the main game loop.
3. **Data Localization and Efficiency:** The simulation backend can freely transform user-defined circuits into heavily optimized data structures (`LayeredCircuit`, `CompiledCircuit`) designed specifically for flat array access and cache-friendly evaluation, rather than managing heavy generic entity component architectures.

## Architecture

The target architecture is divided into three main layers:

### 1. Frontend (Bevy Application)
- **Crate:** Main application crate (`gate_quest`).
- **Responsibility:** Managing user interface, rendering gates, connecting lines, grid placement, and capturing interactions (clicks, drags).
- **Modifications:** The current `Simulation` resource and Bevy `Systems` that process ticks internally via `sim_core::Circuit::tick` will be removed. Bevy will instead maintain a simple "View" state, sending input changes (e.g., clicking an input toggle) to the backend, and receiving gate state updates to visually update colors/lines.

### 2. Communication Layer (Intermediary / Channels)
- **Responsibility:** Safely transporting events, mutations, and tick results between the Bevy ECS thread and the dedicated Simulation thread.
- **Implementation:**
  - `mpsc` channels or an event ring-buffer.
  - **Downstream (Bevy -> Backend):** User actions such as "Add Gate", "Delete Gate", "Connect Port A to B", "Toggle Input".
  - **Upstream (Backend -> Bevy):** Simulation results, such as "Gate States Update" containing an array of states for rendering visually.

### 3. Backend Simulator (`logi-sim` crate)
- **Crate:** `logi-sim`.
- **Responsibility:** Maintaining the source of truth for the circuit graphs and processing ticks rapidly.
- **Structure:**
  - Will heavily rely on layered data structures to ensure the simulation loop is as efficient as possible.
  - Completely agnostic of Bevy `Entity`, `Transform`, or visual components.

#### Simulation Layers within `logi-sim`

To achieve maximum performance, the backend will process circuits through progressively optimized representations:

1. **Graph Representation (Builder/Editable State):**
   - Retains the semantic topology of user actions (Add Gate, Connect A to B).
   - Easily mutable for active editing. Similar to the current graph map in `sim_core`.

2. **`LayeredCircuit`:**
   - An intermediate, flattened state generated when the simulation is triggered or updated.
   - Nodes are grouped into evaluation layers derived from a topological sort.
   - All gates in "Layer 1" can be evaluated, followed by "Layer 2", avoiding complex pointer chasing or depth-first searches during the hot loop.

3. **`CompiledCircuit`:**
   - The most optimal, execution-ready state.
   - Stripped of all object graphs, metadata, or UUIDs.
   - Contains only densely packed, cache-friendly flat arrays or bitsets (e.g., contiguous `u64` blocks representing gate types and truth values).
   - Extremely fast for the core `tick()` loop.

## Migration Steps

### Phase 1: Establish `logi-sim` API and Threading
1. Initialize the core structures (`LayeredCircuit`, `CompiledCircuit`, basic editing graph) in `logi-sim`.
2. Establish the thread spawning logic and MPSC channels that will bridge Bevy and `logi-sim`.
3. Create a wrapper `Resource` in Bevy that holds the `Sender` (to dispatch UI actions) and `Receiver` (to read tick outputs).

### Phase 2: Migrate Core Logic
1. Reimplement gate logic (AND, OR, NOT, Oscillator, Input/Output) in `logi-sim`.
2. Write the compilation logic that converts the editable graph representation into the `LayeredCircuit` and `CompiledCircuit`.
3. Add a simulation loop running independently inside the `logi-sim` thread, communicating states back via the `Receiver`.

### Phase 3: Connect Bevy Frontend
1. Update Bevy's `draw_gates_system`, `update_connections_system`, and related input/UI systems. Instead of interacting with `sim_core::Circuit`, they should emit messages into the channel.
2. Replace Bevy's `simulation_update_system` (which currently directly calls `circuit.tick()`) with a system that polls the `Receiver` channel for state updates and visually reflects them (e.g., updating gate sprite colors).

### Phase 4: Cleanup
1. Remove `sim_core` dependency from `Cargo.toml`.
2. Delete the `crates/sim_core` folder.
3. Clean up legacy component fields in Bevy that duplicated state.
4. Finalize end-to-end tests for the new `logi-sim` pipeline.
