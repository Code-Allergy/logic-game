# Workspace save format (versions 5 and 6)

Status: version 5 implemented in #28 (`src/editor/workspace.rs`); #29 added
the definition-cycle check on load (files written by #28 still load).
Version 6, the hand-editable authoring form, was added in #40
(`src/sim/source.rs`); the built-in demos are written in it.

The format has two forms that share the `format` tag
`"gatequest-workspace"`:

- **Version 5, the expanded form**, is what the editor writes and what the
  app works with in memory (`WorkspaceDocument`): every node and wire has a
  UUID, every gate is listed, every wire connects one output pin to one
  input pin. It is specified first, below.
- **Version 6, the authoring form** ([Version 6](#version-6-the-authoring-form)),
  is for people and scripts: names instead of ids, node arrays, bus and
  bit-range wiring, and references to definitions in other files. A
  version 6 file is expanded into version 5 when it is loaded.

This build reads both versions and writes version 5. A workspace saved by
the editor is therefore always version 5, also when it started as a
version 6 file or as a copy of a demo (see [Round trips](#round-trips)).

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
| `version` | integer | `5` for this form (version 6 is described below). |
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

- `manifest.json` has a `version` other than 5 or 6 (including the pre-v2
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
(Version 6 is stricter: an invalid kind is a load error.)

---

## Version 6: the authoring form

A version 6 file describes the same circuits as version 5, written for
people editing by hand and for scripts generating large circuits:

- circuits, nodes and pins are referred to **by name**;
- a definition is written once and instanced with **`use`**, also from
  another file (`"use": "half_adder/half_adder"`);
- **arrays**: one node entry with `count` and `step` is N nodes;
- **bus and bit-range wiring**: one wire entry connects a word to N bits,
  N bits to a word, or a bit field, and the adapters are generated;
- **positions** are stored for every node; array elements and generated
  adapters get positions computed deterministically.

The built-in demos in `assets/demos/` are version 6 files; they are the
best examples. A workspace folder may also hold a version 6
`manifest.json`; it is expanded when opened (its `"demo/circuit"`
references resolve to the built-in demos).

### File

```json
{
  "format": "gatequest-workspace",
  "version": 6,
  "root": "main",
  "circuits": [ { …circuit… }, … ]
}
```

| Field | Type | Meaning |
|---|---|---|
| `format` | string | `"gatequest-workspace"`. |
| `version` | integer | `6`. |
| `name` | string, optional | Workspace name (informational; demos take theirs from the manifest). |
| `root` | string | Name of the circuit that is opened. |
| `circuits` | array | The circuits of the file, in any order. |

Unknown fields are refused everywhere (so a typo is an error, not a silent
default).

### Circuit

```json
{
  "name": "full_adder",
  "title": "Full adder",
  "nodes": [ { …node… } ],
  "wires": [ [ …wire… ] ]
}
```

| Field | Type | Meaning |
|---|---|---|
| `name` | string | Unique in the file; letters, digits and `_`, not starting with a digit. `use` refers to it. |
| `title` | string, optional | Display name (the definition table, the breadcrumb). Defaults to `name`. |
| `nodes` | array | Nodes, see below. |
| `wires` | array | Wire entries, see below. |

As in version 5, a circuit used as a definition has its `Input` and
`Output` nodes as ports, ordered by label; with names in wires the order
rarely matters.

### Node

```json
{"name": "x", "type": "Xor", "at": [0, 50]}
{"name": "zero", "type": "Const", "w": 8, "value": "0x00", "at": [-400, 250]}
{"name": "ha", "use": "half_adder/half_adder", "label": "first half", "at": [-120, 50]}
{"name": "fa", "use": "full_adder/full_adder", "at": [0, 280], "count": 8, "step": [0, -80]}
```

| Field | Type | Meaning |
|---|---|---|
| `name` | string | Unique in the circuit; letters, digits and `_`. Wires refer to it. |
| `type` | string | A primitive: one of the kinds of the version 5 table (not `SubCircuit`). Its parameters are fields next to it (`w`, `value`, `lo`, `len`, `widths`, `period`, …), with the same meaning. |
| `use` | string | Instead of `type`: an instance of the circuit `"name"` in this file, or of circuit `circuit` in demo `demo` as `"demo/circuit"`. |
| `label` | string, optional | Shown in the editor. For ports it is the port name; it defaults to the node's `name` (and to `name` followed by the index for array elements). Instances also default to their name (`fa[3]`); other primitives have no label unless given. In an array, `{i}` in the label is replaced by the index. |
| `at` | `[x, y]` | Position (required). |
| `count`, `step` | integer, `[dx, dy]` | An array of `count` nodes (1 to 4096). Element `i` is named `name[i]` and placed at `at + i * step`. |

Parameter conveniences: in an array, `{i}` in a string parameter is
replaced by the element's index, and a value that is then an integer is
that number: `{"name": "bit", "type": "Split", "w": 8, "lo": "{i}", "len":
1, "count": 8, …}` is the eight 1-bit slices of a byte, `{"type": "Const",
"w": 3, "value": "{i}", "count": 8, …}` the constants 0 to 7. The elements
of an array must still have the same pins (the same widths), and `{i}`
outside an array is just text. `w` defaults to `1` for every kind that has one;
`value` and the entries of `contents` may be strings in hex (`"0x1F"`),
binary (`"0b101"`) or decimal, with `_` separators allowed. Parameters a
kind does not have are refused, and so are invalid ones (for example a
`Split` past its input width).

### Wires and references

A wire entry is an array of strings: one source and one or more sinks.

```json
["A", "xor.A", "and.A"]
["fa[0..7].Cout", "fa[1..8].Cin"]
["fa.Sum", "Sum"]
["rom:12..16", "control"]
```

A reference is `node[index].pin:bits`, every part but `node` optional:

| Part | Meaning |
|---|---|
| `node` | A node name. For an array it means every element, in order. |
| `[i]`, `[a..b]` | One element, or elements `a` to `b - 1`, of an array node. |
| `.pin` | A pin by name, or by number. Sources name output pins, sinks input pins. Primitive pin names are those of the editor (`a`, `b`, `y`, `sel`, `en`, `d`, `q`, `sum`, `cout`, `cin`, `addr`, `data`, `in`, `a0`, `a1`, … for `Merge`); an instance's pins are its definition's port labels. May be omitted when the node has exactly one pin on that side. |
| `:i`, `:a..b` | Bit `i`, or bits `a` to `b - 1`, of the pin: a bit field. On a sink it drives only those bits of the input. |

Ranges are half-open, like Rust's. A pin name in a reference cannot
contain `:`; use the pin number for such labels.

An entry is read as follows. The source reference names a list of pins
(several for an array); their bits are concatenated, element 0 lowest,
into a source of `W` bits. For each sink reference:

1. if every pin it names takes exactly `W` bits, each gets the whole
   source (**fan-out**: `["Clk", "ff.Clk"]` clocks all eight flip-flops);
2. otherwise, if its pins take `W` bits together, the source bits are
   **dealt out in order** (a bus: `["A", "fa.A"]` sends bit `i` of the
   8-bit `A` to `fa[i]`, `["fa.Sum", "Sum"]` gathers eight bits into one
   word, `["fa[0..7].Cout", "fa[1..8].Cin"]` chains carries);
3. otherwise the entry is refused, with both widths in the message.

Every bit of an input pin is driven at most once over all entries, and an
input pin that is driven at all must have all its bits driven. Pins never
mentioned stay unconnected (they read 0).

### Expansion into version 5

- **Ids** are UUID v5, so expanding the same file always gives the same
  documents. Circuit `c` of a file with key `K` has id
  `v5(NS, "K/c")`, where `NS` is the fixed namespace
  `6a7e5f3c-1b2d-4e8f-9a0b-c1d2e3f4a5b6` and `K` is `demos/<demo id>`
  for a demo and `workspaces/<folder>` for a workspace file. Inside a
  circuit with id `C`: node `n` (array element `n[i]`) has id
  `v5(C, "node/n")`; the wire into input pin `p` of node `n` has id
  `v5(C, "wire/n/p")`. Editor-made ids are random (UUID v4), so they never
  collide with derived ones.
- **Cross-file references** `"demo/circuit"` resolve through the demo
  manifest to the circuit's derived id, and the referenced definition (with
  everything it instances) is added to the workspace as a shared library
  definition. Loading two demos that use the same definition yields one
  library entry.
- **Adapters.** Where a sink pin gets bits that are not one whole source
  pin, `Split` nodes cut the source pins (one per distinct `(pin, lo,
  len)`, shared by every sink that needs it) and a `Merge` node joins the
  pieces (neighbouring bits of the same source pin are joined first, so
  `["A:0..4", "Y:0..4"]` plus `["A:4..8", "Y:4..8"]` is one plain wire).
  A split of output pin `p` of node `n` is node `~split/n/p/lo/len`; the
  splits of `n` stand in a column to its right, the first level with `n`
  and each further one 60 lower (a box and a gap), at a distance that
  leaves 30 between the boxes, rounded up to the 20-unit grid (80 for an
  ordinary 50-wide node). The merge into input pin `p` of node `n` is node
  `~merge/n/p`, in a column to the left of `n` placed the same way. So
  adapters never overlap each other or their node; to place them exactly,
  write the `Split` and `Merge` nodes yourself (an array with `"lo":
  "{i}"`, below, cuts a word into its bits). Their ids follow the rule
  above. (Before the gallery, adapters were placed 70 beside their node
  and stacked 30 apart, so they overlapped; files are read the same way,
  only the generated positions changed.)
- Nodes and wires of each expanded circuit are sorted by id (the order the
  editor writes).

### Canonical form

The canonical text of a file is what `gate_quest::sim::source::to_text`
prints: two-space indentation; fields in the order of this document
(`name`, then `type` and parameters or `use`, then `label`, `at`, `count`,
`step`); an object or array that fits on its line within 100 columns is
written on one line; otherwise objects get one field per line, arrays of
numbers or strings are filled line by line, and other arrays get one
element per line. `cargo run --example demo_fmt` rewrites the demo files
in canonical form (`-- --check` only checks; file arguments select other
files).

### Round trips

- **Text.** `to_text(parse(t)) == t` for a canonical file `t`, and
  `parse(to_text(f)) == f` for any file `f`: loading and saving the
  authoring form keeps everything, arrays and bus wiring included. Every
  demo is checked.
- **Expansion.** Expansion is deterministic: the same file and the same
  referenced demos give identical version 5 documents.
- **Version 5.** Saving the expansion as version 5 and loading it gives
  the same documents (checked for every demo, also through the editor).
- **What the editor keeps.** The editor works on the expanded circuit, so
  a workspace it saves is version 5: definitions stay stored once and
  instanced by id, positions and labels are kept, but arrays become their
  elements, bus wiring becomes its `Split`/`Merge` adapters and wires,
  and cross-file references become definitions stored in the file.
  Nothing is lost semantically; the compact spelling is.

### Imported netlists

`cargo run --example rtl_import` and the editor's **Import RTL…** turn a
Yosys JSON netlist into a version 6 file (positions from the auto-layout,
only whole-pin wires: slices, constants and concatenations are explicit
`Split`, `Const` and `Merge` nodes) and its version 5 expansion. See
`docs/rtl-import.md`.

---

## Demos

The built-in demos live in `assets/demos/`: one version 6 file per demo,
`<id>.json`, and `manifest.json`:

```json
{
  "format": "gatequest-demos",
  "version": 1,
  "levels": [ { "level": 1, "title": "Gates" }, … ],
  "demos": [
    {
      "id": "full_adder",
      "level": 2,
      "title": "Full adder",
      "description": "Adds three bits …",
      "uses": ["gates/or", "half_adder/half_adder"]
    },
    …
  ]
}
```

| Field | Meaning |
|---|---|
| `levels` | Level numbers and their titles (headings in the demo list). |
| `demos` | The demos in pyramid order; the editor lists them in this order. |
| `id` | The demo's id: its file is `<id>.json`, its circuits are referenced as `"<id>/<circuit>"`. |
| `level` | Its level; levels never decrease down the list. |
| `title`, `description` | Shown in the demo list and while the demo is open. |
| `uses` | Exactly the `"demo/circuit"` references in the file (checked on load). Each must name a demo listed earlier, at the same or a lower level. |

A demo opens read-only: inputs can be toggled and subcircuits opened, but
edits are refused until **Copy to my workspace** makes an editable copy
(saved as the version 5 workspace `<title> (copy)`; the copy gets fresh
ids for its root and every definition, so editing it never changes a demo
or another copy).

The files are embedded in the binary (`include_str!`, list in
`src/sim/demos.rs`) because the web build has no filesystem. Native builds
read `assets/demos/` from the working directory instead when it exists, so
edits show up after a restart without a rebuild; if those files do not
load, the embedded copies are used and a warning is logged. To add a demo,
write its file, add it to the manifest and to the `embed!` list, and run
`cargo run --example demo_fmt`; the tests check that the folder, the
embedded list and the manifest agree.

## Gallery

`assets/gallery/` holds the gallery: 27 more circuits beside the
curriculum, from a majority vote to a CPU running Fibonacci. Its
`manifest.json` has the shape of the demo manifest with categories in
place of levels:

```json
{
  "format": "gatequest-gallery",
  "version": 1,
  "categories": [ { "id": "combinational", "title": "Combinational logic" }, … ],
  "items": [
    {
      "id": "magnitude",
      "category": "combinational",
      "title": "4-bit magnitude comparator",
      "description": "Four one-bit comparators …",
      "uses": ["comparator/cmp1"]
    },
    …
  ]
}
```

| Field | Meaning |
|---|---|
| `categories` | Group ids and titles (headings in the editor's Gallery list). |
| `items` | In order, simple to complex; the items of a category are listed together, categories in their order. |
| `id` | The item's id: its file is `<id>.json`, its circuits are referenced as `"<id>/<circuit>"`. Ids share that namespace with the demos, so an item id is never a demo id. |
| `uses` | Exactly the references in the file, each to a curriculum demo or to a gallery item listed earlier. |

Item ids derive from the key `gallery/<id>` (UUID v5, as for demos). The
gallery is loaded after the demos into the same catalog
(`src/sim/demos.rs`, `DemoCatalog::gallery`), so workspace files can use
its circuits too, and it is embedded the same way (`GALLERY_EMBEDDED`).
The editor shows it as a second group, **Gallery**, under the demos; items
open read-only and can be copied like demos. `demo_fmt` formats and checks
both folders.

| Category | Items (circuits beyond the root) |
|---|---|
| Combinational | majority vote; one-bit comparator (`cmp1`); 4-bit magnitude comparator (`mag4`, beside `Eq`/`LtU`); 8-bit parity XOR tree (beside `ReduceXor`); Gray code both ways; 8-to-3 priority encoder; 7-segment decoder driving seven lights; BCD to binary; 8-bit barrel shifter; 4-bit carry-lookahead adder (`lookahead`, `cla4`, beside `ripple4`); 4x4 array multiplier |
| Sequential | edge detector (`rising`); 4-bit shift register; Johnson counter; 8-bit LFSR (`lfsr8`); clock divider; 8-bit up/down counter; BCD counter (`mod_counter`); debouncer; PWM generator; sequence detector (1011, ROM state table); traffic light; stopwatch (59.9 s) |
| Systems | 8-entry stack on a RAM; UART transmitter; reaction timer game (`countdown`, `timer2`); CPU with ADDI/SUBI/ANDI printing the Fibonacci numbers |

### Previews

`cargo run --example render_svg -- <id | file.json>... [--out DIR]
[--circuit NAME] [--ortho]` writes an SVG of every circuit of a demo,
gallery item or file (default folder `target/previews`): the expanded
circuit with the editor's box sizes and colours, labels, generated adapters
dashed, and wires straight from centre to centre as the editor draws them
(`--ortho`: horizontal and vertical segments instead). It prints each
circuit's node count and two layout checks: overlapping boxes and wires
that pass through a box. The gallery was laid out by hand with it, and
its tests check that no boxes overlap and every node is on the 20-unit
grid.
