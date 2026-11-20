# Workspace save format (version 5)

Status: implemented in #28 (`src/editor/workspace.rs`); #29 added the
definition-cycle check on load (files written by #28 still load). It is the
foundation for the hand-editable format planned in #40 (definition dedup,
compact repetition such as instance arrays and bus/bit-range wiring); those
extensions are not implemented yet.

## Layout

A workspace named `<name>` is a folder `workspaces/<name>/` (characters
`/ \ : * ? " < > |` in the name are replaced by `_`) containing one file,
`manifest.json`. The file is pretty-printed JSON, written atomically (to
`manifest.json.tmp`, then renamed).

## `manifest.json`

```json
{
  "format": "gatequest-workspace",
  "version": 5,
  "name": "Half Adder",
  "root": "6c1b…",
  "circuits": [ { …circuit… }, … ]
}
```

| Field | Type | Meaning |
|---|---|---|
| `format` | string | Always `"gatequest-workspace"`. |
| `version` | integer | Save format version. This build reads and writes `5` only. |
| `name` | string | Workspace name (informational). |
| `root` | UUID | Id of the circuit that is opened. |
| `circuits` | array | The root circuit first, then every subcircuit definition it uses (transitively), each stored once. |

## Circuit

```json
{
  "id": "6c1b…",
  "name": "Half Adder",
  "nodes": [ { …node… } ],
  "wires": [ { …wire… } ]
}
```

A subcircuit definition is a circuit like any other. Its ports are its
`Input` and `Output` nodes, ordered by `label` (then `id`); that order gives
the instance's pin numbers. `Display` nodes are not ports.

`circuits` is the definition table. A definition is identified only by its
`id`, which is stable: every instance refers to it by id, it is stored
once however many instances there are, and saving and loading keep it.
Placing a saved workspace as a cell uses that workspace's root `id` as the
definition id. When the editor changes a definition's ports, the pin
numbers in wires to its instances are renumbered before the file is
written, so a saved wire always names the port it was connected to.

## Node

```json
{ "id": "1f0e…", "kind": { "type": "Xor", "w": 1 }, "label": "Sum", "x": 0.0, "y": 50.0 }
```

| Field | Type | Meaning |
|---|---|---|
| `id` | UUID | Stable node id; wires refer to it. |
| `kind` | object | What the node is, see below. |
| `label` | string, optional | Required for `Input`, `Output` and `Display` (it names the port); saving refuses unlabelled ports. |
| `x`, `y` | number | Editor position. |
| `layer`, `layer_order` | UUID, integer, optional | Authoring layer; omitted for the default layer. |
| `properties` | object of strings, optional | Free-form metadata. |

### Node kinds

`kind.type` selects the kind; the other fields are its parameters. Widths
(`w`, `len`, `from`, `to`, `widths[i]`) are bits, 1..=64. Pins are numbered
from 0 in the order listed.

| `type` | Parameters | Input pins | Output pins |
|---|---|---|---|
| `Input` | `w` | – | y(w) |
| `Output` | `w` | in(w) | – |
| `Display` | `w` | in(w) | – |
| `Const` | `w`, `value` | – | y(w) |
| `Not` | `w` | a | y |
| `And`, `Or`, `Xor`, `Nand`, `Nor`, `Xnor` | `w` | a, b | y |
| `Mux` | `w` | sel(1), a, b | y (`sel ? b : a`) |
| `Switch` | `w` | en(1), d | y (`en ? d : 0`) |
| `Add` | `w` | a, b, cin(1) | sum, cout(1) |
| `Eq`, `LtU` | `w` | a, b | y(1) |
| `ReduceOr`, `ReduceAnd`, `ReduceXor` | `w` | a | y(1) |
| `Split` | `w`, `lo`, `len` | a(w) | y(len) = `a[lo..lo+len]` |
| `Merge` | `widths` (array) | a0.., one per entry | y(sum), input 0 lowest |
| `Extend` | `from`, `to`, `signed` | a(from) | y(to) |
| `Clock` | `period`, `duty`, `phase` | – | y(1) |
| `Rom` | `addr_bits`, `w`, `contents` (array of integers) | addr | data(w) |
| `Delay` | `w` | d | q |
| `Register` | `w` | en(1), d | q |
| `DLatch` | `w` | en(1), d | q |
| `SrLatch` | – | s(1), r(1) | q(1) |
| `Ram` | `addr_bits`, `w` | addr, we(1), wdata | rdata(w) |
| `SubCircuit` | `definition` (circuit id) | the definition's inputs | the definition's outputs |

Semantics are those of `docs/sim-v2-design.md` section 3.

## Wire

```json
{ "id": "9a2c…", "from": { "node": "1f0e…", "pin": 0 }, "to": { "node": "77b1…", "pin": 1 } }
```

A wire connects output pin `from.pin` of node `from.node` to input pin
`to.pin` of `to.node`. Its width is the width of the driving pin; it must
equal the width of the input pin. An input pin has at most one wire.
Optional `layer`/`layer_order` as for nodes. Wire routing points are not
stored yet.

## What is refused

Loading fails with a message shown in the editor when:

- `manifest.json` has a `version` other than 5 (including the pre-v2
  bundle format, version 4, and anything without a version), or `format` is
  not `"gatequest-workspace"`. Old workspaces are **not migrated**.
- The folder has no `manifest.json` but a legacy single-file save
  `workspaces/<name>.json` exists (format version 1).
- The root circuit is missing, two circuits share an id, a `SubCircuit`
  refers to a definition that is not in the file, or a port is unlabelled.
- A definition contains itself, directly or through other definitions
  (`subcircuit "A" (…) contains itself`).

Kinds with invalid parameters (for example a `Split` past its input width)
load, but the gate is not simulated and its wires show as not connected.
