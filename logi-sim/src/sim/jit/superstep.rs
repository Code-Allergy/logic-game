//! Supersteps (issue #117): the ticks of whole clock periods of a fused
//! `Sim::run` as one stream of native code.
//!
//! Between two ticks of one `Sim::run`, nothing observes the slots (see
//! `Sim::run`), so the commits and fused settles of `L` consecutive ticks
//! (`L` the clock-phase period, or a multiple of it) can run as one
//! straight line: commits of phase 0, the settle of phase 1, its commits,
//! ..., the settle of phase 0. They are concatenated into one item stream
//! (`stream`), cut into chunk functions (`build_chunk`) and called in
//! order by one entry, `fn(slots, mems, tick, mem_writes, skips)`
//! ([`SuperFn`]), which `Sim::run` calls once per superstep. The chunks
//! use a calling convention without callee-saved registers, so a call
//! costs no register saves; the entry keeps nothing live across them.
//!
//! Across the whole stream (not per body, as the plain settle chunks do):
//!
//! - **Forwarding.** Within a chunk every value stays in an SSA value,
//!   commits included: a register's state after its commit is the value
//!   of its next slot, read by the next settle without a store and a load.
//! - **Dead stores.** A slot write is stored only if a read of that value
//!   loads it from memory: a read in a later chunk, a read after the end
//!   of the guarded range that wrote it, or a read after the superstep
//!   (`live_end`: what the next commit reads, and every state slot). A
//!   state slot rewritten by the next tick's commit with no load in
//!   between is never stored; a value nothing reads at all is not even
//!   computed.
//!
//! # Guards
//!
//! A guarded range of a body is a branch, as in a plain settle chunk, and
//! one range of each tick is a separate region (the same guard in two
//! ticks never merges). A write inside a range may not happen, so it does
//! not end the previous value's life, and the previous value is stored (a
//! conservative rule that only matters when the same slot is written by
//! different phases). When a range ends, its writes leave the SSA cache,
//! so later reads load them (and they were stored).
//!
//! With the `counters` feature, a range that is skipped increments
//! `skips[g]` for its body's guard (`JitSuper::skip_base`), so that
//! `Sim::run` can count what did not run.

use cranelift_codegen::ir::condcodes::IntCC;
#[cfg(feature = "counters")]
use cranelift_codegen::ir::types::I64;
use cranelift_codegen::ir::{Function, InstBuilder, Value};
use cranelift_codegen::isa::TargetFrontendConfig;
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use foldhash::{HashMap, HashMapExt};

#[cfg(feature = "counters")]
use super::flags;
use super::{BodyRef, Item, Slots, emit_instr, emit_pack, emit_ram_write};
use crate::compile::{Commit, Program};

// Deliberate bugs for the tests to catch (0: none): 1, the stores after
// the superstep are dropped; 2, a superstep starts at any phase
// (`Sim::run`); 3, values read by a later chunk are not stored.
#[cfg(test)]
thread_local! {
    pub(crate) static MUTATION: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
}

/// One part of a superstep, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Part {
    /// The commits of body `body` (an index into the bodies compiled).
    Commit { body: usize },
    /// The settle of body `body` at tick `tick + offset` (`tick` is the
    /// superstep function's argument).
    Settle { body: usize, offset: u64 },
}

/// A superstep to compile (see the module documentation).
pub struct SuperRef<'a> {
    pub parts: &'a [Part],
    /// Ticks the superstep advances.
    pub ticks: u64,
    /// Slots read after the superstep before anything writes them: what
    /// the next commit reads and every state slot.
    pub live_end: &'a [u32],
    /// Items (instructions, packed words, commits) per chunk function.
    pub chunk: usize,
}

/// A superstep: `(slots, mems, tick, mem_writes, skips)`.
pub type SuperFn = unsafe extern "C" fn(*mut u64, *const *mut u64, u64, *mut u64, *mut u64);

/// A compiled superstep: one entry that calls its chunks in order.
pub struct JitSuper {
    pub(crate) entry: SuperFn,
    pub info: SuperInfo,
}

/// What a superstep is made of.
#[derive(Debug, Clone, Default)]
pub struct SuperInfo {
    /// Ticks one call advances.
    pub ticks: u64,
    /// Chunk functions the entry calls.
    pub chunks: usize,
    /// Per body (as `Sim`'s run numbers): the index of its first guard in
    /// `skips` (feature `counters`).
    pub skip_base: Vec<usize>,
    /// Guards over every body (the length of `skips`).
    pub skip_len: usize,
    /// Items in the stream (instructions, packed words and commits), and
    /// items left out because nothing reads their value.
    pub items: usize,
    pub dead: usize,
    /// Slot writes in the stream, and those stored to memory.
    pub writes: usize,
    pub stores: usize,
    /// Bytes of native code (chunks and entry).
    pub code_bytes: usize,
}

/// One item of the stream.
#[derive(Clone, Copy)]
pub(super) enum SItem {
    Settle { body: u32, item: Item, offset: u64 },
    Commit { body: u32, k: u32 },
}

/// The stream, cut into chunks: per chunk, its items with their region
/// (0: none; `k + 1`: `regions[k]`, a `(body, guard)`).
pub(super) struct Stream {
    pub chunks: Vec<Vec<(SItem, u32)>>,
    pub regions: Vec<(u32, u32)>,
}

pub(super) fn stream(bodies: &[BodyRef<'_>], s: &SuperRef<'_>) -> Stream {
    let mut flat: Vec<(SItem, u32)> = Vec::new();
    let mut regions = Vec::new();
    for part in s.parts {
        match *part {
            Part::Commit { body } => {
                for k in 0..bodies[body].commits.len() {
                    flat.push((
                        SItem::Commit {
                            body: body as u32,
                            k: k as u32,
                        },
                        0,
                    ));
                }
            }
            Part::Settle { body, offset } => {
                let base = regions.len() as u32;
                let b = &bodies[body];
                regions.extend((0..b.guards.len()).map(|g| (body as u32, g as u32)));
                for (item, seg) in super::chunks(b).into_iter().flatten() {
                    let seg = if seg == 0 { 0 } else { base + seg };
                    flat.push((
                        SItem::Settle {
                            body: body as u32,
                            item,
                            offset,
                        },
                        seg,
                    ));
                }
            }
        }
    }
    let chunks = flat.chunks(s.chunk.max(1)).map(<[_]>::to_vec).collect();
    Stream { chunks, regions }
}

/// Call `r` with every slot `item` reads and `w` with every slot it writes
/// (instructions: one, packed words: their word and scatters, copies: the
/// state).
fn accesses(
    p: &Program,
    bodies: &[BodyRef<'_>],
    item: SItem,
    mut r: impl FnMut(u32),
    mut w: impl FnMut(u32),
) {
    match item {
        SItem::Settle { body, item, .. } => {
            let b = &bodies[body as usize];
            item.reads(p, b, &mut r);
            match item {
                Item::Instr { k, .. } => w(b.instrs[k as usize].dst),
                Item::Pack { k, .. } => {
                    let pk = &p.packs[k as usize];
                    w(pk.dst);
                    let s0 = pk.scatter_start as usize;
                    for x in &p.scatters[s0..s0 + pk.scatter_len as usize] {
                        w(x.slot);
                    }
                }
            }
        }
        SItem::Commit { body, k } => {
            let c = bodies[body as usize].commits[k as usize];
            match c {
                Commit::Copy { state, next } => {
                    r(next);
                    w(state);
                }
                _ => c.reads().into_iter().for_each(r),
            }
        }
    }
}

const NONE: u32 = u32::MAX;

/// Which writes of the stream are stored, in stream order (one entry per
/// written slot, as `accesses` lists them), and which are read at all.
pub(super) struct Plan {
    pub store: Vec<bool>,
    pub used: Vec<bool>,
}

/// Decide the stores (see the module documentation). Walks the stream in
/// the order the code generator does, tracking per slot its last write:
/// `(write, chunk, region instance)`.
pub(super) fn plan(p: &Program, bodies: &[BodyRef<'_>], st: &Stream, live_end: &[u32]) -> Plan {
    let n = p.slot_count as usize;
    // Per slot: its last write `(write, chunk, region instance)`.
    let mut last = vec![(NONE, NONE, NONE); n];
    let mut store = Vec::new();
    let mut used = Vec::new();
    let mut instance = 0u32;
    struct At {
        chunk: u32,
        region: u32,
    }
    let read =
        |last: &[(u32, u32, u32)], store: &mut Vec<bool>, used: &mut Vec<bool>, s: u32, at: &At| {
            let (w, c, r) = last[s as usize];
            if w != NONE {
                used[w as usize] = true;
                let other_chunk = c != at.chunk;
                #[cfg(test)]
                let other_chunk = other_chunk && MUTATION.get() != 3;
                if other_chunk || (r != 0 && r != at.region) {
                    store[w as usize] = true;
                }
            }
        };
    for (ci, ch) in st.chunks.iter().enumerate() {
        let mut cur = 0u32;
        let mut at = At {
            chunk: ci as u32,
            region: 0,
        };
        for &(item, seg) in ch {
            if seg != cur {
                at.region = 0;
                if seg > 0 {
                    instance += 1;
                    let (body, g) = st.regions[seg as usize - 1];
                    for c in &bodies[body as usize].guards[g as usize].conds {
                        read(&last, &mut store, &mut used, c.slot, &at);
                    }
                    at.region = instance;
                }
                cur = seg;
            }
            let mut writes = smallvec::SmallVec::<[u32; 4]>::new();
            accesses(
                p,
                bodies,
                item,
                |s| read(&last, &mut store, &mut used, s, &at),
                |s| writes.push(s),
            );
            for s in writes {
                let prev = last[s as usize].0;
                if at.region != 0 && prev != NONE {
                    // The write may not happen: the previous value lives on.
                    store[prev as usize] = true;
                    used[prev as usize] = true;
                }
                last[s as usize] = (store.len() as u32, at.chunk, at.region);
                store.push(false);
                used.push(false);
            }
        }
    }
    #[cfg(test)]
    let live_end = if MUTATION.get() == 1 { &[] } else { live_end };
    for &s in live_end {
        let w = last[s as usize].0;
        if w != NONE {
            store[w as usize] = true;
            used[w as usize] = true;
        }
    }
    Plan { store, used }
}

/// An open guarded range: the block after it, the caches from before it,
/// and the slots written inside it.
struct Open {
    merge: cranelift_codegen::ir::Block,
    vals: HashMap<u32, Value>,
    ptrs: HashMap<u32, Value>,
    written: Vec<u32>,
}

/// Build chunk `ch` of the stream; `w` is the index (in `plan`) of its
/// first write and advances past its writes; `continues` is the region
/// the previous chunk ended in (0: none). Returns the items left out as
/// dead.
#[allow(clippy::too_many_arguments)]
pub(super) fn build_chunk(
    cfg: TargetFrontendConfig,
    func: &mut Function,
    fctx: &mut FunctionBuilderContext,
    p: &Program,
    bodies: &[BodyRef<'_>],
    st: &Stream,
    ch: &[(SItem, u32)],
    plan: &Plan,
    w: &mut usize,
    skip_base: &[usize],
    continues: u32,
) -> usize {
    let mut b = FunctionBuilder::new(func, fctx);
    let entry = b.create_block();
    b.append_block_params_for_function_params(entry);
    b.switch_to_block(entry);
    b.seal_block(entry);
    let params = b.block_params(entry).to_vec();
    let (slots, mems, tick, writes, skips) =
        (params[0], params[1], params[2], params[3], params[4]);
    #[cfg(not(feature = "counters"))]
    let _ = (skips, skip_base, continues);
    let mut sl = Slots::new(slots, ch.len() * 2);
    let mut mem_ptrs: HashMap<u32, Value> = HashMap::new();
    let mut cur = 0u32;
    let mut open: Option<Open> = None;
    let mut dead = 0;
    let close = |b: &mut FunctionBuilder<'_>,
                 sl: &mut Slots,
                 mem_ptrs: &mut HashMap<u32, Value>,
                 open: &mut Option<Open>| {
        if let Some(o) = open.take() {
            b.ins().jump(o.merge, &[]);
            b.switch_to_block(o.merge);
            b.seal_block(o.merge);
            sl.vals = o.vals;
            for s in o.written {
                sl.vals.remove(&s);
            }
            *mem_ptrs = o.ptrs;
        }
    };
    for &(item, seg) in ch {
        if seg != cur {
            close(&mut b, &mut sl, &mut mem_ptrs, &mut open);
            if seg > 0 {
                let (body, g) = st.regions[seg as usize - 1];
                let mut test: Option<Value> = None;
                for c in &bodies[body as usize].guards[g as usize].conds {
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
                // A range the previous chunk began was counted there.
                #[cfg(feature = "counters")]
                if seg != continues {
                    let skipped = b.create_block();
                    b.ins().brif(test, then, &[], skipped, &[]);
                    b.switch_to_block(skipped);
                    b.seal_block(skipped);
                    let at = ((skip_base[body as usize] + g as usize) * 8) as i32;
                    let n = b.ins().load(I64, flags(), skips, at);
                    let n = b.ins().iadd_imm_u(n, 1);
                    b.ins().store(flags(), n, skips, at);
                    b.ins().jump(merge, &[]);
                }
                #[cfg(feature = "counters")]
                if seg == continues {
                    b.ins().brif(test, then, &[], merge, &[]);
                }
                #[cfg(not(feature = "counters"))]
                b.ins().brif(test, then, &[], merge, &[]);
                b.switch_to_block(then);
                b.seal_block(then);
                open = Some(Open {
                    merge,
                    vals: sl.vals.clone(),
                    ptrs: mem_ptrs.clone(),
                    written: Vec::new(),
                });
            }
            cur = seg;
        }
        match item {
            SItem::Settle { body, item, offset } => {
                let bd = &bodies[body as usize];
                match item {
                    Item::Instr { op, k } => {
                        let i = bd.instrs[k as usize];
                        if !plan.used[*w] {
                            // Nothing reads this value: not even computed.
                            *w += 1;
                            dead += 1;
                            continue;
                        }
                        let m = bd.imms[k as usize];
                        let r = emit_instr(
                            &mut b,
                            &mut sl,
                            &mut mem_ptrs,
                            p,
                            (mems, tick, offset),
                            op,
                            i,
                            m,
                        );
                        put(&mut b, &mut sl, &mut open, plan, w, i.dst, r);
                    }
                    Item::Pack { op, k } => {
                        let pk = p.packs[k as usize];
                        let r = emit_pack(&mut b, &mut sl, p, op, &pk);
                        put(&mut b, &mut sl, &mut open, plan, w, pk.dst, r);
                        let s0 = pk.scatter_start as usize;
                        for x in &p.scatters[s0..s0 + pk.scatter_len as usize] {
                            let lane = b.ins().ushr_imm_u(r, i64::from(x.lane));
                            let lane = b.ins().band_imm_u(lane, 1);
                            put(&mut b, &mut sl, &mut open, plan, w, x.slot, lane);
                        }
                    }
                }
            }
            SItem::Commit { body, k } => {
                let c = bodies[body as usize].commits[k as usize];
                match c {
                    Commit::Copy { state, next } => {
                        let v = sl.read(&mut b, next);
                        put(&mut b, &mut sl, &mut open, plan, w, state, v);
                    }
                    Commit::RamWrite { .. } | Commit::RamWriteMasked { .. } => {
                        emit_ram_write(&mut b, &mut sl, p, &c, mems, writes);
                    }
                    Commit::MemClear { .. } => unreachable!("no superstep with a MemClear"),
                }
            }
        }
    }
    close(&mut b, &mut sl, &mut mem_ptrs, &mut open);
    b.ins().return_(&[]);
    b.finalize(cfg);
    dead
}

/// Write `v` to slot `s` (the next write of the plan): into the SSA cache,
/// and to memory if the plan stores it.
fn put(
    b: &mut FunctionBuilder<'_>,
    sl: &mut Slots,
    open: &mut Option<Open>,
    plan: &Plan,
    w: &mut usize,
    s: u32,
    v: Value,
) {
    let store = plan.store[*w];
    *w += 1;
    if let Some(o) = open.as_mut() {
        o.written.push(s);
    }
    sl.write(b, s, v, store);
}
