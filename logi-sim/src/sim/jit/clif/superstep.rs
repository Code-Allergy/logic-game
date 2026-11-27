//! Superstep chunks as Cranelift functions (see `jit/superstep.rs` for the
//! stream, the store plan and the rules they follow).

#[cfg(feature = "counters")]
use cranelift_codegen::ir::types::I64;
use cranelift_codegen::ir::{Function, InstBuilder, Value};
use cranelift_codegen::isa::TargetFrontendConfig;
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use foldhash::{HashMap, HashMapExt};

#[cfg(feature = "counters")]
use super::flags;
use super::{Load, SlotBits, Slots, emit_instr, emit_pack, emit_ram_write, guard_test};
use crate::compile::{Commit, Program};
use crate::sim::jit::superstep::{Plan, SItem, Stream};
use crate::sim::jit::{BodyRef, Item};

/// An open guarded range: the block after it, the caches from before it,
/// and the slots written inside it.
struct Open {
    merge: cranelift_codegen::ir::Block,
    vals: HashMap<u32, (Value, bool)>,
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
    bits: &SlotBits,
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
    let mut sl = Slots::new(slots, ch.len() * 2, bits);
    let mut mem_ptrs: HashMap<u32, Value> = HashMap::new();
    let mut cur = 0u32;
    let mut open: Option<Open> = None;
    let mut dead = 0;
    let close = |b: &mut FunctionBuilder<'_>,
                 sl: &mut Slots<'_>,
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
        sl.now += 1;
        if seg != cur {
            close(&mut b, &mut sl, &mut mem_ptrs, &mut open);
            if seg > 0 {
                let (body, g) = st.regions[seg as usize - 1];
                let test = guard_test(&mut b, &mut sl, &bodies[body as usize].guards[g as usize]);
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
                        let (r, bound) = emit_instr(
                            &mut b,
                            &mut sl,
                            &mut mem_ptrs,
                            p,
                            (mems, tick, offset),
                            op,
                            i,
                            m,
                        );
                        put(&mut b, &mut sl, &mut open, plan, w, i.dst, r, bound);
                    }
                    Item::Pack { op, k } => {
                        let pk = p.packs[k as usize];
                        let r = emit_pack(&mut b, &mut sl, p, op, &pk);
                        put(&mut b, &mut sl, &mut open, plan, w, pk.dst, r, u64::MAX);
                        let s0 = pk.scatter_start as usize;
                        for x in &p.scatters[s0..s0 + pk.scatter_len as usize] {
                            let lane = b.ins().ushr_imm_u(r, i64::from(x.lane));
                            let lane = b.ins().band_imm_u(lane, 1);
                            put(&mut b, &mut sl, &mut open, plan, w, x.slot, lane, 1);
                        }
                    }
                }
            }
            SItem::Commit { body, k } => {
                let c = bodies[body as usize].commits[k as usize];
                match c {
                    Commit::Copy { state, next } => {
                        let v = sl.read_as(&mut b, next, Load::Zext);
                        let bound = sl.bits(next);
                        put(&mut b, &mut sl, &mut open, plan, w, state, v, bound);
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
/// and to memory if the plan stores it (`bound`: the bits `v` can have).
#[allow(clippy::too_many_arguments)]
fn put(
    b: &mut FunctionBuilder<'_>,
    sl: &mut Slots<'_>,
    open: &mut Option<Open>,
    plan: &Plan,
    w: &mut usize,
    s: u32,
    v: Value,
    bound: u64,
) {
    let store = plan.store[*w];
    *w += 1;
    if let Some(o) = open.as_mut() {
        o.written.push(s);
    }
    sl.write(b, s, v, bound, store);
}
