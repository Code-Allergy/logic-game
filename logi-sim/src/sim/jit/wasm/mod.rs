//! WebAssembly backend (wasm32): the settle chunks, commits and supersteps
//! of `super::compile_body` and `super::define_superstep` as functions of
//! a WebAssembly module generated at run time. See the parent module for
//! what each function computes; this one mirrors `clif/` function by
//! function.
//!
//! # Running the code
//!
//! The module imports the main module's memory (`wasm_bindgen::memory()`),
//! so it works on the `Sim`'s slots and memories in place: a pointer is an
//! `i32` address in that memory. Its functions have the types Rust gives
//! [`SettleFn`], [`CommitFn`] and [`SuperFn`] on wasm32 (pointers `i32`,
//! `u64` `i64`, no result), and are put in the main module's function table
//! (`wasm_bindgen::function_table()`): on wasm32 a Rust function pointer is
//! an index into that table, so `Sim` calls them as it calls native code,
//! and a call is a `call_indirect` from wasm to wasm. JavaScript is only
//! involved when a module is instantiated and when its entries are freed
//! (`rt.rs`). The table must be growable: link with `--growable-table`
//! (logi-sim's and the game's build scripts do); otherwise compiling fails
//! and the interpreter runs.
//!
//! The values of a function live in locals: a slot read is loaded into a
//! local at most once per function, and each result is a new local (the
//! engine's register allocator does the rest), exactly as the Cranelift
//! backend keeps SSA values. Within a guarded range the code is an `if`
//! block; the caches are restored after it, as in `clif/`.
//!
//! # Instantiating
//!
//! Browsers refuse to compile a module larger than 4 KB synchronously on
//! the main thread. [`Builder::finish`] compiles synchronously (Node, a
//! worker, tests); [`JitProgram::compile_async`] starts an asynchronous
//! instantiation and returns a [`PendingJit`] to poll (the
//! JavaScript callbacks run between two frames).
//!
//! [`JitProgram::compile_async`]: super::JitProgram::compile_async

pub(super) mod enc;
mod rt;
mod superstep;
#[cfg(feature = "selftest")]
pub(super) mod tests;

use foldhash::{HashMap, HashMapExt, HashSet, HashSetExt};

pub use rt::PendingModule;

use self::enc::{Func, I32, I64, op};
use super::superstep::{Plan, SItem, Stream};
use super::{BodyRef, CommitFn, Item, JitError, SettleFn, SuperFn, arity, operands};
use crate::compile::{Commit, Instr, Op, Pack, PackOperand, Program, ZERO_SLOT};
use crate::netlist::mask;

/// Bytes per pointer in a memory-pointer table (`Sim::mem_ptrs`).
const PTR: u32 = 4;
const _: () = assert!(size_of::<usize>() == PTR as usize);

/// Function types, by index: settle `(slots, mems, tick)`, commit
/// `(slots, mems, writes)`, superstep `(slots, mems, tick, writes, skips)`.
const TYPES: [&[u8]; 3] = [
    &[I32, I32, I64],
    &[I32, I32, I32],
    &[I32, I32, I64, I32, I32],
];
const T_SETTLE: u32 = 0;
const T_COMMIT: u32 = 1;
const T_SUPER: u32 = 2;

/// A function defined in a [`Builder`]: its index in the module.
pub(super) type FnId = u32;

/// Owns the table entries of a module's functions; frees them on drop.
pub(super) type Owned = rt::Entries;

/// Defines the functions of one module.
pub(super) struct Builder {
    /// Per function: its type and encoded body.
    funcs: Vec<(u32, Vec<u8>)>,
    /// Functions to put in the table (called through function pointers).
    exports: Vec<FnId>,
}

/// The finished code of a [`Builder`]: function pointers by id, and the
/// table entries that hold the functions.
pub(super) struct Finished {
    /// Table index per exported function, in `Builder::exports` order.
    index: HashMap<FnId, u32>,
    entries: rt::Entries,
}

impl Finished {
    pub(super) fn new(exports: &[FnId], entries: rt::Entries) -> Self {
        let index = exports
            .iter()
            .copied()
            .zip(entries.indices().iter().copied())
            .collect();
        Finished { index, entries }
    }

    fn get(&self, id: FnId) -> usize {
        self.index[&id] as usize
    }

    pub fn settle_fn(&self, id: FnId) -> SettleFn {
        // SAFETY: a function pointer is a table index on wasm32, and the
        // entry holds a function of the settle type (`TYPES`), which is
        // what Rust lowers `SettleFn` to (a mismatch would trap at the
        // call, not misbehave). The entry is held by `entries`, which the
        // caller keeps with the pointer.
        unsafe { std::mem::transmute::<usize, SettleFn>(self.get(id)) }
    }

    pub fn commit_fn(&self, id: FnId) -> CommitFn {
        // SAFETY: as in `settle_fn`, for the commit type.
        unsafe { std::mem::transmute::<usize, CommitFn>(self.get(id)) }
    }

    pub fn super_fn(&self, id: FnId) -> SuperFn {
        // SAFETY: as in `settle_fn`, for the superstep type.
        unsafe { std::mem::transmute::<usize, SuperFn>(self.get(id)) }
    }

    /// What owns the table entries.
    pub fn into_owned(self) -> Owned {
        self.entries
    }
}

impl Builder {
    /// A builder (`hint`, the code's estimated size, is not needed).
    pub fn new(_hint: usize) -> Result<Self, JitError> {
        Ok(Builder {
            funcs: Vec::new(),
            exports: Vec::new(),
        })
    }

    fn define(&mut self, ty: u32, f: Func, export: bool) -> (FnId, usize) {
        let body = f.finish();
        let size = body.len();
        let id = self.funcs.len() as FnId;
        self.funcs.push((ty, body));
        if export {
            self.exports.push(id);
        }
        (id, size)
    }

    /// The module's bytes, and the functions it exports (as `f{index}`).
    pub(super) fn bytes(&self) -> (Vec<u8>, Vec<FnId>) {
        let exports: Vec<(String, u32)> = self.exports.iter().map(|&f| (rt::name(f), f)).collect();
        (
            enc::module(&TYPES, &self.funcs, &exports),
            self.exports.clone(),
        )
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
        check_slots(p)?;
        let f = build_settle(p, body, items, live);
        // An inner chunk is only called by its body's entry, directly.
        Ok(self.define(T_SETTLE, f, !inner))
    }

    /// A body's settle entry ([`SettleFn`]): calls its `chunks` (defined
    /// `inner`) in order.
    pub fn settle_entry(&mut self, chunks: &[FnId]) -> Result<(FnId, usize), JitError> {
        let mut f = Func::new(3);
        for &c in chunks {
            for a in 0..3 {
                f.get(a);
            }
            f.call(c);
        }
        Ok(self.define(T_SETTLE, f, true))
    }

    /// A commit chunk ([`CommitFn`]) of `commits` (no `MemClear`).
    pub fn commit(&mut self, p: &Program, commits: &[Commit]) -> Result<(FnId, usize), JitError> {
        check_slots(p)?;
        let f = build_commit(p, commits);
        Ok(self.define(T_COMMIT, f, true))
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
        check_slots(p)?;
        let (f, dead) = superstep::build_chunk(p, bodies, st, ch, plan, w, skip_base, continues);
        // Only the entry calls it, directly.
        let (id, size) = self.define(T_SUPER, f, false);
        Ok((id, size, dead))
    }

    /// The superstep entry ([`SuperFn`]): calls `chunks` in order.
    pub fn super_entry(&mut self, chunks: &[FnId]) -> Result<(FnId, usize), JitError> {
        let mut f = Func::new(5);
        for &c in chunks {
            for a in 0..5 {
                f.get(a);
            }
            f.call(c);
        }
        Ok(self.define(T_SUPER, f, true))
    }

    /// Compile and instantiate the module synchronously, and put its
    /// functions in the table. Fails where synchronous compilation is not
    /// allowed (a browser's main thread, for modules over 4 KB).
    pub fn finish(self) -> Result<Finished, JitError> {
        let (bytes, exports) = self.bytes();
        let entries = rt::instantiate(&bytes, &exports)?;
        Ok(Finished::new(&exports, entries))
    }

    /// Start compiling and instantiating the module asynchronously.
    pub fn finish_async(self) -> Result<PendingModule, JitError> {
        let (bytes, exports) = self.bytes();
        rt::instantiate_async(&bytes, exports)
    }
}

/// Every slot's byte offset must fit a `u32` (a load's offset immediate).
fn check_slots(p: &Program) -> Result<(), JitError> {
    if u64::from(p.slot_count) * 8 > u64::from(u32::MAX) {
        return Err(JitError("too many slots for wasm32".into()));
    }
    Ok(())
}

/// Slot loads and stores within one function, through a cache of locals:
/// a slot is loaded at most once, and a store's value is reused by later
/// reads (as `clif::Slots`).
pub(super) struct Slots {
    /// The local holding the slots' address.
    base: u32,
    pub vals: HashMap<u32, u32>,
    /// A local never set (so 0), for `ZERO_SLOT`.
    zero: Option<u32>,
}

impl Slots {
    pub fn new(base: u32, capacity: usize) -> Self {
        Slots {
            base,
            vals: HashMap::with_capacity(capacity),
            zero: None,
        }
    }

    fn zero(&mut self, f: &mut Func) -> u32 {
        *self.zero.get_or_insert_with(|| f.local(I64))
    }

    /// The local holding slot `slot`'s value.
    pub fn read(&mut self, f: &mut Func, slot: u32) -> u32 {
        if slot == ZERO_SLOT {
            return self.zero(f);
        }
        if let Some(&v) = self.vals.get(&slot) {
            return v;
        }
        let v = f.local(I64);
        f.get(self.base);
        f.i64_load(slot * 8);
        f.set(v);
        self.vals.insert(slot, v);
        v
    }

    pub fn write(&mut self, f: &mut Func, slot: u32, v: u32, store: bool) {
        if store {
            f.get(self.base);
            f.get(v);
            f.i64_store(slot * 8);
        }
        self.vals.insert(slot, v);
    }
}

/// Pop the value on the stack into a new `i64` local.
fn result(f: &mut Func) -> u32 {
    let r = f.local(I64);
    f.set(r);
    r
}

/// Push `x != 0` (an `i32`) for the `i64` local `x`.
fn nonzero(f: &mut Func, x: u32) {
    f.get(x);
    f.op(op::I64_EQZ);
    f.op(op::I32_EQZ);
}

/// Push the test of guard `conds` (an `i32`, nonzero when it holds).
pub(super) fn guard_test(f: &mut Func, sl: &mut Slots, conds: &[crate::compile::Cond]) {
    for (k, c) in conds.iter().enumerate() {
        let v = sl.read(f, c.slot);
        f.get(v);
        f.op(op::I64_EQZ);
        if c.when {
            f.op(op::I32_EQZ);
        }
        if k > 0 {
            f.op(op::I32_AND);
        }
    }
    assert!(!conds.is_empty(), "a guard has conditions");
}

/// An open guarded run in a settle chunk: the slot and memory-pointer
/// caches from before it (values set inside are not set when it is
/// skipped).
type Open = (HashMap<u32, u32>, HashMap<u32, u32>);

/// Build one settle chunk (as `clif::build_settle`).
fn build_settle(
    p: &Program,
    body: &BodyRef<'_>,
    items: &[(Item, u32)],
    live: Option<&HashSet<u32>>,
) -> Func {
    let mut f = Func::new(3);
    let (mems, tick) = (1, 2);
    let mut sl = Slots::new(0, items.len() * 2);
    // Slots this chunk reads: a result computed in a guarded run is only
    // in the cache until the run ends, so a later read in the chunk
    // loads it from its slot.
    let mut chunk_reads: HashSet<u32> = HashSet::new();
    if live.is_some() && items.iter().any(|&(_, seg)| seg > 0) {
        for (item, _) in items {
            item.reads(p, body, |s| {
                chunk_reads.insert(s);
            });
        }
    }
    let mut mem_ptrs: HashMap<u32, u32> = HashMap::new();
    let mut cur = 0u32;
    let mut open: Option<Open> = None;
    for &(item, seg) in items {
        if seg != cur {
            if let Some((vals, ptrs)) = open.take() {
                f.op(op::END);
                sl.vals = vals;
                mem_ptrs = ptrs;
            }
            if seg > 0 {
                guard_test(&mut f, &mut sl, &body.guards[seg as usize - 1].conds);
                f.if_();
                open = Some((sl.vals.clone(), mem_ptrs.clone()));
            }
            cur = seg;
        }
        let store =
            |s: u32| live.is_none_or(|l| l.contains(&s) || (cur > 0 && chunk_reads.contains(&s)));
        match item {
            Item::Instr { op, k } => {
                let i = body.instrs[k as usize];
                let m = body.imms[k as usize];
                let r = emit_instr(&mut f, &mut sl, &mut mem_ptrs, p, (mems, tick, 0), op, i, m);
                sl.write(&mut f, i.dst, r, store(i.dst));
            }
            Item::Pack { op, k } => {
                let pk = p.packs[k as usize];
                let r = emit_pack(&mut f, &mut sl, p, op, &pk);
                sl.write(&mut f, pk.dst, r, store(pk.dst));
                let s0 = pk.scatter_start as usize;
                for x in &p.scatters[s0..s0 + pk.scatter_len as usize] {
                    let lane = lane(&mut f, r, x.lane);
                    sl.write(&mut f, x.slot, lane, store(x.slot));
                }
            }
        }
    }
    if open.is_some() {
        f.op(op::END);
    }
    f
}

/// Bit `lane` of local `r`, as a new local.
pub(super) fn lane(f: &mut Func, r: u32, lane: u8) -> u32 {
    f.get(r);
    f.i64_const(i64::from(lane));
    f.op(op::I64_SHR_U);
    f.i64_const(1);
    f.op(op::I64_AND);
    result(f)
}

/// Build the commit function of a body without `MemClear`s.
fn build_commit(p: &Program, commits: &[Commit]) -> Func {
    let mut f = Func::new(3);
    let (mems, writes) = (1, 2);
    let mut sl = Slots::new(0, 0);
    for c in commits {
        match *c {
            Commit::Copy { state, next } => {
                let v = sl.read(&mut f, next);
                sl.write(&mut f, state, v, true);
            }
            Commit::RamWrite { .. } | Commit::RamWriteMasked { .. } => {
                emit_ram_write(&mut f, &mut sl, p, c, mems, writes);
            }
            Commit::MemClear { .. } => unreachable!("bodies with MemClear commit in Rust"),
        }
    }
    f
}

/// Push the address of word `idx & len_mask` of memory `mem` (an `i32`),
/// its pointer loaded from the table at local `mems` into the cache.
fn mem_addr(
    f: &mut Func,
    mem_ptrs: &mut HashMap<u32, u32>,
    p: &Program,
    mems: u32,
    mem: u32,
    idx: u32,
) {
    let mp = match mem_ptrs.get(&mem) {
        Some(&l) => l,
        None => {
            let l = f.local(I32);
            f.get(mems);
            f.i32_load(mem * PTR);
            f.set(l);
            mem_ptrs.insert(mem, l);
            l
        }
    };
    let len_mask = (1u64 << p.mems[mem as usize].addr_bits) - 1;
    f.get(mp);
    f.get(idx);
    f.i64_const(len_mask as i64);
    f.op(op::I64_AND);
    f.op(op::I32_WRAP_I64);
    f.i32_const(3);
    f.op(op::I32_SHL);
    f.op(op::I32_ADD);
}

/// The value of instruction `i` (opcode `op`, literal `m`) at tick
/// `tick + tick_off` (`tick` a local), as a local (as `clif::emit_instr`).
#[allow(clippy::too_many_arguments)]
pub(super) fn emit_instr(
    f: &mut Func,
    sl: &mut Slots,
    mem_ptrs: &mut HashMap<u32, u32>,
    p: &Program,
    (mems, tick, tick_off): (u32, u32, u64),
    op: Op,
    i: Instr,
    m: u64,
) -> u32 {
    let (sa, sb, sc) = op.slot_operands();
    let a = if sa { sl.read(f, i.a) } else { sl.zero(f) };
    let b = if sb { sl.read(f, i.b) } else { sl.zero(f) };
    let c = if sc { sl.read(f, i.c) } else { sl.zero(f) };
    let m = m as i64;
    // `x & m` and `!x & m` of the value on the stack.
    let and_m = |f: &mut Func| {
        f.i64_const(m);
        f.op(op::I64_AND);
    };
    let not_m = |f: &mut Func| {
        f.i64_const(-1);
        f.op(op::I64_XOR);
        f.i64_const(m);
        f.op(op::I64_AND);
    };
    let bin = |f: &mut Func, o: u8| {
        f.get(a);
        f.get(b);
        f.op(o);
    };
    match op {
        Op::Copy => return a,
        Op::Not => {
            f.get(a);
            not_m(f);
        }
        Op::And => bin(f, op::I64_AND),
        Op::Or => bin(f, op::I64_OR),
        Op::Xor => bin(f, op::I64_XOR),
        Op::Nand => {
            bin(f, op::I64_AND);
            not_m(f);
        }
        Op::Nor => {
            bin(f, op::I64_OR);
            not_m(f);
        }
        Op::Xnor => {
            bin(f, op::I64_XOR);
            not_m(f);
        }
        Op::Mux => {
            // `if a != 0 { c } else { b }`
            f.get(c);
            f.get(b);
            nonzero(f, a);
            f.op(op::SELECT);
        }
        Op::Switch => {
            f.get(b);
            f.i64_const(0);
            nonzero(f, a);
            f.op(op::SELECT);
        }
        Op::AddSum => {
            bin(f, op::I64_ADD);
            f.get(c);
            f.op(op::I64_ADD);
            and_m(f);
        }
        Op::AddCarry => {
            // Carry out of `a + b + c` at bit `m`, as the interpreter's
            // 128-bit sum (see `clif::add_carry`).
            bin(f, op::I64_ADD);
            let s1 = result(f);
            f.get(s1);
            f.get(c);
            f.op(op::I64_ADD);
            let s2 = result(f);
            if m as u64 >= 64 {
                f.get(s1);
                f.get(a);
                f.op(op::I64_LT_U);
                f.op(op::I64_EXTEND_I32_U);
                f.get(s2);
                f.get(s1);
                f.op(op::I64_LT_U);
                f.op(op::I64_EXTEND_I32_U);
                f.op(op::I64_ADD);
            } else {
                f.get(s2);
                f.i64_const(m);
                f.op(op::I64_SHR_U);
            }
            f.i64_const(1);
            f.op(op::I64_AND);
        }
        Op::Eq => {
            bin(f, op::I64_EQ);
            f.op(op::I64_EXTEND_I32_U);
        }
        Op::LtU => {
            bin(f, op::I64_LT_U);
            f.op(op::I64_EXTEND_I32_U);
        }
        Op::ReduceOr => {
            nonzero(f, a);
            f.op(op::I64_EXTEND_I32_U);
        }
        Op::ReduceAnd => {
            f.get(a);
            f.i64_const(m);
            f.op(op::I64_EQ);
            f.op(op::I64_EXTEND_I32_U);
        }
        Op::ReduceXor => {
            f.get(a);
            f.op(op::I64_POPCNT);
            f.i64_const(1);
            f.op(op::I64_AND);
        }
        Op::Shr => {
            // Shift amounts are taken modulo 64, as Cranelift's.
            f.get(a);
            f.i64_const(i64::from(i.b));
            f.op(op::I64_SHR_U);
            and_m(f);
        }
        Op::Concat => {
            f.get(a);
            f.get(b);
            f.i64_const(i64::from(i.c));
            f.op(op::I64_SHL);
            f.op(op::I64_OR);
        }
        Op::Shl => {
            f.get(a);
            f.i64_const(i64::from(i.c));
            f.op(op::I64_SHR_U);
            f.i64_const(i64::from(i.b));
            f.op(op::I64_SHL);
            and_m(f);
        }
        Op::ShlV => {
            // `i64.shl` takes the amount modulo 64: `b & 63`.
            bin(f, op::I64_SHL);
            and_m(f);
        }
        Op::Sext => {
            // `if (a >> (b - 1)) & 1 == 1 { (a | !mask(b)) & m } else { a }`
            let w = i.b;
            f.get(a);
            f.i64_const(!mask(w as u8) as i64);
            f.op(op::I64_OR);
            and_m(f);
            f.get(a);
            f.get(a);
            f.i64_const(i64::from(w - 1));
            f.op(op::I64_SHR_U);
            f.op(op::I32_WRAP_I64);
            f.i32_const(1);
            f.op(op::I32_AND);
            f.op(op::SELECT);
        }
        Op::Clock => {
            // `((tick + c) % a) < b`
            f.get(tick);
            f.i64_const((tick_off + u64::from(i.c)) as i64);
            f.op(op::I64_ADD);
            f.i64_const(i64::from(i.a));
            f.op(op::I64_REM_U);
            f.i64_const(i64::from(i.b));
            f.op(op::I64_LT_U);
            f.op(op::I64_EXTEND_I32_U);
        }
        Op::MemRead => {
            mem_addr(f, mem_ptrs, p, mems, i.b, a);
            f.i64_load(0);
        }
        Op::SrNext => {
            // `if b != 0 { 0 } else if a != 0 { 1 } else { c }`
            f.i64_const(0);
            f.i64_const(1);
            f.get(c);
            nonzero(f, a);
            f.op(op::SELECT);
            nonzero(f, b);
            f.op(op::SELECT);
        }
    }
    result(f)
}

/// Assemble one packed operand word (see [`PackOperand`]) as a local.
fn pack_operand(f: &mut Func, sl: &mut Slots, p: &Program, o: &PackOperand) -> u32 {
    let base = sl.read(f, o.base);
    f.get(base);
    f.i64_const(o.keep as i64);
    f.op(op::I64_AND);
    if o.splat_mask != 0 {
        let s = sl.read(f, o.splat);
        // `-((s >> bit) & 1) & splat_mask`
        f.i64_const(0);
        f.get(s);
        f.i64_const(i64::from(o.splat_bit));
        f.op(op::I64_SHR_U);
        f.i64_const(1);
        f.op(op::I64_AND);
        f.op(op::I64_SUB);
        f.i64_const(o.splat_mask as i64);
        f.op(op::I64_AND);
        f.op(op::I64_OR);
    }
    let g = o.gather_start as usize;
    for gt in &p.gathers[g..g + o.gather_len as usize] {
        let s = sl.read(f, gt.slot);
        f.get(s);
        f.i64_const(i64::from(gt.bit));
        f.op(op::I64_SHR_U);
        f.i64_const(1);
        f.op(op::I64_AND);
        f.i64_const(i64::from(gt.lane));
        f.op(op::I64_SHL);
        f.op(op::I64_OR);
    }
    result(f)
}

/// The value of packed word `pk` (opcode `op`), as a local.
pub(super) fn emit_pack(f: &mut Func, sl: &mut Slots, p: &Program, op: Op, pk: &Pack) -> u32 {
    let n = arity(op);
    let mut v = [0u32; 3];
    for (j, o) in operands(pk).into_iter().enumerate().take(n) {
        v[j] = pack_operand(f, sl, p, o);
    }
    let [a, b, c] = v;
    let not = |f: &mut Func| {
        f.i64_const(-1);
        f.op(op::I64_XOR);
    };
    match op {
        Op::Copy => return a,
        Op::Not => {
            f.get(a);
            not(f);
        }
        Op::And | Op::Switch | Op::Or | Op::Xor | Op::Nand | Op::Nor | Op::Xnor => {
            f.get(a);
            f.get(b);
            f.op(match op {
                Op::And | Op::Switch | Op::Nand => op::I64_AND,
                Op::Or | Op::Nor => op::I64_OR,
                _ => op::I64_XOR,
            });
            if matches!(op, Op::Nand | Op::Nor | Op::Xnor) {
                not(f);
            }
        }
        Op::Mux => {
            // `(a & c) | (!a & b)`
            f.get(a);
            f.get(c);
            f.op(op::I64_AND);
            f.get(a);
            not(f);
            f.get(b);
            f.op(op::I64_AND);
            f.op(op::I64_OR);
        }
        _ => unreachable!("checked: not a packable op"),
    }
    result(f)
}

/// A RAM write commit (`RamWrite` or `RamWriteMasked`): slots are
/// unchanged, so the slot cache stays valid.
pub(super) fn emit_ram_write(
    f: &mut Func,
    sl: &mut Slots,
    p: &Program,
    c: &Commit,
    mems: u32,
    writes: u32,
) {
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
    let en = sl.read(f, we);
    let a = sl.read(f, addr);
    let d = sl.read(f, data);
    nonzero(f, en);
    f.if_();
    // The memory pointer is loaded inside the branch: not cached.
    let mut ptrs = HashMap::new();
    mem_addr(f, &mut ptrs, p, mems, mem, a);
    if masked {
        // `(*m & !bits) | (data & bits)`
        let at = f.local(I32);
        f.set(at);
        f.get(at);
        f.get(at);
        f.i64_load(0);
        f.get(en);
        f.i64_const(-1);
        f.op(op::I64_XOR);
        f.op(op::I64_AND);
        f.get(d);
        f.get(en);
        f.op(op::I64_AND);
        f.op(op::I64_OR);
    } else {
        f.get(d);
    }
    f.i64_store(0);
    f.get(writes);
    f.get(writes);
    f.i64_load(mem * 8);
    f.i64_const(1);
    f.op(op::I64_ADD);
    f.i64_store(mem * 8);
    f.op(op::END);
}
