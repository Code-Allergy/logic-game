# RTL import and auto-layout

Status: implemented for #42 (`src/sim/rtl.rs`, `src/sim/layout.rs`,
`examples/rtl_import.rs`).

A circuit written in Verilog (or anything else Yosys reads) can be brought
in as an editable workspace: Yosys writes a JSON netlist, the importer maps
it onto the editor's primitives and definitions, and every circuit gets an
automatic layered layout. The result is an ordinary workspace: version 5 in
memory (what the editor opens and saves) and, if wanted, version 6 text
(see `docs/save-format.md`).

## Producing the netlist

```sh
yosys -p 'read_verilog top.v; hierarchy -top top; proc; opt; write_json top.json'
```

`proc; opt` turns `always` blocks into `$dff`/`$mux` cells. Running more
of `synth` is fine as long as only supported cells remain (below); add
`flatten` to import one flat circuit instead of one definition per module.
`synth -run :fine` keeps word-level cells; a full `synth` or `abc` gives the
gate-level `$_AND_` family.

The checked-in example `examples/rtl/top.json` (a 4-bit ripple adder made
of full-adder instances, a 4-bit ALU and a counter, see `top.v` next to it)
was written by hand in this format; it was not produced by Yosys.

## Running the importer

```sh
cargo run --example rtl_import -- examples/rtl/top.json out.json          # version 6
cargo run --example rtl_import -- in.json out.json --v5                   # version 5
cargo run --example rtl_import -- in.json out.json --top cpu --name "CPU"
```

Copy the output to `workspaces/<name>/manifest.json` to open it in the
editor. From Rust: `gate_quest::sim::rtl::import_yosys_json(text,
&ImportOptions)` returns the version 6 [`SourceFile`], the expanded
`WorkspaceDocument` and a list of warnings.

In the native editor, **Import RTL…** (next to the workspace buttons) takes
the path of a Yosys JSON file and opens the import as an unsaved, editable
workspace named after the top module; *Save Workspace* writes it as
version 5. The web build has no file system, so the action is not shown
there.

## Mapping

**The top module** is the one named with `--top`, else the module with the
`top` attribute, else the only module no other module instances (several
candidates are an error that lists them). Only modules reachable from it
are imported.

**Modules become circuits** (definitions), one per module, named after it
(non-identifier characters become `_`; the original name is kept as the
title). A cell whose type is a module of the file becomes an instance
(`use`) of that circuit. Ports become `Input`/`Output` nodes labelled with
the port name, so instance pins are the port names.

**Cells become primitives**, keeping word widths (1 to 64 bits):

| Yosys cell | Primitives |
|---|---|
| `$not`, `$_NOT_` | `Not` |
| `$and`, `$or`, `$xor`, `$xnor` | `And`, `Or`, `Xor`, `Xnor` of width `Y_WIDTH` |
| `$_AND_`, `$_OR_`, `$_XOR_`, `$_NAND_`, `$_NOR_`, `$_XNOR_` | the 1-bit gate |
| `$_ANDNOT_`, `$_ORNOT_` | `Not` on B, then `And`/`Or` |
| `$mux`, `$_MUX_` | `Mux` (`S ? B : A`) |
| `$_NMUX_` | `Mux`, then `Not` |
| `$add` | `Add` of width `Y_WIDTH` (carry in unconnected, carry out unused) |
| `$sub` | `Not` on B, `Add` with carry in 1 |
| `$eq`, `$ne` | `Eq` (then `Not`); upper result bits are 0 |
| `$reduce_and`, `$reduce_or`, `$reduce_bool`, `$reduce_xor` | `ReduceAnd`, `ReduceOr`, `ReduceOr`, `ReduceXor` |
| `$reduce_xnor`, `$logic_not` | `ReduceXor` / `ReduceOr`, then `Not` |
| `$pos`, `$_BUF_` | nothing: the output is the input |
| `$dff`, `$_DFF_P_`, `$_DFF_N_` | `Delay` |
| `$dffe`, `$_DFFE_PP_` … `$_DFFE_NN_` | `Register` (`Not` on an active-low enable) |
| `$scopeinfo` | ignored |

Operands are extended or cut to the result width as Verilog does: sign
extension when both operands are signed (`A_SIGNED`/`B_SIGNED`), zero
extension otherwise.

**Wiring.** Yosys connects pins to lists of bits. When a pin takes exactly
one whole output pin, one wire connects them. Otherwise the importer adds
a `Split` for each slice of an output pin it needs (shared by all its
users), a `Const` for each run of constant bits, and a `Merge` that joins
the pieces. These adapters are ordinary nodes, so the layout places them;
the version 6 file has only whole-pin wires.

**Approximations** (reported as warnings, not errors):

- Flip-flops are clocked by the simulator's single global tick: the clock
  pin and its polarity are ignored, so a `$dff` takes `D` once per tick
  (see `docs/sim-v2-design.md` section 3.2). A clock input stays as an
  unused input port.
- `x` and `z` bits, and bits nobody drives, read as 0.

## What is refused

The import fails with a message naming the module and cell when:

- a cell type is not in the table above (for example `$mul`, `$shl`,
  `$lt`, `$pmux`, `$adff`, `$sdff`, `$dlatch`, `$mem_v2`, `$print`); the
  message lists the supported cells. `techmap` or `memory_map` can lower
  many of them first;
- a port or cell pin is wider than 64 bits;
- a port is `inout`;
- a module instances a module that is not in the file, or a black box;
- a net has two drivers;
- the top module is ambiguous, or `--top` names no module.

## Auto-layout

`gate_quest::sim::layout` places a circuit in layers (Sugiyama style). It
runs per circuit; instances are boxes like any other node. It is used for
every imported circuit and can lay out any `CircuitDocument`
(`layout_circuit`), for example one without useful positions.

1. **Cut feedback.** Edges out of state elements (`Delay`, `Register`,
   `SrLatch`) do not count for ranking, as for the compiler's loop check.
   Cycles that remain (through instances that hold state, or a real
   combinational loop the compiler will report) are broken by dropping the
   back edges of a depth-first search in node order.
2. **Rank by logic depth.** Inputs are rank 0. Every other node is one more
   than its deepest predecessor (at least 1). A node with no predecessor
   (a constant) moves right, next to its first consumer. Outputs and
   displays get the last rank. Empty ranks are removed.
3. **Reduce crossings.** Starting from node order (ports in declaration
   order, then cells in file order, then adapters), six rounds of a
   downward and an upward barycenter sweep sort each rank by the mean
   position of its neighbours in earlier (or later) ranks. Crossings
   between adjacent ranks are counted after each sweep, and the best order
   is kept.
4. **Coordinates.** One column per rank, left to right, 100 units apart
   (plus the widest box). Nodes are stacked top to bottom in their order,
   30 units apart, each column centred on y = 0. Centres are snapped to the
   20-unit grid, and boxes (`GateKind::size`) never overlap.

The layout depends only on the node and edge lists, so importing the same
netlist always gives the same positions (and, with ids derived from the
workspace name, the same documents).

## Not yet

- Other formats (BLIF, a Verilog subset): only Yosys JSON is read.
- Laying out version 6 files that leave out positions: `at` is still
  required in version 6; `layout_circuit` is the building block for it.
- The chunked-grid work from the issue's follow-up comment (#59):
  clustering a flat netlist into blocks by min-cut or connectivity, giving
  each block its own chunk region with bus channels between them,
  promoting clusters to synthetic definitions, and incremental re-layout
  that moves as little as possible. Today the only blocks are the RTL
  module boundaries (each module is laid out on its own as a definition),
  and every run lays out from scratch.
- Wire routing: wires are still drawn straight between nodes; the layout
  only orders nodes to keep crossings low. Long edges get no dummy nodes.
- Multiple clock domains, asynchronous resets, latches and memories (see
  "What is refused").
