//! Cranelift backend (native targets): the settle chunks, commits and
//! supersteps of `super::compile_body` and `super::define_superstep` as
//! native code. See the parent module for what each function computes.

mod superstep;

use cranelift_codegen::ir::condcodes::IntCC;
use cranelift_codegen::ir::types::{I32, I64};
use cranelift_codegen::ir::{
    AbiParam, Function, InstBuilder, InstructionData, MemFlagsData, Opcode, Type, UserFuncName,
    Value, ValueDef,
};
use cranelift_codegen::isa::TargetFrontendConfig;
use cranelift_codegen::settings::{self, Configurable};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_jit::{
    ArenaMemoryProvider, BranchProtection, JITBuilder, JITMemoryKind, JITMemoryProvider, JITModule,
    SystemMemoryProvider,
};
use cranelift_module::{FuncId, Linkage, Module, ModuleResult};
use foldhash::{HashMap, HashMapExt, HashSet, HashSetExt};

use super::bits::SlotBits;
use super::superstep::{Plan, SItem, Stream};
use super::{BodyRef, CommitFn, Item, JitError, SettleFn, SuperFn, arity, operands};
use crate::compile::spec::possible_of;
use crate::compile::{Commit, Cond, Guard, Instr, Op, Pack, PackOperand, Program, ZERO_SLOT};
use crate::netlist::mask;

/// Whether native code probes its stack: where the system needs it (see
/// `new_module`).
const PROBE_STACK: bool = cfg!(windows);

/// A function defined in a [`Builder`].
pub(super) type FnId = FuncId;

/// Defines functions into one `JITModule`; [`Builder::finish`] makes their
/// code executable.
pub(super) struct Builder {
    owned: Owned,
    ctx: cranelift_codegen::Context,
    fctx: FunctionBuilderContext,
    /// Numbers the functions.
    n: u32,
    /// The bits each slot can hold (see [`Slots`]).
    bits: SlotBits,
}

/// The finished code of a [`Builder`]: function pointers by id, and the
/// module that owns their code.
pub(super) struct Finished {
    owned: Owned,
}

impl Finished {
    fn get(&self, id: FnId) -> *const u8 {
        self.owned
            .0
            .as_ref()
            .expect("a module")
            .get_finalized_function(id)
    }

    pub fn settle_fn(&self, id: FnId) -> SettleFn {
        // SAFETY: settle chunks are defined with the `SettleFn` signature;
        // the code lives as long as `owned`, which the caller keeps with
        // the pointer.
        unsafe { std::mem::transmute::<*const u8, SettleFn>(self.get(id)) }
    }

    pub fn commit_fn(&self, id: FnId) -> CommitFn {
        // SAFETY: as in `settle_fn`, for commits.
        unsafe { std::mem::transmute::<*const u8, CommitFn>(self.get(id)) }
    }

    pub fn super_fn(&self, id: FnId) -> SuperFn {
        // SAFETY: as in `settle_fn`, for a superstep entry.
        unsafe { std::mem::transmute::<*const u8, SuperFn>(self.get(id)) }
    }

    /// What owns the code.
    pub fn into_owned(self) -> Owned {
        self.owned
    }
}

impl Builder {
    /// A builder for about `hint` bytes of code (see `code_hint`), over
    /// slots of `bits`.
    pub fn new(hint: usize, bits: SlotBits) -> Result<Self, JitError> {
        let owned = new_module(hint)?;
        let ctx = owned.0.as_ref().expect("just created").make_context();
        Ok(Builder {
            owned,
            ctx,
            fctx: FunctionBuilderContext::new(),
            n: 0,
            bits,
        })
    }

    /// Declare and define the function in `ctx` under `prefix`; its id
    /// and bytes of code.
    fn define(&mut self, prefix: &str) -> Result<(FnId, usize), JitError> {
        let err = |e: &dyn std::fmt::Display| JitError(e.to_string());
        let module = self.owned.0.as_mut().expect("a module");
        let id = module
            .declare_function(
                &format!("{prefix}{}", self.n),
                Linkage::Local,
                &self.ctx.func.signature,
            )
            .map_err(|e| err(&e))?;
        self.n += 1;
        module
            .define_function(id, &mut self.ctx)
            .map_err(|e| err(&e))?;
        let size = code_size(&self.ctx);
        module.clear_context(&mut self.ctx);
        Ok((id, size))
    }

    fn start(&mut self, params: &[Ty]) -> TargetFrontendConfig {
        let cfg = self.owned.0.as_ref().expect("a module").target_config();
        let ptr = cfg.pointer_type();
        self.ctx.func.signature.params.clear();
        self.ctx
            .func
            .signature
            .params
            .extend(params.iter().map(|t| match t {
                Ty::Ptr => AbiParam::new(ptr),
                Ty::U64 => AbiParam::new(I64),
            }));
        self.ctx.func.name = UserFuncName::user(0, self.n);
        cfg
    }

    /// A settle chunk ([`SettleFn`]) over `items` of `body`; `live`: the
    /// slots to store in a fused-only body (`None`: every slot).
    pub fn settle(
        &mut self,
        p: &Program,
        body: &BodyRef<'_>,
        items: &[(Item, u32)],
        live: Option<&HashSet<u32>>,
        inner: bool,
    ) -> Result<(FnId, usize), JitError> {
        let cfg = self.start(&[Ty::Ptr, Ty::Ptr, Ty::U64]);
        if inner {
            // Only called by the body's entry, which keeps nothing in
            // registers across calls: no callee-saved registers.
            self.ctx.func.signature.call_conv = cranelift_codegen::isa::CallConv::Winch;
        }
        build_settle(
            cfg,
            &mut self.ctx.func,
            &mut self.fctx,
            p,
            &self.bits,
            body,
            items,
            live,
        );
        self.define("settle")
    }

    /// A commit chunk ([`CommitFn`]) of `commits` (no `MemClear`).
    pub fn commit(&mut self, p: &Program, commits: &[Commit]) -> Result<(FnId, usize), JitError> {
        let cfg = self.start(&[Ty::Ptr, Ty::Ptr, Ty::Ptr]);
        build_commit(
            cfg,
            &mut self.ctx.func,
            &mut self.fctx,
            p,
            &self.bits,
            commits,
        );
        self.define("commit")
    }

    /// Chunk `ch` of superstep stream `st` (see `superstep::build_chunk`):
    /// its id, bytes and the items left out as dead.
    #[allow(clippy::too_many_arguments)]
    pub fn super_chunk(
        &mut self,
        p: &Program,
        bodies: &[BodyRef<'_>],
        st: &Stream,
        ch: &[(SItem, u32)],
        plan: &Plan,
        w: &mut usize,
        skip_base: &[usize],
        continues: u32,
    ) -> Result<(FnId, usize, usize), JitError> {
        // No callee-saved registers: the chunks are only called by the
        // entry, which keeps nothing in registers.
        let cfg = self.start(&[Ty::Ptr, Ty::Ptr, Ty::U64, Ty::Ptr, Ty::Ptr]);
        self.ctx.func.signature.call_conv = cranelift_codegen::isa::CallConv::Winch;
        let dead = superstep::build_chunk(
            cfg,
            &mut self.ctx.func,
            &mut self.fctx,
            p,
            &self.bits,
            bodies,
            st,
            ch,
            plan,
            w,
            skip_base,
            continues,
        );
        let (id, size) = self.define("super")?;
        Ok((id, size, dead))
    }

    /// The superstep entry ([`SuperFn`]): calls `chunks` in order.
    pub fn super_entry(&mut self, chunks: &[FnId]) -> Result<(FnId, usize), JitError> {
        self.entry(
            &[Ty::Ptr, Ty::Ptr, Ty::U64, Ty::Ptr, Ty::Ptr],
            chunks,
            "super",
        )
    }

    /// A body's settle entry ([`SettleFn`]): calls its `chunks` (settle
    /// chunks defined `inner`) in order.
    pub fn settle_entry(&mut self, chunks: &[FnId]) -> Result<(FnId, usize), JitError> {
        self.entry(&[Ty::Ptr, Ty::Ptr, Ty::U64], chunks, "settle")
    }

    /// A function of `params` that calls `chunks` in order with its own
    /// arguments.
    fn entry(
        &mut self,
        params: &[Ty],
        chunks: &[FnId],
        prefix: &str,
    ) -> Result<(FnId, usize), JitError> {
        let cfg = self.start(params);
        let module = self.owned.0.as_mut().expect("a module");
        let refs: Vec<_> = chunks
            .iter()
            .map(|&id| module.declare_func_in_func(id, &mut self.ctx.func))
            .collect();
        {
            let mut b = FunctionBuilder::new(&mut self.ctx.func, &mut self.fctx);
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
        self.define(prefix)
    }

    /// Make the code executable.
    pub fn finish(mut self) -> Result<Finished, JitError> {
        self.owned
            .0
            .as_mut()
            .expect("a module")
            .finalize_definitions()
            .map_err(|e| JitError(e.to_string()))?;
        Ok(Finished { owned: self.owned })
    }
}

/// A parameter type of a generated function.
#[derive(Clone, Copy)]
enum Ty {
    Ptr,
    U64,
}

/// Owns a `JITModule` and frees its code when dropped (a `JITModule` alone
/// leaks it), including when compiling fails half-way.
pub(super) struct Owned(Option<JITModule>);

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

/// A Cranelift module configured for this crate's code (see
/// `Builder`), owned so that its code is freed on drop.
fn new_module(hint: usize) -> Result<Owned, JitError> {
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
    // Windows commits a thread's stack a page at a time, through one
    // guard page: a function whose frame is larger than a page must
    // touch every page on its way down, or its first access below the
    // guard page is an access violation. Chunks of many items
    // (`SimOptions::superstep_chunk`) have such frames. The probes are
    // inline (no call to a `__probestack` symbol), and only functions
    // with a frame over a page get any.
    if PROBE_STACK {
        flags
            .set("enable_probestack", "true")
            .map_err(|e| err(&e))?;
        flags
            .set("probestack_strategy", "inline")
            .map_err(|e| err(&e))?;
    }
    let isa = cranelift_native::builder()
        .map_err(|e| err(&e))?
        .finish(settings::Flags::new(flags))
        .map_err(|e| err(&e))?;
    let mut builder = JITBuilder::with_isa(isa, cranelift_module::default_libcall_names());
    let t = super::tune::get();
    if t.arena || t.align > 1 {
        builder.memory_provider(Box::new(CodeMemory {
            arenas: Vec::new(),
            next: hint.next_power_of_two().clamp(ARENA_MIN, ARENA_MAX),
            align: t.align,
            system: (!t.arena).then(SystemMemoryProvider::new),
        }));
    }
    Ok(Owned(Some(JITModule::new(builder))))
}

/// Bounds of a first arena's size (a module's estimated code size).
const ARENA_MIN: usize = 64 << 10;
const ARENA_MAX: usize = 64 << 20;

/// Where a module's code goes (`tune`: `LOGI_JIT_ARENA`, `LOGI_JIT_ALIGN`).
///
/// By default `cranelift-jit` maps a new page-rounded block for each
/// function that does not fit what is left of the last block, and leaves
/// that rest unused. The system usually maps each block just below the
/// one before, so functions that run one after the other sit in
/// descending pages, each from the start of its page: the hot code of a
/// tick then crowds the instruction cache's sets for the low page
/// offsets (with chunks of about 3 KiB, every set past 3 KiB stays
/// empty), and sequential fetch never runs on into the next function. On
/// `BladeBuster` (spec off), superstep chunks of 128 items ran 26 %
/// slower than chunks of 64 that way, and 3 % faster in arenas.
///
/// Here functions are placed in order, back to back (after `align`), in
/// arenas reserved up front (`ArenaMemoryProvider`: address space only,
/// committed as it is used); a function that does not fit the last arena
/// starts a new one, twice as large.
struct CodeMemory {
    arenas: Vec<ArenaMemoryProvider>,
    /// The size of the next arena.
    next: usize,
    align: u64,
    /// Placement as by default (`LOGI_JIT_ARENA=0`), aligned to `align`.
    system: Option<SystemMemoryProvider>,
}

impl JITMemoryProvider for CodeMemory {
    fn allocate(
        &mut self,
        size: usize,
        align: u64,
        kind: JITMemoryKind,
    ) -> std::io::Result<*mut u8> {
        let align = match kind {
            JITMemoryKind::Executable => align.max(self.align),
            _ => align,
        };
        if let Some(s) = &mut self.system {
            return s.allocate(size, align, kind);
        }
        if let Some(a) = self.arenas.last_mut()
            && let Ok(p) = a.allocate(size, align, same(&kind))
        {
            return Ok(p);
        }
        // Room for the function, its alignment, and a segment per kind
        // (each starts on a page).
        let want = (size + align as usize + (16 << 10)).next_power_of_two();
        let n = self.next.max(want);
        self.next = n.saturating_mul(2);
        let mut a = ArenaMemoryProvider::new_with_size(n).map_err(std::io::Error::other)?;
        let p = a.allocate(size, align, kind);
        self.arenas.push(a);
        p
    }

    unsafe fn free_memory(&mut self) {
        for a in &mut self.arenas {
            // SAFETY: the caller's contract (`Owned::drop`): nothing uses
            // the code any more.
            unsafe { a.free_memory() };
        }
        self.arenas.clear();
        if let Some(s) = &mut self.system {
            // SAFETY: as above.
            unsafe { s.free_memory() };
        }
    }

    fn finalize(&mut self, branch_protection: BranchProtection) -> ModuleResult<()> {
        for a in &mut self.arenas {
            a.finalize(branch_protection)?;
        }
        match &mut self.system {
            Some(s) => s.finalize(branch_protection),
            None => Ok(()),
        }
    }
}

/// A copy of `k` (`JITMemoryKind` is not `Copy`).
fn same(k: &JITMemoryKind) -> JITMemoryKind {
    match k {
        JITMemoryKind::Executable => JITMemoryKind::Executable,
        JITMemoryKind::Writable => JITMemoryKind::Writable,
        JITMemoryKind::ReadOnly => JITMemoryKind::ReadOnly,
    }
}

/// Bytes of machine code `ctx` holds after `define_function`.
fn code_size(ctx: &cranelift_codegen::Context) -> usize {
    ctx.compiled_code().map_or(0, |c| c.code_buffer().len())
}

/// Flags for slot and memory accesses: in bounds (checked), aligned.
fn flags() -> MemFlagsData {
    MemFlagsData::trusted()
}

/// Slot loads and stores within one function, through an SSA cache: a
/// slot is loaded at most once, and a store's value is reused by later
/// reads. Slots only alias themselves, and memories never alias slots, so
/// this is exactly the interpreter's sequence of memory states.
///
/// # Widths
///
/// A slot whose bits (`SlotBits`) fit 32 bits is an `I32` value: loaded
/// from the low half of its word (little-endian), computed in 32-bit
/// operations, and stored zero-extended to the whole word. Every value
/// written is masked to its slot's bits where its own bound (the bits its
/// op can produce from its operands' slots, `compile::spec::possible_of`)
/// exceeds them, which only a body rewritten on demanded bits needs (a
/// known-bits or specialized fused body may read an alias wider than the
/// slot it replaces): there, the bits masked off are ones no reader
/// observes, which are 0 in the true value. So every slot a function
/// reads holds a value within its bits, whoever wrote it: the caller and
/// the commits write values within them (state, inputs and constants),
/// and a settle writes every other slot it reads before reading it (see
/// `super`), in this code or exactly in the interpreter. Each operation
/// is then computed exactly as the interpreter computes it, in fewer
/// bits where its result fits them.
struct Slots<'a> {
    base: Value,
    /// Per slot read or written: its value, and whether its word in
    /// memory holds it (it was loaded, or stored).
    vals: HashMap<u32, (Value, bool)>,
    /// Per slot: the item that last used its value (see `remat`).
    used: HashMap<u32, u32>,
    /// The item being emitted (items are counted by the caller).
    now: u32,
    /// Load a slot's value again (rather than keep it in a register or a
    /// spill slot) once it went unused for more than this many items,
    /// when memory holds it (0: never; `tune`: `LOGI_JIT_REMAT`).
    remat: u32,
    bits: &'a SlotBits,
    /// The zero-extended `I64` form of an `I32` value, where one was made
    /// with it (so that storing it costs no extension).
    wide: HashMap<Value, Value>,
}

impl<'a> Slots<'a> {
    fn new(base: Value, capacity: usize, bits: &'a SlotBits) -> Self {
        Slots {
            base,
            vals: HashMap::with_capacity(capacity),
            used: HashMap::with_capacity(capacity),
            now: 0,
            remat: super::tune::get().remat,
            bits,
            wide: HashMap::new(),
        }
    }

    /// The bits `slot` can hold.
    fn bits(&self, slot: u32) -> u64 {
        self.bits.bits(slot)
    }

    /// The type of `slot`'s values.
    fn ty(&self, slot: u32) -> Type {
        if self.bits.narrow(slot) { I32 } else { I64 }
    }

    fn addr(&self, b: &mut FunctionBuilder<'_>, slot: u32) -> (Value, i32) {
        let off = slot as u64 * 8;
        match i32::try_from(off) {
            Ok(o) => (self.base, o),
            Err(_) => (b.ins().iadd_imm_u(self.base, off as i64), 0),
        }
    }

    /// The value of `slot`: of type [`Self::ty`], or for a 32-bit slot
    /// possibly its whole word (an `I64` within the slot's bits).
    fn read(&mut self, b: &mut FunctionBuilder<'_>, slot: u32) -> Value {
        self.read_as(b, slot, Load::Narrow)
    }

    /// The value of `slot` as [`Self::read`], loaded (if it is not in the
    /// cache yet) as `how` says.
    fn read_as(&mut self, b: &mut FunctionBuilder<'_>, slot: u32, how: Load) -> Value {
        if slot == ZERO_SLOT {
            return b.ins().iconst(self.ty(slot), 0);
        }
        let now = self.now;
        let last = self.used.insert(slot, now);
        if let Some(&(v, mem)) = self.vals.get(&slot) {
            let idle = now - last.unwrap_or(now);
            if !(mem && self.remat > 0 && idle > self.remat) {
                return v;
            }
        }
        let (a, o) = self.addr(b, slot);
        let v = if self.ty(slot) == I64 {
            b.ins().load(I64, flags(), a, o)
        } else {
            match how {
                Load::Narrow => b.ins().load(I32, flags(), a, o),
                Load::Zext => b.ins().uload32(flags(), a, o),
            }
        };
        self.vals.insert(slot, (v, true));
        v
    }

    /// Write `v`, whose bits are within `bound`, to `slot` (see the type's
    /// documentation), and store it if `store`.
    fn write(&mut self, b: &mut FunctionBuilder<'_>, slot: u32, v: Value, bound: u64, store: bool) {
        let keep = self.bits(slot);
        let v = self.fit(b, v, bound, keep, self.ty(slot));
        if store {
            let w = self.wide(b, v);
            let (a, o) = self.addr(b, slot);
            b.ins().store(flags(), w, a, o);
        }
        self.vals.insert(slot, (v, store));
        self.used.insert(slot, self.now);
    }

    /// `v` (bits within `bound`) as a value of type `t` within `keep`.
    fn fit(
        &mut self,
        b: &mut FunctionBuilder<'_>,
        v: Value,
        bound: u64,
        keep: u64,
        t: Type,
    ) -> Value {
        let mut v = v;
        let mut bound = bound;
        if bound & !keep != 0 {
            if t == I32 {
                v = self.narrow(b, v, bound);
            }
            v = b.ins().band_imm_u(v, keep as i64);
            bound &= keep;
        }
        if t == I32 {
            self.narrow(b, v, bound)
        } else {
            self.wide(b, v)
        }
    }

    /// The low 32 bits of `v` (bits within `bound`).
    fn narrow(&mut self, b: &mut FunctionBuilder<'_>, v: Value, bound: u64) -> Value {
        if b.func.dfg.value_type(v) == I32 {
            return v;
        }
        let n = b.ins().ireduce(I32, v);
        if bound >> 32 == 0 {
            self.wide.insert(n, v);
        }
        n
    }

    /// `v` zero-extended to 64 bits.
    fn wide(&mut self, b: &mut FunctionBuilder<'_>, v: Value) -> Value {
        if b.func.dfg.value_type(v) == I64 {
            return v;
        }
        if let Some(&w) = self.wide.get(&v) {
            return w;
        }
        let dfg = &b.func.dfg;
        if let ValueDef::Result(inst, _) = dfg.value_def(v)
            && let InstructionData::UnaryImm {
                opcode: Opcode::Iconst,
                imm,
            } = dfg.insts[inst]
        {
            return b.ins().iconst(I64, imm.bits() as u32 as i64);
        }
        b.ins().uextend(I64, v)
    }

    /// `v` as a value of type `t` (bits within `bound`, which fits `t`).
    fn to(&mut self, b: &mut FunctionBuilder<'_>, v: Value, bound: u64, t: Type) -> Value {
        if t == I32 {
            self.narrow(b, v, bound)
        } else {
            self.wide(b, v)
        }
    }
}

/// How to load a 32-bit slot's value, for its first use in a function.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Load {
    /// A 32-bit load, which Cranelift can fold into a 32-bit operation
    /// reading memory.
    Narrow,
    /// A 32-bit load to a register, zero-extended (no extension to store
    /// it, and no folding: an operation with an immediate operand then
    /// takes the register rather than materializing the immediate).
    Zext,
}

/// The type to compute a value of bits `bound` in.
fn ty_of(bound: u64) -> Type {
    if bound >> 32 == 0 { I32 } else { I64 }
}

/// A 0/1 value of comparison `c`, as an `I64` (a stored result needs its
/// whole word; [`Slots::narrow`] keeps the `I64` for that).
fn bool64(b: &mut FunctionBuilder<'_>, c: Value) -> Value {
    b.ins().uextend(I64, c)
}

/// `x & m`, left out when `x` (bits within `bound`) has no bits outside
/// `m`.
fn mask_to(b: &mut FunctionBuilder<'_>, x: Value, bound: u64, m: u64) -> Value {
    if bound & !m == 0 {
        x
    } else {
        b.ins().band_imm_u(x, m as i64)
    }
}

/// `!x & m` for `x` with bits within `bound`. In 32 bits: `x ^ m` when
/// `x` has no bits outside `m` (as when `m` is the width of `x`), else
/// `(x ^ m) & m`, with `m` an immediate. (`!x & m` becomes
/// an `andn` with `m` loaded from the constant pool: 9 bytes, against 3
/// for `xor r32, imm8`.)
fn not_masked(b: &mut FunctionBuilder<'_>, x: Value, bound: u64, m: u64) -> Value {
    if b.func.dfg.value_type(x) == I32 {
        let y = b.ins().bxor_imm_u(x, m as i64);
        mask_to(b, y, bound | m, m)
    } else {
        let y = b.ins().bnot(x);
        b.ins().band_imm_u(y, m as i64)
    }
}

/// The test of guard `g` (nonzero when it holds): its conditions and'd,
/// and with the or of its `any` conditions.
fn guard_test(b: &mut FunctionBuilder<'_>, sl: &mut Slots<'_>, g: &Guard) -> Value {
    let mut cond = |b: &mut FunctionBuilder<'_>, c: &Cond| {
        let v = sl.read(b, c.slot);
        let cc = if c.when {
            IntCC::NotEqual
        } else {
            IntCC::Equal
        };
        b.ins().icmp_imm_u(cc, v, 0)
    };
    let mut all: Option<Value> = None;
    for c in &g.conds {
        let t = cond(b, c);
        all = Some(match all {
            Some(x) => b.ins().band(x, t),
            None => t,
        });
    }
    let mut any: Option<Value> = None;
    for c in &g.any {
        let t = cond(b, c);
        any = Some(match any {
            Some(x) => b.ins().bor(x, t),
            None => t,
        });
    }
    match (all, any) {
        (Some(x), Some(y)) => b.ins().band(x, y),
        (x, y) => x.or(y).expect("a guard has conditions"),
    }
}

/// An open guarded run in a settle chunk: the block after it, and the
/// slot and memory-pointer caches from before it (values defined inside
/// do not dominate the code after it).
type Open = (
    cranelift_codegen::ir::Block,
    HashMap<u32, (Value, bool)>,
    HashMap<u32, Value>,
);

/// Build one settle chunk.
#[allow(clippy::too_many_arguments)]
fn build_settle(
    cfg: TargetFrontendConfig,
    func: &mut Function,
    fctx: &mut FunctionBuilderContext,
    p: &Program,
    bits: &SlotBits,
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
    let mut sl = Slots::new(slots, items.len() * 2, bits);
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
        sl.now += 1;
        if seg != cur {
            if let Some((merge, vals, ptrs)) = open.take() {
                b.ins().jump(merge, &[]);
                b.switch_to_block(merge);
                b.seal_block(merge);
                sl.vals = vals;
                mem_ptrs = ptrs;
            }
            if seg > 0 {
                let test = guard_test(&mut b, &mut sl, &body.guards[seg as usize - 1]);
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
                let st = store(i.dst);
                let (r, bound) =
                    emit_instr(&mut b, &mut sl, &mut mem_ptrs, p, (mems, tick, 0), op, i, m);
                sl.write(&mut b, i.dst, r, bound, st);
            }
            Item::Pack { op, k } => {
                let pk = p.packs[k as usize];
                let r = emit_pack(&mut b, &mut sl, p, op, &pk);
                sl.write(&mut b, pk.dst, r, u64::MAX, store(pk.dst));
                let s0 = pk.scatter_start as usize;
                for x in &p.scatters[s0..s0 + pk.scatter_len as usize] {
                    let lane = b.ins().ushr_imm_u(r, i64::from(x.lane));
                    let lane = b.ins().band_imm_u(lane, 1);
                    sl.write(&mut b, x.slot, lane, 1, store(x.slot));
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
fn add_carry(
    b: &mut FunctionBuilder<'_>,
    sl: &mut Slots<'_>,
    [a, x, c]: [(Value, u64); 3],
    w: u64,
) -> Value {
    if w < 32 {
        // Bit `w` of the sum only depends on bits up to `w` of the
        // operands: their low halves.
        let a = sl.narrow(b, a.0, a.1);
        let x = sl.narrow(b, x.0, x.1);
        let c = sl.narrow(b, c.0, c.1);
        let s = b.ins().iadd(a, x);
        let s = b.ins().iadd(s, c);
        let s = b.ins().ushr_imm_u(s, w as i64);
        return b.ins().band_imm_u(s, 1);
    }
    let a = sl.wide(b, a.0);
    let x = sl.wide(b, x.0);
    let c = sl.wide(b, c.0);
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
    sl: &mut Slots<'_>,
    p: &Program,
    o: &PackOperand,
) -> Value {
    let base = sl.read(b, o.base);
    let base = sl.wide(b, base);
    let mut v = b.ins().band_imm_u(base, o.keep as i64);
    if o.splat_mask != 0 {
        let s = sl.read(b, o.splat);
        let s = sl.wide(b, s);
        let s = b.ins().ushr_imm_u(s, i64::from(o.splat_bit));
        let s = b.ins().band_imm_u(s, 1);
        let s = b.ins().ineg(s);
        let s = b.ins().band_imm_u(s, o.splat_mask as i64);
        v = b.ins().bor(v, s);
    }
    let g = o.gather_start as usize;
    for gt in &p.gathers[g..g + o.gather_len as usize] {
        let s = sl.read(b, gt.slot);
        let bound = sl.bits(gt.slot);
        // The bit, in the slot's own width.
        let s = if b.func.dfg.value_type(s) == I32 && gt.bit >= 32 {
            b.ins().iconst(I32, 0)
        } else {
            let s = if gt.bit > 0 {
                b.ins().ushr_imm_u(s, i64::from(gt.bit))
            } else {
                s
            };
            mask_to(b, s, bound >> gt.bit, 1)
        };
        let s = sl.wide(b, s);
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
    bits: &SlotBits,
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
    let mut sl = Slots::new(slots, 0, bits);
    for c in commits {
        match *c {
            Commit::Copy { state, next } => {
                let v = sl.read_as(&mut b, next, Load::Zext);
                let bound = sl.bits(next);
                sl.write(&mut b, state, v, bound, true);
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

/// The address of word `a` (bits within `bound`) of memory `mem` (its
/// base pointer `mp`), wrapped to the memory's size.
fn mem_word(
    b: &mut FunctionBuilder<'_>,
    sl: &mut Slots<'_>,
    p: &Program,
    mem: u32,
    mp: Value,
    a: Value,
    bound: u64,
) -> Value {
    let ptr = b.func.dfg.value_type(mp);
    let len_mask = (1u64 << p.mems[mem as usize].addr_bits) - 1;
    let idx = if len_mask >> 32 == 0 {
        let a = sl.narrow(b, a, bound);
        mask_to(b, a, bound, len_mask)
    } else {
        let a = sl.wide(b, a);
        mask_to(b, a, bound, len_mask)
    };
    let idx = sl.wide(b, idx);
    let off = b.ins().ishl_imm_u(idx, 3);
    let off = if ptr == I64 {
        off
    } else {
        b.ins().ireduce(ptr, off)
    };
    b.ins().iadd(mp, off)
}

/// The value of instruction `i` (opcode `op`, literal `m`) at tick
/// `tick + tick_off`, reading its operands through the slot cache, and
/// the bits it can have (computed in 32 bits where they fit).
#[allow(clippy::too_many_arguments)]
fn emit_instr(
    b: &mut FunctionBuilder<'_>,
    sl: &mut Slots<'_>,
    mem_ptrs: &mut HashMap<u32, Value>,
    p: &Program,
    (mems, tick, tick_off): (Value, Value, u64),
    op: Op,
    i: Instr,
    m: u64,
) -> (Value, u64) {
    let ptr_ty = b.func.dfg.value_type(mems);
    let bound = possible_of(op, &i, m, sl.bits.as_slice(), &p.mems);
    let t = ty_of(bound);
    let how = match op {
        // Stored as loaded; or to an operation with an immediate (which
        // Cranelift would otherwise materialize in a register to fold the
        // load into the operation instead).
        Op::Copy | Op::Not => Load::Zext,
        _ => Load::Narrow,
    };
    let (sa, sb, sc) = op.slot_operands();
    // Operands: value and bits (unused ones are a zero placeholder).
    let mut opnd = |b: &mut FunctionBuilder<'_>, used: bool, s: u32, how: Load| {
        if used {
            (sl.read_as(b, s, how), sl.bits(s))
        } else {
            (sl.read(b, ZERO_SLOT), 0)
        }
    };
    let (a, ba) = opnd(b, sa, i.a, how);
    let (bv, bb) = opnd(b, sb, i.b, how);
    let (c, bc) = opnd(b, sc, i.c, how);
    let ins = b;
    let r = match op {
        Op::Copy => a,
        Op::Not => {
            let x = sl.to(ins, a, ba, t);
            not_masked(ins, x, ba, m)
        }
        Op::And | Op::Or | Op::Xor => {
            let x = sl.to(ins, a, ba, t);
            let y = sl.to(ins, bv, bb, t);
            match op {
                Op::And => ins.ins().band(x, y),
                Op::Or => ins.ins().bor(x, y),
                _ => ins.ins().bxor(x, y),
            }
        }
        Op::Nand | Op::Nor | Op::Xnor => {
            let x = sl.to(ins, a, ba, t);
            let y = sl.to(ins, bv, bb, t);
            let (z, bz) = match op {
                Op::Nand => (ins.ins().band(x, y), ba & bb),
                Op::Nor => (ins.ins().bor(x, y), ba | bb),
                _ => (ins.ins().bxor(x, y), ba | bb),
            };
            not_masked(ins, z, bz, m)
        }
        Op::Mux => {
            let x = sl.to(ins, c, bc, t);
            let y = sl.to(ins, bv, bb, t);
            ins.ins().select(a, x, y)
        }
        Op::Switch if ba >> 1 == 0 => {
            // `b & -a` for a 1-bit `a` (no constant 0 to select).
            let x = sl.to(ins, bv, bb, t);
            let s = sl.to(ins, a, ba, t);
            let s = ins.ins().ineg(s);
            ins.ins().band(x, s)
        }
        Op::Switch => {
            let x = sl.to(ins, bv, bb, t);
            let z = ins.ins().iconst(t, 0);
            ins.ins().select(a, x, z)
        }
        Op::AddSum => {
            let x = sl.to(ins, a, ba, t);
            let y = sl.to(ins, bv, bb, t);
            let z = sl.to(ins, c, bc, t);
            let x = ins.ins().iadd(x, y);
            let x = ins.ins().iadd(x, z);
            ins.ins().band_imm_u(x, m as i64)
        }
        Op::AddCarry => add_carry(ins, sl, [(a, ba), (bv, bb), (c, bc)], m),
        Op::Eq | Op::LtU => {
            let cc = if op == Op::Eq {
                IntCC::Equal
            } else {
                IntCC::UnsignedLessThan
            };
            let x = if ty_of(ba | bb) == I32 {
                let x = sl.narrow(ins, a, ba);
                let y = sl.narrow(ins, bv, bb);
                ins.ins().icmp(cc, x, y)
            } else {
                let x = sl.wide(ins, a);
                let y = sl.wide(ins, bv);
                ins.ins().icmp(cc, x, y)
            };
            bool64(ins, x)
        }
        Op::ReduceOr => {
            let x = ins.ins().icmp_imm_u(IntCC::NotEqual, a, 0);
            bool64(ins, x)
        }
        Op::ReduceAnd => {
            if ins.func.dfg.value_type(a) == I32 && m >> 32 != 0 {
                // A 32-bit value never equals it.
                ins.ins().iconst(I64, 0)
            } else {
                let x = ins.ins().icmp_imm_u(IntCC::Equal, a, m as i64);
                bool64(ins, x)
            }
        }
        Op::ReduceXor => {
            let x = ins.ins().popcnt(a);
            ins.ins().band_imm_u(x, 1)
        }
        Op::Shr => {
            // `(a >> b) & imm`, `b` a literal below 64: bits `need` of
            // `a`, shifted in 32 bits when they are in its low half.
            let s = i.b;
            let need = ((u128::from(m) << s) & u128::from(ba)) as u64;
            let a = if need >> 32 == 0 {
                sl.narrow(ins, a, ba)
            } else {
                a
            };
            if need == 0 {
                ins.ins().iconst(t, 0)
            } else {
                let x = if s > 0 {
                    ins.ins().ushr_imm_u(a, i64::from(s))
                } else {
                    a
                };
                let bx = ba >> s;
                let x = sl.to(ins, x, bx, t);
                mask_to(ins, x, bx, m)
            }
        }
        Op::Concat => {
            // `a | (b << c)`, `c` a literal.
            let x = sl.to(ins, a, ba, t);
            let width = if t == I32 { 32 } else { 64 };
            if i.c >= width {
                x
            } else {
                let y = sl.to(ins, bv, bb, t);
                let y = ins.ins().ishl_imm_u(y, i64::from(i.c));
                ins.ins().bor(x, y)
            }
        }
        Op::Shl => {
            // `((a >> c) << b) & imm`, `b` and `c` literals below 64: bits
            // `need` of `a`, shifted in 32 bits when they are in its low
            // half (and the result in its own).
            let need = (u128::from(m >> i.b) << i.c) as u64 & ba;
            let width = if t == I32 { 32 } else { 64 };
            if need == 0 || i.b >= width {
                ins.ins().iconst(t, 0)
            } else {
                let a = if need >> 32 == 0 && t == I32 {
                    sl.narrow(ins, a, ba)
                } else {
                    a
                };
                let x = if i.c > 0 {
                    ins.ins().ushr_imm_u(a, i64::from(i.c))
                } else {
                    a
                };
                let bx = ba >> i.c;
                let x = sl.to(ins, x, bx, t);
                let x = if i.b > 0 {
                    ins.ins().ishl_imm_u(x, i64::from(i.b))
                } else {
                    x
                };
                mask_to(ins, x, bx << i.b, m)
            }
        }
        Op::ShlV => {
            // `(a << (b & 63)) & imm`. Cranelift takes the amount modulo
            // the type's width: 32 bits only for amounts below 32.
            if t == I32 && bb >> 5 == 0 {
                let x = sl.narrow(ins, a, ba);
                let y = sl.narrow(ins, bv, bb);
                let x = ins.ins().ishl(x, y);
                ins.ins().band_imm_u(x, m as i64)
            } else {
                let x = sl.wide(ins, a);
                let y = sl.wide(ins, bv);
                let x = ins.ins().ishl(x, y);
                ins.ins().band_imm_u(x, m as i64)
            }
        }
        Op::Sext => {
            // `if (a >> (b - 1)) & 1 == 1 { (a | !mask(b)) & m } else { a }`
            let w = i.b;
            let at = ins.func.dfg.value_type(a);
            if at == I32 && t == I32 && w <= 32 {
                let bit = if w > 1 {
                    ins.ins().ushr_imm_u(a, i64::from(w - 1))
                } else {
                    a
                };
                let bit = ins.ins().band_imm_u(bit, 1);
                let ext = ins.ins().bor_imm_u(a, !mask(w as u8) as i64);
                let ext = ins.ins().band_imm_u(ext, m as i64);
                ins.ins().select(bit, ext, a)
            } else {
                let a = sl.wide(ins, a);
                let bit = ins.ins().ushr_imm_u(a, i64::from(w - 1));
                let bit = ins.ins().band_imm_u(bit, 1);
                let ext = ins.ins().bor_imm_u(a, !mask(w as u8) as i64);
                let ext = ins.ins().band_imm_u(ext, m as i64);
                ins.ins().select(bit, ext, a)
            }
        }
        Op::Clock => {
            // `((tick + c) % a) < b`
            let x = ins
                .ins()
                .iadd_imm_u(tick, (tick_off + u64::from(i.c)) as i64);
            let r = ins.ins().urem_imm_u(x, i64::from(i.a));
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
            let addr = mem_word(ins, sl, p, i.b, mp, a, ba);
            // Memory words hold their width's bits (as slots do).
            ins.ins().load(t, flags(), addr, 0)
        }
        Op::SrNext if (ba | bb | bc) >> 1 == 0 => {
            // `(a | c) & !b` on single bits.
            let x = sl.narrow(ins, a, ba);
            let y = sl.narrow(ins, c, bc);
            let x = ins.ins().bor(x, y);
            let r = sl.narrow(ins, bv, bb);
            let r = ins.ins().bxor_imm_u(r, 1);
            ins.ins().band(x, r)
        }
        Op::SrNext => {
            // `if b != 0 { 0 } else if a != 0 { 1 } else { c }`
            let one = ins.ins().iconst(t, 1);
            let z = ins.ins().iconst(t, 0);
            let c = sl.to(ins, c, bc, t);
            let x = ins.ins().select(a, one, c);
            ins.ins().select(bv, z, x)
        }
    };
    (r, bound)
}

/// The value of packed word `pk` (opcode `op`).
fn emit_pack(
    b: &mut FunctionBuilder<'_>,
    sl: &mut Slots<'_>,
    p: &Program,
    op: Op,
    pk: &Pack,
) -> Value {
    let n = arity(op);
    let mut v = [None; 3];
    for (j, o) in operands(pk).into_iter().enumerate().take(n) {
        v[j] = Some(pack_operand(b, sl, p, o));
    }
    let [a, bv, c] = v;
    let a = a.expect("a packed op reads a");
    let ins = b;
    match op {
        Op::Copy => a,
        Op::Not => ins.ins().bnot(a),
        Op::And | Op::Switch => ins.ins().band(a, bv.expect("binary")),
        Op::Or => ins.ins().bor(a, bv.expect("binary")),
        Op::Xor => ins.ins().bxor(a, bv.expect("binary")),
        Op::Nand => {
            let x = ins.ins().band(a, bv.expect("binary"));
            ins.ins().bnot(x)
        }
        Op::Nor => {
            let x = ins.ins().bor(a, bv.expect("binary"));
            ins.ins().bnot(x)
        }
        Op::Xnor => {
            let x = ins.ins().bxor(a, bv.expect("binary"));
            ins.ins().bnot(x)
        }
        Op::Mux => {
            // `(a & c) | (!a & b)`
            let x = ins.ins().band(a, c.expect("ternary"));
            let y = ins.ins().band_not(bv.expect("ternary"), a);
            ins.ins().bor(x, y)
        }
        _ => unreachable!("checked: not a packable op"),
    }
}

/// A RAM write commit (`RamWrite` or `RamWriteMasked`): slots are
/// unchanged, so the slot cache stays valid.
fn emit_ram_write(
    b: &mut FunctionBuilder<'_>,
    sl: &mut Slots<'_>,
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
    let at = mem_word(b, sl, p, mem, mp, a, sl.bits(addr));
    let d = sl.wide(b, d);
    let word = if masked {
        // `(*m & !bits) | (data & bits)`
        let en = sl.wide(b, en);
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
