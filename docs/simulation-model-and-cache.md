# ECS authoring, documents, and the simulation bridge

GateQuest has one live authoring model: Bevy ECS. The simulator's `Design`
is derived from it one-to-one; documents are produced from it only on
demand. (This replaces the earlier `GameCircuit` snapshot and `SimCache`
model; see `docs/sim-v2-design.md` sections 10 and 13.)

```text
workspace file / built-in example (CircuitDocument + definitions)
    -> LoadWorkspaceEvent: spawn ECS authoring entities (authoritative)
    -> sync_design_system: edit SimBridge's persistent logi_sim Design
    -> v2 engine: compile revision, swap program, carry state
    -> probe frame -> GateValues / WireState (project_probes_system)
```

## Gate entities

| Component | Responsibility |
| --- | --- |
| `GateIdentity` | Stable UUID (wires and saves refer to it). |
| `GateType { kind: GateKind }` | What the gate is, with its parameters (width, clock period, …). Maps to one `logi_sim::netlist::NodeKind`, or an `Instance` for `SubCircuit`. |
| `GateLabel` | Label; names ports. |
| `Transform` | Position (authoritative). |
| `CircuitMember`, `CircuitLayer`, `PersistentGateData` | Owning circuit, layer, free-form metadata. |
| `InputDrive` | Value the editor drives onto an `Input` gate. |
| `DisplayConfig` | Radix / leading zeroes of a `Display`. |
| `GateValues` | Simulated pin values (one word per pin), from probes. |
| `GateProbes` | Probe indices feeding `GateValues`. |

## Wire entities

| Component | Responsibility |
| --- | --- |
| `WireIdentity` | Stable UUID. |
| `WireEndpoints` | Source gate/pin and sink gate/pin (by gate UUID). |
| `CircuitMember`, `CircuitLayer` | As for gates. |
| `WireState` | Value, width, whether the simulator accepted it, probe index. |

A wire is identified in the `Design` by its sink pin; one driver per input
pin, so creating a wire into a connected pin replaces the old wire.

## Commands

Editor UI never mutates components directly for topology. It sends
messages handled in `Update` (`src/sim/authoring.rs`): `CreateGate`,
`CreateWire`, `DeleteGate`, `DeleteWire`, `SetInputValue`, `SetGateKind`,
`RegisterDefinitions`, `GateLabelChanged`, `DisplayConfigChanged`,
`GateMoved`, `EntityLayerChanged`, `LoadWorkspaceEvent`,
`SaveWorkspaceEvent`.

## Bridge

`SimBridge` (`src/sim/bridge.rs`) owns a persistent `Design`, maps
entities to `NodeId`s, and in `PostUpdate` applies the frame's ECS changes
through the `Design` mutation API (removed wires, removed gates, added or
re-kinded gates, added or changed wires). If anything changed it sends the
new revision to the engine: `engine_v2::Engine` on a worker thread
natively, `engine_v2::LocalEngine` in-frame on wasm. The engine compiles
off the tick loop and carries register/RAM state across the swap. A
compile error (e.g. a combinational loop) keeps the old program running
and is published in `SimDiagnostics`.

Subcircuit definitions live once in the `CircuitLibrary` resource; the
bridge builds one `Design` definition per library entry and instances it.

## Readback

The bridge registers one probe per output net of every root gate. Each
frame `project_probes_system` reads the latest probe frame and writes
`GateValues` and `WireState::value` where they changed. Visual systems read
only those components.
