# RTL import and auto-layout

Status: implemented for #42 (`src/sim/rtl.rs` with `src/sim/rtl/`,
`src/sim/layout.rs`, `examples/rtl_import.rs`); more cells in deviation 60;
memories on the dual-port RAM in deviation 71.

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

`proc; opt` turns `always` blocks into `$dff`/`$adff`/`$sdff`/`$mux`/`$pmux`
cells. A design with memories also needs `memory -nomap` (or at least
`memory_collect`), which combines each memory's ports into one `$mem_v2`
cell. Running more of `synth` is fine as long as only supported cells
remain (below); add `flatten` to import one flat circuit instead of one
definition per module. `synth -run :fine` keeps word-level cells (including
`$alu`; run `maccmap -unmap` if it made `$macc`); a full `synth` or `abc`
gives the gate-level `$_AND_` and `$_DFF_*` families.

The checked-in examples were written by hand in this format; none was
produced by Yosys. Each `.json` has the Verilog it stands for next to it:

| File | What |
|---|---|
| `top.json` | a 4-bit ripple adder of full-adder instances, a 4-bit ALU and a counter |
| `arith.json` | `$mul` (unsigned, signed, 32-bit, constant), `$div`/`$mod` (unsigned, signed, 24-bit), `$neg`, `$logic_and`/`$logic_or` |
| `shift.json` | `$shl`, `$shr`, `$sshl`, `$sshr`, `$shiftx` (part-selects, signed index), `$shift`, constant shifts |
| `compare.json` | `$lt`/`$le`/`$gt`/`$ge`/`$eqx`/`$nex`, unsigned, signed, mixed and 40-bit |
| `select.json` | `$pmux` (a case statement, one-hot selects), `$bmux`, `$demux`, `$bwmux`, `$bweqx`, `$lut` |
| `flops.json` | `$adff`, `$adffe`, `$sdff`, `$sdffe`, `$sdffce`, `$dffsr`, `$dffsre`, `$aldff`, `$ff`, ten gate-level flip-flops, latches |
| `memory.json` | a two-port ROM, single-port RAMs (read first, write first, byte enables) and a register file |
| `dpram.json` | a 1024 x 16 dual-port RAM with initial contents, a memory with nine read ports and byte enables, and one with two write ports |
| `mac.json` | a pipelined multiply-accumulate unit with a coefficient memory |

The files with several modules are imported one module at a time with
`--top`.

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

**Cells become primitives**, keeping word widths (1 to 64 bits). Cells
with no primitive of their own are built from several (no primitive was
added to the simulator for them); constant inputs fold away, so for
example a shift by a constant is only wiring and a multiplication by a
constant has stages only for its 1 bits.

| Yosys cell | Primitives |
|---|---|
| `$not`, `$_NOT_` | `Not` |
| `$and`, `$or`, `$xor`, `$xnor` | `And`, `Or`, `Xor`, `Xnor` of width `Y_WIDTH` |
| `$_AND_`, `$_OR_`, `$_XOR_`, `$_NAND_`, `$_NOR_`, `$_XNOR_` | the 1-bit gate |
| `$_ANDNOT_`, `$_ORNOT_` | `Not` on B, then `And`/`Or` |
| `$_AOI3_`, `$_OAI3_`, `$_AOI4_`, `$_OAI4_` | `And`/`Or`, then `Nor`/`Nand` |
| `$mux`, `$_MUX_` | `Mux` (`S ? B : A`) |
| `$_NMUX_` | `Mux`, then `Not` |
| `$pmux` | a chain of `Mux`, one per select bit (several set: the lowest wins) |
| `$bmux` | a tree of `Mux`, one level per select bit (each level one mux over all pairs, in 64-bit pieces) |
| `$demux` | per output word, `Eq` of the select with its index and a `Switch` |
| `$bwmux` | `Not`, `And`, `And`, `Or` |
| `$add` | `Add` of width `Y_WIDTH` (carry in unconnected, carry out unused) |
| `$sub` | `Not` on B, `Add` with carry in 1 |
| `$neg` | `Not`, `Add` with carry in 1 |
| `$alu` | `Xor` of B with BI, `Add` with carry in CI; X is `Xor` of A and B, CO the carries (`Xor` of Y and X, shifted, and the carry out); X and CO logic only if used |
| `$mul` | shift and add: per bit of B (the operand with more constant bits) that is not constant 0, a `Switch` of A and an `Add` into the upper bits of the sum |
| `$div`, `$mod` | restoring division, per quotient bit an `Add` (subtract), an `Or` and a `Mux`; signed operands through conditional negation (`Xor`, `Add`), the quotient taking the sign of A ^ B and the remainder that of A (truncating, as Verilog) |
| `$shl`, `$sshl` | a barrel shifter: per bit of B that is not constant 0, a `Mux` between the word and the word shifted by 2^i; bits of B worth the whole width or more `ReduceOr`-ed into one `Mux` to all 0 |
| `$shr`, `$sshr` | the same to the right, shifting in 0 (`$sshr` of a signed A: its sign bit) |
| `$shift`, `$shiftx` | the same; with a signed B, a right shift by B, a left shift by -B (`Not`, `Add`) and a `Mux` on B's sign |
| `$eq`, `$ne`, `$eqx`, `$nex` | `Eq` (then `Not`); upper result bits are 0 |
| `$bweqx` | `Xnor` |
| `$lt`, `$gt` | `LtU` (operands swapped for `$gt`) |
| `$le`, `$ge` | `LtU`, then `Not` |
| `$reduce_and`, `$reduce_or`, `$reduce_bool`, `$reduce_xor` | `ReduceAnd`, `ReduceOr`, `ReduceOr`, `ReduceXor` |
| `$reduce_xnor`, `$logic_not` | `ReduceXor` / `ReduceOr`, then `Not` |
| `$logic_and`, `$logic_or` | `ReduceOr` of each operand (none for one bit), `And`/`Or` |
| `$lut` | a 1-bit `Rom` holding the `LUT` table (up to 20 inputs) |
| `$pos`, `$_BUF_`, `$slice`, `$concat` | nothing: the output is (part of) the inputs |
| `$dff`, `$_DFF_P_`, `$_DFF_N_`, `$ff`, `$_FF_` | `Delay` |
| `$dffe`, `$_DFFE_PP_` … `$_DFFE_NN_` | `Register` (`Not` on an active-low enable) |
| `$sdff`, `$_SDFF_*_` | `Delay` of `Mux(SRST, D, value)` |
| `$sdffe`, `$_SDFFE_*_` | `Register` with enable `EN \| SRST` and D `Mux(SRST, D, value)` (the reset wins) |
| `$sdffce`, `$_SDFFCE_*_` | `Register` with enable `EN` and D `Mux(SRST, D, value)` (the reset only while enabled) |
| `$adff`, `$adffe`, `$_DFF_*_` with a reset, `$_DFFE_*_` with a reset | `Delay` or `Register` of `Mux(ARST, D, value)` (enable `EN \| ARST`), then `Mux(ARST, state, value)` on the output |
| `$aldff`, `$aldffe`, `$_ALDFF_*_`, `$_ALDFFE_*_` | the same with `AD` for the value |
| `$dffsr`, `$dffsre`, `$_DFFSR_*_`, `$_DFFSRE_*_` | `Delay` of `(D \| SET) & !CLR` (D through a `Mux` on the enable), and `(state \| SET) & !CLR` on the output |
| `$dlatch`, `$_DLATCH_P_`, `$_DLATCH_N_` | `DLatch` |
| `$adlatch`, `$_DLATCH_*_` with a reset | `DLatch` with enable `EN \| ARST` and D `Mux(ARST, D, value)` |
| `$dlatchsr`, `$_DLATCHSR_*_`, `$sr`, `$_SR_*_` | `(Mux(EN, state, D) \| SET) & !CLR` (for `$sr`: `state`) on the output, held in a `Delay` |
| `$mem_v2`, `$mem` without a write port | a `Rom` per read port holding `INIT` |
| `$mem_v2`, `$mem` with one write port | a `Ram2` (dual-port RAM) holding `INIT`, one read port per read port (see below) |
| `$mem_v2`, `$mem` with several write ports, up to 64 words | a `Register` per word, `Eq` and `And` to enable it, a `Mux` tree per read port |
| `$scopeinfo` | ignored |

Active-low controls (`*_POLARITY` 0, or `N` in a gate-level name) get a
`Not`. In the gate-level names, `$_DFF_PN0_` is clock P, reset N
(active low), reset value 0; `$_DFFE_PN0P_` adds an enable;
`$_DFFSR_PNP_` is clock, set, clear; `$_ALDFF_PP_` clock, load. Every
combination of `P`, `N`, `0` and `1` is accepted.

**Memories.** A memory with one write port becomes a `Ram2`, the
simulator's dual-port RAM: one write port and up to eight read ports, each
with its own address, so the read addresses need not be the write address
and the size can be anything up to 2^20 words. The `Ram2` holds `INIT` as
its initial contents (`init`). A memory with more than eight read ports
becomes several `Ram2` copies, written alike, each serving up to eight
ports. The per-bit write enable is one enable when all bits are the same
signal (the usual case); otherwise (byte enables) it is the `Ram2`'s
per-bit enable (`"mask": true`). A read port with `RD_CLK_ENABLE` is
clocked: a `Delay` (or a `Register` when `RD_EN` is not constant 1) after
the read. A clocked port reads the old word (read first), or the word
being written when the port is transparent to the write port
(`RD_TRANSPARENCY_MASK`, or `RD_TRANSPARENT` of `$mem`) and
`RD_COLLISION_X_MASK` does not mark the pair as undefined: then an `Eq`
of the two addresses (none when they are the same bits) selects the word
being written (`(WR_DATA & WR_EN) | (old & !WR_EN)` with byte enables).

A memory with several write ports is built from a `Register` per word,
up to 64 words (a warning says so): each word takes the write ports in
order, so a later port wins at the same address, which is Yosys's
priority (`WR_PRIORITY_MASK` can only give a later port priority over an
earlier one, and where it gives none the result is undefined). Initial
contents are kept by storing `word ^ INIT[addr]` in each register (they
power up at 0), with the constant `Xor`-ed in on the way in and out. A
clocked transparent read port sees every write port it is transparent
to, in the same order.

Addresses are cut to the bits the size needs; writes with a higher
address bit set do nothing.

Operands are extended or cut to the result width as Verilog does: sign
extension when both operands are signed (`A_SIGNED`/`B_SIGNED`), zero
extension otherwise (for shifts, the amount B is always unsigned except
in `$shift`/`$shiftx`).

**Wiring.** Yosys connects pins to lists of bits. When a pin takes exactly
one whole output pin, one wire connects them. Otherwise the importer adds
a `Split` for each slice of an output pin it needs (shared by all its
users), a `Const` for each run of constant bits, an `Extend` for a bit
repeated (a whole pin followed by copies of its top bit is one signed
`Extend`: a sign extension), and a `Merge` that joins the pieces. These
adapters are ordinary nodes, so the layout places them; the version 6 file
has only whole-pin wires. Helper nodes added to build a cell are left out
when nothing reads them (for example the X and CO logic of an `$alu`
whose X and CO are unused).

**Approximations** (reported as warnings, not errors):

- Flip-flops are clocked by the simulator's single global tick: the clock
  pin and its polarity are ignored, so a `$dff` takes `D` once per tick
  (see `docs/sim-v2-design.md` section 3.2). A clock input stays as an
  unused input port. Clocked memory ports and writes are the same.
- Asynchronous resets, sets, clears and loads cannot act between ticks.
  While one is active, the output shows the reset value at once (a `Mux`,
  or `Or`/`And` for set and clear, after the state element) and the state
  element stores it at the next tick (the same logic before it); when it
  is released, the stored value shows. Sampled once per tick this is what
  the real circuit does, except that a reset pulse cannot be shorter than
  a tick and the output depends combinationally on the reset input: a
  design whose reset is derived from its own outputs within a tick (a
  counter that clears itself asynchronously) becomes a combinational loop,
  which the editor reports.
- `x` and `z` bits, and bits nobody drives, read as 0. Where Yosys leaves
  a result undefined, the import picks one: a `$pmux` with several select
  bits set takes the lowest; a division by 0 gives an all-ones quotient
  and the dividend as remainder (unsigned); a shift past the end of
  `$shiftx` reads 0.
- `init` attributes (initial values of registers) are ignored, with a
  warning: state powers up at 0. Memories keep their `INIT` contents
  (above).

## What is refused

The import fails with a message naming the module and cell when:

- a cell type is not in the table above (for example `$print`, `$check`,
  `$fsm`, `$specify2`); the message lists the supported cells. `techmap`
  or `memory_map` can lower many of them first. Some get specific advice:
  `$macc` (run `maccmap -unmap`), `$pow`, `$divfloor`/`$modfloor`
  (floored division is not mapped; `/` and `%` are), `$lcu`/`$sop`/`$fa`
  (run `techmap`), `$tribuf` (no tri-state wires), and `$memrd`/`$memwr`/
  `$meminit` (run `memory_collect` to make a `$mem_v2`);
- a memory has an `OFFSET`, wide ports, read port resets or initial
  values, more than 2^20 words, or several write ports and more than 64
  words;
- a `$bmux` or `$demux` has more than 10 select bits, or a `$lut` more
  than 20 inputs;
- a port or cell pin is wider than 64 bits;
- a port is `inout`: the simulator has no tri-state wires. The message
  says to split the port into an input and an output in the Verilog;
  treating it as an output instead would silently drop whatever drives it
  from outside;
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
- Multiple clock domains: every flip-flop runs on the one tick.
  Asynchronous resets are approximated (above).
- Memories with several write ports and more than 64 words: `Ram2` has one
  write port, so they would need a primitive with several (or banking
  with a live-value table).
- Floored division, `$pow` with a variable exponent, `$macc`, `$fsm` and
  the formal and print cells.
- No new primitive was added for multiplication, division or shifts:
  they are built from `Add`, `Mux` and `Switch` (deviation 60), so a wide
  multiplier or divider is many nodes: a 32-bit multiplier is 63
  primitives (32 `Switch`, 31 `Add`) and about 130 `Split`/`Merge`
  adapters; a 24-bit divider about 400 nodes in all.
