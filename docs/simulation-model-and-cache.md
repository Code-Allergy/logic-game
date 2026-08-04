# Simulation model, runtime bridge, and ECS cache

GateQuest represents a circuit in three forms. Each form has a different job:

1. The game model is persistent, editable, and addressed with UUIDs.
2. The `logi-sim` circuit is optimized for execution and uses internal IDs.
3. Bevy entities contain a frame-local projection used by rendering and gameplay systems.

The model is the source of truth. The runtime and ECS state can always be rebuilt from it.

```text
Saved JSON / editor
        |
        v
GameCircuit (UUID model)
        |
        | SimBridge builds and maps
        v
logi_sim::Circuit (NodeId / WireId runtime)
        |
        | one snapshot per frame
        v
SimCache (UUID-indexed frame snapshot)
        |
        | SimulationProjectionPlugin
        v
Bevy components (rendering and interaction)
```

## Ownership rules

- `GameCircuit` owns circuit identity, logical structure, labels, and editor positions.
- `SimBridge` exclusively owns translation between UUIDs and `NodeId`/`WireId` values.
- `logi-sim` owns execution state, runner-specific data, and optimized circuit layouts.
- `SimCache` owns the latest game-facing snapshot of runtime values.
- Bevy components own presentation state for a particular entity.
- `Transform`, sprites, text, selection, and spatial layout never belong in `logi-sim`.
- Game and editor code must use UUIDs. Raw `NodeId` and `WireId` values stay inside `src/sim`.

## Stage 1: persistent game model

The model is defined in [`src/sim/model.rs`](../src/sim/model.rs). It is serialized by Serde and is the authoritative representation used by the editor and workspace files.

### `GameCircuit`

`GameCircuit` contains:

| Field | Meaning |
| --- | --- |
| `id: Uuid` | Stable circuit identity. |
| `name: String` | Human-readable circuit name. |
| `nodes: HashMap<Uuid, GameNode>` | Nodes indexed by stable UUID. |
| `wires: HashMap<Uuid, GameWire>` | Wires indexed by stable UUID. |

It is also a Bevy resource because editor and simulation systems share the currently open circuit. Mutating this resource changes the authoritative circuit. Runtime and visual representations must then be synchronized or rebuilt.

### `GameNode`

`GameNode` contains persistent properties only:

| Field | Meaning |
| --- | --- |
| `id: Uuid` | Stable node identity across saves and runtime rebuilds. |
| `kind: GameNodeKind` | Logical behavior and pin shape. |
| `position: Vec2` | Editor world position. This is not simulation state. |
| `label: Option<String>` | Human-readable interface identity. Input/output labels should be present and unique within a circuit. |
| `custom_properties` | Extensible serialized editor metadata. |

`GameNodeKind` is the game-owned, serializable counterpart of `logi_sim::NodeKind`. It defines `input_count()`, `output_count()`, display naming, and conversion to a primitive simulator kind.

Subcircuits are different from primitive nodes:

```rust
GameNodeKind::SubCircuit {
    circuit: Box<GameCircuit>,
}
```

Their public pins are derived from labeled `Input`, `Input8`, `Input64`, `Output`, `Output8`, and `Output64` nodes inside the child circuit. A subcircuit's interface is therefore part of the child model, not an independent list on the parent node.

### `GameWire`

A wire stores UUID endpoints and pin offsets:

```rust
GameWire {
    id: wire_uuid,
    source_node: source_uuid,
    source_pin: 0,
    sink_node: sink_uuid,
    sink_pin: 0,
    kind: WireKind::Vector { bits: 64 },
}
```

`WireKind::Scalar` represents one logical bit. `WireKind::Vector { bits }` is a convenience representation for adjacent pins. A 64-bit model wire starting at pin zero means:

```text
source pin 0  -> sink pin 0
source pin 1  -> sink pin 1
...
source pin 63 -> sink pin 63
```

The model stores this as one UUID-addressed wire. The simulator currently executes it as 64 scalar wires.

### Saved representation

`SavedSimulation` wraps a `GameCircuit` with a schema version. Runtime IDs, cached values, Bevy entities, and runner state are deliberately not serialized.

## Stage 2: executable simulator representation

The integration layer is split between [`src/sim/bridge.rs`](../src/sim/bridge.rs) and [`src/sim/service.rs`](../src/sim/service.rs). The execution engine lives in [`logi-sim`](../logi-sim/src/lib.rs).

### `logi_sim::Circuit`

The executable circuit uses slot-map IDs:

- `NodeId` identifies a runtime node.
- `WireId` identifies a runtime scalar wire.
- `OutputRef` is a `(NodeId, output pin)` source.
- `InputRef` is a `(NodeId, input pin)` sink.

These IDs are valid only for a particular runtime circuit. Rebuilding or switching circuits can produce completely different IDs, so they must never be persisted or attached to long-lived game entities.

`logi-sim` may flatten, pack, reorder, or compile this circuit for a selected runner. Those transformations are implementation details hidden behind the execution service.

### `SimExecutionService`

`SimExecutionService` gives the bridge one interface for both execution modes:

| Implementation | Behavior |
| --- | --- |
| `ThreadedSimService` | Owns a background `SimulationEngine` worker. Native builds use this by default. |
| `DirectSimService` | Steps the engine synchronously on the caller's thread. Web builds and deterministic tools can use this. |

The service accepts raw simulator types because it sits inside the simulation boundary. Higher-level callers interact with `SimBridge` instead.

### `SimBridge`

`SimBridge` converts the model into a `logi_sim::Circuit`, owns the execution service, and maintains identity mappings.

Conceptually it stores:

```text
node UUID -> NodeId
NodeId    -> node UUID

wire UUID -> [WireSegment]
WireId    -> wire UUID
```

A `WireSegment` contains one scalar `WireId` and the source/sink bit positions represented by that runtime wire. Scalar model wires have one segment. Vector model wires have one segment per bit.

For example:

```text
GameWire 7f... (Vector 64)
  -> WireSegment { WireId A, source_pin: 0,  sink_pin: 0  }
  -> WireSegment { WireId B, source_pin: 1,  sink_pin: 1  }
  -> ...
  -> WireSegment { WireId Z, source_pin: 63, sink_pin: 63 }
```

The bridge recursively builds nested `GameCircuit` values as simulator subcircuits. Child input/output nodes determine the runtime subcircuit pin count and ordering. This is why interface labels and stable interface construction are important.

The bridge also owns asynchronous mutation bookkeeping. A newly added model node or wire already has its UUID, while the worker may return its `NodeId` or `WireId` later. Pending entries associate that response with the original UUID.

### Runtime snapshots and events

Runtime values enter the game through two channels:

- A wire-state snapshot provides the current boolean value for every runtime `WireId`.
- `SimEvent` values report lifecycle information such as runner changes and pause/resume state.

Rendering systems do not query the worker directly. The snapshot is read once per frame and translated by `SimCache`.

## Stage 3: UUID cache and Bevy components

The cache is defined in [`src/sim/cache.rs`](../src/sim/cache.rs). Components and shared resources are defined in [`src/sim/mod.rs`](../src/sim/mod.rs).

### `SimCache`

`SimCache` is an opaque Bevy resource containing the latest runtime state in game-space:

```text
wire UUID              -> bool
(node UUID, input pin) -> bool
(node UUID, output pin)-> bool
```

It also contains a private reverse index:

```text
WireId -> [WirePinEntry]
```

Each `WirePinEntry` says which game wire UUID and node UUID/pin correspond to one side of a runtime wire. The index is rebuilt when the active `GameCircuit` changes or when entering simulation.

During each snapshot update, the cache:

1. Clears the previous frame's values.
2. Reads every runtime `WireId` value once.
3. Finds its UUID-facing endpoints through the reverse index.
4. Updates source-node output bits and sink-node input bits.
5. Combines all segments of a model vector wire for its single visual wire state.

The public cache API returns UUID-addressed values:

- `node_input_bits(node_uuid) -> u64`
- `node_output_bits(node_uuid) -> u64`
- `wire_value(wire_uuid) -> bool`
- `is_running() -> bool`

The two node methods pack pins 0 through 63 into a `u64`. Bit `n` corresponds to pin `n`.

For a vector model wire, `wire_value()` is true when any scalar segment is true. This value is intended for visual wire highlighting. Exact bus data comes from the node input/output bitsets.

### Gate entity components

Every rendered node entity has this structure:

| Component | Purpose |
| --- | --- |
| `GateComponent` | Stable node UUID plus editor position. |
| `SimNodeInputs` | Latest input pins packed into `bits: u64`. |
| `SimNodeOutputs` | Latest output pins packed into `bits: u64`. |
| `Transform` | Bevy world position. |
| `Sprite` | Visual gate representation. |
| `Selected` | Optional drag-selection state. |

`GateComponent.id` is the join key between a Bevy entity and `GameCircuit.nodes`. The entity does not store `NodeId`.

### Wire entity components

Every rendered connection entity has:

| Component | Purpose |
| --- | --- |
| `ConnectionLine` | Wire UUID, endpoint node UUIDs, and `WireKind`. |
| `SimWireValue` | Latest visual on/off state. |
| `Transform` | Midpoint, rotation, and length of the rendered line. |
| `Sprite` | Color and thickness. |

The source and sink UUIDs are sufficient to look up gate entities and recalculate geometry after movement. Runtime wire IDs are not needed.

### Display and output text

Display text is a child of its gate entity:

```text
Gate entity
  GateComponent
  SimNodeInputs
  SimNodeOutputs
  Sprite
  `- Text child
       LogicDisplay
       Text2d
       ChildOf(gate entity)
```

`LogicDisplay` describes the number of bits to format. The update system follows `ChildOf` to read `SimNodeInputs` from the parent gate. This is necessary because Bevy queries match components on one entity; a child does not automatically inherit its parent's components.

`Display`, `Output`, `Output8`, and `Output64` values are sink values, so their overlay reads `SimNodeInputs`. Input gates and ordinary logic outputs use `SimNodeOutputs`.

## Frame scheduling and plugins

The plugin structure is defined in [`src/sim/plugin.rs`](../src/sim/plugin.rs).

### `SimulationRuntimePlugin`

Owns execution concerns:

- Creates the initial `GameCircuit` and `SimBridge`.
- Configures threaded or threadless execution.
- Resumes and pauses the runner with game state transitions.
- Flushes pending bridge operations in `PreUpdate`.
- Applies tick-rate and runner configuration.

### `SimulationProjectionPlugin`

Owns the runtime-to-ECS boundary:

- Initializes `SimCache`.
- Builds the reverse UUID/runtime index on state entry.
- Refreshes UUID cache state after bridge flushing.
- Copies cached node and wire values into ECS components.

The important ordering is:

```text
PreUpdate:
  bridge_flush_system
    -> snapshot and lifecycle events enter SimCache
  sync_sim_cache_system
    -> SimCache values enter Bevy components

Update:
  rendering and interaction systems read components only
```

### `SimulationViewPlugin`

Owns presentation concerns:

- Spawns and despawns gate, wire, display, and background entities.
- Updates sprites, display text, wire geometry, and colors.
- Handles picking, dragging, workspace interaction, and simulation UI.

### `SimulationPlugin`

This is the application-facing convenience plugin. It installs runtime, projection, and view plugins in dependency order. Headless tools can install only runtime and projection plugins.

## Example: one 64-bit output path

Consider a counter connected to a display by one vector wire.

### Model

```text
GameNode(counter UUID, 64 outputs)
GameNode(display UUID, 64 inputs)
GameWire(wire UUID, source pin 0, sink pin 0, Vector 64)
```

### Runtime

```text
counter UUID -> counter NodeId
display UUID -> display NodeId
wire UUID    -> 64 WireSegments / 64 WireIds
```

The engine executes 64 scalar connections, regardless of which optimized runner is active.

### Cache and ECS

```text
SimCache.node_output_bits(counter UUID) -> counter value
SimCache.node_input_bits(display UUID)   -> display value
SimCache.wire_value(wire UUID)           -> any bit is high

Counter gate entity: SimNodeOutputs { bits: value }
Display gate entity: SimNodeInputs { bits: value }
Wire entity:         SimWireValue { value: any_bit_high }
```

The display child formats its parent gate's input bitset as hexadecimal. The visual wire can glow when any bit is active without losing the exact 64-bit value stored on the endpoint components.

## Changing or extending the system

When adding a node kind:

1. Add a serializable `GameNodeKind` variant.
2. Define its model input/output counts.
3. Map it to `logi_sim::NodeKind`, or handle it recursively like `SubCircuit`.
4. Add simulator behavior and runner/compiler support.
5. Add its sprite and editor controls.
6. Decide whether its overlay reads `SimNodeInputs` or `SimNodeOutputs`.
7. Test both scalar and vector boundary mappings where applicable.

When adding wider buses, note that the current ECS projection uses `u64`. Supporting widths above 64 requires replacing or supplementing `SimNodeInputs` and `SimNodeOutputs` with a wider representation. The persistent `WireKind` and runtime segment model can remain conceptually the same.

When loading or replacing a circuit:

1. Replace the authoritative `GameCircuit`.
2. Rebuild the `SimBridge` runtime circuit and mappings.
3. Rebuild the `SimCache` reverse index.
4. Respawn or reconcile Bevy entities by UUID.
5. Allow the normal projection pass to populate component values.

Do not preserve runtime IDs across this operation.

## Debugging mapping problems

Set `LOGI_SIM_TRACE` to enable bridge/cache mapping diagnostics.

Useful checks, in order:

1. Confirm source and sink UUIDs exist in `GameCircuit`.
2. Confirm pin offsets plus vector width fit both endpoint pin counts.
3. Confirm subcircuit interface nodes are labeled and expose the expected widths.
4. Confirm `SimBridge::wire_ready(wire_uuid)` after pending operations flush.
5. Confirm `SimCache::wire_value`, `node_input_bits`, or `node_output_bits` by UUID.
6. Confirm the gate/wire Bevy entity has its projected `Sim*` component.
7. For display text, confirm it is a child of the gate carrying `SimNodeInputs`.

If a diagnostic prints an entire nested circuit, the log is formatting a recursive `GameNodeKind` with `Debug`. Use `display_name()` plus UUID and label instead.
