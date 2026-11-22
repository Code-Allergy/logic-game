//! Runtime for compiled programs (design doc section 6).
//!
//! [`Sim`] implements the execution model documented in
//! [`crate::compile::program`]: commit, advance the tick, apply staged
//! inputs, settle by running the blocks in order. Dispatch happens once per
//! block; each block's inner loop is a pass over a contiguous slice of
//! instructions with one opcode.
//!
//! `tick()` allocates nothing, and the code is the same on native and wasm.
//! Every index in the program is checked once in [`Sim::new`], so the inner
//! loops can skip bounds checks. The program is first lowered into the
//! runtime's own streaming encoding (`code.rs`).
//!
//! Single-threaded (the default), a `Sim` uses no locks or atomics. With
//! [`SimOptions::threads`] > 1 it owns a pool of worker threads that share
//! each settle (`pool.rs`, which documents why that is race-free); results
//! are identical.

mod code;
#[cfg(not(target_arch = "wasm32"))]
mod pool;
/// No threads on wasm. `Pool` is uninhabited, so a `Sim`'s pool is always
/// `None`, every settle takes the single-threaded path, and no thread or
/// synchronization code is compiled in. A threaded `SimOptions` silently
/// runs on one thread.
#[cfg(target_arch = "wasm32")]
mod pool {
    pub(super) enum Pool {}
    pub(super) struct Schedule {
        pub phases: Vec<()>,
    }
    impl Schedule {
        pub fn parallel_phases(&self) -> usize {
            0
        }
    }
    impl Pool {
        pub fn threads(&self) -> usize {
            match *self {}
        }
        pub fn schedule(&self) -> &Schedule {
            match *self {}
        }
        pub fn settle(&self, _: &mut [u64], _: &[Vec<u64>], _: u64) {
            match *self {}
        }
    }
}
#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::sync::Arc;

use crate::compile::{Commit, NodePath, Op, Program, StateLoc, ZERO_SLOT};
use crate::netlist::{NetId, NodeId, mask};

/// Index of a root port, in port order.
pub type PortId = usize;

/// A program index is out of range. Programs from [`crate::compile::compile`]
/// never produce this.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BadProgram(pub String);

/// State of every state element, keyed by its node path, plus the tick count.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StateSnapshot {
    pub tick: u64,
    pub words: HashMap<NodePath, u64>,
    /// RAM contents with their geometry `(addr_bits, width)`.
    pub mems: HashMap<NodePath, (u8, u8, Vec<u64>)>,
}

/// Runtime options (issue #41). Every field only changes speed, never
/// results. [`SimOptions::default`] is single-threaded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SimOptions {
    /// Re-encode blocks whose destinations are consecutive so they stream
    /// only their operands (see `sim/code.rs`). Off runs every block from
    /// the program's own instructions.
    pub stream: bool,
    /// Threads per settle, counting the caller (1 = single-threaded). The
    /// [`Sim`] starts `threads - 1` workers and keeps them for its
    /// lifetime; it starts none if no rank reaches `par_min`, and none on
    /// wasm.
    pub threads: usize,
    /// Work (about one unit per instruction) a rank needs to be split
    /// between threads; smaller ranks run on the calling thread.
    pub par_min: u64,
}

/// Default [`SimOptions::par_min`] (see the design doc, section 13).
pub const DEFAULT_PAR_MIN: u64 = 2048;

impl Default for SimOptions {
    fn default() -> Self {
        Self {
            stream: true,
            threads: 1,
            par_min: DEFAULT_PAR_MIN,
        }
    }
}

impl SimOptions {
    /// Everything off: the M3 runtime.
    pub fn baseline() -> Self {
        Self {
            stream: false,
            ..Self::default()
        }
    }

    /// The defaults with `threads` threads.
    pub fn threaded(threads: usize) -> Self {
        Self {
            threads,
            ..Self::default()
        }
    }

    /// The defaults with one thread per available core (1 on wasm).
    pub fn all_cores() -> Self {
        let n = if cfg!(target_arch = "wasm32") {
            1
        } else {
            std::thread::available_parallelism().map_or(1, |n| n.get())
        };
        Self::threaded(n)
    }
}

/// Work done by a [`Sim`] since it was created or [`Sim::reset_counters`]
/// (feature `counters`, off by default; without it nothing is counted and
/// the runtime is unchanged). Every count is deterministic: the same
/// program and the same calls give the same counts on every machine, which
/// makes them suitable for regression gates (`cargo xtask perf check`).
#[cfg(feature = "counters")]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counters {
    /// Calls to [`Sim::tick`].
    pub ticks: u64,
    /// Settles (one per tick, plus construction, [`Sim::reset`],
    /// [`Sim::import_state`] and [`Sim::resettle`]).
    pub settles: u64,
    /// Blocks dispatched.
    pub blocks: u64,
    /// Scalar instructions executed.
    pub instrs: u64,
    /// Packed words executed (each up to 64 lanes).
    pub packed_words: u64,
    /// Commits processed (state copies, RAM writes and clears, taken or
    /// not).
    pub commits: u64,
    /// Memory writes and clears actually taken.
    pub mem_writes: u64,
}

#[cfg(feature = "counters")]
impl Counters {
    /// Executed operations: instructions, packed words and commits.
    pub fn ops(&self) -> u64 {
        self.instrs + self.packed_words + self.commits
    }
}

pub struct Sim {
    program: Arc<Program>,
    code: Arc<code::Code>,
    options: SimOptions,
    /// Worker threads, when `options.threads > 1` and some rank is large
    /// enough. Declared before `slots` and `mems`, so it is dropped (and
    /// its workers joined) before the buffers they may still be using are
    /// freed, even when unwinding out of a settle.
    pool: Option<pool::Pool>,
    slots: Vec<u64>,
    mems: Vec<Vec<u64>>,
    /// Per memory, a count of the commits that wrote it (see
    /// [`Sim::mem_version`]).
    mem_writes: Vec<u64>,
    tick: u64,
    /// Root input values, applied to their slots at the start of each settle.
    inputs: Vec<u64>,
    #[cfg(feature = "counters")]
    counters: Counters,
    /// What one settle executes (`blocks`, `instrs`, `packed_words`).
    #[cfg(feature = "counters")]
    per_settle: Counters,
}

impl Sim {
    /// Load the program and perform the tick-0 settle. Panics if the program
    /// is malformed; see [`Sim::try_new`].
    #[track_caller]
    pub fn new(program: Arc<Program>) -> Self {
        Self::try_new(program).unwrap_or_else(|e| panic!("bad program: {}", e.0))
    }

    pub fn try_new(program: Arc<Program>) -> Result<Self, BadProgram> {
        Self::try_with_options(program, SimOptions::default())
    }

    /// [`Sim::new`] with runtime options.
    #[track_caller]
    pub fn with_options(program: Arc<Program>, options: SimOptions) -> Self {
        Self::try_with_options(program, options).unwrap_or_else(|e| panic!("bad program: {}", e.0))
    }

    /// [`Sim::try_new`] with runtime options.
    pub fn try_with_options(
        program: Arc<Program>,
        options: SimOptions,
    ) -> Result<Self, BadProgram> {
        check(&program)?;
        let code = if options.stream {
            code::Code::new(&program)
        } else {
            code::Code::plain(&program)
        };
        let mems = program
            .mems
            .iter()
            .map(|m| {
                let mut v = vec![0u64; 1usize << m.addr_bits];
                v[..m.init.len()].copy_from_slice(&m.init);
                v
            })
            .collect();
        let code = Arc::new(code);
        let pool = start_pool(&program, &code, &options);
        let mut sim = Sim {
            code,
            options,
            pool,
            slots: vec![0; program.slot_count as usize],
            mem_writes: vec![0; program.mems.len()],
            mems,
            tick: 0,
            inputs: vec![0; program.ports.inputs.len()],
            program,
            #[cfg(feature = "counters")]
            counters: Counters::default(),
            #[cfg(feature = "counters")]
            per_settle: Counters::default(),
        };
        #[cfg(feature = "counters")]
        for b in &sim.code.blocks {
            sim.per_settle.blocks += 1;
            match b.form {
                code::Form::Packed { .. } => sim.per_settle.packed_words += u64::from(b.len),
                _ => sim.per_settle.instrs += u64::from(b.len),
            }
        }
        for &(s, v) in &sim.program.consts {
            sim.slots[s as usize] = v;
        }
        sim.settle();
        Ok(sim)
    }

    pub fn program(&self) -> &Arc<Program> {
        &self.program
    }

    pub fn options(&self) -> &SimOptions {
        &self.options
    }

    /// Threads each settle uses (1 when no worker threads were started).
    pub fn threads(&self) -> usize {
        self.pool.as_ref().map_or(1, pool::Pool::threads)
    }

    /// `(parallel, total)` settle phases (diagnostics; `(0, 0)` when
    /// single-threaded).
    pub fn phases(&self) -> (usize, usize) {
        self.pool.as_ref().map_or((0, 0), |p| {
            (p.schedule().parallel_phases(), p.schedule().phases.len())
        })
    }

    /// Set a root input. It is visible to logic at the next settle (the next
    /// [`Self::tick`]) and to state at the commit after that.
    pub fn set_input(&mut self, port: PortId, value: u64) {
        self.inputs[port] = value & mask(self.program.ports.inputs[port].width);
    }

    /// Set a root input by name. Panics if there is no such port.
    #[track_caller]
    pub fn set(&mut self, name: &str, value: u64) {
        let port = self
            .program
            .ports
            .input(name)
            .unwrap_or_else(|| panic!("no input port {name:?}"));
        self.set_input(port, value);
    }

    /// Value of root output `port` after the last settle.
    pub fn output(&self, port: PortId) -> u64 {
        self.slots[self.program.ports.outputs[port].slot as usize]
    }

    /// Root output by name. Panics if there is no such port.
    #[track_caller]
    pub fn get(&self, name: &str) -> u64 {
        let port = self
            .program
            .ports
            .output(name)
            .unwrap_or_else(|| panic!("no output port {name:?}"));
        self.output(port)
    }

    /// Raw slot value (see [`crate::compile::DebugMap`]). A lane or view
    /// slot (at or above `slot_count`, see [`Program::resolve`]) reads its
    /// bits of the word holding it. Panics if `slot` is none of these.
    pub fn read(&self, slot: u32) -> u64 {
        match self.program.resolve(slot) {
            Some((s, shift, m)) => (self.slots[s as usize] >> shift) & m,
            None => match self.program.shadow(slot) {
                Some((shadow, value)) => self.read_shadow(shadow, value),
                None => panic!("slot {slot} out of range"),
            },
        }
    }

    /// Value `value` of a substituted instance's gate-level evaluation
    /// (see [`crate::compile::Shadow`]), from its inputs and state now.
    fn read_shadow(&self, shadow: &crate::compile::Shadow, value: u32) -> u64 {
        let ins: smallvec::SmallVec<[u64; 8]> =
            shadow.inputs.iter().map(|&s| self.read(s)).collect();
        let state: smallvec::SmallVec<[u64; 8]> =
            shadow.states.iter().map(|&s| self.read(s)).collect();
        let mut vals = vec![0u64; shadow.flat.values()];
        shadow.flat.eval(&ins, &state, &mut vals);
        vals[value as usize]
    }

    /// Write `value` into `slot` (a plain, lane or view slot), leaving the
    /// other bits of the word holding it alone.
    fn write_slot(&mut self, slot: u32, value: u64) {
        let (s, shift, m) = self
            .program
            .resolve(slot)
            .unwrap_or_else(|| panic!("slot {slot} out of range"));
        let w = &mut self.slots[s as usize];
        *w = (*w & !(m << shift)) | ((value & m) << shift);
    }

    /// Value of an authored net, by instance path and `NetId`.
    pub fn read_net(&self, path: &[NodeId], net: NetId) -> Option<u64> {
        self.program.debug.slot(path, net).map(|s| self.read(s))
    }

    /// Contents of memory `index` (see [`crate::compile::MemDesc`]).
    pub fn mem(&self, index: u32) -> &[u64] {
        &self.mems[index as usize]
    }

    /// Changes whenever memory `index` may have changed: a count of the
    /// writes (and clears) committed to it, plus state imports. Lets a
    /// reader that copies a memory (a screen) skip unchanged ones.
    pub fn mem_version(&self, index: u32) -> u64 {
        self.mem_writes[index as usize]
    }

    /// Memory index of the RAM or screen at `path` (its `StateKey`).
    pub fn mem_index(&self, path: &NodePath) -> Option<u32> {
        self.program.state_keys.iter().find_map(|k| match k.loc {
            StateLoc::Mem(m) if k.path == *path => Some(m),
            _ => None,
        })
    }

    pub fn tick_count(&self) -> u64 {
        self.tick
    }

    /// Work done so far (feature `counters`).
    #[cfg(feature = "counters")]
    pub fn counters(&self) -> &Counters {
        &self.counters
    }

    /// Zero the [`Counters`] (feature `counters`).
    #[cfg(feature = "counters")]
    pub fn reset_counters(&mut self) {
        self.counters = Counters::default();
    }

    pub fn tick(&mut self) {
        #[cfg(feature = "counters")]
        {
            self.counters.ticks += 1;
        }
        self.commit();
        self.tick += 1;
        self.settle();
    }

    pub fn run(&mut self, ticks: u64) {
        for _ in 0..ticks {
            self.tick();
        }
    }

    fn commit(&mut self) {
        let p = &*self.program;
        #[cfg(feature = "counters")]
        {
            self.counters.commits += p.commits.len() as u64;
        }
        for c in &p.commits {
            match *c {
                Commit::Copy { state, next } => {
                    // SAFETY: indices checked in `check`.
                    unsafe {
                        let v = *self.slots.get_unchecked(next as usize);
                        *self.slots.get_unchecked_mut(state as usize) = v;
                    }
                }
                Commit::RamWrite {
                    mem,
                    we,
                    addr,
                    data,
                } => unsafe {
                    if *self.slots.get_unchecked(we as usize) != 0 {
                        let m = self.mems.get_unchecked_mut(mem as usize);
                        let a = *self.slots.get_unchecked(addr as usize) as usize & (m.len() - 1);
                        *m.get_unchecked_mut(a) = *self.slots.get_unchecked(data as usize);
                        *self.mem_writes.get_unchecked_mut(mem as usize) += 1;
                        #[cfg(feature = "counters")]
                        {
                            self.counters.mem_writes += 1;
                        }
                    }
                },
                Commit::MemClear { mem, en } => unsafe {
                    if *self.slots.get_unchecked(en as usize) != 0 {
                        self.mems.get_unchecked_mut(mem as usize).fill(0);
                        *self.mem_writes.get_unchecked_mut(mem as usize) += 1;
                        #[cfg(feature = "counters")]
                        {
                            self.counters.mem_writes += 1;
                        }
                    }
                },
                Commit::RamWriteMasked {
                    mem,
                    mask,
                    addr,
                    data,
                } => unsafe {
                    let bits = *self.slots.get_unchecked(mask as usize);
                    if bits != 0 {
                        let m = self.mems.get_unchecked_mut(mem as usize);
                        let a = *self.slots.get_unchecked(addr as usize) as usize & (m.len() - 1);
                        let word = m.get_unchecked_mut(a);
                        *word = (*word & !bits) | (*self.slots.get_unchecked(data as usize) & bits);
                        *self.mem_writes.get_unchecked_mut(mem as usize) += 1;
                        #[cfg(feature = "counters")]
                        {
                            self.counters.mem_writes += 1;
                        }
                    }
                },
            }
        }
    }

    /// Apply the staged root inputs to their slots, then evaluate.
    fn settle(&mut self) {
        let p = &*self.program;
        for (port, &v) in p.ports.inputs.iter().zip(&self.inputs) {
            self.slots[port.slot as usize] = v;
        }
        self.evaluate();
    }

    /// Run the blocks once, from the input slots as they are.
    fn evaluate(&mut self) {
        #[cfg(feature = "counters")]
        {
            let c = &mut self.counters;
            let per = &self.per_settle;
            c.settles += 1;
            c.blocks += per.blocks;
            c.instrs += per.instrs;
            c.packed_words += per.packed_words;
        }
        let p = &*self.program;
        if let Some(pool) = &self.pool {
            pool.settle(&mut self.slots, &self.mems, self.tick);
            debug_assert_eq!(self.slots[ZERO_SLOT as usize], 0);
            return;
        }
        let s = self.slots.as_mut_ptr();
        let env = code::Env {
            program: p,
            code: &self.code,
            mems: &self.mems,
            tick: self.tick,
        };
        for b in &self.code.blocks {
            // SAFETY: the program passed `check`, `code` was built from it,
            // and `slots` has `slot_count` entries. Nothing else touches the
            // slots meanwhile.
            unsafe { code::exec(&env, b, 0, b.len, s) };
        }
        debug_assert_eq!(self.slots[ZERO_SLOT as usize], 0);
    }

    /// Return to power-on: every state element (registers, latches,
    /// delays, RAM, screens) back to its initial value, ROM and RAM `init`
    /// contents restored, tick 0, then the tick-0 settle. Root inputs keep
    /// their current values, so that settle sees them.
    pub fn reset(&mut self) {
        self.slots.fill(0);
        for &(s, v) in &self.program.consts {
            self.slots[s as usize] = v;
        }
        for (mem, d) in self.mems.iter_mut().zip(&self.program.mems) {
            mem.fill(0);
            mem[..d.init.len()].copy_from_slice(&d.init);
        }
        // Every memory may have changed (see `mem_version`).
        for v in &mut self.mem_writes {
            *v += 1;
        }
        self.tick = 0;
        self.settle();
    }

    // ── State carry-over ──────────────────────────────────────────────────

    /// Snapshot every state element, keyed by node path.
    pub fn export_state(&self) -> StateSnapshot {
        let mut snap = StateSnapshot {
            tick: self.tick,
            ..Default::default()
        };
        for key in &self.program.state_keys {
            match key.loc {
                StateLoc::Slot(s) => {
                    snap.words.insert(key.path.clone(), self.read(s));
                }
                StateLoc::Mem(m) => {
                    let d = &self.program.mems[m as usize];
                    snap.mems.insert(
                        key.path.clone(),
                        (d.addr_bits, d.width, self.mems[m as usize].clone()),
                    );
                }
            }
        }
        snap
    }

    /// Restore state from a snapshot (typically taken from a previous
    /// program), then re-settle so every net is consistent. Elements whose
    /// path is not in the snapshot, or whose RAM geometry changed, reset.
    /// Word values are masked to the current width. The tick count is
    /// restored.
    pub fn import_state(&mut self, snap: &StateSnapshot) {
        let program = Arc::clone(&self.program);
        for key in &program.state_keys {
            match key.loc {
                StateLoc::Slot(s) => {
                    let v = snap
                        .words
                        .get(&key.path)
                        .map_or(0, |&v| v & mask(key.width));
                    self.write_slot(s, v);
                }
                StateLoc::Mem(m) => {
                    let d = &program.mems[m as usize];
                    self.mem_writes[m as usize] += 1;
                    let mem = &mut self.mems[m as usize];
                    match snap.mems.get(&key.path) {
                        Some((ab, w, data)) if *ab == d.addr_bits && *w == d.width => {
                            mem.copy_from_slice(data);
                        }
                        _ => {
                            mem.fill(0);
                            mem[..d.init.len()].copy_from_slice(&d.init);
                        }
                    }
                }
            }
        }
        self.tick = snap.tick;
        self.settle();
    }

    // ── Writing single state elements ─────────────────────────────────────
    //
    // For harnesses that set a few registers and memory words before each
    // of many cases: each write costs one slot or word, not a copy of the
    // state. Write what is needed, then call `resettle` once.

    /// Value of state element `key` (an index into
    /// [`Program::state_keys`]); 0 for a memory (read it with [`Sim::mem`]).
    pub fn state(&self, key: usize) -> u64 {
        match self.program.state_keys[key].loc {
            StateLoc::Slot(s) => self.read(s),
            StateLoc::Mem(_) => 0,
        }
    }

    /// Set state element `key` (an index into [`Program::state_keys`]) to
    /// `value`, masked to its width. Nothing is evaluated: nets keep their
    /// values until [`Sim::resettle`] or the next tick. Panics if `key` is
    /// a memory (use [`Sim::set_mem_word`]).
    pub fn set_state(&mut self, key: usize, value: u64) {
        let k = &self.program.state_keys[key];
        let StateLoc::Slot(slot) = k.loc else {
            panic!("state key {key} is a memory; set its words with set_mem_word");
        };
        let value = value & mask(k.width);
        self.write_slot(slot, value);
    }

    /// Set word `addr` of memory `index` (see [`crate::compile::MemDesc`])
    /// to `value`, masked to the memory's width. Nothing is evaluated until
    /// [`Sim::resettle`] or the next tick. A ROM's word changes for this
    /// simulation until [`Sim::reset`]. Panics if `addr` is past the end.
    pub fn set_mem_word(&mut self, index: u32, addr: usize, value: u64) {
        let width = self.program.mems[index as usize].width;
        self.mems[index as usize][addr] = value & mask(width);
        self.mem_writes[index as usize] += 1;
    }

    /// Evaluate every net again from the current state, at the current
    /// tick, without a commit: after [`Sim::set_state`] or
    /// [`Sim::set_mem_word`], every net is consistent with the new state
    /// and the next tick commits from it. Root inputs set since the last
    /// tick are not applied: they still reach the logic at the next tick,
    /// as they would have without this call.
    pub fn resettle(&mut self) {
        self.evaluate();
    }
}

/// Worker threads for `options`, or `None`: always with one thread (the
/// default, so nothing is spawned and the settle never touches pool or
/// synchronization code), when no rank is large enough, and on wasm.
#[cfg(not(target_arch = "wasm32"))]
fn start_pool(
    program: &Arc<Program>,
    code: &Arc<code::Code>,
    options: &SimOptions,
) -> Option<pool::Pool> {
    if options.threads <= 1 {
        return None;
    }
    let sched = pool::Schedule::new(program, code, options.threads, options.par_min);
    pool::Pool::new(
        Arc::clone(program),
        Arc::clone(code),
        sched,
        options.threads,
    )
}

#[cfg(target_arch = "wasm32")]
fn start_pool(_: &Arc<Program>, _: &Arc<code::Code>, _: &SimOptions) -> Option<pool::Pool> {
    None
}

/// Check every index the unchecked loops rely on.
fn check(p: &Program) -> Result<(), BadProgram> {
    let n = p.slot_count;
    let bad = |m: String| Err(BadProgram(m));
    if n == 0 {
        return bad("no zero slot".into());
    }
    let slot_ok = |s: u32| s < n;
    if p.imms.len() != p.instrs.len() {
        return bad("imms and instrs differ in length".into());
    }
    for pk in &p.packs {
        if !slot_ok(pk.dst) || pk.dst == ZERO_SLOT {
            return bad(format!("bad packed dst in {pk:?}"));
        }
        for o in [&pk.a, &pk.b, &pk.c] {
            let end = o.gather_start as u64 + o.gather_len as u64;
            if !slot_ok(o.base) || !slot_ok(o.splat) || o.splat_bit >= 64 {
                return bad(format!("bad packed operand in {pk:?}"));
            }
            if end > p.gathers.len() as u64 {
                return bad(format!("gathers out of range in {pk:?}"));
            }
        }
        let end = pk.scatter_start as u64 + pk.scatter_len as u64;
        if end > p.scatters.len() as u64 {
            return bad(format!("scatters out of range in {pk:?}"));
        }
    }
    for g in &p.gathers {
        if !slot_ok(g.slot) || g.bit >= 64 || g.lane >= 64 {
            return bad(format!("bad gather {g:?}"));
        }
    }
    for x in &p.scatters {
        if !slot_ok(x.slot) || x.slot == ZERO_SLOT || x.lane >= 64 {
            return bad(format!("bad scatter {x:?}"));
        }
    }
    if !p.block_parts.is_empty() && p.block_parts.len() != p.blocks.len() {
        return bad("block_parts and blocks differ in length".into());
    }
    for &w in &p.lane_words {
        if !slot_ok(w) {
            return bad(format!("bad lane word {w}"));
        }
    }
    if p.slot_count as u64 + 64 * p.lane_words.len() as u64 > u32::MAX as u64 + 1 {
        return bad("too many lane slots".into());
    }
    for b in &p.blocks {
        let end = b.start as u64 + b.len as u64;
        if b.packed {
            if end > p.packs.len() as u64 || !code::packable_op(b.op) {
                return bad(format!("packed block {b:?} out of range"));
            }
            continue;
        }
        if end > p.instrs.len() as u64 {
            return bad(format!("block {b:?} out of range"));
        }
        let (sa, sb, sc) = b.op.slot_operands();
        for (k, i) in p.instrs[b.start as usize..end as usize].iter().enumerate() {
            let imm = p.imms[b.start as usize + k];
            if !slot_ok(i.dst) || i.dst == ZERO_SLOT {
                return bad(format!("bad dst in {i:?}"));
            }
            if (sa && !slot_ok(i.a)) || (sb && !slot_ok(i.b)) || (sc && !slot_ok(i.c)) {
                return bad(format!("bad operand in {i:?}"));
            }
            match b.op {
                Op::MemRead if i.b as usize >= p.mems.len() => {
                    return bad(format!("bad memory in {i:?}"));
                }
                Op::Clock if i.a == 0 => return bad(format!("zero clock period in {i:?}")),
                Op::Sext if !(1..=64).contains(&i.b) => return bad(format!("bad sext in {i:?}")),
                Op::Shr if i.b >= 64 => return bad(format!("bad shift in {i:?}")),
                Op::Concat if i.c >= 64 => return bad(format!("bad shift in {i:?}")),
                Op::AddCarry if imm > 64 => return bad(format!("bad width in {i:?}")),
                _ => {}
            }
        }
    }
    for c in &p.commits {
        let ok = match *c {
            Commit::Copy { state, next } => slot_ok(state) && slot_ok(next) && state != ZERO_SLOT,
            Commit::RamWrite { mem, .. } | Commit::RamWriteMasked { mem, .. } => {
                (mem as usize) < p.mems.len()
                    && p.mems[mem as usize].writable
                    && c.reads().into_iter().all(slot_ok)
            }
            Commit::MemClear { mem, en } => {
                (mem as usize) < p.mems.len() && p.mems[mem as usize].writable && slot_ok(en)
            }
        };
        if !ok {
            return bad(format!("bad commit {c:?}"));
        }
    }
    for m in &p.mems {
        if m.addr_bits > crate::netlist::MAX_ADDR_BITS || m.init.len() > 1usize << m.addr_bits {
            return bad(format!("bad memory {m:?}"));
        }
    }
    for &(s, _) in &p.consts {
        if !slot_ok(s) || s == ZERO_SLOT {
            return bad(format!("bad const slot {s}"));
        }
    }
    for port in &p.ports.inputs {
        if !slot_ok(port.slot) || port.slot == ZERO_SLOT {
            return bad(format!("bad port {port:?}"));
        }
    }
    for port in &p.ports.outputs {
        if !slot_ok(port.slot) {
            return bad(format!("bad port {port:?}"));
        }
    }
    for v in &p.views {
        if !slot_ok(v.slot) || v.width == 0 || v.shift as u32 + v.width as u32 > 64 {
            return bad(format!("bad view {v:?}"));
        }
    }
    for sh in &p.shadows {
        let real = |s: u32| p.resolve(s).is_some_and(|(s, _, _)| slot_ok(s));
        if !sh.inputs.iter().chain(&sh.states).all(|&s| real(s))
            || sh.inputs.len() != sh.flat.inputs.len()
            || sh.states.len() != sh.flat.states.len()
        {
            return bad("bad shadow".to_string());
        }
    }
    for &(s, v) in &p.shadow_nets {
        if p.shadows
            .get(s as usize)
            .is_none_or(|sh| v as usize >= sh.flat.values())
        {
            return bad(format!("bad shadow net {s}/{v}"));
        }
    }
    for key in &p.state_keys {
        let ok = match key.loc {
            StateLoc::Slot(s) => p.resolve(s).is_some_and(|(s, _, _)| slot_ok(s)),
            StateLoc::Mem(m) => (m as usize) < p.mems.len(),
        };
        if !ok {
            return bad(format!("bad state key {key:?}"));
        }
    }
    Ok(())
}
