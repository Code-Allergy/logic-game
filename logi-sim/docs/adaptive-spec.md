# Adaptive state-keyed specialization (issue #117)

Status: compile side implemented (`compile/spec.rs`, package P1 below);
P2, P3, the fixed-plan runtime P4 and the adaptive tables P5 (with P6's
sampling window) implemented (`sim/spec.rs`, section 4 "P4 as built" and
"P5b as built"); the pure part of the plan choice (`compile/spec/plan.rs`,
P6, section 3b); P8 measured, partly built ("P8 as built": keyed
supersteps do not pay, the superstep comes back where nothing is keyed);
P7 and P9 designed here.

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

### P5b as built (adaptive tables)

Differences from the sketch below, which P7 and P8 build on:

- **Options.** `SpecOptions { plan, adaptive, max_per_phase (16),
  max_bodies (48), code_budget (1 MiB), window (2^16), hot (256),
  plan_window (2^20), samples (512), max_key_bits (32), min_loss (4),
  inline_build (true) }`, integers only (`SimOptions` stays `Eq`): `hot`
  is a miss count per window instead of `hot_share`, `min_loss` whole ops.
  `Default` is adaptive with an automatic plan; `SpecOptions::fixed(plan)`
  is P4 unchanged (bodies built with the layout, no counting, no caps);
  `SpecOptions::with_plan(plan)` is adaptive on given keys, the plan's
  key values wanted first (built by the first requests, inline at
  construction). `inline_build` defaults to on so that a `Sim` used on
  its own works; an engine turns it off and drives the requests.
- **Layout.** Everything specialization adds lives in `sim::spec::Spec`
  (tables, sampler, installed runs), set aside and restored as a whole
  by a ROM write and `reset` (`Stash::spec`). Runs built with the layout
  are `runs[..base]`; installed bodies are runs past `base`, freed runs
  are reused (a placeholder body keeps the index), so `runs` never
  outgrows `base + caps`. `Spec::key` (the layout's specialized runs plus
  "a plan is still to come") is `RunKey::spec`, computed once when the
  layout is built: installs and evictions never change it, so the base
  `JitProgram` stays valid and `install_jit` keeps accepting code compiled
  for the layout (`bodies.len() == base + 1`). An automatic plan's
  prefixes and fallbacks are installed runs too (pinned: never evicted).
- **Native code.** `build_spec` compiles each body (prefix: not
  `fused_only`) into a `JitUnit` (P5a); `Sim` keeps it beside the run
  (`Spec::dynamic`) and calls it through `settle_raw`/`commit_raw` with
  the cached `mem_ptrs` (`sim::native` picks the program's body or the
  unit). Units run only while native code is installed
  (`install_jit(None)` interprets everything, units included); a
  `MemClear` body commits through the interpreter. A request asks for a
  unit when the `Sim` has `jit` on or runs native code;
  `SpecRequest::set_native` overrides it (tiered `Sim`s).
- **Per fused tick.** `fused_tick` tests one flag (`Spec::hooked`: a
  keyed phase or sampling) and calls `spec_fused`. A hit stores the
  phase clock (`hits + misses`) in the entry; a miss bumps a 1024-entry
  direct-mapped counter (majority vote: a colliding key wears the count
  down before taking the slot); at `hot` misses within a window the key
  is *wanted* (at most 64 wait per phase). A key wanted, requested or
  installed is not wanted again; a refused build (no room, failed check,
  stale) releases it, and a request the caller never answers expires
  after 4 windows. Counters restart every `window` keyed ticks of the
  phase.
- **Requests.** `spec_requests` turns wanted keys into requests while
  the phase has room (`bodies + asked < max_per_phase`, all phases under
  `max_bodies`) or a cold body (not hit for a window) can make room;
  otherwise the key is dropped and must turn hot again. A request holds
  the program (`Weak`), the generation, the context and the `pre` flags
  (`Arc`s), the layout and whether to guard and compile; `build_spec(p,
  r) -> SpecBuilt` needs nothing else (pure, any thread), and builds
  nothing for a `p` other than the request's program (the result is
  refused everywhere). `jit::compile_unit` runs `Sim`'s program and body
  checks itself, so its safe entry points never index out of bounds. The automatic
  plan is a request too (`choose_plan` on the samples, then each keyed
  phase's prefix and fallback built and compiled).
- **Install.** `install_spec` refuses a build of another program or
  `Sim` or generation (stale), a key with a body, and a key over the
  layout's width; then evicts the coldest body (largest age past one
  window: of the phase over `max_per_phase`, of any phase over
  `max_bodies` or `code_budget`) until it fits, or refuses (dropped);
  then takes a run, builds its `Run` (interpreter code) and inserts the
  entry. Every eviction is chosen before any is made: a body that cannot
  fit (larger than `code_budget`, no run index left) evicts nothing. Only between calls (a single-threaded `Sim`: the body that
  settled the slots is the full body there, never an installed one).
- **Generations.** Unique over all `Sim`s (a global counter): a new
  `Sim`, a ROM write (`rom_changed`) and the `reset` that restores the
  folded layout (`rom_restored`) take a new one, clear the pending keys
  and, if the plan's samples went out, sample again. A plain `reset` or
  `import_state` keeps the generation: tables map key values, so builds
  stay exact (snapshots hold no ROM; an import that changes a ROM word
  goes through `rom_changed`).
- **Automatic plan.** At construction each phase keeps the `Arc` of its
  fused body's context; `samples` fused ticks per phase, spread over the
  first `plan_window`, settle with the phase's *full* body (so every slot
  the estimate reads is computed; the commit after is the full body's)
  and keep a copy of the slots; then one plan request, asked again every
  `plan_window` fused ticks per phase until a plan is installed (a
  dropped request, a refused plan; the samples are kept until then). A
  plan is installed whole: one that does not fit under the run-index cap
  leaves nothing behind. No key and no
  superstep meanwhile (`compile_jit` and `run` skip supersteps while a
  plan is awaited); the layout counts as folded from the start, so a ROM
  write stops the sampling. A plan with no keyed phase leaves the `Sim`
  without keys (and, since P8, brings the layout's superstep).
- **Stats.** `SpecStats` adds `requested`, `installed` (a plan counts
  one), `evicted`, `stale`, `dropped`, `code_bytes` (installed units,
  pinned included) and `awaiting_plan`; hits and misses as in P4.
- **Tests** (`sim/spec/adaptive_tests.rs`; every P4 test unchanged on
  `SpecOptions::fixed`): hot detection (nothing below `hot`, then bodies
  for exactly the hot values, then hits); a plan's keys built first;
  `check_batches_with`, `check_pair` (pokes, ROM writes, reset, import)
  and `check_jit` on 5 designs per `standard_configs` entry, default and
  debug options, for three adaptive settings (1 body per phase and a
  window of 2, 3 and 16, an automatic plan): bodies come and go
  mid-run; eviction under a cap of 4 and a window of 8 (a key cycling
  through 8 values: hundreds of installs and evictions, never more than 4
  live, runs reused, a code budget of 0 keeps every native body out);
  stale builds refused after a ROM write, after the `reset` that undoes
  it, from another `Sim` and another program, accepted after a plain
  `reset` and `import_state`; builds on a `std::thread` with installs
  between batches (plan given and automatic); an automatic plan (no key
  and no superstep while sampling, keys after; a ROM write while sampling,
  then `reset`); with the JIT, installed bodies run their units, the
  program's code is not recompiled, `install_jit` of the layout's code
  still works, an automatic `Sim` refuses code with a superstep; 1500
  build/evict cycles keep live units and runs within the cap (P5a's RSS
  test covers freeing). Mutations caught by `check_batches`: a body
  installed under the wrong key (bit 0 flipped), and an eviction that
  frees the run but leaves its entry (the next install reuses the run for
  another key).
- **Measured** (`spec_keys --adaptive 6000000 --rounds 6 --later
  20000000 --bodies 16`, JIT, all runtimes in one process, builds on one
  thread of their own; k ticks/s, median of 6 interleaved rounds of 1M
  ticks, range in brackets, hit rate of keyed ticks; a shared 4-core
  machine). Fixed plans and the adaptive tables' keys come from a
  profile of ticks 1.5M to 6M (Blade Buster) or 1.5M to 2.1M (NES demo);
  "start" is the 6M (2.1M) ticks right after, "later" ticks 20M to 26M.

  | Blade Buster (MMC3) | start | later | live bodies | native KiB |
  |---|---|---|---|---|
  | spec off (supersteps) | 2432 (2303..2513) | 2397 (1873..2494) | 0 | 208 |
  | fixed, 16 keys per phase | 2952 (2449..3027) +21%, 92% | 2318 (1532..2404) -3%, 64% | 48 | 693 |
  | fixed, 64 keys | 2914 (2424..2955) +20%, 98% | 2302 (1579..2395) -4%, 68% | 145 | 1835 |
  | adaptive, cap 8 | 2542 (1845..2567) +5%, 68% | 2732 (2295..2834) +14%, 80% | 24 | 463 |
  | adaptive, cap 16 | 2949 (2049..3000) +21%, 93% | 3003 (2494..3015) +25%, 94% | 48 | 694 |
  | adaptive, cap 32 | 2744 (2260..3062) +13%, 95% | 3013 (2437..3034) +26%, 96% | 96 | 1081 |
  | automatic plan, cap 16 | 2757 (2109..3057) +13%, 94% | 2992 (2721..3076) +25%, 97% | 48 | 685 |

  | NES demo | start | later | live bodies | native KiB |
  |---|---|---|---|---|
  | spec off (supersteps) | 2721 (2239..2758) | 2415 (1894..2808) | 0 | 183 |
  | fixed, 16 keys per phase | 3464 (3131..3784) +27%, 96% | 3509 (3172..3800) +45%, 96% | 48 | 578 |
  | fixed, 64 keys | 3461 (2944..3832) +27%, 100% | 3593 (2908..3874) +49%, 100% | 134 | 1251 |
  | adaptive, cap 8 | 3105 (2526..3171) +14%, 71% | 3187 (1955..3272) +32%, 71% | 24 | 386 |
  | adaptive, cap 16 | 3191 (2303..3648) +17%, 96% | 3625 (2800..3733) +50%, 96% | 48 | 581 |
  | adaptive, cap 32 | 3537 (3175..3750) +30%, 98% | 3461 (3180..3777) +43%, 98% | 96 | 798 |
  | automatic plan, cap 16 | 3612 (2190..3717) +33%, 94% | 3691 (3068..3948) +53%, 98% | 43 | 535 |

  Executed ops per tick on Blade Buster (a `counters` build, the same
  runs): spec off 756; fixed 16 keys 482 at the start, **613 later** (the
  profile's keys hit 64%); adaptive cap 16 480 and 475; automatic plan
  502 and 485 (with `plan_window` 2^16, the first 200k ticks of boot code:
  610 and 606, hence the default of 2^20). The adaptive tables keep the
  later scene at the profiled scene's level where a fixed plan profiled
  early falls back below spec off; cap 16 is the best default on both
  (cap 8 misses too often, 32 holds more code for 1 to 2% more hits).
  Time to the first hit: 30k ticks (the first batch; the profile's
  first key values turn hot within it) with a plan's keys, 6.1M (Blade
  Buster) and 4.6M (NES) ticks with an automatic plan (the sampling
  window, 3.1M ticks, plus the plan's build). Build latency per body
  (specialize, guard, check, compile a unit; on the build thread while
  the runtimes run): median 4.1 ms, 90% 6.8 ms, max 17 ms (Blade
  Buster), 1.9 / 6.7 / 10.4 ms (NES); the automatic plan's build
  (`choose_plan` on 512 samples per phase plus prefixes and fallbacks):
  3.1 s and 2.3 s. Hundreds of evictions over 26M ticks (90 at cap 16 on
  Blade Buster), state and memories equal to spec off at the end.
- **For P7 (engine).** Turn `inline_build` off for `Sim`s the worker
  drives; after each batch send `spec_requests()` to the compile thread
  (`build_spec(&program, &r)`), and `install_spec` the results between
  batches: staleness (another program, `Sim` or generation) is checked
  by `install_spec` itself, so the engine's revision check only saves
  work. A tiered `Sim` (`tier_layout`: `jit` off until `install_jit`)
  should `set_native(true)` on its requests; bodies installed before
  that have no unit and run interpreted until evicted (rebuilding them
  after `install_jit` is a possible refinement). The automatic plan's
  build takes seconds on the NES console: queue it behind the JIT
  compile, never ahead of it. `LocalEngine` keeps `inline_build`, but the
  plan's build then stalls one `run` for that long: on wasm, fewer
  `samples` or a fixed plan.
- **For P8 (keyed supersteps).** Installed bodies are not in the
  `JitProgram`, so a keyed superstep is a unit of its own (per hot period
  key, under the same caps, evicted like a body; `compile_unit` would
  take a `SuperRef`). `Spec::hooked` makes `run` skip the program's
  superstep; a plan that keys nothing leaves no superstep either (the
  layout was compiled without one while the plan was awaited), which P8
  can fix by compiling the superstep with the plan. (Done, see "P8 as
  built": the superstep with the plan, not keyed supersteps.)

### P8 as built (supersteps while keyed)

Measured first, then built what pays:

- **Keyed supersteps per period key (not built).** A superstep per hot
  *period key* (the keys of one period's ticks), with a side exit at the
  first tick whose key differs, only runs when that tuple repeats and can
  be predicted before the call (the keys of a period depend on state the
  superstep computes). `spec_keys --period-keys 3000000` measures this on
  the plan's keys (cap 16 per phase, `--min-loss 4`, as the automatic
  plan; 3M ticks after the profile):

  | | NES demo | Blade Buster |
  |---|---|---|
  | distinct period keys (of 1M periods) | 978 | 4068 |
  | periods covered by the 16 / 64 / 256 most frequent | 47% / 79% / 98% | 39% / 81% / 89% |
  | period key right: the previous one / the one that followed it last time | 0% / 28% | 0% / 28% |
  | ticks run before the first wrong phase (second predictor) | 38% | 45% |
  | phase 1, 2, 0: distinct keys | 49, 97, 49 | 49, 630, 51 |
  | phase 1, 2, 0: key changes per period | 76%, 44%, 76% | 76%, 68%, 76% |

  The PPU phases' keys follow the dot and change in three periods of
  four; the CPU phase's with the microcode step. Under the body cap (16
  superstep units of three bodies each, as much code again as the 48
  bodies) even a perfect predictor would run 39 to 47% of periods as
  supersteps, the best cheap one 28% (38 to 45% of ticks). Supersteps
  gain 8 to 12% where every period is one (spec off), so this design is
  bounded at about 3 to 5% before side exits and the instruction-cache
  cost of the extra code: under the 5% the work needed to pay. The doc's
  assumption ("period keys repeat") does not hold.
- **A superstep of dispatch (not built).** One unit per period that
  computes each tick's key and branches to inlined bodies merges every
  value back at each tick's dispatch, so it forwards within a tick only;
  its gain over the per-tick path is mostly the per-tick dispatch
  overhead. It needs a second copy of every hot body (0.4 to 0.5 MiB
  here) compiled again whenever a body comes or goes (hundreds of times
  over 26M ticks, about 4 ms per body). The overhead it would remove was
  measured instead (next item) and is small.
- **Keyed ticks without the per-tick overhead (built).** Within a `run`,
  `Sim::keyed_ticks` runs the fused ticks of a keyed `Sim` (no plan
  awaited) without what supersteps also leave out: the inputs are
  applied by the first fused tick only (nothing writes their slots within
  a `run`), and the clock phase is counted instead of divided out.
  Automatic plan, cap 16, JIT, k ticks/s, median (range) of interleaved
  rounds of 1M ticks after 6M (NES demo) and 8M (Blade Buster) ticks:

  | | NES demo | Blade Buster |
  |---|---|---|
  | 11 rounds: per-tick path | 3424 (2044..3865) | 2692 (2285..3067) |
  | 11 rounds: `keyed_ticks` | 3552 (2069..4098) +4% | 2736 (2112..3203) +2% |
  | 7 rounds: per-tick path | 3743 (2826..3886) | 2403 (1823..2906) |
  | 7 rounds: `keyed_ticks` | 3964 (3396..4032) +6% | 2707 (1903..3026) +13% |

  (A shared 4-core machine: the ranges overlap; the medians always
  favour `keyed_ticks`.) Spec off with supersteps ran 2586 and 2693 (NES),
  2329 and 2118 (Blade Buster) in the same rounds. A mutation that never
  applies the inputs is caught by `check_batches`.
- **The superstep with a plan that keys nothing (built).** The layout's
  native code is compiled without a superstep while an automatic plan is
  awaited (keys are to come). The plan request now carries the layout's
  superstep (its fused bodies, owned: `SuperBuild`), and `build_spec`
  compiles it into a `jit::SuperUnit` of its own (`compile_superstep`,
  sharing `define_superstep` with `JitProgram`) when the plan keys no
  phase. `install_plan` keeps it in `Spec::superstep`: pinned like the
  plan's prefixes and fallbacks (counted in `SpecStats::code_bytes`, not
  under `code_budget`; 47 KiB on the NES), freed with the `Spec` (a
  `SuperUnit` frees its module on drop; `run` holds only its entry, and
  nothing installs or evicts within a `run`), set aside and restored
  with the layout by a ROM write and `reset`, and run only while native
  code is installed. `compile_jit` after such a plan includes the
  superstep in the program's code (the layout key is unchanged, so that
  code installs). Automatic plan with no key bit (`max_key_bits` 0), JIT,
  median of 7 rounds, four processes each:

  | | NES demo | Blade Buster |
  |---|---|---|
  | spec off (supersteps) | 2609..2776 | 2244..2485 |
  | keying nothing, with the plan's superstep | 2553..2715 (-7%..+2%) | 2226..2488 (-3%..0%) |
  | keying nothing, no superstep (as before P8) | 2191..2430 (-7%..-18%) | 1829..2048 (-16%..-23%) |

- **Tests** (`sim/spec/adaptive_tests.rs`, `sim/spec/tests.rs`): a plan
  that keys nothing brings the superstep (interpreted, native code
  removed and installed again, a ROM write and the `reset` after it,
  `check_batches_with` on the microcoded machine and on every
  `standard_configs` design with default and debug options, `check_pair`
  with pokes and imports); each of the superstep mutations (stores after
  the superstep dropped, a superstep at any phase, values read by a later
  chunk not stored) is caught on a `Sim` whose only superstep is the
  plan's; with the `counters` feature, the plan's superstep counts what
  the program's own counts, batch for batch; `keyed_ticks` under every
  existing P4/P5b test, plus a mutation (inputs never applied) caught by
  `check_batches`. The doc's P8 test plan (side exits at every tick
  offset, a skipped key check) belongs to the keyed supersteps not built.

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
  and the PPU keys rarely within one, so period keys repeat. (Measured
  in P8: they do not. The PPU keys change in three periods of four, and
  period keys hardly repeat; not built, see "P8 as built".)

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
| P5a | per-body JIT units: `jit::compile_unit`, `JitUnit`, freeing (**done**) | new `sim/jit/unit.rs`, `sim/jit/mod.rs` (share emission) | P3 | sonnet |
| P5b | adaptive tables: miss counters, hot detection, `spec_requests`/`build_spec`/`install_spec`, LRU eviction, caps, `spec_stats` (**done**) | `sim/spec.rs`, `sim/mod.rs` | P4, P5a | opus |
| P6 | automatic plan: `spec::choose_plan(contexts, samples, opts) -> SpecPlan` (pure, the report's greedy + `prune_bits`), runtime sampling window (**done**; the window with P5b) | `compile/spec.rs` (pure part), `sim/spec.rs` | P1 (pure part: none) | sonnet |
| P7 | engine integration: compile-thread builds, revision checks, `LocalEngine` inline | `engine/mod.rs`, `engine/tests.rs` | P5b | sonnet |
| P8 | keyed supersteps with side exits; measure against P4's per-tick path (**measured**: not built; the superstep with a plan that keys nothing, and the keyed tick loop, **done**) | `sim/jit/superstep.rs`, `sim/mod.rs` | P4, superstep merged | opus |
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
