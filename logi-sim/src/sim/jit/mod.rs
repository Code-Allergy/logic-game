//! Cranelift JIT backend for the settle and the commit (issues #117, #111;
//! feature `jit`, native only).
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

use std::sync::Mutex;

mod superstep;
#[cfg(test)]
mod tests;

#[cfg(test)]
pub(crate) use superstep::MUTATION;
pub use superstep::{JitSuper, Part, SuperFn, SuperInfo, SuperRef};

use cranelift_codegen::ir::condcodes::IntCC;
use cranelift_codegen::ir::types::I64;
use cranelift_codegen::ir::{AbiParam, Function, InstBuilder, MemFlagsData, UserFuncName, Value};
use cranelift_codegen::isa::TargetFrontendConfig;
use cranelift_codegen::settings::{self, Configurable};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{FuncId, Linkage, Module};
use foldhash::{HashMap, HashMapExt, HashSet, HashSetExt};

use crate::compile::{Block, Commit, Guard, Instr, Op, Pack, PackOperand, Program, ZERO_SLOT};
use crate::netlist::mask;

/// Instructions (or packed words) per settle chunk at most. Small chunks
/// compile faster (register allocation is superlinear in function size)
/// and also run faster: within a chunk every slot is loaded once and kept
/// in an SSA value, and in long functions those long live ranges spill.
/// Measured on `gates1_100k` (23k instructions): chunks of 64 compile in
/// 150 ms and tick in 11.9 µs; 4096, 840 ms and 17.5 µs. On `cpu16`, 64
/// is as fast as 4096 in batches and faster tick by tick.
pub const CHUNK: usize = 64;

/// Commits per commit chunk at most.
pub const COMMIT_CHUNK: usize = 256;

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
    pub(crate) settle: Vec<SettleFn>,
    pub(crate) commit: Option<Vec<CommitFn>>,
}

impl JitBody {
    /// Settle chunks.
    pub fn settle_chunks(&self) -> usize {
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
    _module: Mutex<Owned>,
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
    pub full: Vec<u8>,
    pub fused: Vec<u8>,
}

/// Owns a `JITModule` and frees its code when dropped (a `JITModule` alone
/// leaks it), including when compiling fails half-way.
struct Owned(Option<JITModule>);

impl Drop for Owned {
    fn drop(&mut self) {
        if let Some(m) = self.0.take() {
            // SAFETY: the only pointers into this code are the function
            // pointers of the `JitProgram` that owns this (dropped with
            // it; `Sim` keeps an `Arc<JitProgram>` while it may call them),
            // or none at all if compiling failed.
            unsafe { m.free_memory() };
        }
    }
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
        let err = |e: &dyn std::fmt::Display| JitError(e.to_string());
        let mut flags = settings::builder();
        // The IR is already simplified (constant folding, CSE and DCE ran in
        // the compiler); Cranelift's optimizer gains nothing measurable on
        // it and costs about a quarter of the compile time.
        flags.set("opt_level", "none").map_err(|e| err(&e))?;
        // The IR verifier costs about a tenth of the compile time; the
        // builder's IR is checked in debug builds (and so in tests).
        flags
            .set(
                "enable_verifier",
                if cfg!(debug_assertions) {
                    "true"
                } else {
                    "false"
                },
            )
            .map_err(|e| err(&e))?;
        flags
            .set("use_colocated_libcalls", "false")
            .map_err(|e| err(&e))?;
        flags
            .set(
                "is_pic",
                if cfg!(target_arch = "x86_64") {
                    "true"
                } else {
                    "false"
                },
            )
            .map_err(|e| err(&e))?;
        let isa = cranelift_native::builder()
            .map_err(|e| err(&e))?
            .finish(settings::Flags::new(flags))
            .map_err(|e| err(&e))?;
        let mut owned = Owned(Some(JITModule::new(JITBuilder::with_isa(
            isa,
            cranelift_module::default_libcall_names(),
        ))));
        let module = owned.0.as_mut().expect("just created");
        let ptr = module.target_config().pointer_type();
        let mut ctx = module.make_context();
        let mut fctx = FunctionBuilderContext::new();
        type Ids = (Vec<FuncId>, Option<Vec<FuncId>>);
        let mut ids: Vec<Option<Ids>> = Vec::with_capacity(bodies.len());
        let mut count = 0usize;
        let mut n = 0u32;
        let (mut bytes, mut super_bytes) = (0usize, 0usize);
        for body in bodies {
            let items: usize = body.blocks.iter().map(|b| b.len as usize).sum();
            if !body.used || items > MAX_BODY {
                ids.push(None);
                continue;
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
            for (ch, live) in chunks.iter().zip(&lives) {
                count += ch.len();
                ctx.func.signature.params.clear();
                ctx.func.signature.params.extend([
                    AbiParam::new(ptr),
                    AbiParam::new(ptr),
                    AbiParam::new(I64),
                ]);
                ctx.func.name = UserFuncName::user(0, n);
                build_settle(
                    module.target_config(),
                    &mut ctx.func,
                    &mut fctx,
                    p,
                    body,
                    ch,
                    body.fused_only.then_some(live),
                );
                let id = module
                    .declare_function(&format!("settle{n}"), Linkage::Local, &ctx.func.signature)
                    .map_err(|e| err(&e))?;
                n += 1;
                module.define_function(id, &mut ctx).map_err(|e| err(&e))?;
                bytes += code_size(&ctx);
                module.clear_context(&mut ctx);
                settle.push(id);
            }
            let commit = if body
                .commits
                .iter()
                .all(|c| !matches!(c, Commit::MemClear { .. }))
            {
                let mut out = Vec::new();
                for part in body.commits.chunks(COMMIT_CHUNK) {
                    ctx.func.signature.params.clear();
                    ctx.func.signature.params.extend([
                        AbiParam::new(ptr),
                        AbiParam::new(ptr),
                        AbiParam::new(ptr),
                    ]);
                    ctx.func.name = UserFuncName::user(0, n);
                    build_commit(module.target_config(), &mut ctx.func, &mut fctx, p, part);
                    let id = module
                        .declare_function(
                            &format!("commit{n}"),
                            Linkage::Local,
                            &ctx.func.signature,
                        )
                        .map_err(|e| err(&e))?;
                    n += 1;
                    module.define_function(id, &mut ctx).map_err(|e| err(&e))?;
                    bytes += code_size(&ctx);
                    module.clear_context(&mut ctx);
                    out.push(id);
                }
                Some(out)
            } else {
                None
            };
            ids.push(Some((settle, commit)));
        }
        // The superstep: only over bodies compiled with a native commit.
        let mut sup = None;
        if let Some(s) = superstep {
            let ok = s.parts.iter().all(|part| {
                let (Part::Commit { body } | Part::Settle { body, .. }) = *part;
                matches!(ids.get(body), Some(Some((_, Some(_)))))
            });
            if ok {
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
                    // No callee-saved registers: the chunks are only called
                    // by the entry below, which keeps nothing in registers.
                    ctx.func.signature.call_conv = cranelift_codegen::isa::CallConv::Winch;
                    ctx.func.signature.params.clear();
                    ctx.func.signature.params.extend([
                        AbiParam::new(ptr),
                        AbiParam::new(ptr),
                        AbiParam::new(I64),
                        AbiParam::new(ptr),
                        AbiParam::new(ptr),
                    ]);
                    ctx.func.name = UserFuncName::user(0, n);
                    dead += superstep::build_chunk(
                        module.target_config(),
                        &mut ctx.func,
                        &mut fctx,
                        p,
                        bodies,
                        &st,
                        ch,
                        &plan,
                        &mut w,
                        &skip_base,
                        continues,
                    );
                    continues = ch.last().map_or(0, |&(_, seg)| seg);
                    let id = module
                        .declare_function(&format!("super{n}"), Linkage::Local, &ctx.func.signature)
                        .map_err(|e| err(&e))?;
                    n += 1;
                    module.define_function(id, &mut ctx).map_err(|e| err(&e))?;
                    let size = code_size(&ctx);
                    bytes += size;
                    super_bytes += size;
                    module.clear_context(&mut ctx);
                    fns.push(id);
                }
                debug_assert_eq!(w, plan.store.len());
                {
                    // One entry (`SuperFn`) calling every chunk directly.
                    let params = [
                        AbiParam::new(ptr),
                        AbiParam::new(ptr),
                        AbiParam::new(I64),
                        AbiParam::new(ptr),
                        AbiParam::new(ptr),
                    ];
                    ctx.func.signature.params.extend(params);
                    ctx.func.name = UserFuncName::user(0, n);
                    let refs: Vec<_> = fns
                        .iter()
                        .map(|&id| module.declare_func_in_func(id, &mut ctx.func))
                        .collect();
                    {
                        let cfg = module.target_config();
                        let mut b = FunctionBuilder::new(&mut ctx.func, &mut fctx);
                        let blk = b.create_block();
                        b.append_block_params_for_function_params(blk);
                        b.switch_to_block(blk);
                        b.seal_block(blk);
                        let args = b.block_params(blk).to_vec();
                        for r in refs {
                            b.ins().call(r, &args);
                        }
                        b.ins().return_(&[]);
                        b.finalize(cfg);
                    }
                    let id = module
                        .declare_function(&format!("super{n}"), Linkage::Local, &ctx.func.signature)
                        .map_err(|e| err(&e))?;
                    module.define_function(id, &mut ctx).map_err(|e| err(&e))?;
                    let size = code_size(&ctx);
                    bytes += size;
                    super_bytes += size;
                    module.clear_context(&mut ctx);
                    sup = Some((
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
                    ));
                }
            }
        }
        module.finalize_definitions().map_err(|e| err(&e))?;
        let module = &*module;
        // SAFETY: each id was defined with the matching signature, and the
        // code stays alive as long as `owned`, which the result owns.
        let settle = |id| unsafe {
            std::mem::transmute::<*const u8, SettleFn>(module.get_finalized_function(id))
        };
        let commit = |id| unsafe {
            std::mem::transmute::<*const u8, CommitFn>(module.get_finalized_function(id))
        };
        let bodies = ids
            .into_iter()
            .map(|b| {
                b.map(|(s, c)| JitBody {
                    settle: s.into_iter().map(settle).collect(),
                    commit: c.map(|c| c.into_iter().map(commit).collect()),
                })
            })
            .collect();
        let superstep = sup.map(|(id, info)| JitSuper {
            // SAFETY: as above.
            entry: unsafe {
                std::mem::transmute::<*const u8, SuperFn>(module.get_finalized_function(id))
            },
            info,
        });
        Ok(JitProgram {
            bodies,
            superstep,
            instrs: count,
            code_bytes: bytes,
            key: None,
            _module: Mutex::new(owned),
        })
    }
}

/// Bytes of machine code `ctx` holds after `define_function`.
fn code_size(ctx: &cranelift_codegen::Context) -> usize {
    ctx.compiled_code().map_or(0, |c| c.code_buffer().len())
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
    for (bi, b) in body.blocks.iter().enumerate() {
        while g < body.guards.len() && body.guards[g].end as usize <= bi {
            g += 1;
        }
        let seg = match body.guards.get(g) {
            Some(x) if x.start as usize <= bi => g as u32 + 1,
            _ => 0,
        };
        for k in b.start..b.start + b.len {
            if out.last().is_some_and(|c| c.len() >= CHUNK) {
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

/// Flags for slot and memory accesses: in bounds (checked), aligned.
fn flags() -> MemFlagsData {
    MemFlagsData::trusted()
}

/// Slot loads and stores within one function, through an SSA cache: a
/// slot is loaded at most once, and a store's value is reused by later
/// reads. Slots only alias themselves, and memories never alias slots, so
/// this is exactly the interpreter's sequence of memory states.
struct Slots {
    base: Value,
    vals: HashMap<u32, Value>,
}

impl Slots {
    fn new(base: Value, capacity: usize) -> Self {
        Slots {
            base,
            vals: HashMap::with_capacity(capacity),
        }
    }

    fn addr(&self, b: &mut FunctionBuilder<'_>, slot: u32) -> (Value, i32) {
        let off = slot as u64 * 8;
        match i32::try_from(off) {
            Ok(o) => (self.base, o),
            Err(_) => (b.ins().iadd_imm_u(self.base, off as i64), 0),
        }
    }

    fn read(&mut self, b: &mut FunctionBuilder<'_>, slot: u32) -> Value {
        if slot == ZERO_SLOT {
            return b.ins().iconst(I64, 0);
        }
        if let Some(&v) = self.vals.get(&slot) {
            return v;
        }
        let (a, o) = self.addr(b, slot);
        let v = b.ins().load(I64, flags(), a, o);
        self.vals.insert(slot, v);
        v
    }

    fn write(&mut self, b: &mut FunctionBuilder<'_>, slot: u32, v: Value, store: bool) {
        if store {
            let (a, o) = self.addr(b, slot);
            b.ins().store(flags(), v, a, o);
        }
        self.vals.insert(slot, v);
    }
}

fn bool64(b: &mut FunctionBuilder<'_>, c: Value) -> Value {
    b.ins().uextend(I64, c)
}

/// An open guarded run in a settle chunk: the block after it, and the
/// slot and memory-pointer caches from before it (values defined inside
/// do not dominate the code after it).
type Open = (
    cranelift_codegen::ir::Block,
    HashMap<u32, Value>,
    HashMap<u32, Value>,
);

/// Build one settle chunk.
fn build_settle(
    cfg: TargetFrontendConfig,
    func: &mut Function,
    fctx: &mut FunctionBuilderContext,
    p: &Program,
    body: &BodyRef<'_>,
    items: &[(Item, u32)],
    live: Option<&HashSet<u32>>,
) {
    let mut b = FunctionBuilder::new(func, fctx);
    let entry = b.create_block();
    b.append_block_params_for_function_params(entry);
    b.switch_to_block(entry);
    b.seal_block(entry);
    let params = b.block_params(entry).to_vec();
    let (slots, mems, tick) = (params[0], params[1], params[2]);
    let mut sl = Slots::new(slots, items.len() * 2);
    // Slots this chunk reads: a result computed in a guarded run is only
    // in the SSA cache until the run ends, so a later read in the chunk
    // loads it from its slot.
    let mut chunk_reads: HashSet<u32> = HashSet::new();
    if live.is_some() && items.iter().any(|&(_, seg)| seg > 0) {
        for (item, _) in items {
            item.reads(p, body, |s| {
                chunk_reads.insert(s);
            });
        }
    }
    let mut mem_ptrs: HashMap<u32, Value> = HashMap::new();
    // The guard of the current run of items (0: none), the block after
    // it, and the caches from before it.
    let mut cur = 0u32;
    let mut open: Option<Open> = None;
    for &(item, seg) in items {
        if seg != cur {
            if let Some((merge, vals, ptrs)) = open.take() {
                b.ins().jump(merge, &[]);
                b.switch_to_block(merge);
                b.seal_block(merge);
                sl.vals = vals;
                mem_ptrs = ptrs;
            }
            if seg > 0 {
                let mut test: Option<Value> = None;
                for c in &body.guards[seg as usize - 1].conds {
                    let v = sl.read(&mut b, c.slot);
                    let cc = if c.when {
                        IntCC::NotEqual
                    } else {
                        IntCC::Equal
                    };
                    let t = b.ins().icmp_imm_u(cc, v, 0);
                    test = Some(match test {
                        Some(x) => b.ins().band(x, t),
                        None => t,
                    });
                }
                let test = test.expect("a guard has conditions");
                let then = b.create_block();
                let merge = b.create_block();
                b.ins().brif(test, then, &[], merge, &[]);
                b.switch_to_block(then);
                b.seal_block(then);
                open = Some((merge, sl.vals.clone(), mem_ptrs.clone()));
            }
            cur = seg;
        }
        let store =
            |s: u32| live.is_none_or(|l| l.contains(&s) || (cur > 0 && chunk_reads.contains(&s)));
        match item {
            Item::Instr { op, k } => {
                let i = body.instrs[k as usize];
                let m = body.imms[k as usize];
                let r = emit_instr(&mut b, &mut sl, &mut mem_ptrs, p, (mems, tick, 0), op, i, m);
                sl.write(&mut b, i.dst, r, store(i.dst));
            }
            Item::Pack { op, k } => {
                let pk = p.packs[k as usize];
                let r = emit_pack(&mut b, &mut sl, p, op, &pk);
                sl.write(&mut b, pk.dst, r, store(pk.dst));
                let s0 = pk.scatter_start as usize;
                for x in &p.scatters[s0..s0 + pk.scatter_len as usize] {
                    let lane = b.ins().ushr_imm_u(r, i64::from(x.lane));
                    let lane = b.ins().band_imm_u(lane, 1);
                    sl.write(&mut b, x.slot, lane, store(x.slot));
                }
            }
        }
    }
    if let Some((merge, _, _)) = open.take() {
        b.ins().jump(merge, &[]);
        b.switch_to_block(merge);
        b.seal_block(merge);
    }
    b.ins().return_(&[]);
    b.finalize(cfg);
}

/// Carry out of `a + b + c` at bit `w` (0..=64), as the interpreter's
/// 128-bit sum: `((a + b + c) >> w) & 1`.
fn add_carry(b: &mut FunctionBuilder<'_>, a: Value, x: Value, c: Value, w: u64) -> Value {
    let s1 = b.ins().iadd(a, x);
    let c1 = b.ins().icmp(IntCC::UnsignedLessThan, s1, a);
    let s2 = b.ins().iadd(s1, c);
    let c2 = b.ins().icmp(IntCC::UnsignedLessThan, s2, s1);
    if w >= 64 {
        // Bit 64 of the sum: the carries (at most one of them can be set
        // when `c` is 1 bit; the sum of both is the high word).
        let c1 = bool64(b, c1);
        let c2 = bool64(b, c2);
        let hi = b.ins().iadd(c1, c2);
        b.ins().band_imm_u(hi, 1)
    } else {
        let _ = (c1, c2);
        let x = b.ins().ushr_imm_u(s2, w as i64);
        b.ins().band_imm_u(x, 1)
    }
}

/// Assemble one packed operand word (see [`PackOperand`]).
fn pack_operand(
    b: &mut FunctionBuilder<'_>,
    sl: &mut Slots,
    p: &Program,
    o: &PackOperand,
) -> Value {
    let base = sl.read(b, o.base);
    let mut v = b.ins().band_imm_u(base, o.keep as i64);
    if o.splat_mask != 0 {
        let s = sl.read(b, o.splat);
        let s = b.ins().ushr_imm_u(s, i64::from(o.splat_bit));
        let s = b.ins().band_imm_u(s, 1);
        let s = b.ins().ineg(s);
        let s = b.ins().band_imm_u(s, o.splat_mask as i64);
        v = b.ins().bor(v, s);
    }
    let g = o.gather_start as usize;
    for gt in &p.gathers[g..g + o.gather_len as usize] {
        let s = sl.read(b, gt.slot);
        let s = b.ins().ushr_imm_u(s, i64::from(gt.bit));
        let s = b.ins().band_imm_u(s, 1);
        let s = b.ins().ishl_imm_u(s, i64::from(gt.lane));
        v = b.ins().bor(v, s);
    }
    v
}

/// Build the commit function of a body without `MemClear`s.
fn build_commit(
    cfg: TargetFrontendConfig,
    func: &mut Function,
    fctx: &mut FunctionBuilderContext,
    p: &Program,
    commits: &[Commit],
) {
    let mut b = FunctionBuilder::new(func, fctx);
    let entry = b.create_block();
    b.append_block_params_for_function_params(entry);
    b.switch_to_block(entry);
    b.seal_block(entry);
    let params = b.block_params(entry).to_vec();
    let (slots, mems, writes) = (params[0], params[1], params[2]);
    // Commits are sequential: a copy's load sees every store before it.
    // Values are not cached across a RAM write's control flow, so each
    // commit loads its own operands (the program has few RAM writes).
    let mut sl = Slots::new(slots, 0);
    for c in commits {
        match *c {
            Commit::Copy { state, next } => {
                let v = sl.read(&mut b, next);
                sl.write(&mut b, state, v, true);
            }
            Commit::RamWrite { .. } | Commit::RamWriteMasked { .. } => {
                emit_ram_write(&mut b, &mut sl, p, c, mems, writes);
            }
            Commit::MemClear { .. } => unreachable!("bodies with MemClear commit in Rust"),
        }
    }
    b.ins().return_(&[]);
    b.finalize(cfg);
}

/// The value of instruction `i` (opcode `op`, literal `m`) at tick
/// `tick + tick_off`, reading its operands through the slot cache.
#[allow(clippy::too_many_arguments)]
fn emit_instr(
    b: &mut FunctionBuilder<'_>,
    sl: &mut Slots,
    mem_ptrs: &mut HashMap<u32, Value>,
    p: &Program,
    (mems, tick, tick_off): (Value, Value, u64),
    op: Op,
    i: Instr,
    m: u64,
) -> Value {
    let slots = sl.base;
    let ptr_ty = b.func.dfg.value_type(mems);
    let (sa, sb, sc) = op.slot_operands();
    let a = if sa { Some(sl.read(b, i.a)) } else { None };
    let bb = if sb { Some(sl.read(b, i.b)) } else { None };
    let c = if sc { Some(sl.read(b, i.c)) } else { None };
    let (a, bv, c) = (
        a.unwrap_or_else(|| b.ins().iconst(I64, 0)),
        bb.unwrap_or(slots),
        c.unwrap_or(slots),
    );
    let ins = b;
    match op {
        Op::Copy => a,
        Op::Not => {
            let x = ins.ins().bnot(a);
            ins.ins().band_imm_u(x, m as i64)
        }
        Op::And => ins.ins().band(a, bv),
        Op::Or => ins.ins().bor(a, bv),
        Op::Xor => ins.ins().bxor(a, bv),
        Op::Nand => {
            let x = ins.ins().band(a, bv);
            let x = ins.ins().bnot(x);
            ins.ins().band_imm_u(x, m as i64)
        }
        Op::Nor => {
            let x = ins.ins().bor(a, bv);
            let x = ins.ins().bnot(x);
            ins.ins().band_imm_u(x, m as i64)
        }
        Op::Xnor => {
            let x = ins.ins().bxor(a, bv);
            let x = ins.ins().bnot(x);
            ins.ins().band_imm_u(x, m as i64)
        }
        Op::Mux => ins.ins().select(a, c, bv),
        Op::Switch => {
            let z = ins.ins().iconst(I64, 0);
            ins.ins().select(a, bv, z)
        }
        Op::AddSum => {
            let x = ins.ins().iadd(a, bv);
            let x = ins.ins().iadd(x, c);
            ins.ins().band_imm_u(x, m as i64)
        }
        Op::AddCarry => add_carry(ins, a, bv, c, m),
        Op::Eq => {
            let x = ins.ins().icmp(IntCC::Equal, a, bv);
            bool64(ins, x)
        }
        Op::LtU => {
            let x = ins.ins().icmp(IntCC::UnsignedLessThan, a, bv);
            bool64(ins, x)
        }
        Op::ReduceOr => {
            let x = ins.ins().icmp_imm_u(IntCC::NotEqual, a, 0);
            bool64(ins, x)
        }
        Op::ReduceAnd => {
            let x = ins.ins().icmp_imm_u(IntCC::Equal, a, m as i64);
            bool64(ins, x)
        }
        Op::ReduceXor => {
            let x = ins.ins().popcnt(a);
            ins.ins().band_imm_u(x, 1)
        }
        Op::Shr => {
            let x = ins.ins().ushr_imm_u(a, i64::from(i.b));
            ins.ins().band_imm_u(x, m as i64)
        }
        Op::Concat => {
            let x = ins.ins().ishl_imm_u(bv, i64::from(i.c));
            ins.ins().bor(a, x)
        }
        Op::Sext => {
            // `if (a >> (b - 1)) & 1 == 1 { (a | !mask(b)) & m } else { a }`
            let w = i.b;
            let bit = ins.ins().ushr_imm_u(a, i64::from(w - 1));
            let bit = ins.ins().band_imm_u(bit, 1);
            let ext = ins.ins().bor_imm_u(a, !mask(w as u8) as i64);
            let ext = ins.ins().band_imm_u(ext, m as i64);
            ins.ins().select(bit, ext, a)
        }
        Op::Clock => {
            // `((tick + c) % a) < b`
            let t = ins
                .ins()
                .iadd_imm_u(tick, (tick_off + u64::from(i.c)) as i64);
            let r = ins.ins().urem_imm_u(t, i64::from(i.a));
            let x = ins
                .ins()
                .icmp_imm_u(IntCC::UnsignedLessThan, r, i64::from(i.b));
            bool64(ins, x)
        }
        Op::MemRead => {
            let mp = *mem_ptrs.entry(i.b).or_insert_with(|| {
                ins.ins().load(
                    ptr_ty,
                    flags(),
                    mems,
                    (i.b as usize * size_of::<usize>()) as i32,
                )
            });
            let len_mask = (1u64 << p.mems[i.b as usize].addr_bits) - 1;
            let idx = ins.ins().band_imm_u(a, len_mask as i64);
            let off = ins.ins().ishl_imm_u(idx, 3);
            let off = if ptr_ty == I64 {
                off
            } else {
                ins.ins().ireduce(ptr_ty, off)
            };
            let addr = ins.ins().iadd(mp, off);
            ins.ins().load(I64, flags(), addr, 0)
        }
        Op::SrNext => {
            // `if b != 0 { 0 } else if a != 0 { 1 } else { c }`
            let one = ins.ins().iconst(I64, 1);
            let z = ins.ins().iconst(I64, 0);
            let x = ins.ins().select(a, one, c);
            ins.ins().select(bv, z, x)
        }
    }
}

/// The value of packed word `pk` (opcode `op`).
fn emit_pack(b: &mut FunctionBuilder<'_>, sl: &mut Slots, p: &Program, op: Op, pk: &Pack) -> Value {
    let slots = sl.base;
    let n = arity(op);
    let mut v = [slots; 3];
    for (j, o) in operands(pk).into_iter().enumerate().take(n) {
        v[j] = pack_operand(b, sl, p, o);
    }
    let [a, bv, c] = v;
    let ins = b;
    match op {
        Op::Copy => a,
        Op::Not => ins.ins().bnot(a),
        Op::And | Op::Switch => ins.ins().band(a, bv),
        Op::Or => ins.ins().bor(a, bv),
        Op::Xor => ins.ins().bxor(a, bv),
        Op::Nand => {
            let x = ins.ins().band(a, bv);
            ins.ins().bnot(x)
        }
        Op::Nor => {
            let x = ins.ins().bor(a, bv);
            ins.ins().bnot(x)
        }
        Op::Xnor => {
            let x = ins.ins().bxor(a, bv);
            ins.ins().bnot(x)
        }
        Op::Mux => {
            // `(a & c) | (!a & b)`
            let x = ins.ins().band(a, c);
            let y = ins.ins().band_not(bv, a);
            ins.ins().bor(x, y)
        }
        _ => unreachable!("checked: not a packable op"),
    }
}

/// A RAM write commit (`RamWrite` or `RamWriteMasked`): slots are
/// unchanged, so the slot cache stays valid.
fn emit_ram_write(
    b: &mut FunctionBuilder<'_>,
    sl: &mut Slots,
    p: &Program,
    c: &Commit,
    mems: Value,
    writes: Value,
) {
    let ptr = b.func.dfg.value_type(mems);
    let (mem, we, addr, data) = match *c {
        Commit::RamWrite {
            mem,
            we,
            addr,
            data,
        }
        | Commit::RamWriteMasked {
            mem,
            mask: we,
            addr,
            data,
        } => (mem, we, addr, data),
        _ => unreachable!("a RAM write"),
    };
    let masked = matches!(c, Commit::RamWriteMasked { .. });
    let en = sl.read(b, we);
    let a = sl.read(b, addr);
    let d = sl.read(b, data);
    let then = b.create_block();
    let done = b.create_block();
    b.ins().brif(en, then, &[], done, &[]);
    b.switch_to_block(then);
    b.seal_block(then);
    let mp = b.ins().load(
        ptr,
        flags(),
        mems,
        (mem as usize * size_of::<usize>()) as i32,
    );
    let len_mask = (1u64 << p.mems[mem as usize].addr_bits) - 1;
    let idx = b.ins().band_imm_u(a, len_mask as i64);
    let off = b.ins().ishl_imm_u(idx, 3);
    let off = if ptr == I64 {
        off
    } else {
        b.ins().ireduce(ptr, off)
    };
    let at = b.ins().iadd(mp, off);
    let word = if masked {
        // `(*m & !bits) | (data & bits)`
        let old = b.ins().load(I64, flags(), at, 0);
        let keep = b.ins().band_not(old, en);
        let new = b.ins().band(d, en);
        b.ins().bor(keep, new)
    } else {
        d
    };
    b.ins().store(flags(), word, at, 0);
    let wo = (mem as usize * 8) as i32;
    let n = b.ins().load(I64, flags(), writes, wo);
    let n = b.ins().iadd_imm_u(n, 1);
    b.ins().store(flags(), n, writes, wo);
    b.ins().jump(done, &[]);
    b.switch_to_block(done);
    b.seal_block(done);
    // Slots are unchanged by a RAM write; the cache stays valid.
}
