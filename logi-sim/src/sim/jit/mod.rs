//! JIT backends for the settle and the commit (issues #117, #111; feature
//! `jit`): Cranelift on native targets (`clif/`), and on wasm32
//! WebAssembly modules generated at run time (`wasm/`).
//!
//! The unit is the one the interpreter runs: a body (the program's own, a
//! clock-phase variant, or a fused-run body pruned to its commits' cone,
//! see `Sim::settle`). [`JitProgram::compile`] turns each body into
//!
//! - one or more *settle chunks*, `fn(slots, mems, tick)`, each a straight
//!   line of native code over a contiguous range of the body's
//!   instructions (packed words included), in the body's order; and
//! - a *commit* function, `fn(slots, mems, mem_writes)`, doing the body's
//!   commits in order (or `None` when a commit kind is not supported, and
//!   the interpreter commits).
//!
//! Inside a chunk every slot is read from memory at most once and every
//! result stays in an SSA value, so intermediates live in registers (or
//! Cranelift's spill slots); literal fields (shifts, masks, widths, clock
//! periods, memory sizes) are immediates. Every result is still stored to
//! its slot, so all nets read the same as with the interpreter, except in
//! a body that only runs between two ticks of one `Sim::run` (a fused-run
//! body that is never a full body): there, only the slots its commits or a
//! later chunk read are stored. Nothing can observe the others (see
//! `Sim::run`), and the next settle writes every slot it reads before
//! reading it.
//!
//! A guarded range of the body (see `compile::guard`) becomes a
//! conditional branch around its code: the conditions are tested in the
//! chunk, and values defined inside the range leave the SSA cache when it
//! ends (later reads load them, so in a fused-run body a result computed
//! inside a range is also stored when the chunk reads it again).
//!
//! Chunks are cut at instruction boundaries (at most [`CHUNK`]
//! instructions), which bounds Cranelift's compile time and the live
//! ranges per function, and makes a chunk the natural unit for running
//! part of a body later (activity skipping could call the chunks of a
//! region only).
//!
//! # Soundness
//!
//! The generated code does unchecked loads and stores. It is only built
//! for a program that passed `Sim`'s `check`, which validates every slot
//! index, memory index, shift and width the code uses; memory reads and
//! writes mask the address to the memory's size (`2^addr_bits` words, the
//! length of `Sim`'s buffers), exactly as the interpreter does.
//!
//! # Backends
//!
//! This module and `superstep.rs` hold what does not depend on the
//! target: the bodies' chunks, the slots each must store, the superstep's
//! stream and its store plan, and the program and unit types `Sim` uses.
//! A backend (`backend::Builder`) turns one chunk, commit chunk, superstep
//! chunk or superstep entry into one function, and makes them callable as
//! the function pointers [`SettleFn`], [`CommitFn`] and [`SuperFn`]:
//!
//! - `clif/` (native): Cranelift IR, compiled to machine code in a
//!   `JITModule`. A slot read is an SSA value.
//! - `wasm/` (wasm32): a WebAssembly module encoded by hand (`wasm/enc.rs`),
//!   that imports the main module's memory, so the code works on the
//!   `Sim`'s slots in place. A slot read is a local. Its functions are
//!   put in the main module's function table, where a Rust function
//!   pointer is a table index: `Sim` calls them as it calls native code,
//!   wasm to wasm, with no JavaScript in between.

use std::sync::Mutex;

#[cfg(not(target_arch = "wasm32"))]
mod clif;
mod superstep;
#[cfg(any(test, feature = "selftest"))]
pub mod tests;
mod tune;
mod unit;
#[cfg(any(test, feature = "selftest"))]
pub mod unit_tests;
#[cfg(target_arch = "wasm32")]
mod wasm;
/// The wasm backend's own tests (`wasm/tests.rs`), for the selftest runner.
#[cfg(all(target_arch = "wasm32", feature = "selftest"))]
pub fn wasm_tests() -> Vec<(&'static str, fn())> {
    wasm::tests::all()
}
// The wasm backend's encoder is plain Rust: its unit tests run natively.
// (Only some of it is used by them.)
#[cfg(all(test, not(target_arch = "wasm32")))]
#[path = "wasm/enc.rs"]
#[allow(dead_code)]
mod wasm_enc;

#[cfg(not(target_arch = "wasm32"))]
use clif as backend;
#[cfg(target_arch = "wasm32")]
use wasm as backend;

#[cfg(any(test, feature = "selftest"))]
pub(crate) use superstep::MUTATION;
pub use superstep::{JitSuper, Part, SuperFn, SuperInfo, SuperRef};
pub use unit::{JitUnit, SuperUnit, compile_superstep, compile_unit};

use foldhash::{HashSet, HashSetExt};

use crate::compile::{Block, Commit, Guard, Instr, Op, Pack, PackOperand, Program};

/// Instructions (or packed words) per settle chunk at most. Small chunks
/// compile faster (register allocation is superlinear in function size)
/// and also run faster: within a chunk every slot is loaded once and kept
/// in an SSA value, and in long functions those long live ranges spill.
/// Measured on `gates1_100k` (23k instructions): chunks of 64 compile in
/// 150 ms and tick in 11.9 µs; 4096, 840 ms and 17.5 µs. On `cpu16`, 64
/// is as fast as 4096 in batches and faster tick by tick. Measured again
/// with the settle entry and contiguous code (`SETTLE_ENTRY`, `ARENA`):
/// `gates1_100k` ticks in 13-14 µs with chunks of 16 to 64, 15 µs with
/// 128 and 18 µs with 256 (Cranelift spills 361 values at 64, 9934 at
/// 128, 22883 at 256), and compiles in 85-100, 110-140 and 160-200 ms;
/// the NES workspaces with spec on (settle chunks are most of a tick)
/// run no faster with 128 or 256 (within ±2 %), and their bodies compile
/// 25 % and 55 % slower.

/// Commits per commit chunk at most.
pub const COMMIT_CHUNK: usize = 256;

/// Bytes each native function is aligned to (Cranelift's own minimum).
/// Aligning to 64 measured the same.
pub const FN_ALIGN: u64 = 1;

/// Whether native code is placed in order in contiguous arenas (see
/// `clif::CodeMemory`) rather than where `cranelift-jit` puts it.
pub const ARENA: bool = true;

/// Whether a body of several settle chunks gets an entry function that
/// calls each chunk directly, so that a settle is one call from `Sim`.
/// Otherwise `Sim` calls the chunks one by one from a single call site:
/// an indirect call with as many targets as chunks, which the branch
/// predictor follows for some dozens of targets but not for hundreds:
/// `gates1_100k` tick by tick with chunks of 16 items (1440 per settle)
/// took 30 µs per tick that way and 12.6 µs with the entry (chunks of 64:
/// 12-14 µs either way). The chunks called by an entry save no
/// callee-saved registers. Off on wasm32 (not measured there).
pub const SETTLE_ENTRY: bool = cfg!(not(target_arch = "wasm32"));

/// Bodies with more instructions (or packed words) than this run
/// interpreted. Native code is about 20 bytes per instruction, so beyond
/// this it no longer fits the instruction caches and runs slower than the
/// interpreter, which streams compact operands instead: `gates1_100k`
/// (23k instructions) ticks 1.5x faster with the JIT, `gates1_1m` (101k)
/// 4.9x slower.
pub const MAX_BODY: usize = 1 << 15;

/// A settle chunk: `(slots, mems, tick)`. `mems` points to one pointer per
/// memory of the program.
pub type SettleFn = unsafe extern "C" fn(*mut u64, *const *mut u64, u64);
/// A commit: `(slots, mems, mem_writes)`; `mem_writes` has one counter per
/// memory, incremented for every write or clear taken.
pub type CommitFn = unsafe extern "C" fn(*mut u64, *const *mut u64, *mut u64);

/// One body to compile (see the module documentation).
pub struct BodyRef<'a> {
    pub blocks: &'a [Block],
    pub instrs: &'a [Instr],
    pub imms: &'a [u64],
    pub commits: &'a [Commit],
    /// Guarded block ranges (see `compile::guard`): a guarded run of
    /// items becomes a conditional branch around its code.
    pub guards: &'a [Guard],
    /// Compile this body (`false` leaves a body no tick runs uncompiled).
    pub used: bool,
    /// The body only ever settles a tick inside a `Sim::run` that is not
    /// the last (a fused-run body): slots that neither its commits nor a
    /// later chunk read need not be stored.
    pub fused_only: bool,
}

/// One compiled body: its settle chunks in order, and its commit chunks
/// in order (`None`: the interpreter commits, for a body with a
/// `MemClear`).
///
/// The function pointers point into the code owned by the
/// [`JitProgram`] that holds this body, so they are private to the crate:
/// safe code outside it can neither read them nor move a body to another
/// program (which could outlive the code it points into).
pub struct JitBody {
    /// What a settle calls in order: the chunks, or the body's entry,
    /// which calls them (see [`SETTLE_ENTRY`]).
    pub(crate) settle: Vec<SettleFn>,
    pub(crate) commit: Option<Vec<CommitFn>>,
    chunks: usize,
}

impl JitBody {
    /// Settle chunks.
    pub fn settle_chunks(&self) -> usize {
        self.chunks
    }

    /// Settle functions called from outside the code per settle: the
    /// chunks, or 1 with an entry.
    pub fn settle_calls(&self) -> usize {
        self.settle.len()
    }

    /// Commit chunks (`None`: the interpreter commits this body).
    pub fn commit_chunks(&self) -> Option<usize> {
        self.commit.as_ref().map(Vec::len)
    }
}

/// Every body of one `Sim`, indexed as `Sim`'s run numbers; `None` for a
/// body that was not compiled (not used, or larger than [`MAX_BODY`]),
/// which the interpreter runs.
///
/// The bodies are private (read them with [`JitProgram::body`]), so that
/// their function pointers cannot be separated from the module that owns
/// their code. Moving bodies between programs does not compile:
///
/// ```compile_fail,E0616
/// use logi_sim::sim::jit::JitProgram;
/// fn steal(a: &mut JitProgram, b: &mut JitProgram) {
///     std::mem::swap(&mut a.bodies, &mut b.bodies);
/// }
/// ```
///
/// while reading them does:
///
/// ```
/// use logi_sim::sim::jit::JitProgram;
/// fn chunks(a: &JitProgram) -> usize {
///     (0..a.len()).filter_map(|i| a.body(i)).map(|b| b.settle_chunks()).sum::<usize>() + a.instrs
/// }
/// ```
pub struct JitProgram {
    pub(crate) bodies: Vec<Option<JitBody>>,
    /// The superstep, if one was asked for and could be compiled.
    pub(crate) superstep: Option<JitSuper>,
    /// Instructions compiled (packed words count one each).
    pub instrs: usize,
    /// Bytes of native code, every function included.
    pub code_bytes: usize,
    /// Which `Sim` layout this was compiled for (set by
    /// `Sim::compile_jit`, checked by `Sim::install_jit`).
    pub(crate) key: Option<RunKey>,
    /// Owns the code (behind a `Mutex` only so that the program is `Sync`;
    /// it is never locked).
    _module: Mutex<backend::Owned>,
}

// A `Sim` with JIT code must stay `Send`, and the code can be compiled on
// another thread and shared (`Sim::install_jit`).
const _: fn() = || {
    fn shareable<T: Send + Sync>() {}
    shareable::<JitProgram>();
};

/// The program and the run layout (`Sim`'s `full`/`fused` tables) a
/// `JitProgram`'s bodies were compiled from. The `Weak` keeps the
/// program's allocation, so a pointer comparison cannot be fooled by a
/// later program at the same address.
pub(crate) struct RunKey {
    pub program: std::sync::Weak<Program>,
    pub full: Vec<u16>,
    pub fused: Vec<u16>,
    /// Whether the fused bodies fold ROM contents (`SimOptions::known_bits`).
    pub folded: bool,
    /// The specialized runs (`Sim::spec_layout`).
    pub spec: Vec<u64>,
}

/// Why a program was not compiled.
#[derive(Debug, Clone)]
pub struct JitError(pub String);

impl JitProgram {
    /// Body `run` (as `Sim`'s run numbers), if it was compiled.
    pub fn body(&self, run: usize) -> Option<&JitBody> {
        self.bodies.get(run)?.as_ref()
    }

    /// Bodies, compiled or not.
    pub fn len(&self) -> usize {
        self.bodies.len()
    }

    /// No bodies at all.
    pub fn is_empty(&self) -> bool {
        self.bodies.is_empty()
    }

    /// Compile `bodies` of `p`, which passed `Sim`'s `check` (so did every
    /// body).
    pub fn compile(p: &Program, bodies: &[BodyRef<'_>]) -> Result<Self, JitError> {
        Self::compile_with(p, bodies, None)
    }

    /// The superstep, if compiled (see `superstep.rs`).
    pub fn superstep(&self) -> Option<&JitSuper> {
        self.superstep.as_ref()
    }

    /// [`JitProgram::compile`], plus a superstep over `bodies` (every body
    /// it names must be compiled as well, without a `MemClear` commit;
    /// otherwise no superstep is built).
    pub fn compile_with(
        p: &Program,
        bodies: &[BodyRef<'_>],
        superstep: Option<&SuperRef<'_>>,
    ) -> Result<Self, JitError> {
        let (b, layout) = define_program(p, bodies, superstep)?;
        Ok(layout.assemble(b.finish()?))
    }
}

#[cfg(target_arch = "wasm32")]
impl JitProgram {
    /// [`JitProgram::compile_with`], instantiated asynchronously (wasm32:
    /// browsers refuse to compile modules over 4 KB synchronously on their
    /// main thread). The code is generated now; poll the result between
    /// frames.
    pub fn compile_async(
        p: &Program,
        bodies: &[BodyRef<'_>],
        superstep: Option<&SuperRef<'_>>,
    ) -> Result<PendingJit, JitError> {
        let (b, layout) = define_program(p, bodies, superstep)?;
        Ok(PendingJit {
            module: b.finish_async()?,
            layout: Some(layout),
            key: None,
        })
    }
}

/// A [`JitProgram`] being instantiated ([`JitProgram::compile_async`],
/// `Sim::compile_jit_async`).
#[cfg(target_arch = "wasm32")]
pub struct PendingJit {
    module: wasm::PendingModule,
    layout: Option<Layout>,
    pub(crate) key: Option<RunKey>,
}

#[cfg(target_arch = "wasm32")]
impl PendingJit {
    /// The program once instantiated, or why it failed; `None` while it is
    /// in flight (and after it was returned once).
    pub fn poll(&mut self) -> Option<Result<JitProgram, JitError>> {
        let fin = self.module.poll()?;
        let layout = self.layout.take()?;
        Some(fin.map(|f| {
            let mut code = layout.assemble(f);
            code.key = self.key.take();
            code
        }))
    }
}

/// What [`define_program`] defined: per body its settle and commit chunks,
/// and the superstep.
struct Layout {
    bodies: Vec<Option<Ids>>,
    superstep: Option<(backend::FnId, SuperInfo)>,
    instrs: usize,
    code_bytes: usize,
}

impl Layout {
    /// The program of the functions defined, now finished.
    fn assemble(self, fin: backend::Finished) -> JitProgram {
        let bodies = self
            .bodies
            .into_iter()
            .map(|b| b.map(|ids| ids.body(&fin)))
            .collect();
        let superstep = self.superstep.map(|(id, info)| JitSuper {
            entry: fin.super_fn(id),
            info,
        });
        JitProgram {
            bodies,
            superstep,
            instrs: self.instrs,
            code_bytes: self.code_bytes,
            key: None,
            _module: Mutex::new(fin.into_owned()),
        }
    }
}

/// Define every body of `bodies` (and the superstep) in a new backend
/// builder, not finished yet.
fn define_program(
    p: &Program,
    bodies: &[BodyRef<'_>],
    superstep: Option<&SuperRef<'_>>,
) -> Result<(backend::Builder, Layout), JitError> {
    let mut b = backend::Builder::new(code_hint(bodies) * if superstep.is_some() { 2 } else { 1 })?;
    let mut ids: Vec<Option<Ids>> = Vec::with_capacity(bodies.len());
    let mut count = 0usize;
    let mut bytes = 0usize;
    for body in bodies {
        ids.push(compile_body(&mut b, p, body, &mut count, &mut bytes)?);
    }
    // The superstep: only over bodies compiled with a native commit.
    let mut sup = None;
    if let Some(s) = superstep {
        let ok = s.parts.iter().all(|part| {
            let (Part::Commit { body } | Part::Settle { body, .. }) = *part;
            matches!(
                ids.get(body),
                Some(Some(Ids {
                    commit: Some(_),
                    ..
                }))
            )
        });
        if ok {
            let (id, info) = define_superstep(&mut b, p, bodies, s)?;
            bytes += info.code_bytes;
            sup = Some((id, info));
        }
    }
    Ok((
        b,
        Layout {
            bodies: ids,
            superstep: sup,
            instrs: count,
            code_bytes: bytes,
        },
    ))
}

/// About how many bytes of native code `bodies` compile to (for the size
/// of the first arena, see `clif::CodeMemory`).
fn code_hint(bodies: &[BodyRef<'_>]) -> usize {
    bodies
        .iter()
        .filter(|b| b.used)
        .map(|b| b.blocks.iter().map(|x| x.len as usize).sum::<usize>() + b.commits.len())
        .sum::<usize>()
        * 32
}

/// Define the chunks and the entry of superstep `s` over `bodies` in `b`
/// (every body it names checked by the caller: compiled, or compilable,
/// with a native commit).
fn define_superstep(
    b: &mut backend::Builder,
    p: &Program,
    bodies: &[BodyRef<'_>],
    s: &SuperRef<'_>,
) -> Result<(backend::FnId, SuperInfo), JitError> {
    let mut super_bytes = 0usize;
    let st = superstep::stream(bodies, s);
    let plan = superstep::plan(p, bodies, &st, s.live_end);
    let mut skip_base = Vec::with_capacity(bodies.len());
    let mut skip_len = 0;
    for b in bodies {
        skip_base.push(skip_len);
        skip_len += b.guards.len();
    }
    let (mut w, mut dead, mut items) = (0usize, 0usize, 0usize);
    let mut fns = Vec::with_capacity(st.chunks.len());
    let mut continues = 0;
    for ch in &st.chunks {
        items += ch.len();
        let (id, size, d) =
            b.super_chunk(p, bodies, &st, ch, &plan, &mut w, &skip_base, continues)?;
        dead += d;
        continues = ch.last().map_or(0, |&(_, seg)| seg);
        super_bytes += size;
        fns.push(id);
    }
    debug_assert_eq!(w, plan.store.len());
    // One entry (`SuperFn`) calling every chunk directly.
    let (id, size) = b.super_entry(&fns)?;
    super_bytes += size;
    Ok((
        id,
        SuperInfo {
            ticks: s.ticks,
            chunks: fns.len(),
            skip_base,
            skip_len,
            items,
            dead,
            writes: plan.store.len(),
            stores: plan.store.iter().filter(|&&x| x).count(),
            code_bytes: super_bytes,
        },
    ))
}

/// The functions of one compiled body: settle chunks, and commit chunks
/// (`None` when a `MemClear` is committed by the interpreter).
struct Ids {
    /// What a settle calls (see [`JitBody`]).
    settle: Vec<backend::FnId>,
    commit: Option<Vec<backend::FnId>>,
    chunks: usize,
}

impl Ids {
    /// The body, with the functions of `fin`.
    fn body(self, fin: &backend::Finished) -> JitBody {
        JitBody {
            settle: self
                .settle
                .into_iter()
                .map(|id| fin.settle_fn(id))
                .collect(),
            commit: self
                .commit
                .map(|c| c.into_iter().map(|id| fin.commit_fn(id)).collect()),
            chunks: self.chunks,
        }
    }
}

/// Compile one body into `b` (`None`: not used, or larger than
/// [`MAX_BODY`]). `count` and `bytes` sum instructions and bytes of code.
fn compile_body(
    b: &mut backend::Builder,
    p: &Program,
    body: &BodyRef<'_>,
    count: &mut usize,
    bytes: &mut usize,
) -> Result<Option<Ids>, JitError> {
    let items: usize = body.blocks.iter().map(|b| b.len as usize).sum();
    if !body.used || items > MAX_BODY {
        return Ok(None);
    }
    let chunks = chunks(body);
    // Slots read by commits and by later chunks: live out of a chunk.
    let mut live: HashSet<u32> = HashSet::new();
    if body.fused_only {
        for c in body.commits {
            live.extend(c.reads());
        }
        // The counters test each guard's conditions after the
        // settle (`Sim::evaluate`), so they must be in the slots.
        #[cfg(feature = "counters")]
        for g in body.guards {
            live.extend(g.conds.iter().map(|c| c.slot));
        }
    }
    let mut lives = vec![HashSet::new(); chunks.len()];
    for (k, ch) in chunks.iter().enumerate().rev() {
        if body.fused_only {
            lives[k] = live.clone();
            for &(item, seg) in ch {
                item.reads(p, body, |s| {
                    live.insert(s);
                });
                if seg > 0 {
                    for c in &body.guards[seg as usize - 1].conds {
                        live.insert(c.slot);
                    }
                }
            }
        }
    }
    let mut settle = Vec::with_capacity(chunks.len());
    let entry = tune::get().settle_entry && chunks.len() > 1;
    for (ch, live) in chunks.iter().zip(&lives) {
        *count += ch.len();
        let (id, size) = b.settle(p, body, ch, body.fused_only.then_some(live), entry)?;
        *bytes += size;
        settle.push(id);
    }
    if entry {
        let (id, size) = b.settle_entry(&settle)?;
        *bytes += size;
        settle = vec![id];
    }
    let commit = if body
        .commits
        .iter()
        .all(|c| !matches!(c, Commit::MemClear { .. }))
    {
        let mut out = Vec::new();
        for part in body.commits.chunks(tune::get().commit_chunk) {
            let (id, size) = b.commit(p, part)?;
            *bytes += size;
            out.push(id);
        }
        Some(out)
    } else {
        None
    };
    Ok(Some(Ids {
        settle,
        commit,
        chunks: chunks.len(),
    }))
}

/// One unit of a chunk: an instruction or a packed word.
#[derive(Clone, Copy)]
enum Item {
    Instr { op: Op, k: u32 },
    Pack { op: Op, k: u32 },
}

impl Item {
    /// Call `f` with every slot this item reads.
    fn reads(&self, p: &Program, body: &BodyRef<'_>, mut f: impl FnMut(u32)) {
        match *self {
            Item::Instr { op, k } => {
                let i = body.instrs[k as usize];
                let (sa, sb, sc) = op.slot_operands();
                for (is, s) in [(sa, i.a), (sb, i.b), (sc, i.c)] {
                    if is {
                        f(s);
                    }
                }
            }
            Item::Pack { op, k } => {
                let pk = &p.packs[k as usize];
                for o in &operands(pk)[..arity(op)] {
                    f(o.base);
                    f(o.splat);
                    let g = o.gather_start as usize;
                    for gt in &p.gathers[g..g + o.gather_len as usize] {
                        f(gt.slot);
                    }
                }
            }
        }
    }
}

/// The body's items in order, cut into chunks of at most [`CHUNK`], each
/// with its guard (`k + 1` for `body.guards[k]`, 0 for none).
fn chunks(body: &BodyRef<'_>) -> Vec<Vec<(Item, u32)>> {
    let mut out: Vec<Vec<(Item, u32)>> = vec![Vec::new()];
    let mut g = 0;
    let chunk = tune::get().settle_chunk;
    for (bi, b) in body.blocks.iter().enumerate() {
        while g < body.guards.len() && body.guards[g].end as usize <= bi {
            g += 1;
        }
        let seg = match body.guards.get(g) {
            Some(x) if x.start as usize <= bi => g as u32 + 1,
            _ => 0,
        };
        for k in b.start..b.start + b.len {
            if out.last().is_some_and(|c| c.len() >= chunk) {
                out.push(Vec::new());
            }
            let item = if b.packed {
                Item::Pack { op: b.op, k }
            } else {
                Item::Instr { op: b.op, k }
            };
            out.last_mut().expect("non-empty").push((item, seg));
        }
    }
    out
}

fn arity(op: Op) -> usize {
    let (a, b, c) = op.slot_operands();
    a as usize + b as usize + c as usize
}

fn operands(pk: &Pack) -> [&PackOperand; 3] {
    [&pk.a, &pk.b, &pk.c]
}
