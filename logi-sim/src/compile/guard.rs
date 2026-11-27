//! Enable-guarded cones (issue #117, stage 6).
//!
//! A settle is straight-line code that evaluates every instruction of its
//! body each tick, but much of a design only computes the next value of
//! state whose enable is 0 on most ticks: the data input of a register
//! (`next = Mux(en, state, d)`) matters only when `en` is 1. [`guard`]
//! finds, for every `Mux` arm (and the data of a `Switch`), its
//! *exclusive cone*: the instructions whose every consumer, transitively,
//! is that arm. When the select picks the other arm, nothing reads the
//! cone, so the body can skip it. Nested arms give nested regions (a reset
//! mux around an enable mux), and sibling regions with the same condition
//! (several registers sharing one enable) are merged into one.
//!
//! # What counts as a consumer
//!
//! An instruction operand, a commit read, and, for a body whose slots are
//! observed after the settle (a full settle), every slot the program
//! exposes (`phases::exposed`: the debug map, views, shadows, ports and
//! state). A slot read by anything outside the arm, or exposed, is not in
//! the arm's cone, so a skipped region only ever holds slots nothing
//! observes: they keep stale values, exactly like the next-state
//! temporaries a clock-phase variant does not compute. A fused-run body
//! (`Sim::run`'s intermediate ticks) is observed by its commits only.
//!
//! # Layout
//!
//! The result keeps every instruction (with its rank) and the commits, and
//! reorders the instructions into *segments*: runs of blocks that either
//! always run, or run only when every [`Cond`] of their [`Guard`] holds
//! (the conditions of the enclosing regions, outermost first, then the
//! region's own). A region is placed as one contiguous range at its
//! *ready* point: after every value it reads from outside (and its
//! condition) is computed, and before the instruction consuming it. Inside
//! a segment, blocks are `(rank, op)` runs whose instructions never read
//! each other, as in any body. A guard's conditions are slots computed
//! before it: an outer condition before its region began, the own one
//! before the region's ready point. A nested condition read while an outer
//! one is false would be stale, which is why the conditions are tested
//! outermost first and the region runs only if all hold.
//!
//! Regions with fewer than [`MIN_REGION`] instructions are not worth a
//! branch and stay in their parent.
//!
//! The pass is linear in the body (plus the depth of nested regions per
//! operand) and runs when a `Sim` builds its bodies, like tick fusion's
//! pruning; the compiled `Program` is unchanged.

use super::{Block, Body, Cond, FastMap, Guard, Op, Program};

/// Fewest instructions (with nested regions) a region needs to be guarded.
/// A guard costs a test and a branch, splits blocks (interpreter) and
/// ends the SSA cache's reach (JIT). Measured on the NES console (fused
/// batches): 4 runs 791 ops per tick and 8 to 12 run fastest on the JIT
/// (12 also keeps the interpreter at least as fast as without guards);
/// past 24 too little is guarded.
pub const MIN_REGION: usize = 12;

const NONE: u32 = u32::MAX;

/// A region: the exclusive cone of one mux arm.
#[derive(Clone, Copy)]
struct Region {
    parent: u32,
    depth: u32,
    cond: Cond,
    /// Rank of the instruction consuming the region (the mux).
    mux_rank: u32,
}

/// `body` with every exclusive mux-arm cone of at least [`MIN_REGION`]
/// instructions guarded (see the module docs), or `None` when there is
/// none (or the body is packed, or already guarded). `exposed`: the slots
/// observed after the settle besides the commits' reads (`None` for a
/// fused-run body, which only its commits observe).
pub fn guard(p: &Program, body: &Body, exposed: Option<&[bool]>) -> Option<Body> {
    guard_with(p.slot_count as usize, body, exposed, MIN_REGION)
}

/// [`guard`] with the smallest region size as a parameter.
pub fn guard_with(
    slot_count: usize,
    body: &Body,
    exposed: Option<&[bool]>,
    min_region: usize,
) -> Option<Body> {
    if body.blocks.iter().any(|b| b.packed) || !body.guards.is_empty() {
        return None;
    }
    if body.blocks.windows(2).any(|w| w[0].rank > w[1].rank) {
        return None;
    }
    let m = body.instrs.len();
    let mut writer = vec![NONE; slot_count];
    let mut op_rank = vec![(Op::Copy, 0u32); m];
    for b in &body.blocks {
        for k in b.start..b.start + b.len {
            writer[body.instrs[k as usize].dst as usize] = k;
            op_rank[k as usize] = (b.op, b.rank);
        }
    }
    // Rank + 1 of the value in a slot (0 for slots no instruction writes).
    let ready_of = |s: u32| match writer[s as usize] {
        NONE => 0,
        w => op_rank[w as usize].1 + 1,
    };

    // Regions, root first; a child is created after its parent.
    let mut regions = vec![Region {
        parent: NONE,
        depth: 0,
        cond: Cond {
            slot: 0,
            when: true,
        },
        mux_rank: 0,
    }];
    let mut child: FastMap<(u32, u8), u32> = FastMap::default();
    // Per slot: the deepest region containing every use seen so far.
    let mut acc = vec![NONE; slot_count];
    for c in &body.commits {
        for s in c.reads() {
            acc[s as usize] = 0;
        }
    }
    if let Some(e) = exposed {
        for (s, &x) in e.iter().enumerate() {
            if x {
                acc[s] = 0;
            }
        }
    }
    let lca = |regions: &[Region], mut a: u32, mut b: u32| {
        if a == NONE {
            return b;
        }
        while regions[a as usize].depth > regions[b as usize].depth {
            a = regions[a as usize].parent;
        }
        while regions[b as usize].depth > regions[a as usize].depth {
            b = regions[b as usize].parent;
        }
        while a != b {
            a = regions[a as usize].parent;
            b = regions[b as usize].parent;
        }
        a
    };
    // Users come after what they read: walk the body backwards.
    let mut home = vec![0u32; m];
    for b in body.blocks.iter().rev() {
        let (sa, sb, sc) = b.op.slot_operands();
        for k in (b.start..b.start + b.len).rev() {
            let i = body.instrs[k as usize];
            let h = match acc[i.dst as usize] {
                NONE => 0,
                h => h,
            };
            home[k as usize] = h;
            for (port, is, s) in [(0u8, sa, i.a), (1, sb, i.b), (2, sc, i.c)] {
                if !is || writer[s as usize] == NONE {
                    continue;
                }
                let cond = match (b.op, port) {
                    (Op::Mux, 1) => Some(Cond {
                        slot: i.a,
                        when: false,
                    }),
                    (Op::Mux, 2) | (Op::Switch, 1) => Some(Cond {
                        slot: i.a,
                        when: true,
                    }),
                    _ => None,
                };
                let r = match cond {
                    None => h,
                    Some(cond) => *child.entry((k, port)).or_insert_with(|| {
                        regions.push(Region {
                            parent: h,
                            depth: regions[h as usize].depth + 1,
                            cond,
                            mux_rank: b.rank,
                        });
                        regions.len() as u32 - 1
                    }),
                };
                acc[s as usize] = lca(&regions, acc[s as usize], r);
            }
        }
    }
    let nr = regions.len();
    if nr == 1 {
        return None;
    }

    // Keep regions big enough to pay for their branch; the others' own
    // instructions run in the nearest kept ancestor.
    let mut size = vec![0usize; nr];
    for &h in &home {
        size[h as usize] += 1;
    }
    for r in (1..nr).rev() {
        let parent = regions[r].parent as usize;
        size[parent] += size[r];
    }
    let mut kmap: Vec<u32> = (0..nr as u32).collect();
    for r in 1..nr {
        if size[r] < min_region {
            kmap[r] = kmap[regions[r].parent as usize];
        }
    }
    // Kept parent of each kept region.
    let eff_parent = |r: usize| kmap[regions[r].parent as usize];
    if (1..nr).all(|r| kmap[r] != r as u32) {
        return None;
    }
    for h in &mut home {
        *h = kmap[*h as usize];
    }

    // Ready point of each kept region: past every value its subtree reads
    // from outside, and past its condition. Propagating a child's ready
    // point to its parent is conservative (the parent starts no earlier
    // than its children could), and stays before the parent's mux: a
    // child's mux is inside the parent, so it ranks below the parent's.
    let mut ready = vec![0u32; nr];
    for r in 1..nr {
        if kmap[r] == r as u32 {
            ready[r] = ready_of(regions[r].cond.slot);
        }
    }
    for b in &body.blocks {
        let (sa, sb, sc) = b.op.slot_operands();
        for k in b.start..b.start + b.len {
            let h = home[k as usize];
            if h == 0 {
                continue;
            }
            let i = body.instrs[k as usize];
            for (is, s) in [(sa, i.a), (sb, i.b), (sc, i.c)] {
                if !is {
                    continue;
                }
                let w = writer[s as usize];
                let inside = w != NONE && {
                    let hw = home[w as usize];
                    hw == h || (hw != 0 && eff_parent(hw as usize) == h)
                };
                if !inside {
                    ready[h as usize] = ready[h as usize].max(ready_of(s));
                }
            }
        }
    }
    for r in (1..nr).rev() {
        if kmap[r] == r as u32 {
            let parent = eff_parent(r) as usize;
            if parent != 0 {
                ready[parent] = ready[parent].max(ready[r]);
            }
        }
    }

    // Merge sibling regions with the same condition (registers sharing an
    // enable), parents first, where one ready point fits before every
    // consumer.
    let mut rep: Vec<u32> = (0..nr as u32).collect();
    let mut mux_rank: Vec<u32> = regions.iter().map(|r| r.mux_rank).collect();
    let mut by_depth: Vec<usize> = (1..nr).filter(|&r| kmap[r] == r as u32).collect();
    by_depth.sort_by_key(|&r| (regions[r].depth, r));
    let mut start = 0;
    while start < by_depth.len() {
        // Every region of one depth (kept regions keep their relative
        // depth order, so parents are merged before their children).
        let d = regions[by_depth[start]].depth;
        let end = start
            + by_depth[start..]
                .iter()
                .take_while(|&&r| regions[r].depth == d)
                .count();
        let mut groups: FastMap<(u32, u32, bool), Vec<usize>> = FastMap::default();
        for &r in &by_depth[start..end] {
            let parent = rep[eff_parent(r) as usize];
            let c = regions[r].cond;
            groups.entry((parent, c.slot, c.when)).or_default().push(r);
        }
        let mut groups: Vec<Vec<usize>> = groups.into_values().filter(|g| g.len() > 1).collect();
        groups.sort();
        for mut g in groups {
            g.sort_by_key(|&r| (ready[r], r));
            let mut lead = g[0];
            for &r in &g[1..] {
                let rd = ready[lead].max(ready[r]);
                let mx = mux_rank[lead].min(mux_rank[r]);
                if rd <= mx {
                    rep[r] = lead as u32;
                    ready[lead] = rd;
                    mux_rank[lead] = mx;
                } else {
                    lead = r;
                }
            }
        }
        start = end;
    }
    // The final region of an instruction, and the parent of a final region.
    let fin = |r: u32| rep[r as usize];
    let parent_of = |r: usize| fin(eff_parent(r));

    // Items of each final region: its own instructions and its child
    // regions, ordered by `(position, kind)`: a child region at its ready
    // point, before the instructions of that rank.
    let mut items: Vec<Vec<(u32, u8, u8, u32)>> = vec![Vec::new(); nr];
    for (k, &h) in home.iter().enumerate() {
        let (op, rank) = op_rank[k];
        items[fin(h) as usize].push((rank, 1, op as u8, k as u32));
    }
    for r in 1..nr {
        if kmap[r] == r as u32 && rep[r] == r as u32 {
            items[parent_of(r) as usize].push((ready[r], 0, 0, r as u32));
        }
    }
    for list in &mut items {
        list.sort_unstable();
    }
    // Conditions of each final region: its parents', then its own.
    let mut conds: Vec<Vec<Cond>> = vec![Vec::new(); nr];
    for r in 1..nr {
        if kmap[r] == r as u32 && rep[r] == r as u32 {
            let mut chain = Vec::new();
            let mut x = r;
            while x != 0 {
                chain.push(regions[x].cond);
                x = parent_of(x) as usize;
            }
            chain.reverse();
            conds[r] = chain;
        }
    }

    // Emit depth first; each region's items are contiguous.
    let mut order: Vec<(u32, u32)> = Vec::with_capacity(m);
    let mut stack: Vec<(u32, usize)> = vec![(0, 0)];
    while let Some(top) = stack.last_mut() {
        let (r, at) = *top;
        let list = &items[r as usize];
        if at == list.len() {
            stack.pop();
            continue;
        }
        top.1 += 1;
        let (_, kind, _, x) = list[at];
        if kind == 1 {
            order.push((x, r));
        } else {
            stack.push((x, 0));
        }
    }
    debug_assert_eq!(order.len(), m);

    let mut out = Body {
        commits: body.commits.clone(),
        ..Body::default()
    };
    let mut seg_region = 0u32;
    let mut seg_start = 0u32;
    let close = |out: &mut Body, region: u32, from: u32| {
        let to = out.blocks.len() as u32;
        if region != 0 && to > from {
            out.guards.push(Guard {
                start: from,
                end: to,
                conds: conds[region as usize].clone(),
                any: Vec::new(),
            });
        }
    };
    for (pos, &(k, r)) in order.iter().enumerate() {
        let (op, rank) = op_rank[k as usize];
        let new_seg = pos == 0 || r != seg_region;
        if new_seg {
            close(&mut out, seg_region, seg_start);
            seg_region = r;
            seg_start = out.blocks.len() as u32;
        }
        match out.blocks.last_mut() {
            Some(b) if !new_seg && b.rank == rank && b.op == op => b.len += 1,
            _ => out.blocks.push(Block {
                op,
                rank,
                start: out.instrs.len() as u32,
                len: 1,
                packed: false,
            }),
        }
        out.instrs.push(body.instrs[k as usize]);
        out.imms.push(body.imms[k as usize]);
    }
    close(&mut out, seg_region, seg_start);
    if out.guards.is_empty() {
        return None;
    }
    Some(out)
}

#[cfg(test)]
mod tests;
