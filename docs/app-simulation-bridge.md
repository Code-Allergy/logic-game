# ECS authoring, documents, and the simulation bridge

GateQuest has one live authoring model: Bevy ECS. The simulator's `Design`
is derived from it one-to-one; documents are produced from it only on
demand. (This replaces the earlier `GameCircuit` snapshot and `SimCache`
model; see `docs/sim-v2-design.md` sections 10 and 13.)

```text
workspace file / built-in demo (CircuitDocument + definitions;
                                 demos and version 6 files expanded first)
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
| `LoopHighlight` | On a combinational loop of the last failed compile (visual only). |

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
messages handled in `Update` (`src/sim/authoring.rs`, `src/sim/view.rs`):
`OpenInstanceView`, `OpenDefinitionView`, `CloseView`, `CreateGate`,
`CreateWire`, `DeleteGate`, `DeleteWire`, `SetInputValue`, `SetGateKind`,
`RegisterDefinitions`, `GateLabelChanged`, `DisplayConfigChanged`,
`GateMoved`, `EntityLayerChanged`, `LoadWorkspaceEvent`,
`SaveWorkspaceEvent`.

`LoadWorkspaceEvent::read_only` opens a built-in demo read-only
(`ActiveCircuit::read_only`): the handlers refuse every edit (create,
delete, kind, label, move, layer) and saves with `READ_ONLY_MESSAGE` in
`AuthoringFeedback`, while inputs, display settings and instance views
keep working. The editor's "Copy to my workspace" loads an editable copy
with fresh definition ids (`sim::demos::editable_copy`).

## Bridge

`SimBridge` (`src/sim/bridge.rs`) owns a persistent `Design`, maps
entities to `NodeId`s, and in `PostUpdate` applies the frame's ECS changes
through the `Design` mutation API (removed wires, removed gates, added or
re-kinded gates, added or changed wires). If anything changed it sends the
new revision to the engine: `engine::Engine` on a worker thread
natively, `engine::LocalEngine` in-frame on wasm. The engine compiles
off the tick loop and carries register/RAM state across the swap. A
compile error (e.g. a combinational loop) keeps the old program running
and is published in `SimDiagnostics`.

Subcircuit definitions live once in the `CircuitLibrary` resource (the
definition table), keyed by stable `Uuid`; the bridge builds one `Design`
definition per instanced library entry and instances it. When a library
entry changes, the bridge updates that definition in place, matching nodes
by document id, so unchanged nodes keep their `NodeId` and their state.

## Subcircuit views

`CircuitView` (`src/sim/view.rs`) is the stack of opened instances, as gate
ids from the root. Opening one (`OpenInstanceView`, or `OpenDefinitionView`
from the definition table; `CloseView` goes back) parks the root's
entities with Bevy's `Disabled` marker and spawns the definition's gates
and wires marked `InstanceViewItem`. The bridge resolves the gate ids to
the instance's node path and probes the definition's nets there, so the
view shows the live values of that instance in the running root
simulation; no second simulation runs and nothing is recompiled.

Edits in a view edit the shared definition: `write_back_view_system`
writes the view's document into the library (renumbering wires to its
instances if ports moved), and the bridge updates the design. Inputs in a
view are driven by the parent and cannot be toggled. Placing a definition
that would contain itself is refused with a message in
`AuthoringFeedback`.

## Readback

The open view decides the probes: at the root, one probe per output net
of every root gate; in a subcircuit view, one per output net of the
definition at the instance path. When a new probe frame arrives
(new tick, revision or probe layout), `project_probes_system` writes
`GateValues` and `WireState::value` where they changed; otherwise it does
nothing.

## Visuals

`SimulationVisualsPlugin` (`src/sim/visuals.rs`) draws gates and wires
from those components, driven by change detection, so an idle circuit
costs no per-entity work. The look ("Studio Workshop") comes from
`src/sim/art.rs` and the `Theme` resource (`src/theme.rs`):

- a gate is a tile: an ink outline with a solid bottom edge, a face tinted
  by family (logic white, state violet, I/O sky), a symbol (AND, OR, XOR,
  NOT and their inverses, MUX, clock) or a caption, and pins on the left
  (inputs) and right (outputs) edges, pin 0 at the top (`PinLayout`). The
  pieces are `Mesh2d` children built from cached meshes (`MeshKey`) with
  materials shared by colour role (`Paint`), so a theme change rewrites
  about twenty materials, not every entity;
- pins, lamps and input switches follow `GateValues` by swapping shared
  materials; display text is re-formatted only when its value, width or
  format changes;
- wires are routed on the 20-unit grid (`src/sim/routing.rs`): each net
  (a driver pin and its sinks) becomes a tree of horizontal and vertical
  runs that leave outputs to the right and enter inputs from the left,
  keep clear of the tiles, share trunks, branch at T junctions (never
  four-way) and never overlap another net; where two nets meet they
  cross straight. It is an A* search per sink on a sparse grid graph with
  costs for length, bends and crossings, nets shortest first, with
  rip-up and reroute for sinks that fall back or cut a tile.
  `route_wires_system` routes the open view when gates or wires are added
  or removed, a pin layout or the view changes, or a gate moves; a move
  keeps the path of every sink whose pins stayed put, so dragging a gate
  re-routes only its wires (and branches hanging off them) and never
  touches the `Design`. Paths a dragged gate now covers are fixed on the
  first frame without movement. `render_svg --ortho` uses the same router;
- a wire is cased: an ink casing under a coloured core (marigold on, slate
  off, teal or pale teal for buses, thicker for buses), each a mesh along
  the routed path with r 20 corners. A value change only swaps the core's
  shared material; the meshes are rebuilt only when the path changes. A
  branch gets a junction dot; where one wire passes over another the
  bench shows through a gap in the lower one (nets get distinct depth
  slots, and one gap mesh per view sits just below each upper wire). A
  bus carries a value pill on its net's longest horizontal run;
- a combinational-loop compile error (`SimDiagnostics::compile_error`,
  with `loops` as gate ids and instance paths) marks the gates and wires
  on the loop in the open view with `LoopHighlight` (tomato), until a
  later revision compiles. The previous program keeps running meanwhile;
- the selected gate gets a halo; the bench is a single quad with a
  repeating dot texture.
