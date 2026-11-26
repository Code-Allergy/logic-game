//! Superstep chunks as wasm functions (as `clif/superstep.rs`; see
//! `jit/superstep.rs` for the stream, the store plan and their rules).

use foldhash::{HashMap, HashMapExt};

use super::enc::{Func, op};
use super::{Slots, emit_instr, emit_pack, emit_ram_write, guard_test, lane};
use crate::compile::{Commit, Program};
use crate::sim::jit::superstep::{Plan, SItem, Stream};
use crate::sim::jit::{BodyRef, Item};

/// An open guarded range: the caches from before it, the slots written
/// inside it, and (with the counters) the skip counter to bump when it is
/// skipped, as a byte offset into `skips`.
struct Open {
    vals: HashMap<u32, u32>,
    ptrs: HashMap<u32, u32>,
    written: Vec<u32>,
    skip: Option<u32>,
}

/// Build chunk `ch` of the stream as a function `(slots, mems, tick,
/// writes, skips)`; `w` is the index (in `plan`) of its first write and
/// advances past its writes; `continues` is the region the previous chunk
/// ended in (0: none). Returns the function and the items left out as
/// dead.
#[allow(clippy::too_many_arguments)]
pub(super) fn build_chunk(
    p: &Program,
    bodies: &[BodyRef<'_>],
    st: &Stream,
    ch: &[(SItem, u32)],
    plan: &Plan,
    w: &mut usize,
    skip_base: &[usize],
    continues: u32,
) -> (Func, usize) {
    let mut f = Func::new(5);
    let (mems, tick, writes, skips) = (1, 2, 3, 4);
    #[cfg(not(feature = "counters"))]
    let _ = (skips, skip_base, continues);
    let mut sl = Slots::new(0, ch.len() * 2);
    let mut mem_ptrs: HashMap<u32, u32> = HashMap::new();
    let mut cur = 0u32;
    let mut open: Option<Open> = None;
    let mut dead = 0;
    let close = |f: &mut Func,
                 sl: &mut Slots,
                 mem_ptrs: &mut HashMap<u32, u32>,
                 open: &mut Option<Open>| {
        if let Some(o) = open.take() {
            if let Some(at) = o.skip {
                // Skipped: `skips[g] += 1`.
                f.op(op::ELSE);
                f.get(skips);
                f.get(skips);
                f.i64_load(at);
                f.i64_const(1);
                f.op(op::I64_ADD);
                f.i64_store(at);
            }
            f.op(op::END);
            sl.vals = o.vals;
            for s in o.written {
                sl.vals.remove(&s);
            }
            *mem_ptrs = o.ptrs;
        }
    };
    for &(item, seg) in ch {
        if seg != cur {
            close(&mut f, &mut sl, &mut mem_ptrs, &mut open);
            if seg > 0 {
                let (body, g) = st.regions[seg as usize - 1];
                guard_test(
                    &mut f,
                    &mut sl,
                    &bodies[body as usize].guards[g as usize].conds,
                );
                f.if_();
                // A range the previous chunk began was counted there.
                #[cfg(feature = "counters")]
                let skip = (seg != continues)
                    .then(|| ((skip_base[body as usize] + g as usize) * 8) as u32);
                #[cfg(not(feature = "counters"))]
                let skip = None;
                open = Some(Open {
                    vals: sl.vals.clone(),
                    ptrs: mem_ptrs.clone(),
                    written: Vec::new(),
                    skip,
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
                            &mut f,
                            &mut sl,
                            &mut mem_ptrs,
                            p,
                            (mems, tick, offset),
                            op,
                            i,
                            m,
                        );
                        put(&mut f, &mut sl, &mut open, plan, w, i.dst, r);
                    }
                    Item::Pack { op, k } => {
                        let pk = p.packs[k as usize];
                        let r = emit_pack(&mut f, &mut sl, p, op, &pk);
                        put(&mut f, &mut sl, &mut open, plan, w, pk.dst, r);
                        let s0 = pk.scatter_start as usize;
                        for x in &p.scatters[s0..s0 + pk.scatter_len as usize] {
                            let l = lane(&mut f, r, x.lane);
                            put(&mut f, &mut sl, &mut open, plan, w, x.slot, l);
                        }
                    }
                }
            }
            SItem::Commit { body, k } => {
                let c = bodies[body as usize].commits[k as usize];
                match c {
                    Commit::Copy { state, next } => {
                        let v = sl.read(&mut f, next);
                        put(&mut f, &mut sl, &mut open, plan, w, state, v);
                    }
                    Commit::RamWrite { .. } | Commit::RamWriteMasked { .. } => {
                        emit_ram_write(&mut f, &mut sl, p, &c, mems, writes);
                    }
                    Commit::MemClear { .. } => unreachable!("no superstep with a MemClear"),
                }
            }
        }
    }
    close(&mut f, &mut sl, &mut mem_ptrs, &mut open);
    (f, dead)
}

/// Write local `v` to slot `s` (the next write of the plan): into the
/// cache, and to memory if the plan stores it.
fn put(
    f: &mut Func,
    sl: &mut Slots,
    open: &mut Option<Open>,
    plan: &Plan,
    w: &mut usize,
    s: u32,
    v: u32,
) {
    let store = plan.store[*w];
    *w += 1;
    if let Some(o) = open.as_mut() {
        o.written.push(s);
    }
    sl.write(f, s, v, store);
}
