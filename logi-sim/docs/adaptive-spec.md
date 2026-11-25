# Adaptive state-keyed specialization (issue #117)

Status: compile side implemented (`compile/spec.rs`, package P1 below);
P2, P3 and the fixed-plan runtime P4 implemented (`sim/spec.rs`, section
4 "P4 as built"); the pure part of the plan choice (`compile/spec/plan.rs`,
P6, section 3b); P5 to P9 and P6's sampling window designed here.

## 1. Goal

Within `Sim::run(n)` every tick but the last is a *fused* tick: nothing
observes it except the commit that follows (tick fusion). A fused body
can therefore be replaced by any body that commits the same state.
State-keyed specialization picks, each fused tick, a body pre-built for
the current values of a few *key slots* (a microcode word, a counter's
region flags, a control register), with those values folded through the
logic like the clocks of a clock-phase variant. Misses run the general
body. The prototype (`claude/extreme-experiments`, `SimOptions::spec`,
`examples/nes_spec.rs`) gained 18 to 20% JIT `run(n)` on SMB3 (902 ->
697 executed ops per tick), but with hand-named nets, keys profiled
offline in one scene (title-screen keys lost 3% in gameplay), whole-slot
keys, at most 255 bodies and no bound on code size.

Goals:

- keys found automatically (no hand-named nets), masked to the bits the
  logic reads, chosen per clock phase from a run's values;
- bodies built while running, for the key values that are hot *now*, in
  the background, with a cap on live bodies and code size;
- exact: every observable behaviour unchanged (section 6).

Non-goals: specializing observed (full) settles; specializing across
edits (a new program starts with empty tables); multi-threaded `Sim`s
(fusion and phases are single-threaded already); speculation that needs
rollback (a body is only used after its key was checked this tick).

## 2. What the compile side found on the NES console

`cargo run -p logi-sim --release --example spec_keys -- nes/workspace
--demo` (P1's report; the demo program renders sprites and background,
`PPUMASK` = `$1E`, and is rerun with `$18` for the transfer check). Ops
are instructions plus commits per tick; *executed* counts the guarded
bodies on 3000 sampled states per phase (the prototype's measure),
*static* every instruction.

| | phase 0, 1 (PPU) | phase 2 (CPU + PPU) |
|---|---|---|
| fused body: static / executed | 698 / 521 | 1337 / 1242 |
| no key, known bits only (`fallback(&[])`) | 537 / 494 | 1329 / 1229 |
| candidates (ROM words, flags, registers) | 108 (16, 68, 24) | 177 (27, 118, 32) |
| greedy key: fields, bits, distinct values | 16, 22, 64 | 14, 27, 93 |
| greedy key: executed ops saved (64 bodies) | 328 (63%) | 509 (41%) |

Greedy keys over all phases: 388 of 761 executed ops per tick (51%,
static count; the real gain is smaller, see section 7). The analysis
takes 3 ms for all three bodies; specializing and guarding the 1337-op
body takes 0.3 ms.

Hand-picked keys of the prototype, as this analysis sees them:

- `ppu_regs/mask` shrinks from 8 to 5 bits (`0x1f`: greyscale, the two
  left-column clipping bits, the two enables; the emphasis bits are never
  read) and `ppu_regs/ctrl` from 8 to 3 (`0x38`). The microcode word
  `ref_decode/cw` stays 44 bits on the CPU phase (every bit drives a
  decision there) but is 5 bits on the PPU phases.
- Masking alone does not fix the title-to-game loss: `$1E` and `$18`
  differ in the clipping bits, which the logic does read. Bodies built on
  the `$1E` run hit 0% on the `$18` run, whole or masked.
- **Profitable bits** (`prune_bits`, dropping key bits worth less than 10
  ops per settle) leave the PPU key 8 one-bit flags (`mask` and `ctrl`
  drop out entirely: the `render_ctl` flags already carry the enables;
  11 values instead of 21) and
  the CPU word 10 bits (7 values instead of 14): 146 instead of 228 ops
  per tick saved on the `$1E` run, but the same 146 on the `$18` run
  with the `$1E` bodies (whole keys: 135). With a 2-op threshold the
  clipping bits stay and the transfer fails, so the threshold is a real
  trade-off; the adaptive runtime (section 4) removes the need to guess:
  a new scene's keys get their own bodies within milliseconds.
- The automatically found candidates beat the hand-picked ones:
  `ref_decode/ir` (8 bits, a state register: no prefix) folds 179 ops on
  the CPU phase alone; PPU flags such as `ppu_sprites/ge_4_lt` or the
  fine-x register `ppu_bg/fx` each fold 50 to 60.
- The known-bits analysis with **no key at all** folds 161 static ops (27
  executed) off each PPU fused body: possible bits of state and ports,
  ROM reads with partly known addresses, demanded-bits dead code. That is
  free (P2).
- The hand-picked PPU flags are computed values whose prefix costs ~100
  ops per tick; a miss would lose them unless the miss body skips what
  the prefix computed (`fallback(pre)`, which the runtime must use).

## 3. Compile side: `compile::spec` (implemented, P1)

Pure functions over `Program`/`Body`; nothing here touches `sim/`.

```rust
pub const ALL: u64 = u64::MAX;
pub struct Known { pub zeros: u64, pub ones: u64 }           // known bits of a value
pub fn possible_bits(p: &Program) -> Vec<u64>;               // per slot: bits that can be 1
pub fn transfer(op: Op, i: &Instr, imm: u64, k: &dyn Fn(u32) -> Known,
                mems: &[MemDesc], phase: Option<u64>) -> Known;
pub struct SpecContext { .. }                                // one fused body, prepared
impl SpecContext {
    pub fn new(p: &Program, body: &Body, possible: &[u64], phase: Option<u64>) -> Option<Self>;
    pub fn body(&self) -> &Body;
    pub fn demanded(&self) -> &[u64];
    pub fn exact_support(&self, slot: u32) -> u64;           // keying on these = whole value
    pub fn support(&self, slot: u32) -> u64;                 // bits some decision reads
    pub fn computes(&self, slot: u32) -> bool;               // needs a prefix
    pub fn prefix(&self, keys: &[u32]) -> (Body, Vec<bool>); // key cone, slots it writes
    pub fn specialize(&self, p: &Program, assume: &[(u32, Known)], pre: &[bool]) -> Body;
    pub fn fallback(&self, p: &Program, pre: &[bool]) -> Body;  // miss body (no prefix work)
    pub fn gain(&self, p: &Program, layout: &KeyLayout, value: u64) -> i64;
    pub fn prune_bits(&self, p: &Program, layout: &KeyLayout,
                      samples: &[(u64, u64)], min_loss: f64) -> KeyLayout;
    pub fn candidates(&self, p: &Program) -> Vec<Candidate>;
}
pub struct Candidate { pub slot: u32, pub class: KeyClass, pub support: u64,
                       pub prefix_ops: usize, pub values: Option<Vec<u64>> }
pub enum KeyClass { RomWord, Flag, Register }
pub struct KeyField { pub slot: u32, pub mask: u64 }
pub struct KeyLayout { pub fields: Vec<KeyField> }          // pext of each field, concatenated
impl KeyLayout { pub fn width(&self) -> u32; pub fn pack(&self, slots: &[u64]) -> u64;
                 pub fn assume(&self, key: u64) -> Vec<(u32, Known)>; }
pub fn pext(x: u64, m: u64) -> u64;  pub fn pdep(x: u64, m: u64) -> u64;
```

**Key-bit minimization** is a ladder, each step a sub-mask of the one
before: possible bits (widths, through every op) -> `exact_support`
(demanded bits: what readers observe, backwards from the commits; a
commit's state slot counts, since whether the copy changes anything reads
it) -> `support` (the demanded bits some *decision* reads: a select, a
comparison, a reduction, a carry, a memory address, a bitwise `And`/`Or`
that a known bit can kill; bits that only travel to a commit as data,
such as a register's hold path, fold nothing) -> `prune_bits` (cost
model: drop the bit whose loss over sampled keys is smallest while below
a threshold). Any sub-mask is exact; the ladder trades folding for reach.

**Candidates** by the shape of the logic: `RomWord` (a ROM read: a
microcode word; `values` lists its distinct masked contents), `Flag` (a
1-bit `Eq`/`LtU`/reduction computed from state alone, no root input or
RAM), `Register` (a state slot the body does not write, or writes
through a mux with itself as one arm). Only slots with a nonzero support.

**Specialization** is forward known-bits propagation (`transfer`) with
constants restricted to demanded bits, aliases only onto slots demanded
at least as much, mux selection, and dead-code elimination from the
commits (module docs of `compile/spec.rs` give the full argument).

Tests (`compile/spec/tests.rs`): every candidate and every state slot of
fixtures, hand-built designs (a control register, a counter flag, a ROM
word, two clock periods) and 60 random designs, exhaustively over keys of
up to 8 (fixtures) or 4 (random) bits and a sample beyond: the prefix
plus the specialized body commits the same state and memories as the
general body on random states satisfying the key, for the exact support,
the fold support and a random sub-mask; keying on the exact support gives
the *same body* as keying on the whole value; dropping any support bit of
the hand-built registers is caught by the same check; possible bits cover
every value a running program produces; the report's `--check` repeats
the equality on 900 ticks of the NES console and catches bodies built for
a wrong key.

## 3b. Plan choice: `compile::spec::choose_plan` (implemented, P6 pure part)

```rust
pub fn choose_plan(p: &Program, contexts: &[Option<&SpecContext>],
                   samples: &[&[Vec<u64>]], opts: &PlanOptions) -> SpecPlan;
pub fn estimate(p, cx, layout, rows, bodies) -> Estimate;   // one layout's worth
pub struct PlanOptions { max_key_bits: 32, min_loss: 4.0, max_bodies: 24,
                         max_distinct: 256, pool: 20, min_gain: 2.0 }
pub struct SpecPlan { pub phases: Vec<Option<PhasePlan>> }
pub struct PhasePlan { pub layout: KeyLayout, pub saved: f64,
                       pub distinct: usize, pub hit: f64 }
```

`samples[k]` are rows of settled slot values (every slot, indexed by
slot) of ticks of phase `k`: a window the runtime recorded, or a profile.
Per phase: each candidate (key = its fold support) is scored alone;
the best `pool` are combined greedily while a field adds more than
`min_gain` and the width stays within `max_key_bits`; `prune_bits` then
drops bits worth less than `min_loss` ops; a phase whose result saves no
more than `min_gain` (or has no field) gets `None`. Keys with more than
`max_distinct` distinct values in the samples are never chosen.

The score (`estimate`) is the doc's cost model in *executed* ops per tick
over the rows: the guarded general body, minus the prefix (all of its
instructions, unguarded, every tick, hit or miss), minus the guarded
specialized body for the `max_bodies` most frequent keys and the guarded
fallback for the rest. Guards matter: a key whose cone the general body
skips most ticks does not pay for its prefix (tested). The function is
pure and deterministic (ties broken by candidate order and key value);
it runs no `Sim`. Open details chosen simply: samples are whole slot rows
(not only candidate slots), the plan has one layout per phase, and
`SpecOptions` has no `PlanOptions` of its own yet (P4 maps its caps onto
them). On the NES demo with `min_loss` 2 it picks the same keys as the
report's former ad hoc greedy (every kept bit is worth at least 2 ops).

## 4. Runtime design

### Data

```rust
// sim/spec.rs (new)
pub struct SpecOptions {
    pub max_bodies: usize,        // live specialized bodies, all phases (64)
    pub max_per_phase: usize,     // (24: I-cache; 54 PPU bodies were slower than 8)
    pub code_budget: usize,       // native bytes of live bodies (1 MiB)
    pub hot_share: f64,           // build a key once it has this share of its
                                  // phase's ticks in the window (0.005)
    pub window: u64,              // fused ticks per counting window (1 << 16)
    pub max_key_bits: u32,        // (32)
    pub min_loss: f64,            // prune_bits threshold, ops per settle (4.0)
    pub inline_build: bool,       // build on the calling thread (tests, wasm)
    pub plan: Option<Arc<SpecPlan>>, // fixed keys (benches, tests); None: automatic
}
// SimOptions gains `pub spec: Option<SpecOptions>` (None, off, until benchmarked on).

pub struct SpecPlan { pub phases: Vec<Option<PhasePlan>> }
pub struct PhasePlan { pub layout: KeyLayout }        // what is keyed, per phase
// as implemented (compile::spec): PhasePlan also carries the estimate it was
// chosen by (`saved`, `distinct`, `hit`); `SpecOptions` maps onto `PlanOptions`.

struct PhaseSpec {                       // in Sim, per phase with a plan
    layout: KeyLayout,
    prefix: u16,                         // run computing the key cone (0: none)
    fallback: u16,                       // fallback(pre): the miss body
    cx: Arc<SpecContext>,                // shared with the builder thread
    pre: Arc<[bool]>,
    table: FastMap<u64, u16>,            // key -> run
    hot: [u32; 1024], hot_tag: [u64; 1024], // miss counters, direct-mapped by key hash
    last_hit: Vec<u64>,                  // per live run: tick of its last hit (LRU)
    pending: FastSet<u64>,               // requested, not installed
}
```

Run indices become `u16` (`full`, `fused`, `RunKey`, `commit`,
`evaluate`): up to 65535 bodies, of which the cap keeps a few dozen.

### P4 as built (fixed plan)

Differences from the sketch above, which P5b builds on:

- `SpecOptions` holds only `plan: Option<Arc<SpecPlan>>` for now (the
  caps, window and thresholds come with P5b). The runtime's plan is
  `sim::SpecPlan { phases: Vec<Option<sim::PhasePlan>> }` with
  `PhasePlan { layout: KeyLayout, keys: Vec<u64> }`: the key values to
  build bodies for are part of a fixed plan (P6's `compile::spec::plan`
  types carry estimates instead; P5b maps one onto the other). A layout
  wider than 64 bits or naming a slot past the program's leaves its
  phase unkeyed; keys wider than the layout are ignored.
- `PhaseSpec { fields, prefix: Option<u16>, fallback: u16, table:
  FastMap<u64, u16>, hits, misses }` (run 0 is the program's own body,
  so "no prefix" is `None`). The key is packed with a shift per
  contiguous field mask and `pext` otherwise. When every key slot is
  state, an input or a constant there is no prefix and the fallback *is*
  the phase's fused run (the general body), so a miss costs nothing.
- Bodies are built in `build_layout` (so `rom_changed`'s rebuild with
  folds off drops them and the stash keeps them for `reset`). `folded`
  is also set by a keyed phase, so a ROM poke switches specialization
  off with `known_bits` off too.
- `Sim::spec_stats() -> SpecStats { keyed_phases, bodies, hits, misses }`
  (P5b extends it). `sim::spec_contexts(p, options)` gives the context of
  each phase's fused body as the runtime builds it (for plans: P6's
  sampling, tests, benches).
- JIT: `compile_jit` compiles the spec runs with the others; a prefix is
  not `fused_only` (the key and the body after it read every result).
  `RunKey` gains `spec` (the tables' runs), so code for another plan is
  refused. No superstep is compiled while any phase is keyed (step 1).
- Counters: the prefix's instructions count, but not as a settle (one
  settle per tick, as before).
- Tests (`sim/spec/tests.rs`): `testkit::diff::check_batches_with` with
  `testkit::diff::spec_plan` (up to three candidates per phase drawn by
  seed, support or a random sub-mask, at most 12 bits; every value of
  keys of up to 4 bits plus the 8 hottest of a sample run) on 5 designs
  per `standard_configs` entry, default and debug options, plus
  `check_pair` spec on against off and `check_jit` with spec; a
  microcoded design (ROM word plus register key, with and without clock
  phases, all, some and no keys); an always-missing key equal to off;
  `last` across fused ticks, `tick`, `run`, `resettle`, `reset`,
  `import_state`; counters against the bodies' sizes along the known key
  sequence; ROM pokes; supersteps; native code of another plan refused;
  300 bodies; mutations (bodies for the wrong key, skipped prefix) caught.
- Measured (`spec_keys --bench`, greedy keys with bodies for the 64 most
  frequent keys per phase, JIT, spec on against the default JIT with
  supersteps; executed ops from a `counters` build): NES demo 739 -> 375
  ops per tick; Blade Buster (MMC3, after 6M ticks) 805 -> 437. Speed
  (k ticks/s, median of 5 alternating rounds of 1M ticks, range in
  brackets, on a shared 4-core machine): NES demo 3145 (2749..3338)
  against 2246 (1856..2425), +40%; Blade Buster with 64 bodies per phase
  1955 (1701..1970) against 1663 (1518..1971), +18%, with 16 per phase
  (91% hit) 2761 (2616..2906) against 2105 (1970..2233), +31%. State
  and memories equal after 6.5M and 11M ticks. 192 bodies take 1.5 to
  1.9 MiB of native code (0.2 MiB without), 48 bodies 0.7 MiB: the
  I-cache, not the op count, decides past a few dozen bodies, which is
  what P5b's caps are for.

### Per fused tick (`Sim::run`, the path without a superstep)

```text
commit(last); tick += 1; k = tick % period
if let Some(ps) = spec[k]:
    apply inputs to their slots; evaluate(ps.prefix)      // key cone, unguarded
    key = ps.layout.pack(&slots)
    run = ps.table.get(key) -> hit: last_hit[run] = tick
                             -> miss: bump hot[hash(key)]; maybe request; run = ps.fallback
    evaluate(run); last = run       // the commit after belongs to the body that settled
else: evaluate(fused[k]); last = fused[k]
```

The last tick of `run(n)` and every `tick()` stay full settles of the
general variant; `last` makes the first commit of the next call use the
commits of the body that settled (a specialized body drops only commits
that would change nothing, so the state is the same either way).

### Building and installing

```rust
impl Sim {
    pub fn spec_requests(&mut self) -> Vec<SpecRequest>;   // drained hot keys
    pub fn install_spec(&mut self, b: SpecBuilt) -> bool;  // false: stale or full
    pub fn spec_stats(&self) -> SpecStats;                 // hits, misses, live, bytes
}
pub struct SpecRequest { pub program: Weak<Program>, pub phase: u8, pub key: u64,
                         pub cx: Arc<SpecContext>, pub pre: Arc<[bool]>,
                         pub layout: KeyLayout, pub guards: Option<usize> }
pub fn build_spec(p: &Program, r: &SpecRequest, jit: bool) -> SpecBuilt; // pure, any thread
pub struct SpecBuilt { pub phase: u8, pub key: u64, pub body: Body,
                       pub native: Option<jit::JitUnit>, program: Weak<Program> }
```

`build_spec` is `cx.specialize(p, &layout.assume(key), &pre)`, then
`guard_with` when guards apply (the same rule as fused bodies: with the
JIT, or `guard_interpreter`), then `check_body`, then (JIT) a separately
compiled unit. `install_spec` checks the program pointer, phase and
layout (a plan change makes old builds stale), evicts if over
`max_per_phase`, `max_bodies` or `code_budget` (the least recently hit
body whose last hit is older than one window; if none is that cold, the
new body is dropped and its key cooled), and inserts. With
`inline_build` (tests, wasm, a `Sim` without an engine) `run` builds
synchronously at the end of a batch, so behaviour is deterministic.

**JIT units.** Today `JitProgram` compiles all bodies into one
`JITModule`. Specialized bodies need their own lifetime:
`jit::JitUnit { settle: SettleFn, commit: Option<Vec<CommitFn>>, bytes:
usize, module: JITModule }`, compiled by `jit::compile_unit(p, &BodyRef)
-> Result<JitUnit, JitError>` with the existing chunking and emission,
freed (`free_memory`) on drop. `Sim` keeps `spec_native: Vec<Option<
Arc<JitUnit>>>` beside `jit`, indexed by run. The interpreter runs every
body as before (`Run`), so a body is usable the moment it is installed;
native code is a second install.

**Key choice (automatic plan).** At `Sim` construction each phase gets a
`SpecContext` of its fused body and its candidates. For the first
`window` fused ticks no key runs; `run` samples the candidate slots
after each fused settle (every 8th tick, 512 rows per phase). Then the
plan is chosen as `spec_keys` does (P6): candidates scored alone, a
greedy combination up to `max_key_bits`, then `prune_bits`
(`compile::spec::choose_plan`, implemented: see section 3b). The plan
is fixed for the life of the program; the adaptive part is which key
values have bodies. (Re-planning on a long run of misses is a possible
follow-up.)

### Engine

The engine's compile thread already builds JIT code off the tick loop.
After each batch the worker calls `sim.spec_requests()` and sends them
(`Msg::SpecBuild`) to the compile thread, which runs `build_spec` and
sends `Msg::SpecBuilt(revision, built)` back; the worker calls
`core.install_spec` if the revision still matches (like `install_jit`).
`tier_layout` is unchanged: the layout of spec bodies follows `guard` and
`jit` exactly like fused bodies, and native units are installed per body,
so JIT tiering (interpreted first, native later) applies per body too.
`LocalEngine` (wasm) uses `inline_build`.

### Guards, phases, supersteps

- **Guards**: a specialized body is guarded exactly like the fused body
  it replaces (`guard_with(.., None, guard_min)`), with the JIT or
  `guard_interpreter`; the prefix is unguarded. Specialization removes
  many enable muxes outright, so fewer guards remain.
- **Phases**: plans and tables are per phase; a phase without a plan runs
  its fused body. Programs without phases are phase 0 of period 1.
- **Supersteps** (`claude/superstep`): a superstep is the `L` fused ticks
  of a period as one native call, with values forwarded across ticks.
  Step 1 (P4): `run` uses a superstep only when no phase has a plan (or
  `spec` off), so the two are alternatives, measured against each other.
  Step 2 (P8): *keyed supersteps*: the key of each tick is checked inside
  the superstep (the prefix items, a compare against the constant the
  superstep was built for), and a mismatch side-exits: it stores the
  live values (the superstep already knows which, `live_end`) and returns
  the tick reached, from which `run` continues tick by tick. A superstep
  is built per hot *period key* (the tuple of the `L` ticks' keys), under
  the same cap. On the NES the CPU phase's key changes once per period
  and the PPU keys rarely within one, so period keys repeat.

## 5. Cost model and the cap

Per phase `k` and key value `v` seen in a fraction `f(v)` of phase-`k`
fused ticks: saved ops per tick `= f(v) (E[general] - E[spec_v]) -
prefix_k`, with `E` the executed ops of the guarded body. A miss costs
the dispatch only (the fallback skips the prefix's instructions). Build
cost: about 0.3 ms to specialize and guard, 3 ms to JIT a body (the
prototype: 67 bodies in 0.23 s). Space: each native body is tens of KB;
past roughly 8 to 24 hot bodies per phase the I-cache, not the op count,
decides (the prototype: 54 PPU bodies ran slower than 8). Hence: build a
key when it holds `hot_share` of its phase's ticks in a window, keep at
most `max_per_phase` and `code_budget`, evict the least recently hit, and
measure the defaults (P9) rather than trusting op counts: op counts
overstate the gain (the report's 51% against the prototype's measured
18 to 23% for keys of similar reach).

## 6. Exactness

1. A specialized body is run only on a tick whose key, computed this
   tick by the prefix (the general body's own instructions for those
   slots), equals the body's key; so the assumption holds.
2. Under a holding assumption the body commits the same state and memory
   writes as the general fused body (section 3; tested exhaustively on
   small designs, on random designs, and on the NES run).
3. Fused ticks are observed by nothing but their commit; observed ticks
   (`tick()`, the last tick of `run`) are never specialized; so at every
   batch boundary slots, state and memories equal the general path's:
   `testkit::diff::check_batches` (state at every boundary, batches 1 to
   41) with `spec` on is the acceptance test, interpreter and JIT, with
   guards and phases, debug and default options.
4. Nothing is cached across state: tables map key *values* to bodies,
   so `set_state`, `resettle`, `reset`, `import_state` and `set_input`
   need no invalidation. **ROM pokes** are the exception: bodies fold ROM
   reads with known addresses from the program's ROM contents, so
   `set_mem_word` on a ROM (`!writable`) turns specialization (and the
   known-bits fallback) off for that `Sim` until `reset`. (The compiler
   and the phase variants already fold constant-address ROM reads; the
   specializer folds far more of them.)
5. `Options::debug`: fused bodies are pruned to the commits' cone under
   any options, so specialization is unaffected; observed settles keep
   every net. Counters (feature `counters`) count what each run executes,
   so `ops()` shows the saving.

## 7. Work breakdown

Each package fits one worker's context; files are the main ones touched.
Order: P2, P3 first (small, in `sim/mod.rs`), then P4 with P5a and P6 in
parallel (separate files), then P5b, P7, P8, P9.

| | package | files | depends | tier |
|---|---|---|---|---|
| P1 | compile side: support analysis, candidates, specializer, `prune_bits`, report (**done**) | `compile/spec.rs`, `compile/spec/tests.rs`, `examples/spec_keys.rs` | - | opus |
| P2 | known-bits fused bodies: every fused body becomes `SpecContext::fallback(p, &[])` (no key); ROM-poke switch-off | `sim/mod.rs` (fused-body construction), `sim/tests.rs` | P1, superstep merged | sonnet |
| P3 | run index `u16` | `sim/mod.rs`, `sim/jit/mod.rs`, `sim/jit/superstep.rs` | superstep merged | sonnet |
| P4 | fixed-plan runtime: `SpecOptions` with `plan`, `PhaseSpec`, prefix/fallback runs, dispatch in `run`, `last`, inline builds, JIT of spec bodies in `compile_jit`, counters, superstep exclusion (**done**) | `sim/mod.rs`, new `sim/spec.rs`, `testkit/diff.rs` | P3 | opus |
| P5a | per-body JIT units: `jit::compile_unit`, `JitUnit`, freeing | new `sim/jit/unit.rs`, `sim/jit/mod.rs` (share emission) | P3 | sonnet |
| P5b | adaptive tables: miss counters, hot detection, `spec_requests`/`build_spec`/`install_spec`, LRU eviction, caps, `spec_stats` | `sim/spec.rs`, `sim/mod.rs` | P4, P5a | opus |
| P6 | automatic plan: `spec::choose_plan(contexts, samples, opts) -> SpecPlan` (pure, the report's greedy + `prune_bits`), runtime sampling window | `compile/spec.rs` (pure part), `sim/spec.rs` | P1 (pure part: none) | sonnet |
| P7 | engine integration: compile-thread builds, revision checks, `LocalEngine` inline | `engine/mod.rs`, `engine/tests.rs` | P5b | sonnet |
| P8 | keyed supersteps with side exits; measure against P4's per-tick path | `sim/jit/superstep.rs`, `sim/mod.rs` | P4, superstep merged | opus |
| P9 | benches and docs: `nes_bench` rows (spec on/off, hits, live bodies), `perf-metrics` row, `ARCHITECTURE.md`, `PROFILING.md`, tune defaults | `examples/nes_bench.rs`, `xtask/`, docs | P5b (P7 for the game) | haiku |

P2 and P3 are implemented (`SimOptions::known_bits`, on by default; run
indices are `u16`). Findings that shape P4:

- A known-bits body may alias or drop slots the general body writes, so
  the commit after a fused settle must be the settled body's own, not the
  full body's (`Sim::last`, set by every settle; between calls it is the
  full body). `tick()`, `fused_tick` and the superstep commit with it.
  A fused body can no longer be assumed to leave the full commits' operands
  in their slots.
- A ROM write that changes a word (`set_mem_word`, `import_state`) sets the
  folded layout aside and rebuilds the plain fused bodies (and the JIT code
  when active); `reset` puts the stash back. `RunKey` carries `folded`, so
  native code built for the other layout is refused. The compiler's and the
  phase variants' constant-address ROM folds live in the `Program` and are
  not covered: a poke of such a word never reaches the folded read.

Test plans:

- **P2**: `check_batches`, `check_jit`, `check_pair` unchanged and green
  (they already compare fused batches with single ticks); a test that a
  ROM poke turns it off (poke a ROM word the fused body folds, compare
  batches with ticks); counters show fewer ops on the NES-like fixtures.
- **P3**: the whole suite; a test with more than 255 runs (a design with
  many phases or, after P4, many keys) that commits through run 300.
- **P4**: `check_batches` with a plan on every random design of
  `testkit::standard_configs` (keys from `candidates`, all values of
  narrow keys plus the hot ones), interpreter and JIT, guards on/off,
  debug and default options; a design whose key changes every tick (all
  misses: equal to off); `last` across `run`/`tick`/`resettle`; counters
  equal hand-computed ops; a mutation (install a body under the wrong
  key) that `check_batches` must catch.
- **P5a**: unit compile/run/free of single bodies against the
  interpreter (`check_jit` style); many compile/free cycles do not grow
  memory.
- **P5b**: deterministic `inline_build` tests: hot keys get bodies after
  `window` ticks; eviction keeps the cap and the code budget; a stale
  build (program swapped, plan changed) is refused; exactness as P4
  while bodies come and go mid-run.
- **P6**: on the hand-built designs of `compile/spec/tests.rs` the plan
  picks the obvious key; on random designs the plan is exact (P4 tests
  with the chosen plan); `spec_keys` prints the chosen plan.
- **P7**: engine tests with a paced clock: bodies arrive and are
  installed, an edit mid-build drops them, frames identical with spec on
  and off.
- **P8**: the superstep's own `check_batches` variants with keys; side
  exits at every tick offset (a key that flips at a chosen tick); the
  superstep mutation tests extended (a skipped key check must be caught).
- **P9**: `cargo xtask perf check` stays green; numbers on SMB3 title and
  1-1 (the transfer case) recorded in `PROFILING.md`.
