# Bold ideas: measurements and feasibility notes

Three performance ideas for the gate-level NES (CPU, PPU and MMC3 built
from gates), measured before anything is built: speculative CPU/PPU
parallelism with rollback (idea 3), skipping idle stretches (idea 4), and
simulating many consoles at once (idea 6). The goal is real time:
5.37 M ticks/s (one tick is one PPU dot; the CPU runs every third tick).

No simulator code changed. The measurements are examples under
`logi-sim/examples/` (`bold_*.rs`); every table below was produced by
one of them, with the command given in each section. Numbers marked
*estimate* are not measured. Where the data says something that makes an idea
look worse, it is said.

## Summary

| idea | what the data says | estimated gain | build cost (agent-days, estimate) | recommendation |
|---|---|---|---|---|
| 3. CPU/PPU partitions with speculation and rollback | The interface is narrow (13 bits CPU to PPU, 9 bits back, plus the mapper) and the CPU-to-PPU events are rare on games (11 to 49 per frame, 92% of ticks have 1000 ticks or more before the next one) but not on raster-effect demos (585 per frame, 46%). The PPU side is about 2/3 of the time, so two threads cap at 1.5 to 1.75x before sync and rollback costs. | 1.3 to 1.45x on games like the two quiet ones, none (<= 1.0x) on RasterDemo | 15 to 25 | **No-go as a speed project now**; go for the cheap experiment named in 1.7 first. The partition and the interface contract are the useful part, and idea 4's replay needs them. |
| 4. Skipping idle stretches | The CPU is in an exactly repeating loop for 84% (BladeBuster), 79% (snow) and 17% (RasterDemo) of ticks; replaying it is sound (0 wrong predictions in 92 M ticks). But the CPU side is only about a quarter of the executed work. Counter-only ticks do exist in the PPU side but only in vblank (8% of ticks) unless the screen is blank. | 17 to 21% of executed ops on idle-heavy ROMs, 4% on RasterDemo; fast-forwarding PPU counters: <= 4% | replay 8 to 12 (6 if partitions exist), counter jump 5 | **No-go standalone.** Go for the replay as a layer on the partitions of idea 3 if those get built; the counter jump overlaps state-keyed specialization and is not worth building. |
| 6. Many consoles at once | Independent scalar `Sim`s already give 10 M ticks/s aggregate on 4 cores (1.9 real-time consoles). Guard divergence across 64 lanes is small (1.07x). Instruction counts of a batched layout are 0.15 to 0.34 of scalar (estimate); memory reads dominate the bit-sliced cost. | 2 to 3x aggregate (word-SIMD or bit-sliced, estimate, wide error bar) | 3 for the harness and footprint fixes, 20 to 30 for a batched backend | **Go on the cheap half** (harness, shared ROMs, drop the screen when unobserved): it enables the data gathering of 3.6. **Hold the batched backend** until a 2 to 3 day microbenchmark confirms the gain. |

Reading guide: sections 1 to 3 are the three ideas; each has *measurements*
(with the command), *soundness*, *gain and cost*, *risks* and a
*recommendation*. Section 4 collects what to measure next.

## 0. Setup

* Machine: 4 cores (the throughput numbers move by about 5% between runs).
  The whole console runs at 4.8 to 5.9 M ticks/s in these runs (`Sim::run`,
  JIT, default compile options; the figure quoted for the NES demo
  elsewhere is 3.3 to 4.1 M, so treat absolute numbers as this machine's
  and read the ratios).
* ROM workspaces (the committed `nes/workspace` has the cartridges empty;
  load a ROM with `python3 nes/gen/cartridge.py insert <workspace> <rom>`):
  **BladeBuster** (MMC3 game; with no input it sits in what looks like a
  title or attract screen, the CPU waiting for NMI 84% of the time; a second
  run presses Start, A and Right from frame 100, frames 270 to 400 measured,
  and shows the same pattern; whether that is gameplay is not known), **RasterDemo**
  (NROM, a demo with a raster effect: many `$2005`/`$2006` writes and
  `$2002` polling every scanline), **snow** (NROM, a CPU idle loop and a
  few register writes per frame). A game with real gameplay, or one
  using the MMC3 IRQ, is not among them (the MMC3 IRQ never fired in the
  sampled frames), and three ROMs are a small sample.
* The examples take any workspace folder: `<workspace>` below is a folder
  with a `manifest.json`. Build once with
  `cargo build -p logi-sim --profile profiling --features jit --examples`.
* Names. The examples find signals by hierarchical net name
  (`cpu6502/addr`, `ppu_ref/ppu_regs/mask`, `ppu_ref/cs`, `oam_dma_ref/own`,
  `cart_mmc3/irq_n`); `bold_names <workspace> <substring>` lists them with
  their slots. The compile keeps every authored net observable in the
  tracing tools (`Options::debug`), and uses default options where the
  executed-op counts matter.

| tool | idea | what it prints |
|---|---|---|
| `bold_trace` | 3, 4 | CPU-PPU bus events per frame and gaps; repeating instruction laps; per-tick state changes |
| `bold_split` | 3, 4 | signals crossing between sides; state size; executed ops per side and per sub-chip; throughput with a side removed; snapshot cost |
| `bold_replay` | 4 | soundness and coverage test of the idle-loop replay rule |
| `bold_many` | 6 | op census and per-layout cost estimate; footprint; guard divergence over lanes; aggregate throughput |
| `bold_names` | all | named nets with slots |

## 1. Idea 3: CPU and PPU on separate threads, speculating, with rollback

### 1.1 How the circuit splits

```sh
cargo run -p logi-sim --profile profiling --features jit --example bold_split -- <workspace>
```

The root of the NES workspace is flat: chips (`cpu6502`, `oam_dma_ref`,
`joypad`, `ppu_ref`, `ppu_timing`, `nes_palette`, `cart_*`), RAMs, the screen,
and stateless glue (bus multiplexers, decoders, splits). Assigning each
stateful chip to a side and treating glue as transparent gives the
interface. *Distinct destination bits* counts each receiving pin once (the
CPU and the DMA unit share the buses through multiplexers, which makes the
raw pin-pair count larger).

| from -> to | signals (receiving pins) | bits |
|---|---|---:|
| CPU -> PPU | `ppu_ref.cs` 1, `ra` 3, `rw` 1, `cpu_wd` 8 | 13 |
| PPU -> CPU | `cpu_rd` 8 (to `cpu6502.din`, `oam_dma_ref.din` and the bus latch), `nmi_n` 1 | 9 (25 as pins) |
| CPU -> mapper | `a` 16, `d` 8, `rw` 1, `ce` 1 | 26 |
| mapper -> CPU | `dout` 8, `irq_n` 1 | 9 (25 as pins) |
| PPU -> mapper | `pa` 14, `pwd` 8, `pwe` 1 | 23 |
| mapper -> PPU | `pd` 8, `ciram_a10` 1 | 9 |

The CPU and PPU sides are the same on all three workspaces (the mapper
differs: `cart_mmc3` has 16 state words, `cart_nrom` none). The 22 bits
between CPU and PPU are the narrow interface the idea needs. The mapper
touches both sides: its bank registers are written by the CPU and read by
the PPU, its IRQ counter is clocked by the PPU address bus and signals the
CPU. A clean cut puts a *replica* of the register file on the PPU side,
fed by the CPU's write stream (a write-only interface can be replicated
instead of shared) and leaves the IRQ as a PPU-to-CPU event.

All crossings are **combinational within a tick**: the CPU's registered
address and data go through the decoders to `cs`/`ra` and the PPU's
register logic in the same tick. The read path is a round trip: `cpu_rd`
is a function of the PPU's registers *and* of `ra`, which the CPU
supplies in that tick. A PPU running ahead cannot know `ra`, so it logs
the whole readable view (status, OAM data, data buffer, bus latch: about
32 bits, only when it changes) and the CPU side selects.

State per side (`Sim` keeps one `u64` per memory word):

| side | state words (bits) | writable memories (words) | raw snapshot, `u64` words |
|---|---:|---|---:|
| CPU (`cpu6502`, DMA, pads, bus latch, divider, RAM) | 41 (163) | RAM 2048 | 16.7 KB |
| PPU (without the screen) | 50 (253) | VRAM 2048, OAM 64, palette 32+512+8 | 21 KB |
| PPU screen | | 65536 (output only) | not copied |
| mapper (MMC3) | 16 (96) | PRG-RAM 8192, `chr` `Ram2` 262144 | 2.2 MB |

A raw copy of the PPU side without the screen is 50 words plus 2664 memory
words: **21 KiB, 0.3 us** when cache-hot (a loop of `Sim::read` and
`copy_from_slice`; expect 1 to 2 us cold, *estimate*). `Sim::export_state`
keys by `NodePath` in a `HashMap` and takes 177 to 247 us on the whole
machine, too slow to use. A real implementation needs a slot-range and
memory-subset snapshot in `Sim` (the compiler can number partitions'
state slots contiguously with `Options::slot_order`). The screen need not be
snapshotted: replaying a frame rewrites the same addresses. The cartridge's
`chr` memory is large and its write port is wired to the PPU
(`cwe`): it needs an undo log (address and old value per write) or a proof
that the cartridge has CHR ROM.

### 1.2 Where the time goes

Executed operations per tick (the guarded fused bodies on sampled settled
states, the way the JIT runs them; 180 000 ticks in bursts over about 36
frames at tick 9 M):

| ROM | CPU | PPU | mapper | glue and unnamed | total |
|---|---:|---:|---:|---:|---:|
| BladeBuster | 209 (24%) | 441 (50%) | 112 (13%) | 127 (14%) | 889 |
| RasterDemo | 159 (22%) | 440 (62%) | 14 (2%) | 99 (14%) | 712 |
| snow | 156 (22%) | 440 (62%) | 14 (2%) | 99 (14%) | 708 |

The PPU's executed ops are the same in vblank (440) as on visible lines
(441): nothing in the PPU is skipped when it has nothing to do. Inside the
PPU: sprites 38%, background 16%, loopy 14%, render control 12%, registers
7%, OAM 6%, mux 4%, palette 3%. The sub-chip crossings are small (4 to 72
bits out per chip; the sprites chip takes 16 bits from the registers and 32
from OAM and gives 6 to the mux, 7 to OAM and 1 to the registers), so a PPU
split exists but its signals interact every tick, with feedback through the
status flags.

Throughput with one side's chips deleted (ns per tick, JIT; the remaining
side sees zero inputs, so it idles: the CPU polls, the PPU does not
render, which is why this is a rough guide next to the op counts):

| ROM | whole | CPU + mapper | PPU + mapper | ceiling, two threads, no overhead |
|---|---:|---:|---:|---:|
| BladeBuster | 201 | 50 | 128 | 1.57x |
| RasterDemo | 186 | 33 | 106 | 1.75x |
| snow | 170 | 37 | 113 | 1.50x |

So the **PPU thread bounds the pair at about 7.8 to 9.4 M ticks/s**, 1.45 to
1.75 times real time and 1.5 to 1.75 times today's speed. Real time (5.37)
needs 1.1 to 1.3x over the 4.1 to 4.8 M we see, so partitioning would be
enough on this machine only if sync and rollback cost less than about a
third of the ceiling. A 3-way split with the sprites chip on its own thread
would raise the ceiling to about 2.4x (*estimate*, from the executed
shares) but adds a per-tick interface with feedback.

### 1.3 How often the sides interact

```sh
cargo run -p logi-sim --profile profiling --features jit --example bold_trace -- \
    <workspace> --skip-frames 30 --frames 300 [--press p1_start:100:3,...]
```

The tool ticks the machine (`Sim::tick`) and records, at the PPU's
register port and the CPU's bus, one event per access. *Class A* is what
the CPU side sends or changes on the PPU side: register writes, the `$4014`
DMA start, mapper writes, and the reads of `$2002` and `$2007`, which
clear the vblank flag and the write toggle or advance the address. *Class
B* is what comes back: register reads, NMI, IRQ. An OAM DMA makes 256
class-A writes to `$2004` 6 ticks apart for 1536 ticks, during which the
CPU is stalled; it counts as one event.

| ROM | A events/frame (median gap, ticks) | A events in vblank / visible | share of ticks with next A event >= 1000 / >= 10 000 ticks away | B events/frame |
|---|---:|---|---:|---:|
| BladeBuster, title, 300 frames | 49 (24) | 38 / 9 | 92% / 79% | 2.0 |
| BladeBuster, after Start and A, 130 frames | 37 (21) | 33 / 2 | 92% / 79% | 2.0 |
| snow | 11 (18) | 11 / 0 | 97% / 87% | 1.0 (NMI only) |
| RasterDemo | 585 (21) | 48 / 537 | 46% / 12% | 93 (92 reads of `$2002`) |

The first two columns are bursts: BladeBuster's NMI handler writes
`$2005`, `$2000`, `$2001`, DMA, and mapper registers within a few hundred
ticks of vblank, then nothing for a frame. RasterDemo writes `$2005` 325
times and `$2006` 119 times a frame, in the visible lines (a raster
effect), and polls `$2002` 92 times (sprite-0 or status waits, 27 ticks
apart). NMI is one event per frame on all four; the MMC3 IRQ did not fire.
A12 rises 1928 times a frame (BladeBuster), 11 276 (RasterDemo): the MMC3
counter is clocked inside the PPU side's interface, so the IRQ time is a
PPU-side fact that the CPU side only learns from a log.

**Which side runs ahead.** The PPU-to-CPU direction carries only `$2002`
status (vblank flag, sprite hit), `$2004`/`$2007` data, NMI and IRQ:
1 to 2 events a frame on the games, one every 27 ticks on RasterDemo's
polling. The CPU-to-PPU direction has the bursts above. So:

* *CPU ahead of the PPU, PPU lags and consumes a log* needs no rollback for
  writes, but the CPU must wait for the PPU at every read, NMI and IRQ, and
  cannot even run through a tick without knowing whether an NMI arrives. A
  conservative bound exists (the NMI time is a function of the PPU's own
  timing counters and `ctrl` bit 7) but it needs the PPU to run first.
  It is lockstep with extra steps on RasterDemo.
* *PPU ahead of the CPU, speculating that nothing arrives* serves reads
  from a log of the PPU's readable view, needs no wait, and pays only on
  class-A events: roll back to a snapshot at or before the event, apply the
  write, replay. This is the direction the numbers favour on games.

### 1.4 Cost of rolling back

Upper bound of the discarded PPU work with a lead of H ticks (each class-A
event discards at most `min(H, gap since the previous event)` ticks), as a
share of all ticks:

| lead H | BladeBuster title | BladeBuster, in game | snow | RasterDemo |
|---:|---:|---:|---:|---:|
| 300 | 4% | 3% | 1% | 48% |
| 1000 | 8% | 7% | 2% | 54% |
| 3000 | 13% | 13% | 5% | 65% |
| 10 000 | 21% | 20% | 13% | 88% |

(The bound is generous on bursts: a burst of 20 writes in 400 ticks costs
at most 400 ticks once if the replay restarts from one snapshot.) Add the
snapshot itself: one per rollback point, 21 KB (about 1 to 2 us), against
about 130 ns per tick of PPU work, so a snapshot every 1000 ticks is
about 1% overhead; the replay from the snapshot to the event adds on
average half the interval.

Speedup estimate (*estimate*, from the measured ns per tick of the PPU side
and the waste at H = 1000, minus an assumed 5 ns per tick of
synchronization): BladeBuster 201 / (128 x 1.08 + 5) = **1.4x**; snow
170 / (113 x 1.02 + 5) = **1.4x**; RasterDemo 186 / (106 x 1.54 + 5) =
**1.1x**, and its bursts of 21-tick gaps would have to run in lockstep, so
call it 1.0x or less. Today's ~4.1 M ticks/s becomes ~5.7 M on the games, which
clears 5.37 M with little margin, and a faster PPU (from other work,
such as the specialization of `docs/adaptive-spec.md`) shrinks the
benefit: the pair is bounded by the larger side.

### 1.5 Soundness

The result must equal the monolithic run in every state element and memory
at every tick the observer can see (the existing `nes_bench --compare`
differential check applies as is).

1. *Determinism.* A tick is commit then settle, a pure function of state,
   inputs and `tick mod L`. With cross signals cut, side X's state at tick
   t is a function of its state at t-1 and the cross values at t-1 and t.
2. *Speculation predicate.* The PPU assumes the **quiet interface value**:
   `cs` = 0 and no mapper write. This is sound only if, with `cs` = 0, the
   PPU's next state does not depend on `ra`, `rw`, `cpu_wd` (and the CPU
   address bus through the mapper). This is a *checkable contract*: constant-
   propagate `cs` = 0 through the PPU partition's program and verify that no
   state element's next-value cone reaches the other three inputs. It is
   **not verified yet** (the PPU has an internal bus latch; if it latches
   `cpu_wd` every tick the contract fails and the latch's inputs become
   class-A events). It is the first thing to check.
3. *Rollback.* On a class-A event at tick t the PPU restores the newest
   snapshot taken at or before t and replays with the real cross values.
   Its log entries for ticks after t are discarded. In the PPU-ahead model
   the CPU thread is the one producing the event, so it is exactly at tick
   t and has consumed log entries only for ticks up to t, which depend only
   on PPU state up to t and are valid. This is why the direction matters: a
   CPU that ran ahead of the PPU would have consumed values that a
   rollback invalidates and would need rollback itself.
4. *Reads with side effects* (`$2002`, `$2007`) are class-A events; their
   data comes from the log, their effect on the PPU is applied by replay.
5. *Mapper replica*: the PPU side's copy of the bank registers is written by
   replaying the CPU's write stream, so it is exact; the IRQ is a PPU-to-CPU
   event delivered from the log at its tick.

### 1.6 Generic version

Nothing above is NES-specific except the names. For any design:

1. **Cut.** Group stateful instances (the hierarchy gives candidates),
   treat stateless glue as transparent, and count the *distinct receiving
   bits* between groups (`bold_split` does it for the root level and one
   level down). Prefer cuts where bits crossing is under about 5% of the
   state bits and where the groups have comparable executed-op shares (the
   speedup bound is 1/max share).
2. **Direction and rate.** Trace the cross nets in a few representative
   runs (`bold_trace` does it for this design): which carry events
   (change rarely) and which carry data every tick.
3. **Contract.** Pick for each rarely-changing cross input its quiet value,
   and prove by constant propagation that the receiving partition is
   independent of the other inputs when it holds.
4. **Run.** Receiver ahead (on the side with the bigger cost), sender's
   events as rollback points, snapshots of the receiver's state words and
   writable memories, undo logs for big memories.

### 1.7 Risks and recommendation

* The PPU side is 2/3 of the work, so two threads cap at about 1.5 to
  1.75x, and the speculation fails on programs with raster effects. Sync
  cost per interaction (cache-line transfers, 100 to 300 ns each,
  *estimate*) is small against 21-tick gaps only if the burst is run by
  one thread.
* The contract in 1.5.2 may fail and need netlist changes.
* Mapper split and a log of the PPU's readable view are new circuit-level
  work; threads add determinism hazards (a run must not depend on timing).
* The interactive `Engine` pacing and probes assume one `Sim`.

**Recommendation: no-go as a speed project today; do the 2 to 3 day
experiment first.** The experiment: (a) verify the contract of 1.5.2 with
the constant-propagation check on the compiled PPU partition; (b) run the
PPU alone from a CPU-side log recorded by `bold_trace` (the ticks, `cs`,
`ra`, `rw`, `cpu_wd`, the mapper writes) and require equal state at the end
(exactness of the partition and the log, without threads); (c) measure real
cross-thread handoff cost for 1000-tick batches. If (a) holds and (c) is
under 10 ns/tick, the 1.4x on games is worth 15 to 25 agent-days;
otherwise stop. Partitioning is also what makes idea 4's replay cheap, so
the answer depends on idea 4 too.

## 2. Idea 4: skipping idle stretches

### 2.1 (a) Busy-wait loops

```sh
cargo run -p logi-sim --profile profiling --features jit --example bold_trace -- <workspace> ...
```

Instructions are found by `cpu6502/sync` (the opcode fetch) and the PC on
the address bus. An instruction whose PC equals the one a lap earlier,
for a whole lap of at most 16 instructions, is "in a repeating lap";
the lap is classified by its data accesses (operand fetches excluded). The
first lap of every loop is not counted.

| ROM | register loop | poll `$2002` | poll joypad | poll RAM flag (wait for NMI) | ROM table | loop with writes | strict idle total |
|---|---:|---:|---:|---:|---:|---:|---:|
| BladeBuster (title) | 0 | 0 | 0 | 84.3% | 0.1% | 2.2% | **84.3%** |
| BladeBuster (in game) | 0 | 0 | 0 | 89.1% | 0.2% | 1.5% | **89.1%** |
| snow | 79.0% | 0 | 0.3% | 0 | 0 | 0.1% | **79.4%** |
| RasterDemo | 7.9% | 2.4% | 0 | 28.6% | 0 | 5.3% | **38.9%** |

"Strict idle" is the first four classes: no writes and no ROM data reads.
Episodes are long: BladeBuster's wait-for-NMI loops have a median of
70 920 ticks (80% of a frame; 544 episodes in 300 frames), snow's register loop has a p90 episode of 71 685
ticks (one long episode per frame). RasterDemo's idle loops are short (a
`$2002` poll has a median of 12 ticks; its delay loops 96 ticks, 21 900 of
them). The CPU spends 9.2 ticks per instruction on average (3 CPU cycles of
3 ticks).

Waiting for NMI is the dominant idle pattern and it ends at a time the
PPU decides (the start of vblank); the poll of `$2002` ends when the flag
flips.

### 2.2 (b) What changes per tick

The same tool, in windows of 3 frames at 3 places, compares all state
words (107 on BladeBuster, 91 on the NROM workspaces) tick by tick.

* **No tick has zero changes.** Changed words per tick (BladeBuster): 1:
  3.4%, 2: 15.7%, 3 to 4: 28.3%, 5 to 8: 30.2%, 9 to 16: 22.3%.
* The PPU's dot and line counters, and the screen's scan address, are kept
  by the compiler as 1-bit words (a binary counter shows as words toggling
  every 1, 2, 4, ... ticks, about 2 words per tick), so a width test finds
  no counters; the tool counts those two groups plus any 4-bit-or-wider
  word that moves by +1 as "counter-like".
* PPU side only counter-like words changed in **96 to 97% of vblank ticks**
  on all three ROMs, but only in 67% (BladeBuster), 24%
  (RasterDemo) and 54% (snow) of the visible-line ticks. Vblank is 7.6% of
  ticks (lines 241 to 260), plus 0.8% for lines 240 and 261. On a static
  background (BladeBuster's title, snow) the rendering logic changes
  nothing on most ticks, but the straight-line code still runs it.
* Ticks where *only* counter-like words changed anywhere in the machine:
  45% (BladeBuster), 38% (snow), 20% (RasterDemo). This counts ticks after
  the fact; it is the room for event-driven execution, not a rule that can
  be used without running the tick.
* The CPU side changes state on at most one tick in three (the clock
  enable: in the two of every three ticks without it, 66.7%, the CPU side
  shows no change).

### 2.3 The replay rule

Skipping *the whole machine* never applies (the PPU's counters always
move). Skipping *the CPU side* does: the CPU side is a deterministic
machine with a small interface, so when its state, outputs and inputs have
repeated exactly over one lap, it will repeat again for as long as its
inputs do.

**Rule.** Let the CPU side be the CPU chip, the DMA unit, the pad logic and
the bus latch (the data-bus multiplexer and decoders stay live). Record per
tick a hash of its state words (plus the RAM write count), its inputs
(`cpu6502/din`, `nmi_n`, `irq_n`, `rdy`, `oam_dma_ref/din`) and its
outputs (CPU and DMA addresses, `rw`, data, `sync`, `own`, `rdy`). If the
last P ticks (P a multiple of the clock-phase period, at most 96) equal the
P before, **enter replay**. In replay, at each tick whose inputs equal those
of one lap earlier, state and outputs are those of one lap earlier and
need not be computed; at the first tick whose inputs differ, **wake**: the
CPU side's state is the replayed one and it runs normally from there. The
check per tick is one comparison of a few input words.

**Check.** `bold_replay` runs the machine tick by tick and applies the rule
as a prediction, comparing it with the real values (nothing is skipped).

```sh
cargo run -p logi-sim --profile profiling --features jit --example bold_replay -- <workspace> --frames 300
```

| ROM | ticks tested | ticks predicted | wrong predictions | regimes (woken by input change) | median regime (ticks) |
|---|---:|---:|---:|---:|---:|
| BladeBuster title | 26.8 M | 84.3% | **0** | 300 | 72 800 |
| BladeBuster in game | 11.6 M | 89.1% | **0** | 130 | 79 587 |
| snow | 26.8 M | 79.0% | **0** | 300 | 71 290 |
| RasterDemo | 26.8 M | 17.2% | **0** | 600 | 2 523 |

Two input rules were run, comparing every input every tick (strict) and
only at the ticks where the CPU's clock enable is high (relaxed, the
ticks where the CPU samples its inputs): the same coverage and no wrong
predictions either way. The wake-ups are one per frame (the NMI) and,
on RasterDemo, the `$2002` polls. A lap of 1 to 3 ticks was never found;
the laps are 4 to 12 ticks (snow) and 13 to 48 ticks (BladeBuster).

Why RasterDemo is at 17% while the PC-level count says 39% strict idle:
the difference is loops in which a register counts (`INX`, a countdown) and
whose state is therefore not periodic. That is the same kind of idle as
(b)'s counters, at the CPU level; here the hypothesis is consistent with
the numbers but untested.

### 2.4 Counter fast-forward: when to jump N ticks

Applies to a partition P with counter words C (state words whose
next-value is `q + k` under their enable, found in the program as
`Commit::Copy { state, next }` with `next = AddSum(state, const, 0)`)
and every other state word being unchanged under the current conditions.

**Rule.** The next N ticks are a pure counter advance if (1) the non-counter
state words are all held (each one's hold condition, such as an enable
slot, is false for every counter value in the window), (2) P's inputs are
constant for the window, and (3) no output of P changes other than by the
counters. The first tick where (1) or (3) can fail is the earliest tick
where a *predicate* on a counter flips: `Eq(c, k)`, `ReduceAnd(c) == imm`,
`LtU(c, k)` and the slots that read them (guard conditions, enables).
These have closed forms: `Eq` flips when `c` reaches `k`, `LtU(c, k)` when
`c` reaches `k`; with several counters, take the smallest distance. For the
PPU's timing: the dot counter against 339 and 340 and the line counter
against 240, 241 and 261, so a line jumps from dot 2 to dot 338, 336
ticks in one step, the NMI edge being a predicate on (line, dot, `ctrl`
bit 7).

**Check at run time**, cheap and exact: the state-keyed specialization
machinery already builds a body for a key (a set of slot values) in which
unchanged state folds away; if the specialized body's commits for every
non-counter word are `Copy { state, next: state }` and the counters
advance by +1, a key plus a distance to the next predicate flip gives
the jump. The check that the key still holds is the predicate evaluation
itself.

**Gain.** The PPU side is counter-only in 7.6% of ticks (vblank) plus 0.8%
(lines 240, 261): about 4% of all ticks, and its ~440 executed ops per tick
there would drop to about 20: **at most 4% of the executed work**
(8% x 440 / 889 on BladeBuster). In the visible lines the rendering logic
is not predictably idle (24 to 67% of ticks are counter-only after the
fact). Not worth building on its own; the same gain falls out of
specialization keyed on the line range. The CPU-side counters (`DEX`,
`BNE` delay loops) are 7.9% of RasterDemo's ticks and 0 elsewhere:
inside the idle loops of 2.1, the same rule extended with an affine
word (state advancing by a constant per lap) would cover them; the exit
tick is where the counter reaches the compare value, again a closed-form
predicate.

### 2.5 Estimated gain per ROM

The saving of replay is the CPU side's executed ops in replayed ticks
(a replayed tick still evaluates the glue and the PPU):

| ROM | replayed ticks | CPU side's share of executed ops | saving |
|---|---:|---:|---:|
| BladeBuster, title | 84% | 24% | **20%** of executed ops (speedup <= 1.25x) |
| BladeBuster, in game | 89% | 24% | 21% |
| snow | 79% | 22% | 17% |
| RasterDemo | 17% | 22% | 4% (9% if the counting loops were covered) |

(The share was measured on the title screen at tick 9 M; the CPU side
executes about as much per tick in vblank as elsewhere. It is a
guard-aware count: the CPU's guarded cones already run only on the
clock-enable ticks, which is why 491 static ops become 209 executed.) Time
is not exactly proportional to ops (the CPU+mapper side alone ran at 33
to 50 ns per tick of the 170 to 201 ns), so the measured version of the
bound is 17 to 25% of the time on the idle-heavy ROMs.

Adding the vblank counter jump: at most 4% more.

### 2.6 Soundness

The replay rule is exact by determinism: if the CPU side is a function
`(state, inputs, tick mod L) -> (next state, outputs)` and the state and the
inputs of a full lap equal those of the lap before, then by induction the
next lap's state and outputs equal them while the inputs do. The
conditions that make the premise hold are checkable at run time from the
state hash and the input words; the experiment above found no
counterexample in 92 M ticks (hash collisions aside: one 64-bit hash per
vector; a production version compares words, 41 + 5 + 9 of them). Writes to
RAM end the lap (the RAM write count is part of the hash, and a write has
its own cs/addr outputs). Inputs that change while the CPU does not
sample them (the relaxed rule) were not observed to matter, but the strict
rule is the sound default.

The replay needs the interface outputs (7 to 9 words) written into the
glue's slots each tick, and the CPU cone skipped: in a monolithic program
that means gating the CPU's guarded regions and overriding the output
slots; in a partitioned runtime it is a cached lap per partition. The first
is JIT and guard work; the second comes with idea 3.

### 2.7 Risks and recommendation

* The gain is bounded by the CPU side's executed share (a quarter) and by the
  idle fraction of the ROM (17 to 89% here): a **17 to 21%** saving on idle-
  heavy ROMs, 4% on a raster demo, and nothing on a busy game (not measured:
  none of the three is busy).
* Complexity: a run-time mode switch with exactness at every wake.
* PC-level idle detection (84% on BladeBuster) is a hint only; the
  state-level rule is the sound one.

**Recommendation: no-go as a standalone project (8 to 12 agent-days for
17 to 21% on some ROMs); build it as a layer on partitions if idea 3
proceeds (6 agent-days then).** The counter jump: no-go (<= 4%, overlaps
specialization). Measure next: the CPU side's share and idle fraction on a
busy ROM (a game with scrolling, sprites and audio), and whether a lap
cache could cover the counting loops (affine laps).

## 3. Idea 6: many consoles at once

### 3.1 Op set and lanes

```sh
cargo run -p logi-sim --profile profiling --features jit --example bold_many -- <workspace> --lanes 64 --throughput
```

The compiled program on BladeBuster is 1396 instructions plus commits per
tick (unguarded, mean over the 3 clock phases; 889 executed with guards),
1802 slots. Result widths: 1 bit 51%, 2 to 8 bits 35%, 9 to 16 bits 11%,
wider 2%. By opcode (static count per tick): `Mux` 299 (mean width 7),
`Shr` 266, `Concat` 148, `And` 130, `Eq` 99, `Or` 82, `Not` 77, commits 66,
`Copy` 48, `MemRead` 39 (mean width 10.6), `AddSum` 36, `ReduceOr` 32,
`LtU` 24; the full census is the tool's first table.

How ops map to batched lanes (cost per instruction for 64 instances in
bit-sliced planes, and in word SIMD; the table behind these estimates is
in the source of `bold_many.rs`):

| op | bit-sliced (64 lanes) | word-SIMD (4 or 8 lanes) |
|---|---|---|
| `And`, `Or`, `Xor`, `Not`, `Copy` | w plane ops: a 1-bit gate is 1 op for 64 lanes | 1 vector op |
| `Mux`, `Switch` | 3w and w (+ the select reduction) | compare plus blend: 2 (AVX2), 1 (AVX-512) |
| `Eq`, `LtU`, `Reduce*` | 2w, 3w, w | 1 to 3 |
| `AddSum` | 5w (ripple) | 1 |
| `Shr`, `Concat`, `Sext`, `Shl` by literal | free (rename planes) | 1 |
| `MemRead`, `RamWrite` | per-lane scalar access plus transposes: about 5 ops per lane, 320 per instruction | gather (4 to 8 lanes) |
| `Clock` | free (the phase is common to all lanes) | free |

Estimated instructions per tick per instance (not time; the table in the
source is the whole model):

| layout | BladeBuster | snow | relative to scalar |
|---|---:|---:|---:|
| scalar words (today, unguarded) | 1396 | 1241 | 1.00 |
| word-SIMD, AVX2 (4 lanes) | 478 | 428 | 0.34 |
| word-SIMD, AVX-512 (8 lanes) | 211 | 192 | 0.15 |
| bit-sliced, 64 lanes | 393 | 377 | 0.28 to 0.30 |

Observations: (1) half the bit-sliced cost is `MemRead`, whose per-lane
random access defeats the planes (instruction fetch and the PPU's pattern
fetches use different addresses in every instance); (2) `Mux` is the next
largest, because the compiled program is full of 8-bit register
enables; (3) to my knowledge (not checked) the Cranelift in use (0.138)
vectorizes only to 128 bits, so a word-SIMD backend through it would be 2
lanes unless it generates machine code another way; bit-sliced code needs only 64-bit
integer ops, which the current JIT emits, so it is the lower-risk
backend; (4) code size grows: the bit-sliced tick is about 25 000 plane ops
(about 18 times today's instruction count; at about 5 bytes each, ~125 KB
of code per tick, *estimate*), which
exceeds the instruction cache: the real gain is lower than the table, my
guess 1.5 to 3x per core instead of 3.5x (*estimate*).

### 3.2 Divergence

64 instances run in lock step with different start times (1.2 M plus
53 001 ticks per lane) and different button schedules; per tick, a
guarded region runs when its condition holds in *any* lane of the batch.

| batch | BladeBuster, ops per tick vs scalar's 889 | RasterDemo vs 713 | snow vs 708 |
|---:|---:|---:|---:|
| 4 lanes | 1.03x | 1.03x | 1.02x |
| 16 | 1.06x | 1.05x | 1.04x |
| 64 | 1.07x | 1.06x | 1.07x |

The guards (register-enable cones) are phase-locked to the clock, so the
lanes agree almost always: batching loses 3 to 7% of the guard savings on
this sample. A population with different ROMs and gameplay would diverge
more, and it is lock-step so every lane is at the same tick residue. This
test does not exercise that.

Specialization (`docs/adaptive-spec.md`) keys bodies on state words; with
batching each lane has its own key. The CPU decode word `cw`, the CPU
phase's key, has a mean of **8.5 to 14.8 distinct values among 64 lanes
per tick**, so a batched run would need all those bodies at once or
fall back to the general body; the adaptive bodies cannot be used in a
batched layout without sorting lanes by key. Treat specialization and
batching as exclusive for now.

### 3.3 Footprint

Per instance (BladeBuster default compile; RasterDemo and snow are
about 1.2 MB with ROMs copied, 0.7 MB shared):

| part | as `Sim` keeps it | packed to the word width |
|---|---:|---:|
| slots (1802) | 14 KB | 14 KB |
| writable memories (RAM, VRAM, OAM, palette, screen 65536 words, PRG-RAM 8192, `chr` 262144) | 2.7 MB | 0.34 MB |
| ROMs (PRG 524 288 words, ...) | 4.5 MB | 0.68 MB |
| total, ROMs copied | 7.2 MB | 1.0 MB |
| total, ROMs shared | 2.7 MB | 0.36 MB |

`Sim` keeps a `u64` per memory word, and each `Sim` copies its ROMs; the
resident growth measured per `Sim` is 4.3 MB (BladeBuster, untouched zero
pages do not count) and 0.9 MB (NROM games). Two cheap fixes: share ROMs
between instances (they are `MemDesc::init` already and never written),
and drop the screen memory (512 KB of `u64`s, the largest writable
memory of the NROM games) when nobody observes it. An NROM instance (slots,
RAMs, OAM, palette, cartridge RAM) is then about 170 KB as `u64` words, or
about 35 KB with memory words packed to their width: 64 instances are
11 MB or 2.2 MB. BladeBuster's 2 MB `chr` memory dominates its instance
and needs packing (256 KB) or sharing if the cartridge's CHR is a ROM.

### 3.4 Aggregate throughput today

`Sim`s of the same program sharing one JIT code, each copied from one
warmed-up state, round-robin in 20 000-tick slices:

| instances | threads | aggregate M ticks/s | real-time consoles (5.37 M each) |
|---:|---:|---:|---:|
| 1 | 1 | 5.73 | 1.07 |
| 4 | 1 | 4.82 | 0.90 |
| 16 | 1 | 4.79 | 0.89 |
| 64 | 1 | 4.20 | 0.78 |
| 4 | 4 | 8.80 | 1.64 |
| 16 | 4 | 9.78 | 1.82 |
| 64 | 4 | 10.04 | 1.87 |

Four threads give 1.8x of one core (the machine has 4 cores, not
necessarily 4 independent ones). So the status quo already runs 64
consoles at 0.16 M ticks/s each, a 300-frame run (26.8 M ticks) in 170 s
for a whole batch of 64.

**Estimated aggregate with a batched backend** (*estimate*): at 0.28x
instructions and the cache and gather penalties above, 1.5 to 3x per
core: 15 to 30 M ticks/s on this machine, i.e. 3 to 6 real-time
consoles, a 64-console x 300-frame run in 60 to 110 s. This is the number
to check with a microbenchmark before building anything.

### 3.5 Build cost

| step | agent-days (estimate) |
|---|---:|
| harness: N `Sim`s on T threads, per-lane inputs and ROMs, result collection | 2 to 3 |
| share ROMs between `Sim`s, optional screen | 1 |
| batched backend, bit-sliced: new layout in the compiler (planes, transposes at the ports and memories), JIT emission, interpreter reference, differential tests against scalar | 20 to 30 |
| batched backend, word-SIMD: would need a Cranelift version with wide vectors or hand-emitted code | more; not recommended |

### 3.6 What the data would be good for

With N instances running different ROMs, input scripts or fault sets:

* **Specialization keys for real**: a distribution of the keys of
  `docs/adaptive-spec.md` (decode word, PPU mask, render control) over many
  ROMs and play styles, instead of the three ROMs measured today. It tells
  which bodies to precompile and how much a plan transfers between
  games.
* **Idle and interaction profiles per ROM** (this note's `bold_trace`
  tables over a corpus): the idle fraction, class-A event rates, raster-effect
  detection; the numbers that decide ideas 3 and 4.
* **Coverage**: which ops, guards and 1-bit nets ever toggle in a corpus
  (toggle coverage), which guards are never taken, dead logic in the
  gate-level CPU and PPU.
* **Differential testing** against the reference oracle
  (`logi_sim::reference`) and the RTL model (`nes/model`), and screen-hash
  comparison with a software NES emulator over test ROMs
  (`nes/tests/*`) and random controller input.
* **Fault grading and mutation testing**: 64 lanes = 64 injected stuck-at
  faults or netlist mutations per run, to find which faults the stored
  tests (the Harte suite, blargg's tests) do not catch. Bit-slicing
  was invented for this and the harness needs only per-lane net forcing.
* **Fuzzing the emulator itself**: random PRG programs for the 6502 against
  `nes/model` for instruction-level agreement at scale.

### 3.7 Risks and recommendation

* The batched gain is an estimate from instruction counts; gather cost,
  transposes at the ports, code size and the loss of JIT specialization are
  the unknowns, and each can halve it.
* Lanes of different ROMs need per-lane memories (a base per lane and a
  gather), which the model charges at 5 ops per lane access.
* A bit-sliced backend is a second backend to keep exact with the
  interpreter (differential tests are available).

**Recommendation: go on the cheap half now (harness, shared ROMs, optional
screen: 3 agent-days) since 10 M ticks/s aggregate on 4 cores already makes
data gathering useful; hold the batched backend until a 2 to 3 day
microbenchmark** (generate the bit-sliced tick of the NES program as
straight-line code with 39 per-lane gathers and time it against the same
number of scalar ticks) **shows more than 2x per core.**

## 4. What to measure next

1. **Contract check for idea 3** (1.5.2): constant-propagate `cs` = 0 on
   the PPU partition and report which state elements still depend on
   `ra`, `rw`, `cpu_wd`. One day. Decides whether speculation is exact.
2. **Log-driven PPU alone** (1.7): record the CPU-to-PPU stream, run the
   PPU partition by itself, require equal state at the end of 300 frames.
   Two days. Gives exactness of the log format and the true wasted work.
3. **Cross-thread handoff** with 1000-tick batches on this machine. One
   day.
4. **A busy ROM**: idle fraction, CPU share, event rate on a game with
   scrolling and music; the three ROMs here are idle-heavy or raster-heavy.
   One day, once a ROM is available.
5. **Batched microbenchmark** (3.7). Two to three days.
6. **An MMC3 IRQ ROM**: the IRQ path was never exercised (zero events); it
   is the third PPU-to-CPU channel and a likely source of rollbacks.

## Appendix: how the examples measure

* `bold_trace`: `Sim::tick` per tick with `Options::debug` so nets are
  readable; PPU register accesses from `ppu_ref/cs`, `ra`, `rw`; CPU
  cycles when `cpu6502/ce` is high and `oam_dma_ref/own` low; NMI and IRQ
  as falling edges of `ppu_ref/nmi_n` and `cart_mmc3/irq_n`. Frames
  are counted by the PPU line counter wrapping. Laps are found by a run
  counter per lag. State changes read every state key per tick
  (`Sim::state`) and memory write counts (`Sim::mem_version`).
* `bold_split`: the root netlist (`Design`) for crossings and sides; the
  default compile's clock-phase variants, guarded and pruned as the
  runtime does, evaluated on `Sim` states for executed ops; removal of a
  side's chips with `Design::remove_node` for the throughput rows.
* `bold_replay`: hashes of the CPU side's state words, input pins and
  output pins per tick, a 256-entry ring, lags that are multiples of the
  clock-phase period.
* `bold_many`: op census from `possible_bits` widths and the opcode of each
  block; footprint from `Program::mems`; divergence from guard conditions
  evaluated on 64 `Sim`s in lock step; throughput from `Sim`s sharing one
  `JitProgram` (`compile_jit`, `install_jit`).
* Limits: three ROMs, one machine; `bold_split`'s removal rows are a rough
  guide; the cost tables of `bold_many` are estimates.
