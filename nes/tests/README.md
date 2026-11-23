# Suite configs for the native runner

Example configs for `examples/nes_suite.rs` (`docs/suite-runner.md`),
written before the 6502 exists. **Every part path in them is a
placeholder** (`cpu/regs/a`, `cpu/timing/t`, `cpu.sync`, `prg_rom`, ...):
replace them with the real ones once `cpu6502` and `cpu_tb` are built
(`view` the outline to find them). JSON has no comments, so they are
here; the runner ignores keys that start with `_`.

```sh
cargo run --example nes_suite -- nes/workspace nes/tests/harte.example.json --only a9 --limit 100
```

Test ROMs and case files are not committed; they go in `nes/roms/`
(gitignored), which the paths below point into.

## `harte.example.json`: Tom Harte's `nes6502/v1`

- `cases`: the folder of `00.json` ... `ff.json` (one file per opcode).
- `signals`: short names for the bus. `din` is what the CPU reads (the
  testbench RAM's output), `dout` what it writes.
- `registers`: the six registers of the case files, as state in the
  design. A gate-level register chip is fine (`Rig::target` writes its
  flip-flops); state the runner cannot write is refused with the reason
  when the run starts.
- `fixed`: state written at the start of every case. A Harte case starts
  at the first cycle of an instruction (the opcode fetch at `pc`), so the
  sequencer must be at that step: here a placeholder `cpu/timing/t` of 0.
  Add any latch whose stale value would change the first cycle.
- `memory`: one 64 KiB RAM in the testbench; every address of a case is a
  word of it.
- `bus`: the address, the data read and written, and `rw` (1 on a read,
  as the 6502's R/W). Every cycle of a case is compared.
- `inputs`: reset released, clock enable and RDY high, no interrupts.
- `timing`: one tick per CPU cycle, the bus read before the cycle's tick
  (`sample` 0: a one-tick cycle shows the state it starts from). A design
  with two ticks per cycle (phi1, phi2) sets `ticks_per_cycle` 2 and
  `sample` to the tick after which the bus is valid.

## `nestest.example.json`

- `start`: the 16 KiB PRG ROM of `nestest.nes` (after its 16-byte
  header) into the testbench's ROM, then the automation-mode start state
  ($C000, S = $FD, P = $24).
- `boundary`: `sync`, high in the opcode fetch cycle: a row is recorded
  once per instruction.
- `record`: the columns, named as nestest.log names them (`A:`, `X:`,
  `Y:`, `P:`, `SP:`), so `sp` is the stack register here. The first
  value of a line is the PC (`columns`), the rest of the disassembly is
  skipped. `cycle_column` checks `CYC:` (the log starts at 7).
- A CPU with only the documented opcodes stops matching at the first
  undocumented one (row 5004 of the usual log); cut a copy of the log
  there to test the documented ones alone.

## `klaus.example.json`

- `start`: the assembled `6502_functional_test.bin` (a 64 KiB image) into
  the RAM at 0, the program counter at $0400.
- `success`: the address of the success trap, which depends on the
  assembly options; the listing says it (`success` label). A trap
  anywhere else fails and names its address.

## `blargg.example.json`

- `memory`: 2 KiB of internal RAM mirrored by `mask`, and the 8 KiB of
  PRG RAM at $6000 where the status protocol is.
- `start`: the PRG ROM (32 KiB for a mapper-0 single; a 16 KiB one is
  mirrored by the testbench), then a reset: the CPU starts at the reset
  vector.
- The status is read at each instruction boundary; the text at $6004 is
  in the report.
