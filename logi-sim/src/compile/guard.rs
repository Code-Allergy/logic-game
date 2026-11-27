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
//! mux around an enable mux).
//!
//! Arms with one condition under one parent region (registers sharing an
//! enable, or the fields of one word register that lifting split into
//! several muxes of one select) are one region from the start, so the
//! values only they read are guarded with them too.
//!
//! An arm too small to pay for a branch (fewer than [`MIN_REGION`]
//! instructions with its nested arms) runs in its parent. Such arms of a
//! select at 1 under one kept region are *grouped* (at most [`MAX_ANY`]
//! conditions, at least [`MIN_REGION`] instructions to a group): a group
//! runs when any of its conditions holds ([`Guard::any`]), and holds the
//! values only its arms read.
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
//! region's own) and, for a group, one of its `any`. Each region is one
//! contiguous range, its items (its own instructions and its child
//! regions) in a topological order of the reads between them, by rank
//! where the reads allow: a region where its inputs and conditions are
//! computed (its *ready* point), a mux that reads a region later than its
//! rank when the region is ready after it (and what reads the mux after
//! it). A guard's conditions are slots computed before it: an outer
//! condition before its region began, the own ones before the region. A
//! nested condition read while an outer one is false would be stale,
//! which is why a region runs only if all hold. Inside a segment, blocks
//! are `(rank, op)` runs whose instructions never read each other, as in
//! any body.
//!
//! A region of several arms can be impossible to make contiguous: when
//! one arm reads, through instructions outside the region, the mux of
//! another (a chain of registers under one enable). The layout then finds
//! no order, and those arms are split off into regions of their own and
//! the tree is built again; the last round merges and groups nothing,
//! which always lays out.
//!
//! The pass runs when a `Sim` builds its bodies, like tick fusion's
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

/// A region: the exclusive cone of one mux arm, of several arms with one
/// condition under one parent, or of a group of small arms with different
/// conditions (see [`Tree::build`]).
#[derive(Clone)]
struct Region {
    parent: u32,
    depth: u32,
    cond: Cond,
    /// A group's conditions (it runs when any holds); empty otherwise.
    any: Vec<Cond>,
    /// The region holds more than one arm.
    merged: bool,
}

/// Most rounds of [`Tree::build`] that undo groups and merged regions the
/// layout cannot make contiguous, before arms are no longer merged or
/// grouped at all.
const MERGE_ROUNDS: usize = 8;

/// Most conditions of one group of small arms ([`Tree::groups`]): the
/// more conditions, the more often one of them holds.
pub const MAX_ANY: usize = 4;

/// Region key of the arms of group `g` under one parent.
const GROUP: u32 = 1 << 31;

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
    let cx = Cx {
        body,
        exposed,
        min_region,
        writer,
        op_rank,
    };
    // Arms of one condition under one parent share a region; the first
    // tree's small arms are grouped (`Tree::groups`) and the tree is built
    // again with the groups. Merged regions no order makes contiguous are
    // split into their arms and the tree is built again. The last round
    // merges and groups nothing (each arm its own region, which always
    // lays out).
    let mut undo = Undo {
        ungroup: vec![false; 3 * m],
        split: vec![false; 3 * m],
    };
    let mut groups: Option<Grouping> = None;
    let none = Grouping::default();
    for round in 0..=MERGE_ROUNDS {
        let merge = (round < MERGE_ROUNDS).then_some(&undo);
        let g = groups.as_ref().filter(|_| merge.is_some()).unwrap_or(&none);
        let tree = Tree::build(&cx, merge, g);
        if merge.is_some() && groups.is_none() {
            let g = tree.groups(&cx);
            let again = !g.group.is_empty();
            groups = Some(g);
            if again {
                continue;
            }
        }
        let result = tree.layout(&cx);
        #[cfg(test)]
        if let Err((_, group)) = &result {
            UNDONE.with(|u| u.set(u.get() + if *group { 1 << 16 } else { 1 }));
        }
        match result {
            Ok(b) => return b,
            Err((arms, group)) => {
                let set = if group {
                    &mut undo.ungroup
                } else {
                    &mut undo.split
                };
                for (k, port) in arms {
                    set[arm(k, port)] = true;
                }
            }
        }
    }
    None
}

#[cfg(test)]
thread_local! {
    /// Layouts undone on this thread: merged regions split (low 16 bits)
    /// and groups undone (above).
    static UNDONE: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// What a layout could not make contiguous: the arms of the regions, and
/// whether they are groups (else merged regions).
type Undone = (Vec<(u32, u8)>, bool);

/// Index of operand `port` of instruction `k` in the per-arm tables.
fn arm(k: u32, port: u8) -> usize {
    k as usize * 3 + port as usize
}

/// Per arm ([`arm`]): not to group, and not to merge either (each its
/// own region).
struct Undo {
    ungroup: Vec<bool>,
    split: Vec<bool>,
}

/// Per arm ([`arm`]): its group's number (`NONE` for none), and whether
/// it gets no region (see [`Tree::groups`]). Empty: nothing grouped.
#[derive(Default)]
struct Grouping {
    group: Vec<u32>,
    flat: Vec<bool>,
}

/// What every round of [`Tree::build`] reads.
struct Cx<'a> {
    body: &'a Body,
    exposed: Option<&'a [bool]>,
    min_region: usize,
    /// Instruction writing each slot (`NONE` for none).
    writer: Vec<u32>,
    /// Op and rank of each instruction.
    op_rank: Vec<(Op, u32)>,
}

impl Cx<'_> {
    /// The instruction writing an operand (`NONE` for none, or for an
    /// operand that is a literal, not a slot: `is` false).
    fn operand_writer(&self, is: bool, s: u32) -> u32 {
        if is { self.writer[s as usize] } else { NONE }
    }

    /// Rank + 1 of the value in a slot (0 for slots no instruction writes).
    fn ready_of(&self, s: u32) -> u32 {
        match self.writer[s as usize] {
            NONE => 0,
            w => self.op_rank[w as usize].1 + 1,
        }
    }
}

/// The regions of a body and where each instruction runs.
struct Tree {
    regions: Vec<Region>,
    /// Kept region (itself, or its nearest kept ancestor) of each region.
    kmap: Vec<u32>,
    /// Region of each instruction (a kept one).
    home: Vec<u32>,
    /// Region of each instruction, kept or not.
    home0: Vec<u32>,
    /// Ready point of each region (its subtree's): where it is placed in
    /// rank order when the reads allow.
    ready: Vec<u32>,
    /// Instructions of each region with its subtree.
    size: Vec<usize>,
    /// Every arm: (instruction, port, region).
    arms: Vec<(u32, u8, u32)>,
}

impl Tree {
    /// The region tree. With `merge`, the arms with one condition under
    /// one parent region (registers sharing an enable, the fields of a
    /// word register split by lifting) are one region from the start, so
    /// values they share stay inside it, except the arms `merge` splits
    /// (instruction, port), which get a region each. The arms in `groups`
    /// (with a group number, unless `merge` ungroups them) share a region
    /// per parent, which runs when any of their conditions holds, and the
    /// other arms in `flat` (too small to guard) get no region. Without
    /// `merge`, every arm gets a region of its own.
    fn build(cx: &Cx<'_>, merge: Option<&Undo>, grouping: &Grouping) -> Tree {
        let body = cx.body;
        let m = body.instrs.len();
        let slot_count = cx.writer.len();
        // Regions, root first; a child is created after its parent.
        let mut regions = vec![Region {
            parent: NONE,
            depth: 0,
            cond: Cond {
                slot: 0,
                when: true,
            },
            any: Vec::new(),
            merged: false,
        }];
        // Region of each (parent, condition) when merged.
        let mut shared: FastMap<(u32, u32, bool), u32> = FastMap::default();
        let mut arms: Vec<(u32, u8, u32)> = Vec::new();
        // Per slot: the deepest region containing every use seen so far.
        let mut acc = vec![NONE; slot_count];
        for c in &body.commits {
            for s in c.reads() {
                acc[s as usize] = 0;
            }
        }
        if let Some(e) = cx.exposed {
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
                    if !is || cx.writer[s as usize] == NONE {
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
                    let a = arm(k, port);
                    // A small arm of the tree before grouping runs in its
                    // parent anyway: without a region, its muxes (and the
                    // arms below) sit in the parent a group belongs to.
                    let group = merge
                        .filter(|u| !u.split[a] && !u.ungroup[a])
                        .and_then(|_| grouping.group.get(a).copied())
                        .filter(|&g| g != NONE);
                    let flatten =
                        merge.is_some() && grouping.flat.get(a) == Some(&true) && group.is_none();
                    let r = match cond {
                        None => h,
                        Some(_) if flatten => h,
                        Some(cond) => {
                            let new = |regions: &mut Vec<Region>| {
                                regions.push(Region {
                                    parent: h,
                                    depth: regions[h as usize].depth + 1,
                                    cond,
                                    any: Vec::new(),
                                    merged: false,
                                });
                                regions.len() as u32 - 1
                            };
                            let r = match merge {
                                Some(u) if !u.split[a] => {
                                    let key = match group {
                                        Some(g) => (h, GROUP | g, false),
                                        None => (h, cond.slot, cond.when),
                                    };
                                    let r = match shared.get(&key) {
                                        Some(&r) => {
                                            regions[r as usize].merged = true;
                                            r
                                        }
                                        None => {
                                            let r = new(&mut regions);
                                            shared.insert(key, r);
                                            r
                                        }
                                    };
                                    let g = &mut regions[r as usize];
                                    if group.is_some() && !g.any.contains(&cond) {
                                        g.any.push(cond);
                                    }
                                    r
                                }
                                _ => new(&mut regions),
                            };
                            arms.push((k, port, r));
                            r
                        }
                    };
                    acc[s as usize] = lca(&regions, acc[s as usize], r);
                }
            }
        }
        let nr = regions.len();

        // Keep regions big enough to pay for their branch; the others' own
        // instructions run in the nearest kept ancestor. Regions inside a
        // group run with it (a guard tests one group).
        let mut size = vec![0usize; nr];
        for &h in &home {
            size[h as usize] += 1;
        }
        for r in (1..nr).rev() {
            let parent = regions[r].parent as usize;
            size[parent] += size[r];
        }
        let mut kmap: Vec<u32> = (0..nr as u32).collect();
        let mut in_group = vec![false; nr];
        for r in 1..nr {
            let parent = regions[r].parent as usize;
            in_group[r] = in_group[parent] || !regions[r].any.is_empty();
            if size[r] < cx.min_region || in_group[parent] {
                kmap[r] = kmap[parent];
            }
        }
        // Ready point of every region (its subtree's): past every value it
        // reads from outside, and past its conditions. Propagating a
        // child's to its parent is conservative.
        let mut ready = vec![0u32; nr];
        for (r, g) in regions.iter().enumerate().skip(1) {
            ready[r] = std::iter::once(&g.cond)
                .chain(&g.any)
                .map(|c| cx.ready_of(c.slot))
                .max()
                .unwrap_or(0);
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
                    let w = cx.operand_writer(is, s);
                    let inside = w != NONE && {
                        let hw = home[w as usize];
                        hw == h || (hw != 0 && regions[hw as usize].parent == h)
                    };
                    if is && !inside {
                        ready[h as usize] = ready[h as usize].max(cx.ready_of(s));
                    }
                }
            }
        }
        for r in (1..nr).rev() {
            let parent = regions[r].parent as usize;
            if parent != 0 {
                ready[parent] = ready[parent].max(ready[r]);
            }
        }
        let home0 = home.clone();
        for h in &mut home {
            *h = kmap[*h as usize];
        }
        Tree {
            regions,
            kmap,
            home,
            home0,
            ready,
            size,
            arms,
        }
    }

    /// Groups of small arms (regions too small to guard on their own) that
    /// run in one kept region: the topmost small arms of a select at 1 (an
    /// arm inside one goes with it). Arms that read the same values, which
    /// are then read by nothing else (the write paths of one register, say),
    /// are grouped first, with those values; the rest in ready order. At
    /// most [`MAX_ANY`] conditions to a group, and a group is kept where it
    /// reaches the smallest region size. Each grouped arm gets its group's
    /// number; the other small arms get no region in the next round (they
    /// ran in their parent anyway), so that the muxes of the arms of a
    /// group sit in the one region the group belongs to.
    ///
    /// Why a select at 1: in the designs measured (the NES console), the
    /// small arms skipped on 90 % of ticks or more were mostly the arms of
    /// a select at 1 (an enable, a write strobe, a load), and the arms of a
    /// select at 0 mostly ran (the hold or default path): a group of rare
    /// conditions is rarely run.
    fn groups(&self, cx: &Cx<'_>) -> Grouping {
        let body = cx.body;
        let m = body.instrs.len();
        let nr = self.regions.len();
        let kept = |r: usize| self.kmap[r] == r as u32;
        // The candidate each region belongs to (`NONE`: none), and the
        // candidates (index `i` of `list[i]`).
        let mut cand = vec![NONE; nr];
        let mut list: Vec<usize> = Vec::new();
        let mut index = vec![NONE; nr];
        for r in 1..nr {
            let g = &self.regions[r];
            let parent = g.parent as usize;
            cand[r] = cand[parent];
            if !kept(r) && cand[r] == NONE && g.cond.when && g.any.is_empty() {
                cand[r] = r as u32;
                index[r] = list.len() as u32;
                list.push(r);
            }
        }
        let nc = list.len();
        if nc < 2 {
            return Grouping::default();
        }
        let mut arm_region = vec![NONE; 3 * m];
        for &(k, p, r) in &self.arms {
            arm_region[arm(k, p)] = r;
        }
        // Region of a read: an arm's, or the reader's.
        let read_region = |k: u32, port: u8| match arm_region[arm(k, port)] {
            NONE => self.home0[k as usize],
            r => r,
        };
        let of = |k: u32| cand[self.home0[k as usize] as usize];
        // Which candidates' muxes each value depends on, and which each
        // candidate's inputs (and conditions) depend on: a group whose
        // inputs depend on its own muxes could not be contiguous.
        let words = nc.div_ceil(64);
        let mut dep = vec![0u64; m * words];
        let mut inputs = vec![0u64; nc * words];
        // Instructions come after the instructions they read (the body is
        // in rank order), so a row is complete before it is read.
        let row = |x: u32| x as usize * words..(x as usize + 1) * words;
        let or_into = |dst: &mut [u64], src: &[u64]| {
            for (d, s) in dst.iter_mut().zip(src) {
                *d |= s;
            }
        };
        let mut d = vec![0u64; words];
        for b in &body.blocks {
            let (sa, sb, sc) = b.op.slot_operands();
            for k in b.start..b.start + b.len {
                let i = body.instrs[k as usize];
                d.fill(0);
                for (port, is, s) in [(0u8, sa, i.a), (1, sb, i.b), (2, sc, i.c)] {
                    let w = cx.operand_writer(is, s);
                    if w == NONE {
                        continue;
                    }
                    or_into(&mut d, &dep[row(w)]);
                    // Into a candidate from outside it (its arm reads
                    // included; a select is read by its mux, outside).
                    let c = cand[read_region(k, port) as usize];
                    if c != NONE && of(w) != c {
                        let ci = index[c as usize];
                        or_into(&mut inputs[row(ci)], &dep[row(w)]);
                    }
                    // A candidate's mux.
                    let r = arm_region[arm(k, port)];
                    if r != NONE && index[r as usize] != NONE {
                        let ci = index[r as usize];
                        d[ci as usize / 64] |= 1 << (ci % 64);
                        // Its condition is read where the group begins.
                        let cw = cx.writer[i.a as usize];
                        if cw != NONE {
                            or_into(&mut inputs[row(ci)], &dep[row(cw)]);
                        }
                    }
                }
                dep[row(k)].copy_from_slice(&d);
            }
        }
        // A set of candidates: its members' bits, its inputs' dependences,
        // its conditions, its size and kept region.
        #[derive(Clone)]
        struct Set {
            bits: Vec<u64>,
            ins: Vec<u64>,
            conds: Vec<Cond>,
            n: usize,
            parent: u32,
            members: Vec<usize>,
        }
        let set_of = |i: usize| {
            let r = list[i];
            let mut bits = vec![0u64; words];
            bits[i / 64] |= 1 << (i % 64);
            Set {
                bits,
                ins: inputs[i * words..(i + 1) * words].to_vec(),
                conds: vec![self.regions[r].cond],
                n: self.size[r],
                parent: self.kmap[self.regions[r].parent as usize],
                members: vec![r],
            }
        };
        // `b` joined into `a`, if they can share a group.
        let join = |a: &mut Set, b: &Set| -> bool {
            let clash = |x: &[u64], y: &[u64]| x.iter().zip(y).any(|(p, q)| p & q != 0);
            let new = b.conds.iter().filter(|c| !a.conds.contains(c)).count();
            if a.parent != b.parent
                || a.conds.len() + new > MAX_ANY
                || clash(&a.ins, &b.bits)
                || clash(&b.ins, &a.bits)
            {
                return false;
            }
            for c in &b.conds {
                if !a.conds.contains(c) {
                    a.conds.push(*c);
                }
            }
            for (x, y) in a.bits.iter_mut().zip(&b.bits) {
                *x |= y;
            }
            for (x, y) in a.ins.iter_mut().zip(&b.ins) {
                *x |= y;
            }
            a.n += b.n;
            a.members.extend_from_slice(&b.members);
            true
        };
        // Values read only by candidates (more than one): theirs join.
        let mut sets: Vec<Option<Set>> = (0..nc).map(|i| Some(set_of(i))).collect();
        let mut set_of_cand: Vec<usize> = (0..nc).collect();
        let mut readers: Vec<(u32, u32)> = Vec::new();
        let mut outside = vec![false; m];
        for c in &body.commits {
            for s in c.reads() {
                if let Some(&w) = cx.writer.get(s as usize).filter(|&&w| w != NONE) {
                    outside[w as usize] = true;
                }
            }
        }
        if let Some(e) = cx.exposed {
            for (s, &x) in e.iter().enumerate() {
                if x && cx.writer[s] != NONE {
                    outside[cx.writer[s] as usize] = true;
                }
            }
        }
        for b in &body.blocks {
            let (sa, sb, sc) = b.op.slot_operands();
            for k in b.start..b.start + b.len {
                let i = body.instrs[k as usize];
                for (port, is, s) in [(0u8, sa, i.a), (1, sb, i.b), (2, sc, i.c)] {
                    let w = cx.operand_writer(is, s);
                    if !is || w == NONE || of(w) != NONE {
                        continue;
                    }
                    match cand[read_region(k, port) as usize] {
                        NONE => outside[w as usize] = true,
                        c => readers.push((w, index[c as usize])),
                    }
                }
            }
        }
        readers.sort_unstable();
        readers.dedup();
        for run in readers.chunk_by(|a, b| a.0 == b.0) {
            if outside[run[0].0 as usize] || run.len() < 2 {
                continue;
            }
            let lead = set_of_cand[run[0].1 as usize];
            let mut joined = false;
            for x in &run[1..] {
                let other = set_of_cand[x.1 as usize];
                if other == lead {
                    continue;
                }
                let Some(b) = sets[other].take() else {
                    continue;
                };
                let a = sets[lead].as_mut().expect("a lead set");
                if join(a, &b) {
                    joined = true;
                    for &r in &b.members {
                        set_of_cand[index[r] as usize] = lead;
                    }
                } else {
                    sets[other] = Some(b);
                }
            }
            if joined {
                // The value joins the group too.
                sets[lead].as_mut().expect("a lead set").n += 1;
            }
        }
        // Then, per kept region, the largest set left with the next
        // largest that join it, until it is big enough.
        let mut left: Vec<Set> = sets.into_iter().flatten().collect();
        left.sort_by_key(|x| (x.parent, std::cmp::Reverse(x.n), x.members[0]));
        let mut members: Vec<Vec<usize>> = Vec::new();
        let mut used = vec![false; left.len()];
        for seed in 0..left.len() {
            if used[seed] {
                continue;
            }
            let mut cur = left[seed].clone();
            let mut took = vec![seed];
            for j in seed + 1..left.len() {
                if cur.n >= cx.min_region || left[j].parent != cur.parent {
                    break;
                }
                if used[j] {
                    continue;
                }
                if join(&mut cur, &left[j]) {
                    took.push(j);
                }
            }
            if cur.n >= cx.min_region && cur.members.len() > 1 {
                for j in took {
                    used[j] = true;
                }
                members.push(cur.members);
            }
        }
        let mut group_of = vec![NONE; nr];
        for (gi, m) in members.iter().enumerate() {
            for &r in m {
                group_of[r] = gi as u32;
            }
        }
        // The arms of a grouped candidate get the group (the arms inside it
        // keep their regions, inside the group).
        let mut out = Grouping {
            group: vec![NONE; 3 * m],
            flat: vec![false; 3 * m],
        };
        let mut any = false;
        for &(k, port, r) in &self.arms {
            out.group[arm(k, port)] = group_of[r as usize];
            out.flat[arm(k, port)] = !kept(r as usize);
            any |= group_of[r as usize] != NONE;
        }
        if !any {
            out.group.clear();
        }
        out
    }

    /// The guarded body (`Ok(None)` when no region is kept), or `Err`
    /// with the arms of the regions no order makes contiguous: the groups
    /// among them (`true`), else the merged regions.
    fn layout(&self, cx: &Cx<'_>) -> Result<Option<Body>, Undone> {
        let Tree {
            regions,
            kmap,
            home,
            ready,
            arms,
            ..
        } = self;
        let body = cx.body;
        let m = body.instrs.len();
        let op_rank = &cx.op_rank;
        let nr = regions.len();
        let kept = |r: usize| kmap[r] == r as u32;
        if (1..nr).all(|r| !kept(r)) {
            return Ok(None);
        }
        // The kept tree: parent and depth of each kept region.
        let parent_of = |r: usize| kmap[regions[r].parent as usize];
        let mut depth = vec![0u32; nr];
        for r in 1..nr {
            if kept(r) {
                depth[r] = depth[parent_of(r) as usize] + 1;
            }
        }
        // The child of `l` holding region `r` (`r` inside `l`), or `r`.
        let below = |mut r: u32, l: u32| {
            while r != l && parent_of(r as usize) != l {
                r = parent_of(r as usize);
            }
            r
        };
        let lca = |mut a: u32, mut b: u32| {
            while depth[a as usize] > depth[b as usize] {
                a = parent_of(a as usize);
            }
            while depth[b as usize] > depth[a as usize] {
                b = parent_of(b as usize);
            }
            while a != b {
                a = parent_of(a as usize);
                b = parent_of(b as usize);
            }
            a
        };

        // Items of each kept region: `k` for instruction `k`, `m + r` for
        // child region `r`.
        let item = |x: u32, l: u32, region: u32| -> u32 {
            if region == l {
                x
            } else {
                m as u32 + below(region, l)
            }
        };
        let mut items: Vec<Vec<u32>> = vec![Vec::new(); nr];
        for (k, &h) in home.iter().enumerate() {
            items[h as usize].push(k as u32);
        }
        for r in 1..nr {
            if kept(r) {
                items[parent_of(r) as usize].push(m as u32 + r as u32);
            }
        }
        // Reads between the items of one region: an instruction's
        // operands, and the conditions of a region's guard (read where the
        // region begins).
        let n_items = m + nr;
        let mut edges: Vec<(u32, u32)> = Vec::with_capacity(2 * m);
        let mut edge = |w: u32, x: u32, rx: u32| {
            let rw = home[w as usize];
            let l = lca(rw, rx);
            let (a, b) = (item(w, l, rw), item(x, l, rx));
            if a != b {
                edges.push((a, b));
            }
        };
        for b in &body.blocks {
            let (sa, sb, sc) = b.op.slot_operands();
            for k in b.start..b.start + b.len {
                let i = body.instrs[k as usize];
                for (is, s) in [(sa, i.a), (sb, i.b), (sc, i.c)] {
                    let w = cx.operand_writer(is, s);
                    if is && w != NONE {
                        edge(w, k, home[k as usize]);
                    }
                }
            }
        }
        // Conditions of each kept region: its parents', then its own (a
        // group, which holds no kept region, tests its own as `any`).
        let mut conds: Vec<(Vec<Cond>, Vec<Cond>)> = vec![Default::default(); nr];
        for r in 1..nr {
            if !kept(r) {
                continue;
            }
            let mut chain = Vec::new();
            let mut x = r;
            while x != 0 {
                if regions[x].any.is_empty() {
                    chain.push(regions[x].cond);
                } else {
                    debug_assert_eq!(x, r, "a group holds no kept region");
                }
                x = parent_of(x) as usize;
            }
            chain.reverse();
            let any = regions[r].any.clone();
            for c in chain.iter().chain(&any) {
                let w = cx.writer[c.slot as usize];
                // `r`'s own entry, at the level of the condition's writer.
                if w != NONE {
                    let rw = home[w as usize];
                    let l = lca(rw, r as u32);
                    if l != r as u32 {
                        let a = item(w, l, rw);
                        let b = m as u32 + below(r as u32, l);
                        if a != b {
                            edges.push((a, b));
                        }
                    }
                }
            }
            conds[r] = (chain, any);
        }

        // Each region's items in topological order, by rank where the
        // reads allow (a child region by its ready point, before the
        // instructions of that rank).
        let key = |x: u32| -> (u32, u8, u8, u32) {
            if (x as usize) < m {
                let (op, rank) = op_rank[x as usize];
                (rank, 1, op as u8, x)
            } else {
                (ready[x as usize - m], 0, 0, x)
            }
        };
        // The reads as adjacency lists (by source), and in-degrees.
        let mut start = vec![0u32; n_items + 1];
        let mut preds = vec![0u32; n_items];
        for &(a, b) in &edges {
            start[a as usize + 1] += 1;
            preds[b as usize] += 1;
        }
        for x in 0..n_items {
            start[x + 1] += start[x];
        }
        let mut succ = vec![0u32; edges.len()];
        let mut fill = start.clone();
        for &(a, b) in &edges {
            succ[fill[a as usize] as usize] = b;
            fill[a as usize] += 1;
        }
        let mut sorted: Vec<Vec<u32>> = vec![Vec::new(); nr];
        let mut stuck: Vec<u32> = Vec::new();
        let mut heap = std::collections::BinaryHeap::new();
        for (r, list) in items.iter().enumerate() {
            if list.is_empty() {
                continue;
            }
            for &x in list {
                if preds[x as usize] == 0 {
                    heap.push(std::cmp::Reverse(key(x)));
                }
            }
            let out = &mut sorted[r];
            while let Some(std::cmp::Reverse((_, _, _, x))) = heap.pop() {
                out.push(x);
                for &y in &succ[start[x as usize] as usize..start[x as usize + 1] as usize] {
                    preds[y as usize] -= 1;
                    if preds[y as usize] == 0 {
                        heap.push(std::cmp::Reverse(key(y)));
                    }
                }
            }
            if out.len() < list.len() {
                // A cycle through merged regions: split them.
                stuck.extend(
                    list.iter()
                        .filter(|&&x| x as usize >= m && preds[x as usize] > 0)
                        .map(|&x| x - m as u32),
                );
            }
        }
        if !stuck.is_empty() {
            let groups = stuck.iter().any(|&r| !regions[r as usize].any.is_empty());
            let mut bad = vec![false; nr];
            for &r in &stuck {
                let g = &regions[r as usize];
                if if groups { !g.any.is_empty() } else { g.merged } {
                    bad[r as usize] = true;
                }
            }
            let split: Vec<(u32, u8)> = arms
                .iter()
                .filter(|a| bad[kmap[a.2 as usize] as usize])
                .map(|&(k, port, _)| (k, port))
                .collect();
            // Single arms always lay out (a cycle through one would be a
            // combinational loop); were one stuck, guard nothing.
            debug_assert!(!split.is_empty(), "single arms always lay out");
            return if split.is_empty() {
                Ok(None)
            } else {
                Err((split, groups))
            };
        }

        // Emit depth first; each region's items are contiguous.
        let mut order: Vec<(u32, u32)> = Vec::with_capacity(m);
        let mut stack: Vec<(u32, usize)> = vec![(0, 0)];
        while let Some(top) = stack.last_mut() {
            let (r, at) = *top;
            let list = &sorted[r as usize];
            if at == list.len() {
                stack.pop();
                continue;
            }
            top.1 += 1;
            let x = list[at];
            if (x as usize) < m {
                order.push((x, r));
            } else {
                stack.push((x - m as u32, 0));
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
                let (all, any) = conds[region as usize].clone();
                out.guards.push(Guard {
                    start: from,
                    end: to,
                    conds: all,
                    any,
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
        Ok((!out.guards.is_empty()).then_some(out))
    }
}

#[cfg(test)]
mod tests;
