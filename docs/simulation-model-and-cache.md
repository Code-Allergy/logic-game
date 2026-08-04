# ECS authoring, workspace documents, and simulation cache

GateQuest has one live authoring model: Bevy ECS. Workspace and simulator
representations are generated from it and must not be mutated as competing
sources of truth.

```text
workspace package
    -> portable GameCircuit document
    -> ECS authoring entities (authoritative while open)
    -> generated GameCircuit snapshot (only when revision changes)
    -> SimBridge UUID-to-runtime compilation
    -> logi-sim NodeId/WireId graph
    -> SimCache UUID snapshot
    -> GatePinState / WireState ECS projection
```

`GameCircuit` remains the portable document and simulator adapter so fixture and
workspace formats stay independent of Bevy internals. It is no longer the
routine editor mutation target. Editor commands change ECS; the snapshot system
then regenerates `GameCircuit` before bridge rebuilds and saves.

## Gate entities

Each gate is composed from focused components:

| Component | Responsibility |
| --- | --- |
| `GateIdentity` | Stable game UUID. |
| `CircuitMember` | Owning circuit UUID. |
| `CircuitLayer` | Domain layer UUID and ordering. |
| `GateLabel` | Required/editable display label. |
| `GateType` | Lightweight logical gate classification. |
| `GatePorts` | Input and output pin widths. |
| `GatePinState` | Cached runtime input/output bitmasks. |
| `PersistentGateData` | Extra portable properties. |
| `Transform` | Sole runtime location authority. |
| `ClockConfig` | Optional clock period and duty configuration. |
| `DisplayConfig` | Optional width, radix, and zero-padding configuration. |
| `SubCircuitInstance` | Optional definition/instance identity and interface metadata. |
| `Name`, `Sprite`, `Visibility` | Derived presentation state. |

`GateComponent` is a temporary non-reflected compatibility component used by
older selection code. New systems should query the focused components above.

`SubCircuitInstance` exposes the definition UUID and name, runtime instance
UUID, input/output counts, nesting depth, and labeled interface ports. Its full
definition document is hidden from reflection and is used only when generating
the portable bridge/workspace snapshot.

## Wire entities

One logical wire is one entity, regardless of scalar/vector width:

| Component | Responsibility |
| --- | --- |
| `WireIdentity` | Stable logical wire UUID. |
| `WireEndpoints` | Source/sink gate UUIDs and pin indexes. |
| `WireProperties` | Width and portable wire kind. |
| `WireState` | Packed current value (`u64`). |
| `CircuitMember` | Owning circuit UUID. |
| `CircuitLayer` | Domain layer UUID and ordering. |
| `ConnectionLine`, `Transform`, `Sprite` | Non-reflected rendering compatibility. |

Wire entities are children of a `WireCollection` entity only to make the world
inspector collapsible. Bevy hierarchy does not define circuit or layer
ownership; `CircuitMember` and `CircuitLayer` do.

A vector game wire can compile into many simulator `WireId` segments. The
bridge/cache boundary packs those segment values back into one `WireState`:

```text
WireIdentity(UUID, width=64)
    -> SimBridge segments [WireId; 64]
    -> SimCache.wire_bits(UUID)
    -> WireState.value
```

## Authoring commands

Cross-plugin edits use validated Bevy messages:

- `GateLabelChanged`
- `ClockConfigChanged`
- `DisplayConfigChanged`
- `GateMoved`
- `EntityLayerChanged`
- `AuthoringTopologyChanged`

The application order is:

```text
messages
    -> validate and mutate ECS
    -> increment WorkspaceRevision
    -> regenerate portable snapshot
    -> rebuild bridge/cache when topology changed
    -> process workspace save
```

Direct world-inspector edits are detected through Bevy change detection. Clock
and display edits emit a topology rebuild; transform, label, and layer edits
mark the workspace dirty.

## Saving and loading

`WorkspaceRevision.current` increments after authoring changes and
`WorkspaceRevision.saved` advances after a successful save. Snapshot generation
is skipped when the revision and active circuit have not changed.

Saving is explicitly scheduled after authoring messages and snapshot generation,
so a save requested in the same frame sees the new values.

The package remains a versioned, portable document format rather than a raw
serialized Bevy world. This intentionally excludes rendering entities,
selection, simulator IDs, cache values, and Bevy type paths. Layer membership is
stored in gate portable properties and explicit wire fields; old files default
to the default layer.

Loading performs the inverse operation:

1. Read and validate the workspace package.
2. Resolve subcircuit definitions.
3. Replace `ActiveCircuit` and the portable import snapshot.
4. Spawn focused ECS authoring components.
5. Compile the bridge and rebuild cache mappings.
6. Reset workspace revision state.

## Simulation boundary

Only `SimBridge` and `SimCache` interact with simulator `NodeId` and `WireId`
values. Every game/editor-facing relationship uses UUIDs.

`SimCache` takes one runner snapshot per frame and stores:

- packed node input bits by gate UUID;
- packed node output bits by gate UUID;
- packed logical wire bits by wire UUID;
- runner lifecycle status.

The projection system writes cache values into `GatePinState` and `WireState`.
Rendering, overlays, inspectors, and gameplay systems should consume these ECS
components rather than locking or querying the simulator directly.

## Adding a gate configuration

When a new gate needs persistent settings:

1. Add a focused reflected component, such as `ClockConfig`.
2. Attach it in `configuration_components` during load/spawn.
3. Convert it back in `node_kind_from_components` during snapshot generation.
4. Add a validated change message if other plugins or UI can edit it.
5. Mark topology dirty if the setting changes simulator structure or behavior.
6. Register the component type in `SimulationViewPlugin` for inspection.

Do not add simulator IDs, location, or cached electrical state to configuration
components. Use UUIDs, `Transform`, and the projection components respectively.
