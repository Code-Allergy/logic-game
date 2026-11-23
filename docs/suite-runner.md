# The native suite runner

Big CPU test suites run against a design outside the MCP server: Tom
Harte's single-step tests (10,000 cases per opcode, about 2.5 million in
all), nestest, Klaus Dormann's functional tests and blargg's test ROMs.
This is item 0.5 of `docs/nes-plan.md`. MCP calls are for building and
debugging; this is for bulk.

The logic is the library module `gate_quest::mcp::suite`
(`src/mcp/suite/`); `examples/nes_suite.rs` is its command line. Nothing in
it knows a particular CPU: a suite config says how the suite's registers,
memory, bus and timing map onto the design.

## Running it

```sh
cargo run --example nes_suite -- <workspace> <suite config> [options]
```

`<workspace>` is a workspace folder (holding `manifest.json`, version 5 or
6) or its manifest file. `<suite config>` is a JSON file (below).

| Option | Meaning |
|---|---|
| `--jobs N`, `-j N` | Simulations run side by side, one per thread (default: every core). Harte suites only; a program suite is one simulation. |
| `--only P[,P..]` | Only the case files whose name or stem matches one of the patterns (`*` any run, `?` one character, case-insensitive): `--only a9`, `--only "6?,7?"`. |
| `--limit N` | At most the first N cases of each file. |
| `--stop-after N` | Stop once N cases have failed. |
| `--out DIR` | Where the report goes (default: `<config stem>.report/` beside the config). |
| `--unoptimized` | Compile with `func_match`, `lift` and `words` off. A case that fails with the default options and passes with this is a compiler bug, not a design bug. The report says which was used. |

The terminal gets a few lines:

```
harte PASSED: 16000 of 16000 cases passed in 16 file(s)
compiled with the default options: 335 instructions (335 with the default options, 12294 without func_match, lift and words); 10 of 36 chips matched
0.68 s on 4 job(s): 23.5 kcases/s, 158.9 kticks/s (built with debug assertions)
report: cpu16.harte.report/report.md
```

The exit code is 0 when every case ran and passed (or the program
passed), 1 when anything failed or `--stop-after` cut the run, 2 when the
suite could not run (a bad config, a path the design does not have, a
case file that does not parse).

**Speed.** Every worker compiles the circuit once, resolves its registers,
memories and bus signals once, and runs whole units of cases. Work is cut
into units of `chunk` cases of one file; workers take units in order from
one queue. A case file is read when its first unit is taken and dropped
when its last unit is done, so memory stays small however many files
there are. Results are put back in unit order, so the report is the same
for any number of jobs, `--stop-after` included (the cut is at the N-th
failure in file and case order). Compiling is not counted in the
throughput.

## The suite config

A JSON object. Keys starting with `_` or `//` are comments and ignored;
any other unknown key is an error. Relative paths are relative to the
config file's folder. Numbers may be JSON numbers or text: `"0x6000"`,
`"$6000"`, `"0b101"`.

### Common fields

| Field | Meaning |
|---|---|
| `kind` | `"harte"`, `"nestest"`, `"klaus"` or `"blargg"`. Required. |
| `circuit` | The circuit to simulate, as a root (`cpu_tb`). Default: the workspace's root. |
| `signals` | `{name: signal path}`: short names for signals, as the `run` tool's `signals`. Names are letters, digits and `_`. Every field below that takes a signal takes a name from here, a register name, or a path. |
| `registers` | `{name: state path}`: each register of the suite's vocabulary and the state it is in the design, resolved with `Rig::target` (a `Register`, a chip whose output is wired to state such as a gate-level register, an array of flip-flops, a memory word `stack@0x3`, `:lo..hi` bits of any of these). Any names: `pc`, `s`, `a`, `x`, `y`, `p` for a 6502. |
| `fixed` | `{state path or register: value}`: state written at the start of every Harte case (the sequencer at its first step, a halt latch cleared). |
| `ignore` | Keys of a case's `initial`/`final` that are not registers and are not checked. |
| `memory` | Where the suite's addresses go. `"ram"` (one memory part), or `{"part": "ram", "mask": "0x07FF", "regions": [{"from": "0x6000", "to": "0x7FFF", "part": "prg_ram", "at": 0}]}`: an address inside a region is word `address - from + at` of that region's memory; any other address, masked by `mask`, is a word of `part`. The memories must be RAMs (state), not ROMs. |
| `bus` | `{"addr": s, "data": s}` or `{"addr": s, "read_data": s, "write_data": s}`, with `"rw": s` and `"read_value": 1` (the value of `rw` on a read cycle; default 1, the 6502's). Separate read and write data need `rw`. Without `rw` the read/write of a cycle is not compared. Without `bus` no cycle is compared, only the count of cycles run. |
| `inputs` | `{root input: value}`: held from the start (reset released, clock enable high). |
| `timing` | `{"ticks_per_cycle": 1, "sample": 1, "before": 0, "after": 0}`. A cycle is `ticks_per_cycle` ticks. The bus and the boundary condition are read after tick `sample` of each cycle (0: before its first tick, which is when a one-tick-per-cycle design shows the state the cycle starts from). `before` ticks run after a case's pokes before its first cycle; `after` ticks after its last cycle before the final state is read. Defaults: 1 tick per cycle, read after the last tick of the cycle, nothing before or after. |
| `start` | A list of `run` steps (`set`, `poke`, `ticks`, `load`, `until`, ...) run once on each fresh simulation, before any case or program cycle. `load` files are found from the config's folder. This is where a ROM image is loaded (`{"load": {"part": "prg_rom", "file": "nestest.nes", "skip": 16}}`), a program counter set, a reset sequence run. Registers and `signals` are names in these steps. |

### `harte`

| Field | Meaning |
|---|---|
| `cases` | A case file, or a folder whose `*.json` files are taken in name order. Required. |
| `chunk` | Cases per unit of work (default 250). |

A case file is an array of cases:

```json
{"name": "a9 2b 3c",
 "initial": {"pc": 1234, "s": 253, "a": 0, "x": 1, "y": 2, "p": 36,
             "ram": [[1234, 169], [1235, 43]]},
 "final":   {"pc": 1236, "s": 253, "a": 43, "x": 1, "y": 2, "p": 36,
             "ram": [[1234, 169], [1235, 43]]},
 "cycles":  [[1234, 169, "read"], [1235, 43, "read"]]}
```

Every register of the config must be in `initial` and `final`, and every
other key there must be a register, `ram` or in `ignore`.

Per case: the words the previous case wrote (its `initial` and `final`
addresses and every address its bus showed) are set back to 0; `fixed`
and the registers are written, the `ram` words written; one settle; then
`before` ticks, one cycle per entry of `cycles` (reading the bus at
`sample` when `bus` is mapped), `after` ticks; then the registers and the
words at the `final` addresses are read and compared. Memory is never
cleared whole. A design that writes where its bus does not show, with no
bus mapped, can leave a word for the next case.

### `nestest`

| Field | Meaning |
|---|---|
| `log` | The expected log. Required. |
| `record` | The names (registers or signals) recorded at each instruction boundary; a log value is matched to a column by `NAME:value` tokens (`A:1F`), `columns`, or a first line of names, as the `run` tool's `compare` reads a file (the same reader). Required. |
| `compare` | `{"columns": ["pc"], "radix": 16, "skip_lines": 0, "context": 5}`, as `compare` of `run`. |
| `cycle_column` | `{"name": "cyc", "start": 7}`: one more column, the cycle the instruction starts in (`start` plus the cycles since the start steps). With `radix` 16 a log's decimal `CYC:` is matched as decimal. |
| `boundary` | When an instruction starts: an expression over registers and signals (`sync`, `fetch == 1`), read once per cycle at `sample`. Default: every cycle. |
| `max_cycles` | The most cycles the program runs (default 10,000,000). |

It passes when every row of the log matched. The first differing row is
reported with the expected values, the values seen, and the `context`
rows before it.

### `klaus`

| Field | Meaning |
|---|---|
| `pc` | The register or signal that is the program counter (default `pc`). |
| `success` | The address of the success trap. Required. |
| `boundary`, `max_cycles` | As for nestest. |

A trap is the program counter the same at two boundaries in a row (an
instruction that jumps or branches to itself). It passes when the trap is
at `success`; otherwise the report gives the trap address and the
program counter at the last 16 boundaries. Load the binary and set the
program counter with `start` steps.

### `blargg`

| Field | Meaning |
|---|---|
| `memory` | Must map `status_at` and the bytes after it. Required. |
| `status_at` | The status byte (default `0x6000`). |
| `boundary`, `max_cycles` | As for nestest; the status is read at each boundary. |

The test runs while the status is `$80` and `DE B0 61` is at
`status_at + 1`; any other status after that ends it: 0 passes, anything
else fails, and `$81` (a request to press reset, which the runner does not
do) fails with that said. The zero-terminated text at `status_at + 4` is
reported.

## The report

In the `--out` folder:

- `report.md`: the verdict; the compiler summary (instructions with the
  default options and with the word passes off, depth, and for every chip
  what the function matcher made of it, as `view compile` says); the
  options the run compiled with; throughput (cases/s, ticks/s, jobs).
  For Harte suites a scoreboard, one row per case file (passed, total,
  the first failure in one line), then for the first failing case of each
  file the full story: its differences, a table of registers (initial,
  expected, got), a table of the memory words (initial, expected, got),
  and the cycles around the first differing one (expected and got). For
  program suites the verdict and its details (the differing log row with
  the rows before it, the trap, the text the test printed).
- `report.json`: the same, for programs to read: `ok`, `compile`,
  `cases_per_second`, `ticks_per_second`, and per file `cases`,
  `run`, `passed`, `failed`, the first ten failures (`index`, `name`,
  `diffs`), and `first_failure` in full (`initial`, `expected`, `actual`,
  `first_differing_cycle`).
- `repro.json` (Harte, when a case failed): a `run` script that replays
  the first failing case on a fresh simulation: the `inputs` as `set`, the
  `start` steps, one `poke` of `fixed`, the registers and the memory
  words, a `record` of the bus signals, per cycle `ticks` and an `expect`
  of the bus, and an `expect` of the final registers and memory words.
  Give it to the MCP server's `run` tool as its `file` argument (it names
  its `circuit`). Run on a fresh simulation, state the suite's earlier
  cases left behind (latches no register names) is not there, which can
  itself tell a missing `fixed` entry.

## What is tested, on what

`src/mcp/tests/suite.rs`:

- Harte, on a one-instruction-per-tick accumulator machine built in the
  test: a passing suite with the same report on 1 and 4 jobs, and with
  `--unoptimized`; a register, a memory word and a bus cycle that differ,
  each reported exactly, in the scoreboard and the story; the repro
  script failing the same way through the `run` engine and passing once
  fixed; memory a case wrote cleared before the next; `--limit`, `--only`
  and `--stop-after` (the same cut on 1 and 4 jobs); configs that do not
  fit refused with the reason.
- Harte on the gallery's 16-bit CPUs: a suite generated from a software
  model of the instruction set (one instruction from a random register,
  flag, stack and RAM state, per opcode), run on `cpu16` (gates, 3 or 4
  clock periods of 2 ticks per instruction, registers inside gate-level
  register chips, the sequencer's state written by `fixed`) and on
  `cpu16_rtl` (words, one tick per instruction), and with the bus of the
  word CPU's loads and stores compared. A wrong mapping, a wrong timing
  and a missing `fixed` entry fail.
- nestest: the gallery CPU's own program (the primes) against a log
  written by the model, with registers as `NAME:value` tokens and a
  decimal `CYC:` column, on both CPUs (the gate CPU's boundary is its
  `Fetch` signal); a log with one wrong register is reported at that row.
- klaus and blargg on purpose-built circuits: a program counter that
  follows a table loaded from a binary (a trap at the success address, at
  another address, and no trap); a writer that plays a script of memory
  writes into an 8 KiB RAM mapped at `$6000` (pass with its text, a
  failing status with its text, no signature).

What only a 6502 can prove: the real case files (`nes6502/v1`), the real
nestest.log, Klaus's and blargg's binaries, and the timing and `fixed`
entries a real microarchitecture needs.
