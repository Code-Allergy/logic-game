# A gate-level NES, starting with the 6502

Status: plan, not started. Phase 0 (simulator and MCP prerequisites) comes
first; the CPU is phases 1 to 5; the rest of the NES is phases 6 to 9.

## Goals

1. A **cycle-accurate NES CPU (the 2A03's 6502 core)** built in GateQuest
   that passes the CPU test ROMs we pick (list under "Test suites") at
   100%.
2. Built **at gate level, as a deep hierarchy of small, logically named
   chips** (gate → cell → block → unit → CPU → console). The design is
   also a test of the compiler: flattening (`elaborate`), function
   matching (`func/`) and word lifting (`lift.rs`) must turn it back into
   a compact word-level program, and the result must behave exactly as the
   unoptimised circuit.
3. Then the rest of the console (PPU, APU, controllers, mappers), on the
   same rules.

RTL may be used as a starting point and as the reference every gate-level
chip is checked against, as long as the shipped design is gates.

What "gate level" means here: the 1-bit `Not`/`And`/`Or`/`Xor`/`Nand`/
`Nor`/`Xnor`/`Mux` primitives, `Const`, wiring (`Split`, `Merge`), and
`Delay` as the only storage element of logic (the "gates plus one delay"
rule of `docs/sim-v2-design.md` section 2). Memory arrays stay `Ram`/
`Ram2`/`Rom` primitives (2 KiB of work RAM as flip-flops would be 16 384
chips of nothing new), as `cpu16` does. Word primitives (`Add`, wide
`Mux`, `Register`, `Eq` …) appear only in the `*_ref` reference chips.

## What I found

- **The simulator** is synchronous: one global tick commits state, then
  settles every gate once, in rank order. Two-state, one driver per net,
  no tri-state; a combinational loop not through state is a compile error.
  `Ram`/`Rom` reads are combinational within the tick, writes land at
  commit. Memories go up to 2^20 words, values up to 64 bits.
- **Matching and lifting work at CPU scale already.** `cpu_bench` (today,
  this machine): the 14 667-gate `cpu16` compiles to **672** instructions
  and runs 2.34 M ticks/s (344 k instructions/s); its word twin
  `cpu16_rtl` is 137 instructions at 7.6 M ticks/s. So a gate-level 6502
  of a few thousand gates should run CPU test ROMs at roughly 1 M cycles/s.
- **The MCP server** covers the whole loop: build chips in one `edit`
  (or from a generated JSON `file`), store `formula`/`sequence`/
  `equivalent` tests with each chip, `run` experiment scripts with memory
  loading, `record … when`, `compare` against a log, snapshots and
  sessions, `import_rtl` through Yosys 0.71 (on `PATH`), and `expand`
  word parts into gate chips. I checked each on a scratch workspace: an
  8-bit ADC/SBC unit with 6502 C and V passed 2 002 formula cases, its
  `expand`ed gate copy passed an `equivalent` test against it, and a 64 KiB
  RAM image loaded from a file was compared row by row against a log.
- **`compare` already reads nestest.log as is.** With `columns: ["pc"]`
  the leading address is the PC, `A:`/`X:`/`Y:`/`P:`/`SP:`/`CYC:` tokens
  fill the recorded signals of those names (case-insensitively), and the
  opcode bytes, disassembly and `PPU:` field are ignored.

### Gaps that matter for this project

| Gap | Why it matters | Phase 0 item |
|---|---|---|
| No view of what the compiler did to a design: which chips matched as what (or why not), instruction count, lifted banks. `view stats` stops at "flattened: N parts". | Testing matching and lifting is a stated goal; today it is only visible from Rust (`cpu_bench`). | 0.1 |
| No test that the optimised program behaves as the unoptimised one on *our* design. | The differential sweep covers random designs, not a CPU. | 0.2 |
| `expand` is flat and one level: `Add_8_gates` is one circuit of gates; chips inside are not expanded. | Imported RTL must become nested gate chips (adders of full adders, registers of flip-flops). | 0.3 |
| No way to set CPU registers or scattered RAM bytes before a run. | Tom Harte's single-step tests start every case from a given CPU state. | 0.4 |
| Big suites (≈2.5 M single-step cases) cannot go through one MCP call each. | Needs a native runner. | 0.5 |
| Loop errors name simulator node ids, not part paths. | At several thousand gates an unnamed loop is unfindable. | 0.6 |
| `record`/`until` see only current values. | Trap detection ("PC did not change") and edge triggers. | 0.7 |
| No NES palette for `Screen`. | PPU output (phase 7 only). | 0.8 |
| `.gatequest/` is ignored by git. | The design must be committed somewhere. | "Where the design lives" |

## Design rules

These hold for every chip, CPU and console alike.

1. **One tick is one cycle of the fastest clock; slower parts use clock
   enables.** CPU-only test benches tie the CPU's `ce` to 1 (one tick per
   CPU cycle). In the console, one tick is one PPU dot and a divide-by-3
   counter gives the CPU its `ce` (2A03 : PPU = 1 : 3, NTSC).
2. **Every flip-flop is the `dff` chip: a `Delay` plus gate logic.**
   `dffe` adds an enable (a `mux2` chip before the `Delay`), `dffr` a
   synchronous reset. This is what Yosys's `$dff`/`$dffe` import to, so a
   gate chip and its imported `_ref` have the **same tick timing** and
   `equivalent` tests compare them tick for tick. It is also the form the
   matcher recognises as `Register` (`next = en ? d : state`).
   The curriculum master-slave flip-flop (two D latches, two ticks per
   clock) would double every tick count and break tick-for-tick
   equivalence; it is kept as a later stress variant (phase 9), not the
   default.
3. **CPU outputs are Moore outputs.** `addr`, `dout`, `rw`, `sync` depend
   only on state, never combinationally on `din`, `irq`, `nmi` or `rdy`.
   Memory and device reads are combinational within the tick, so this is
   what keeps CPU → address decode → device → `din` → CPU free of loops.
   Every side effect of an access (a write; a read of `$2002`, `$2007`,
   `$4015`) is qualified with `ce`, so it happens once per CPU cycle.
4. **Small chips, named by function, tested alone.** A chip stays well
   under the matcher's limits (4 096 flattened steps; 20 input-plus-state
   bits for an exhaustive proof, more only for bit-sliced or adder-chain
   proofs) and under the 150-part layout advisory. Each chip carries:
   a functional test (`table`/`formula`/`sequence`), an `equivalent` test
   against its `_ref` where one exists, and a compile assertion (0.1):
   what it should match as, and an instruction budget.
5. **Generated, not hand-typed.** Regular structure (bit slices, the
   decode PLA, register banks) is produced by scripts that write `edit`
   request files, so a design change is a script change and a re-run.

## Where the design lives

- `nes/` at the repository root (committed):
  - `nes/gen/`: Python generators. Each writes `edit` request JSON for a
    group of chips, and `make.py` applies them in order into a fresh
    workspace through the MCP server (or by `edit {file}` calls).
  - `nes/model/`: the cycle-level reference model and the opcode table
    (phase 2), plus the Verilog it generates.
  - `nes/workspace/`: the saved version 6 workspace, committed after each
    milestone so a fresh checkout opens the design without regenerating.
  - `nes/tests/`: test scripts (`run` JSON), suite configs, expected logs
    we produce ourselves, and `SCOREBOARD.md` (per suite, per opcode).
- **Test ROMs are not committed** (licences vary). They go in
  `nes/roms/` (gitignored); the runner reads them from there.

## Architecture of the CPU

The 6502's real internals (two-phase clock, precharged buses, the
130-line decode PLA feeding random logic) do not map onto a two-state,
single-driver, synchronous simulator, so we do not copy its transistor
structure. We build an original microarchitecture with the **same
cycle-by-cycle bus behaviour**: the same address, data and read/write on
every cycle, including dummy reads, double writes of read-modify-write
instructions, page-crossing penalties, branch timing and interrupt
polling and hijacking. That bus trace is exactly what Tom Harte's tests
and blargg's timing and dummy-access tests check.

```
cpu6502                         ports: in din[8] ce rdy irq_n nmi_n res_n
│                                      out addr[16] dout[8] rw sync
├── regs        A, X, Y, S          4 × reg8 (8 × dffe)
├── status      P: N V – B D I Z C  per-flag set/clear logic, push/pull format
├── pc_unit     PCL, PCH            reg8 × 2, inc16 (inc8 × 2, half-adder chains), next-PC mux
├── latches     IR, DL, ADL, ADH, BA   operand and address temporaries
├── alu         adder8 (cla4 × 2 or full_adder × 8), logic8, shift8,
│               flags (N, Z, C, V), [bcd_adjust, behind the D strap]
├── agu         address mux: PC, {ADH,ADL}, {01,S}, {00,ZP}, vectors;
│               index add (base + X/Y) with page-cross carry
├── decode      opcode → instruction class lines (PLA: and-plane chips
│               over IR bits, or-plane chips), × T-state → control word
├── timing      T-state sequencer, sync, rdy stall, skip/extra cycles
├── interrupt   NMI edge detector, IRQ level, reset, pending/hijack logic,
│               BRK/IRQ/NMI/RESET vector select, B flag on push
└── bus         rw, data-out latch, din latch
```

Below that, the shared **cell library**: `half_adder`, `full_adder`,
`cla4`, `mux2`/`mux4`/`mux8` (1 bit and 8 bit), `dff`, `dffe`, `dffr`,
`reg8`, `inc8`, `adder8`, `eq8`, `zero8`, `decoder2`/`3`/`4`, `and_plane`
and `or_plane` row chips.

**Decode is a PLA of gates, generated from a table.** One table (phase 2)
says, for each opcode and each cycle: address source, read or write, what
is latched where, the ALU operation, flags written, and the next T-state
(or end). A script turns it into minimised sum-of-products (Yosys/ABC
two-level synthesis of the table, or espresso) and emits and-plane and
or-plane chips. The same table generates the Verilog reference and the
Python model, so all three agree by construction and the tests decide
whether the table is right.

**Decimal mode.** The 2A03 has the D flag but no BCD arithmetic. The ALU
gets a strap input `bcd_en`: 0 in the 2A03, 1 for an NMOS 6502 build that
can run Klaus Dormann's full functional test and Bruce Clark's decimal
test with decimal on. Optional (decision 3).

**Undocumented opcodes** follow from the same table: the stable ones (LAX,
SAX, DCP, ISC, SLO, RLA, SRE, RRA, ANC, ALR, ARR, SBX, the NOPs, USBC)
fall out of the PLA naturally; the unstable ones (XAA/ANE, LXA, SHA/AHX,
SHX, SHY, TAS) use the constants and page-cross behaviour the chosen test
suites expect; the 12 JAM opcodes halt the bus as the tests specify.

## Phases

Each phase ends in a commit and a scoreboard update.

### Phase 0: prerequisites in the simulator and the MCP server

Rust work in this repository, each with unit tests, before any CPU work.

- **0.1 Compiler observability.** `view {what: "compile"}`: per
  definition, matched as what (and the proof) or why not; flattened part
  count; compiled instructions, packed lanes, slots, depth; state banks
  lifted. A new stored test kind `compiled`, e.g.
  `{"compiled": {"matches": "Add w=8", "max_instructions": 3}}`, so a
  regression in matching or lifting fails `test` like a wrong output. Also
  on the native runner's report.
- **0.2 Optimised = unoptimised, on our design.** A stored test option and
  a `run` option to compile with matching and lifting off
  (`Options::debug()`-style), and an `equivalent`-style test that runs one
  chip compiled both ways (and, for small chips, through `Reference`)
  side by side on random inputs, comparing every output, and every
  observable internal net, every tick. This is the direct test of
  flattening and lifting on real hardware.
- **0.3 Hierarchical expansion.** `expand` with `"deep": true` expands
  chips inside chips; and the gate chips it makes are built from library
  cells instead of flat gates (`Add_8_gates` = 8 × `full_adder`, or
  2 × `cla4`; `Register_8_gates` = 8 × `dffe`; wide `Mux` = bit-slice
  `mux2` chips; `Eq` = xnor slices and an and-tree chip). The library can
  be the workspace's own cell chips by name, so the importer reuses ours.
  This makes "import RTL, then expand" produce the nested gate design the
  goals ask for.
- **0.4 State and memory pokes.** `Sim` gains cheap single-element writes
  (a state element by key; one memory word) next to `export_state`/
  `import_state`. `run` gains `{"poke": {"cpu/regs/a": 0x12}}`, where a
  chip made of `dff` bits takes a word across its bit array (`ff[0..7]`),
  and `load` takes `{"pairs": [[addr, value], …]}`.
- **0.5 Native suite runner.** `cargo run --release --example nes_suite --
  <workspace> <suite> <path>`: loads the workspace (`design_of`),
  compiles once, and runs a suite: `harte` (JSON cases: poke state, poke
  RAM, run the listed cycles, compare every cycle's address, data and
  read/write, then registers and RAM), `nestest`, `klaus`, `blargg`
  (the `$6000` status protocol). It writes a JSON and Markdown report per
  opcode and the first failing case in full, and can run cases on every
  core. MCP calls are for building and debugging; this is for bulk.
- **0.6 Loop errors with part paths.** Report a combinational loop as
  label paths (`cpu/agu/addr_mux.sel ← cpu/decode/…`).
- **0.7 Previous values in expressions.** `prev(x)` (value at the previous
  recorded or ticked row) and a `changed` condition for `record`/`until`,
  for trap detection and edge-triggered recording.
- **0.8 (for phase 7) NES palette.** A `Screen` palette of the 64 NES
  colours (6 bits, emphasis later), in `src/sim/screen.rs`.
- **0.9 Benchmarks.** The 6502 (and later the console) joins `perf/` as a
  case, so compile time, instruction count after lifting and ticks/s are
  under the regression gate.

### Phase 1: the cell library

Build and test every library cell (design rule 4). Each must match as the
expected word function where one exists (`full_adder` → `Add w=1`,
`adder8` → `Add w=8` by the chained-adder proof, `mux2` → `Mux`,
`dffe` → `Register`, `reg8` → `Register w=8`; where the catalogue has no
entry, such as `zero8`, the assertion records that and the budget still
applies), with
an instruction budget. Exit: all cell tests pass, including the 0.2
optimised-vs-unoptimised tests.

### Phase 2: the opcode table and a reference model (outside the simulator)

- Write the cycle table for all 256 opcodes from public documentation of
  6502 bus behaviour (the cycle-by-cycle descriptions in "6502.txt"/
  "64doc", and the Harte cases themselves as the arbiter).
- Generate from it a **Python cycle-level model** and validate it against
  the Harte `nes6502` suite and nestest. This is minutes per iteration
  instead of compiles, and it settles the table before any gates exist.
- Generate **Verilog reference RTL**, one module per block of the
  architecture above (so the import gives one `_ref` chip per block, with
  the same ports as the gate chip), and the decode PLA's terms.

Alternative start: an existing open-source 6502 Verilog core, if its
licence allows. Most are cycle-count accurate but not bus accurate (they
skip or move dummy reads), so they would fail the Harte cycle checks; it
is worth a one-day spike as a second reference, not as the base.

Exit: the Python model passes 100% of the chosen Harte suite and nestest.

### Phase 3: the reference CPU in the simulator

- `import_rtl` the Verilog: `cpu6502_ref` and its block chips, word level.
- Build `cpu_tb`, the CPU-only test bench: 64 KiB `Ram`, a cycle counter,
  a trap detector (PC unchanged across two syncs), and Klaus Dormann's
  interrupt feedback register driving `irq_n`/`nmi_n`.
- Run nestest and the Harte suite on `cpu6502_ref` through 0.5.

Exit: `cpu6502_ref` passes what the Python model passes. This proves the
import and the test harness, and gives every gate chip its reference.

### Phase 4: the gate-level CPU, bottom up

Build each block from library cells, with an `equivalent` test against its
`_ref` block, a 0.2 test and a compile assertion, in this order:

1. `alu` (adder, logic, shifter, flags, optional BCD)
2. `regs`, `status`
3. `pc_unit`, `latches`, `agu`
4. `decode` (generated PLA) and `timing`
5. `interrupt`, `bus`
6. `cpu6502`: an `equivalent` test against `cpu6502_ref` with random
   `din`, `irq_n`, `nmi_n`, `rdy` and `res_n` for many thousands of
   cycles; a 0.2 test of the whole CPU.

Two routes to each block, both allowed: build it by hand from cells, or
import its `_ref` and `expand` it deeply (0.3) and then reorganise. The
hand route gives the "logically organised" hierarchy; the expansion route
is a quick first version and a check of `expand`. We also keep one
**Yosys-synthesised gate netlist** of the whole CPU (`synth` without
`-flatten`, ABC to AND/OR/XOR/MUX, which the importer reads) as a second
gate-level design: unstructured logic, a harder test for lifting than our
tidy bit slices.

Exit: every block equivalent to its reference; the compiled gate CPU
within a set factor of the reference's instruction count (target: 3×).

### Phase 5: the CPU test campaign

On `cpu6502` (gates) in `cpu_tb`, through the native runner:

| Suite | Checks | Needs |
|---|---|---|
| Klaus Dormann `6502_functional_test` | every documented instruction and flag; success is a trap at a known PC | assembled with decimal tests off (or on, with the BCD strap) |
| nestest (automation mode at `$C000`) | registers and cycle count before every instruction, documented then undocumented | `compare` with nestest.log, `record … when sync` |
| Tom Harte SingleStepTests `nes6502` | 10 000 cases per opcode, every bus cycle | 0.4, 0.5 |
| Klaus Dormann interrupt test | IRQ/NMI/BRK and their priorities | the feedback register in `cpu_tb` |
| Bruce Clark decimal test (optional) | ADC/SBC in decimal mode, NMOS flags | BCD strap on |

Exit: 100% on the agreed list, the scoreboard committed, and both the
reference and the gate CPU green in CI (a short subset there; the full
Harte run as an ignored test).

### Phase 6: the NES around the CPU

- `rp2a03`: the CPU, the `ce` divider, OAM DMA (`$4014`, the 513/514-cycle
  halt and its alignment), APU and controller register stubs.
- `nes`: the memory map (2 KiB RAM mirrored to `$1FFF`, PPU registers
  mirrored to `$3FFF`, `$4000`–`$401F`, cartridge), an **open-bus latch**
  (unmapped reads return the last bus value; blargg's `cpu_exec_space`
  and others check it), reset button.
- A cartridge interface chip (CPU side, PPU side, IRQ, mirroring) and
  mappers: NROM (0) first, then MMC1 (1), the mapper many blargg ROMs
  use; others as the ROMs we pick need (check each iNES header).
- blargg's CPU ROMs that report through `$6000` (status at `$6000`, the
  `DE B0 61` signature, text from `$6004`): `instr_test-v5`, `instr_misc`,
  `cpu_dummy_reads` and `cpu_dummy_writes` (need PPU register side
  effects), `cpu_exec_space`, `cpu_reset`, `instr_timing` and
  `cpu_interrupts_v2` (need the APU frame counter, length counter and DMC,
  phase 8). Older ROMs that only print to the screen are read from the
  nametable RAM with `read`, before any rendering exists.

### Phase 7: PPU

Timing (341 × 262 dots, the odd-frame skip), registers and the internal
`v`/`t`/`x`/`w` address logic, the background fetch and shift registers,
sprite evaluation into secondary OAM, the sprite pipeline, priority,
palette RAM, NMI. Output to a `Screen` in `Scan` mode (256 × 240) with the
NES palette (0.8). Tests: blargg's `ppu_vbl_nmi`, `sprite_hit`,
`sprite_overflow`, `ppu_read_buffer`, `oam_read`; frame comparisons
(`store` the screen memory, compare with frames we capture from a trusted
emulator for the same ROMs).

### Phase 8: APU and input

Frame counter and its IRQ, length counters, the DMC and its DMA cycle
stealing (it changes CPU timing), channels as logic producing sample
values (audio output in the app is a separate question), controller
shift registers at `$4016`/`$4017` driven by root inputs.

### Phase 9: speed and stress variants

Measure the console at the PPU tick: a frame is 89 342 ticks, so real time
is 5.37 M ticks/s. Expect well below that on one thread; use partitioned
threads (`SimOptions::threads`) and profile what lifting misses. Build the
master-slave flip-flop variant of the CPU (swap the `dff` definition and
derive a two-phase clock) as a stress test of latch matching.

## Test suites: what I need from you

ROMs go in `nes/roms/` (gitignored), keeping the paths of the
`christopherpow/nes-test-roms` repository. Mappers below are from each
file's iNES header. Every blargg "single" is NROM (mapper 0); the
combined ROMs (`all_instrs.nes`, `official_only.nes`, `cpu_test5`) are
256K MMC1, so we use the singles.

**Tier 1, the CPU alone (phases 2 to 5).** These need only the CPU, RAM,
the PRG ROM and 8 KiB of RAM at `$6000`, where blargg's tests write their
status. If a test waits for vblank at start-up, it also needs a stub that
sets bit 7 of `$2002`.

| Path | Mapper | Checks |
|---|---|---|
| `other/nestest.nes`, `.log`, `.txt` | 0 (16K) | every opcode, documented and not, logged per instruction |
| `instr_test-v5/rom_singles/01-basics` … `16-special` | 0 | every instruction's result and flags, undocumented ones included |

Not in that repository, but CPU-only and the most thorough:

- Tom Harte `SingleStepTests/65x02`, `nes6502/v1/*.json`: every bus
  cycle of 10 000 cases per opcode.
- Klaus Dormann `6502_65C02_functional_tests`: `6502_functional_test`
  and `6502_interrupt_test` (assembled for our options: decimal on or
  off, load address).

**Tier 2, the console around the CPU (phases 6 and 8).**

| Path | Mapper | Needs |
|---|---|---|
| `instr_misc/rom_singles/01`–`04` | 0 | dummy reads; 04 the APU registers |
| `instr_timing/rom_singles/1-instr_timing`, `2-branch_timing` | 0 | the APU as a timer |
| `cpu_interrupts_v2/rom_singles/1`–`5` | 0 | APU frame IRQ, NMI, DMA |
| `cpu_reset/registers`, `ram_after_reset` | 0 | the reset button |
| `cpu_exec_space/test_cpu_exec_space_ppuio`, `_apu` | 0 | open bus, PPU and APU registers |
| `cpu_dummy_writes/cpu_dummy_writes_ppumem`, `_oam` | 0 | PPU register side effects |
| `cpu_dummy_reads/cpu_dummy_reads` | **3 (CNROM)** | PPU register side effects |
| `cpu_timing_test6/cpu_timing_test`, `branch_timing_tests/1`–`3` | 0 | older tests that report on screen only: read the nametable |

Skipped: `instr_test-v3`, `nes_instr_test` and `blargg_nes_cpu_test5`,
which are older versions of `instr_test-v5`.

## Decisions for you

1. **Scope of "100%" for the CPU milestone.** Recommended: Klaus
   functional, nestest (all of it, undocumented included), Harte
   `nes6502` including the unstable opcodes as that suite defines them,
   Klaus interrupt, and blargg's `instr_test-v5` singles (tier 1 under
   "Test suites"). The other blargg ROMs count at phase 6/8, since they
   need the rest of the console.
2. **Flip-flop style.** Recommended: `Delay`-based `dff` (rule 2), one
   tick per cycle, master-slave later as a variant.
3. **Decimal mode.** Recommended: build the BCD path behind a strap (off
   in the 2A03) so the plain-6502 suites can run too; it is one small
   chip with a test of its own. Skip it if you only care about the NES.
4. **Reference.** Recommended: our own table-generated Verilog, with an
   open-source core only as a spike.

## Risks

- **Matching limits.** A chip over 4 096 flattened steps is not analysed,
  and wide carry chains need the chained-adder proof; keep chips small
  (rule 4) and assert matches (0.1) so a miss shows up as a test failure,
  not as a slow simulation.
- **Loop-free bus.** Rule 3 is easy to break by accident (an address that
  depends on `din` in the same cycle). 0.6 makes the error findable.
- **Unstable opcodes** differ between real chips; we match the suite.
- **Console speed** may be far from real time; correctness comes first,
  and phase 9 is where we spend effort on speed.
- **Save-format round trips** of a large generated workspace: regenerate
  from scripts and compare with the committed workspace in CI, so the two
  never drift.
