//! Cone tabling (issue #117): K-input lookup-table mapping of the
//! word-level program.
//!
//! Function matching turns a whole combinational *definition* with few
//! input bits into a ROM read. Much combinational logic of a flattened
//! design is not one definition: an opcode decoder spread over chips, a
//! priority mux whose select bits come from three places, the control
//! logic inside a large chip. This pass finds such logic as *cones* of
//! the instruction graph: a root instruction and the instructions under
//! it down to a *cut*, a set of bit fields of slots whose total width is
//! at most `K` bits. A cone with enough instructions runs as one table
//! read: the cut's fields concatenated into an index (`Shr`s and
//! `Concat`s), and one `MemRead` of a ROM holding the root's value for
//! every index, built by evaluating the cone on every index, so exact by
//! construction.
//!
//! **Cuts** are enumerated as in FPGA technology mapping, bottom up with
//! *priority cuts*: each instruction keeps its best few cuts (largest
//! estimated cone first), formed by merging one cut (or the operand
//! itself) of each operand; a cut is a set of `(slot, bit mask)` leaves,
//! costing the span of each mask (a slice reads only its bits). Constants
//! are evaluated inside the cone and cost no bits. Memory reads and clocks
//! are never inside a cone (a memory read is not a function of slots, a
//! clock not of slots at all); state, inputs and constants are leaves.
//!
//! **Selection** is area flow: per instruction, the cheaper of the
//! instruction itself (one op plus its operands' flow) and each cut (its
//! index ops plus the table read, plus its leaves' flow), operand flow
//! shared out over fanout. The required instructions are then found from
//! the sinks down: an instruction run as a table needs its leaves, one
//! run as itself its operands; the others are no longer computed. A cone's
//! leaves and masks are recomputed exactly from the chosen cut before its
//! table is built.

use super::FastMap;
use super::program::*;
use super::simplify::fold;
use crate::netlist::mask;
use smallvec::SmallVec;

pub(crate) const NONE: u32 = u32::MAX;

/// Rounds of dropping cones that save too little.
const MAX_ROUNDS: usize = 32;

/// Default most index bits of one cone table.
pub const DEFAULT_K: u32 = 12;

/// Mapping parameters.
#[derive(Debug, Clone, Copy)]
pub struct Cfg {
    /// Most index bits of one table.
    pub k: u32,
    /// Cuts kept per instruction.
    pub cuts: usize,
    /// Cost of one table read, in instructions.
    pub read_cost: f32,
    /// Fewest instructions a table must save (net of its index and read).
    pub min_gain: u32,
    /// Most table words in all.
    pub budget: usize,
}

impl Default for Cfg {
    fn default() -> Self {
        Self {
            k: DEFAULT_K,
            cuts: 8,
            read_cost: 1.0,
            min_gain: 1,
            budget: 1 << 16,
        }
    }
}

/// The instruction graph the pass maps.
pub(crate) struct Graph<'a> {
    pub ops: &'a [Op],
    pub instrs: &'a [Instr],
    pub imms: &'a [u64],
    /// Instruction indices in a topological order.
    pub order: &'a [u32],
    /// Per slot: an upper bound on the bits its value can have set.
    pub width: Vec<u8>,
    /// Per slot: its constant value, if it is a constant (or the zero
    /// slot).
    pub konst: FastMap<u32, u64>,
    pub mems: &'a [MemDesc],
}

fn hull(m: u64) -> (u8, u8) {
    if m == 0 {
        return (0, 0);
    }
    let lo = m.trailing_zeros() as u8;
    let hi = 64 - m.leading_zeros() as u8;
    (lo, hi - lo)
}

fn span(m: u64) -> u32 {
    hull(m).1 as u32
}

/// Whether `op` may be inside a cone.
pub(crate) fn tabulable(op: Op) -> bool {
    !matches!(op, Op::MemRead | Op::Clock)
}

/// Slot operands of an instruction.
pub(crate) fn operands(op: Op, i: &Instr) -> SmallVec<[u32; 3]> {
    let (a, b, c) = op.slot_operands();
    let mut v = SmallVec::new();
    if a {
        v.push(i.a);
    }
    if b {
        v.push(i.b);
    }
    if c {
        v.push(i.c);
    }
    v
}

/// Upper bound on the bits instruction `k`'s result can have, from its
/// operands' bounds.
pub(crate) fn result_width(op: Op, i: &Instr, imm: u64, w: &[u8], mems: &[MemDesc]) -> u8 {
    let bits = |m: u64| (64 - m.leading_zeros()) as u8;
    let wa = || w[i.a as usize];
    let wb = || w[i.b as usize];
    let wc = || w[i.c as usize];
    match op {
        Op::Copy => wa(),
        Op::Not | Op::Nand | Op::Nor | Op::Xnor | Op::AddSum => bits(imm),
        Op::And => wa().min(wb()),
        Op::Or | Op::Xor => wa().max(wb()),
        Op::Mux => wb().max(wc()),
        Op::Switch => wb(),
        Op::AddCarry | Op::Eq | Op::LtU | Op::ReduceOr | Op::ReduceAnd | Op::ReduceXor => 1,
        Op::Clock | Op::SrNext => 1,
        Op::Shr => bits(imm).min(wa().saturating_sub(i.b.min(64) as u8)),
        Op::Concat => wa().max((i.c as u8).saturating_add(wb()).min(64)),
        Op::Sext => {
            if (wa() as u32) < i.b {
                wa()
            } else {
                bits(imm)
            }
        }
        Op::MemRead => mems.get(i.b as usize).map_or(64, |m| m.width),
    }
}

/// Cut leaves: `(slot, mask)` sorted by slot.
type Leaves = SmallVec<[(u32, u64); 6]>;

/// One cut: leaves `(slot, mask)` sorted by slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Cut {
    pub leaves: Leaves,
    pub bits: u32,
    /// Estimated instructions in the cone (for ranking).
    pub vol: u32,
}

fn merge_into(acc: &mut Leaves, add: &[(u32, u64)]) {
    for &(s, m) in add {
        match acc.binary_search_by_key(&s, |x| x.0) {
            Ok(k) => acc[k].1 |= m,
            Err(k) => acc.insert(k, (s, m)),
        }
    }
}

fn bits_of(leaves: &[(u32, u64)]) -> u32 {
    leaves.iter().map(|&(_, m)| span(m)).sum()
}

/// A cone chosen to run as a table.
#[derive(Debug, Clone)]
pub(crate) struct Cone {
    /// The root instruction.
    pub root: u32,
    /// Leaves: `(slot, lo, len)`, the index fields from bit 0 up.
    pub fields: Vec<(u32, u8, u8)>,
    pub bits: u32,
    /// The instructions of the cone, in topological order (root last).
    pub nodes: Vec<u32>,
}

impl Cone {
    /// Index-building instructions: one `Shr` per field that is not a
    /// whole value (a top field starting at bit 0 needs none: the read
    /// masks the address), and one `Concat` per field after the first.
    pub fn index_cost(&self, width: &[u8]) -> usize {
        let n = self.fields.len();
        let shr = self
            .fields
            .iter()
            .enumerate()
            .filter(|&(j, &(s, lo, len))| !(lo == 0 && (len >= width[s as usize] || j + 1 == n)))
            .count();
        shr + n.saturating_sub(1)
    }
}

/// The mapping: which instructions still run, and the tables.
pub(crate) struct Mapping {
    /// Per instruction: runs as itself.
    pub keep: Vec<bool>,
    pub cones: Vec<Cone>,
}

impl Graph<'_> {
    fn writer_table(&self, n_slots: usize) -> Vec<u32> {
        let mut w = vec![NONE; n_slots];
        for (k, i) in self.instrs.iter().enumerate() {
            w[i.dst as usize] = k as u32;
        }
        w
    }

    fn is_const(&self, s: u32) -> bool {
        self.width[s as usize] == 0 || self.konst.contains_key(&s)
    }

    /// The bits of operand slot `s` that instruction `k` reads.
    fn read_mask(&self, k: usize, s: u32) -> u64 {
        let full = mask(self.width[s as usize]);
        let i = &self.instrs[k];
        match self.ops[k] {
            Op::Shr if i.a == s => (self.imms[k] << i.b.min(63)) & full,
            _ => full,
        }
    }

    /// Map the graph. `sink[slot]`: the slot must stay computed (read by
    /// a commit, a port, a probe, a state key ...).
    pub fn map(&self, sink: &[bool], cfg: &Cfg) -> Mapping {
        let n_slots = self.width.len();
        let n = self.instrs.len();
        let writer = self.writer_table(n_slots);
        let tab_writer = |s: u32| -> Option<usize> {
            let w = writer[s as usize];
            (w != NONE && tabulable(self.ops[w as usize])).then_some(w as usize)
        };
        // Fanout of each slot (instruction reads plus sinks).
        let mut fo = vec![0u32; n_slots];
        for (k, i) in self.instrs.iter().enumerate() {
            for s in operands(self.ops[k], i) {
                fo[s as usize] += 1;
            }
        }
        for (s, &is) in sink.iter().enumerate() {
            if is {
                fo[s] += 1;
            }
        }

        // ── Priority cuts ──────────────────────────────────────────────
        let mut cuts: Vec<Vec<Cut>> = vec![Vec::new(); n];
        for &k in self.order {
            let k = k as usize;
            if !tabulable(self.ops[k]) {
                continue;
            }
            let ins = operands(self.ops[k], &self.instrs[k]);
            let mut partial: Vec<Cut> = vec![Cut {
                leaves: SmallVec::new(),
                bits: 0,
                vol: 1,
            }];
            let mut ok = true;
            for &s in &ins {
                if self.is_const(s) {
                    continue;
                }
                let trivial = (s, self.read_mask(k, s));
                if span(trivial.1) > cfg.k {
                    ok = false;
                    break;
                }
                let mut opts: Vec<(Leaves, u32)> = vec![(SmallVec::from_slice(&[trivial]), 0)];
                if let Some(w) = tab_writer(s) {
                    for c in &cuts[w] {
                        opts.push((c.leaves.clone(), c.vol));
                    }
                }
                let mut next: Vec<Cut> = Vec::new();
                for p in &partial {
                    for (l, v) in &opts {
                        let mut leaves = p.leaves.clone();
                        merge_into(&mut leaves, l);
                        let bits = bits_of(&leaves);
                        if bits > cfg.k {
                            continue;
                        }
                        let cut = Cut {
                            leaves,
                            bits,
                            vol: p.vol + v,
                        };
                        match next.iter_mut().find(|c| c.leaves == cut.leaves) {
                            Some(c) => c.vol = c.vol.max(cut.vol),
                            None => next.push(cut),
                        }
                    }
                }
                next.sort_by(|a, b| b.vol.cmp(&a.vol).then(a.bits.cmp(&b.bits)));
                next.truncate(cfg.cuts * 2);
                partial = next;
                if partial.is_empty() {
                    ok = false;
                    break;
                }
            }
            if !ok {
                continue;
            }
            // (A cut of only the instruction's own operands is kept for
            // its readers' cuts, but is no table: see the area flow.)
            partial.retain(|c| !c.leaves.is_empty());
            partial.truncate(cfg.cuts);
            cuts[k] = partial;
        }

        // ── Area flow ──────────────────────────────────────────────────
        let mut af = vec![0f32; n_slots];
        let mut choice: Vec<u32> = vec![NONE; n];
        let idx_est = |c: &Cut| -> f32 {
            let nl = c.leaves.len();
            let shr = c
                .leaves
                .iter()
                .filter(|&&(s, m)| m != mask(self.width[s as usize]))
                .count();
            (shr + nl.saturating_sub(1)) as f32
        };
        for &k in self.order {
            let k = k as usize;
            let i = &self.instrs[k];
            let flow = |s: u32| -> f32 {
                if writer[s as usize] == NONE {
                    0.0
                } else {
                    af[s as usize] / fo[s as usize].max(1) as f32
                }
            };
            let mut best = 1.0
                + operands(self.ops[k], i)
                    .iter()
                    .map(|&s| flow(s))
                    .sum::<f32>();
            for (j, c) in cuts[k].iter().enumerate() {
                if c.vol < 2 {
                    continue;
                }
                let f = cfg.read_cost
                    + idx_est(c)
                    + c.leaves.iter().map(|&(s, _)| flow(s)).sum::<f32>();
                if f + 1e-3 < best {
                    best = f;
                    choice[k] = j as u32;
                }
            }
            af[i.dst as usize] = best;
        }

        // ── Required instructions, from the sinks down ─────────────────
        // A cone that saves too little once the others are chosen (its
        // nodes are needed elsewhere), or does not fit the budget, runs
        // as instructions; then the rest are chosen again.
        let mut exact: FastMap<u32, Option<Cone>> = FastMap::default();
        for _round in 0..MAX_ROUNDS {
            let m = self.select(sink, &choice, &cuts, &writer, cfg, &mut exact);
            let mut in_cones = vec![0u32; n];
            let mut is_root = vec![false; n];
            for c in &m.cones {
                is_root[c.root as usize] = true;
                for &x in &c.nodes {
                    in_cones[x as usize] += 1;
                }
            }
            let mut gains: Vec<(i64, usize)> = m
                .cones
                .iter()
                .enumerate()
                .map(|(j, c)| {
                    // Each node no longer computed counts for the cones
                    // holding it, in equal shares.
                    let saved: f32 = c
                        .nodes
                        .iter()
                        .filter(|&&x| x != c.root && !m.keep[x as usize] && !is_root[x as usize])
                        .map(|&x| 1.0 / in_cones[x as usize] as f32)
                        .sum();
                    (saved.floor() as i64 - c.index_cost(&self.width) as i64, j)
                })
                .collect();
            // Over the budget: the cones that save least go first.
            gains.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
            let mut words = 0usize;
            let mut banned = false;
            for &(_, j) in &gains {
                let c = &m.cones[j];
                words += 1 << c.bits;
                if words > cfg.budget {
                    choice[c.root as usize] = NONE;
                    banned = true;
                }
            }
            // Cones that save too little: one of each overlapping group
            // per round (the largest of the least saving), since dropping
            // one makes the nodes it shares exclusive to the others.
            let mut bad: Vec<(i64, usize, usize)> = gains
                .iter()
                .filter(|&&(g, _)| g < cfg.min_gain as i64)
                .map(|&(g, j)| (g, usize::MAX - m.cones[j].nodes.len(), j))
                .collect();
            bad.sort();
            let mut touched = vec![false; n];
            for &(_, _, j) in &bad {
                let c = &m.cones[j];
                if choice[c.root as usize] == NONE || c.nodes.iter().any(|&x| touched[x as usize]) {
                    continue;
                }
                for &x in &c.nodes {
                    touched[x as usize] = true;
                }
                choice[c.root as usize] = NONE;
                banned = true;
            }
            if !banned {
                return m;
            }
        }
        // Still changing after the rounds: no tables.
        choice.iter_mut().for_each(|c| *c = NONE);
        self.select(sink, &choice, &cuts, &writer, cfg, &mut exact)
    }

    /// The instructions the sinks need, and the cones, for `choice`.
    fn select(
        &self,
        sink: &[bool],
        choice: &[u32],
        cuts: &[Vec<Cut>],
        writer: &[u32],
        cfg: &Cfg,
        exact: &mut FastMap<u32, Option<Cone>>,
    ) -> Mapping {
        let n = self.instrs.len();
        let mut req = sink.to_vec();
        let mut keep = vec![false; n];
        let mut cones = Vec::new();
        for &k in self.order.iter().rev() {
            let k = k as usize;
            let i = &self.instrs[k];
            if !req[i.dst as usize] {
                continue;
            }
            if choice[k] != NONE {
                let cone = exact
                    .entry(k as u32)
                    .or_insert_with(|| {
                        self.exact_cone(k, &cuts[k][choice[k] as usize], writer, cfg.k)
                    })
                    .clone();
                if let Some(cone) = cone {
                    for &(s, _, _) in &cone.fields {
                        req[s as usize] = true;
                    }
                    cones.push(cone);
                    continue;
                }
            }
            keep[k] = true;
            for s in operands(self.ops[k], i) {
                req[s as usize] = true;
            }
        }
        Mapping { keep, cones }
    }

    /// The cone of instruction `root` down to `cut`'s slots, with its
    /// leaves' masks recomputed from what the cone reads. `None` when the
    /// cone reaches a slot that is not a cut leaf or a constant, or an
    /// instruction that cannot be tabled, or is too wide.
    fn exact_cone(&self, root: usize, cut: &Cut, writer: &[u32], k: u32) -> Option<Cone> {
        let is_leaf = |s: u32| cut.leaves.binary_search_by_key(&s, |x| x.0).is_ok();
        let mut seen: FastMap<u32, ()> = FastMap::default();
        let mut nodes: Vec<u32> = Vec::new();
        let mut masks: Leaves = SmallVec::new();
        // Iterative post-order.
        let mut stack: Vec<(u32, bool)> = vec![(root as u32, false)];
        while let Some((x, done)) = stack.pop() {
            if done {
                nodes.push(x);
                continue;
            }
            if seen.insert(x, ()).is_some() {
                continue;
            }
            if !tabulable(self.ops[x as usize]) {
                return None;
            }
            stack.push((x, true));
            for s in operands(self.ops[x as usize], &self.instrs[x as usize]) {
                if self.is_const(s) {
                    continue;
                }
                if is_leaf(s) {
                    merge_into(&mut masks, &[(s, self.read_mask(x as usize, s))]);
                    continue;
                }
                let w = writer[s as usize];
                if w == NONE {
                    return None;
                }
                if !seen.contains_key(&w) {
                    stack.push((w, false));
                }
            }
        }
        let bits = bits_of(&masks);
        if bits > k || bits == 0 {
            return None;
        }
        if !self.arms_stay_guarded(&nodes, &masks, writer) {
            return None;
        }
        // Fields: whole values first, then partial ones; a field starting
        // at bit 0 on top needs no slice.
        let mut fields: Vec<(u32, u8, u8)> = masks
            .iter()
            .map(|&(s, m)| {
                let (lo, len) = hull(m);
                (s, lo, len)
            })
            .collect();
        let whole = |&(s, lo, len): &(u32, u8, u8)| lo == 0 && len >= self.width[s as usize];
        fields.sort_by_key(|f| (!whole(f), f.1 == 0, f.0));
        Some(Cone {
            root: root as u32,
            fields,
            bits,
            nodes,
        })
    }

    /// Whether every leaf of the cone that an instruction computes is
    /// read outside the arms of the cone's muxes. A leaf read only through
    /// an arm is needed only when its select picks that arm, and the
    /// runtime skips it otherwise (`guard.rs`); as an index field it would
    /// be computed every tick.
    fn arms_stay_guarded(&self, nodes: &[u32], masks: &[(u32, u64)], writer: &[u32]) -> bool {
        let pos: FastMap<u32, usize> = nodes.iter().enumerate().map(|(j, &x)| (x, j)).collect();
        let mut uncond = vec![false; nodes.len()];
        let mut leaf_ok: SmallVec<[u32; 6]> = SmallVec::new();
        *uncond.last_mut().expect("the root") = true;
        for j in (0..nodes.len()).rev() {
            if !uncond[j] {
                continue;
            }
            let x = nodes[j] as usize;
            let (op, i) = (self.ops[x], &self.instrs[x]);
            let arm = |s: u32| {
                (op == Op::Mux && (s == i.b || s == i.c)) && s != i.a
                    || (op == Op::Switch && s == i.b && s != i.a)
            };
            for s in operands(op, i) {
                if arm(s) || self.is_const(s) {
                    continue;
                }
                match pos.get(&writer[s as usize]) {
                    Some(&q) if masks.binary_search_by_key(&s, |m| m.0).is_err() => {
                        uncond[q] = true
                    }
                    _ => leaf_ok.push(s),
                }
            }
        }
        masks
            .iter()
            .all(|&(s, _)| writer[s as usize] == NONE || leaf_ok.contains(&s))
    }

    /// The table of `cone`: the root's value at every index.
    pub fn contents(&self, cone: &Cone) -> Vec<u64> {
        // Local values: 0 is zero, then the leaves, the constants the cone
        // reads, and the nodes.
        let mut local: FastMap<u32, u32> = FastMap::default();
        let mut init: Vec<u64> = vec![0];
        local.insert(ZERO_SLOT, 0);
        for &(s, _, _) in &cone.fields {
            local.entry(s).or_insert_with(|| {
                init.push(0);
                (init.len() - 1) as u32
            });
        }
        let mut prog: Vec<(Op, Instr, u64)> = Vec::with_capacity(cone.nodes.len());
        for &k in &cone.nodes {
            let k = k as usize;
            let (op, i) = (self.ops[k], self.instrs[k]);
            let (sa, sb, sc) = op.slot_operands();
            let mut map = |s: u32, is: bool| -> u32 {
                if !is {
                    return s;
                }
                *local.entry(s).or_insert_with(|| {
                    init.push(self.konst.get(&s).copied().unwrap_or(0));
                    (init.len() - 1) as u32
                })
            };
            let li = Instr {
                a: map(i.a, sa),
                b: map(i.b, sb),
                c: map(i.c, sc),
                dst: 0,
            };
            let dst = (init.len()) as u32;
            init.push(0);
            local.insert(i.dst, dst);
            prog.push((op, Instr { dst, ..li }, self.imms[k]));
        }
        let fields: Vec<(u32, u8, u8)> = cone
            .fields
            .iter()
            .map(|&(s, lo, len)| (local[&s], lo, len))
            .collect();
        let out_at = local[&self.instrs[cone.root as usize].dst] as usize;
        let mut vals = init;
        let mut out = Vec::with_capacity(1 << cone.bits);
        for x in 0u64..1 << cone.bits {
            for &(l, _, _) in &fields {
                vals[l as usize] = 0;
            }
            let mut at = 0u32;
            for &(l, lo, len) in &fields {
                vals[l as usize] |= ((x >> at) & mask(len)) << lo;
                at += len as u32;
            }
            for (op, i, imm) in &prog {
                let v = {
                    let get = |s: u32| Some(vals[s as usize]);
                    fold(*op, i, *imm, &get, self.mems).expect("tabulable ops fold")
                };
                vals[i.dst as usize] = v;
            }
            out.push(vals[out_at]);
        }
        out
    }
}

/// Width bounds of every slot of a body, from `init` (the leaves'
/// widths, 0 elsewhere).
pub(crate) fn width_bounds(
    mut w: Vec<u8>,
    ops: &[Op],
    instrs: &[Instr],
    imms: &[u64],
    order: &[u32],
    commits: &[Commit],
    mems: &[MemDesc],
) -> Vec<u8> {
    loop {
        for &k in order {
            let k = k as usize;
            let i = &instrs[k];
            w[i.dst as usize] = result_width(ops[k], i, imms[k], &w, mems);
        }
        let mut changed = false;
        for c in commits {
            if let Commit::Copy { state, next } = *c
                && w[next as usize] > w[state as usize]
            {
                w[state as usize] = w[next as usize];
                changed = true;
            }
        }
        if !changed {
            return w;
        }
    }
}

/// Statistics for one body of a compiled program (`nes_bench --cones`).
#[derive(Debug, Clone, Default)]
pub struct ConeStats {
    /// Instructions the sinks need, as compiled.
    pub base: usize,
    /// Instructions after mapping: kept instructions, plus index ops and
    /// one read per table.
    pub mapped: usize,
    /// Instructions inside tabled cones.
    pub covered: usize,
    pub tables: usize,
    /// Table words.
    pub words: usize,
    /// The largest cones: (instructions, index bits, index cost, root slot).
    pub top: Vec<(usize, u32, usize, u32)>,
}

/// Map one body of `p` (`phase`: a variant, or `None` for the
/// unspecialized body) with every slot `p` exposes as a sink, or (with
/// `fused`) only what commits and root outputs read, and report.
pub fn stats(p: &Program, phase: Option<usize>, fused: bool, cfg: &Cfg) -> ConeStats {
    let (blocks, instrs, imms, commits) = match phase {
        None => (&p.blocks, &p.instrs, &p.imms, &p.commits),
        Some(v) => {
            let b = &p.phases.as_ref().expect("phases").variants[v];
            (&b.blocks, &b.instrs, &b.imms, &b.commits)
        }
    };
    let n_slots = p.slot_count as usize;
    let mut ops = vec![Op::Copy; instrs.len()];
    let mut order = Vec::with_capacity(instrs.len());
    let mut packed_dst: Vec<u32> = Vec::new();
    for b in blocks {
        if b.packed {
            for pk in &p.packs[b.start as usize..(b.start + b.len) as usize] {
                packed_dst.push(pk.dst);
            }
            continue;
        }
        for k in b.start..b.start + b.len {
            ops[k as usize] = b.op;
            order.push(k);
        }
    }
    let mut leaf_w: Vec<(u32, u8)> = Vec::new();
    let mut konst: FastMap<u32, u64> = FastMap::default();
    konst.insert(ZERO_SLOT, 0);
    for &(s, v) in &p.consts {
        konst.insert(s, v);
        leaf_w.push((s, (64 - v.leading_zeros()) as u8));
    }
    for port in &p.ports.inputs {
        leaf_w.push((port.slot, port.width));
    }
    for k in &p.state_keys {
        if let StateLoc::Slot(s) = k.loc
            && let Some((real, _, _)) = p.resolve(s)
        {
            leaf_w.push((real, k.width.max(if real == s { 0 } else { 64 })));
        }
    }
    for &d in &packed_dst {
        leaf_w.push((d, 64));
    }
    for s in &p.scatters {
        leaf_w.push((s.slot, 1));
    }
    let mut init = vec![0u8; n_slots];
    for &(s, x) in &leaf_w {
        let v = &mut init[s as usize];
        *v = (*v).max(x);
    }
    let width = width_bounds(init, &ops, instrs, imms, &order, commits, &p.mems);
    let g = Graph {
        ops: &ops,
        instrs,
        imms,
        order: &order,
        width,
        konst,
        mems: &p.mems,
    };
    let mut sink = vec![false; n_slots];
    let mut mark = |s: u32| {
        if let Some((real, _, _)) = p.resolve(s)
            && (real as usize) < n_slots
        {
            sink[real as usize] = true;
        }
    };
    for c in commits {
        for s in c.reads() {
            mark(s);
        }
    }
    for port in &p.ports.outputs {
        mark(port.slot);
    }
    for pk in &p.packs {
        for op in [&pk.a, &pk.b, &pk.c] {
            mark(op.base);
            mark(op.splat);
            for gt in
                &p.gathers[op.gather_start as usize..(op.gather_start + op.gather_len) as usize]
            {
                mark(gt.slot);
            }
        }
    }
    if !fused {
        for map in &p.debug.net_slots {
            for (_, &s) in map {
                mark(s);
            }
        }
        for v in &p.views {
            mark(v.slot);
        }
        for sh in &p.shadows {
            for &s in sh.inputs.iter().chain(&sh.states) {
                mark(s);
            }
        }
        for k in &p.state_keys {
            if let StateLoc::Slot(s) = k.loc {
                mark(s);
            }
        }
    }
    // Baseline: every instruction as itself.
    let base_cfg = Cfg { cuts: 0, ..*cfg };
    let base = g.map(&sink, &base_cfg).keep.iter().filter(|&&x| x).count();
    let m = g.map(&sink, cfg);
    let mut st = ConeStats {
        base,
        ..Default::default()
    };
    let kept = m.keep.iter().filter(|&&x| x).count();
    st.mapped = kept;
    for c in &m.cones {
        st.mapped += c.index_cost(&g.width) + 1;
        st.covered += c.nodes.len();
        st.tables += 1;
        st.words += 1 << c.bits;
        st.top.push((
            c.nodes.len(),
            c.bits,
            c.index_cost(&g.width),
            instrs[c.root as usize].dst,
        ));
    }
    st.top.sort_by_key(|a| std::cmp::Reverse(a.0));
    st.top.truncate(25);
    st
}

/// The pass (see the module docs) on the program after word-level
/// cleanup. `views` are the views so far (`(view slot, field)`).
///
/// The mapping's sinks are what a tick must compute: commits, root
/// outputs, probes and state keys. Without `Options::debug`, an
/// authored net left inside a table is no longer computed and leaves the
/// debug map, as dead code does. With it, every net stays computed and
/// readable: the instructions that compute observed nets stay, but
/// nothing on the way to a commit reads them, so a fused tick (which
/// settles only what commits read) runs the tables alone.
pub(super) fn run(
    em: &mut super::emit::Emitter,
    ports: &PortMap,
    debug: &mut DebugMap,
    probe_slots: &[u32],
    views: &mut Vec<(u32, super::lift::Seg)>,
    opts: &super::Options,
    cfg: &Cfg,
) {
    let n_slots = em.widths.len();
    if em.instrs.is_empty() {
        return;
    }
    let view_base: FastMap<u32, u32> = views.iter().map(|&(f, seg)| (f, seg.base)).collect();
    let base = |s: u32| view_base.get(&s).copied().unwrap_or(s);
    let (_, order) = super::emit::levelize(&em.ops, &em.instrs, n_slots);
    let mut konst: FastMap<u32, u64> = FastMap::default();
    konst.insert(ZERO_SLOT, 0);
    for &(s, v) in &em.consts {
        konst.insert(s, v);
    }
    let mut width = width_bounds(
        em.widths.clone(),
        &em.ops,
        &em.instrs,
        &em.imms,
        &order,
        &em.commits,
        &em.mems,
    );
    for (w, &d) in width.iter_mut().zip(&em.widths) {
        *w = (*w).min(d);
    }
    let mut sink = vec![false; n_slots];
    for c in &em.commits {
        for s in c.reads() {
            sink[s as usize] = true;
        }
    }
    for p in &ports.outputs {
        sink[base(p.slot) as usize] = true;
    }
    for &s in probe_slots {
        sink[base(s) as usize] = true;
    }
    for k in &em.state_keys {
        if let StateLoc::Slot(s) = k.loc {
            sink[base(s) as usize] = true;
        }
    }
    let g = Graph {
        ops: &em.ops,
        instrs: &em.instrs,
        imms: &em.imms,
        order: &order,
        width,
        konst,
        mems: &em.mems,
    };
    let m = g.map(&sink, cfg);
    if m.cones.is_empty() {
        return;
    }
    let mut keep = m.keep.clone();
    let mut is_root = vec![false; em.instrs.len()];
    for c in &m.cones {
        is_root[c.root as usize] = true;
    }
    let mut writer = vec![NONE; n_slots];
    for (k, i) in em.instrs.iter().enumerate() {
        writer[i.dst as usize] = k as u32;
    }
    if opts.debug {
        // Keep every observed net computed (see above).
        let mut stack: Vec<u32> = Vec::new();
        for map in &debug.net_slots {
            stack.extend(map.values().map(|&s| base(s)));
        }
        stack.extend(views.iter().map(|&(_, seg)| seg.base));
        while let Some(s) = stack.pop() {
            let Some(&w) = writer.get(s as usize) else {
                continue;
            };
            if w == NONE || keep[w as usize] || is_root[w as usize] {
                continue;
            }
            keep[w as usize] = true;
            stack.extend(operands(em.ops[w as usize], &em.instrs[w as usize]));
        }
    }
    let contents: Vec<Vec<u64>> = m.cones.iter().map(|c| g.contents(c)).collect();
    let width = g.width;

    // ── Emission ───────────────────────────────────────────────────────
    let old_ops = std::mem::take(&mut em.ops);
    let old_instrs = std::mem::take(&mut em.instrs);
    let old_imms = std::mem::take(&mut em.imms);
    for (k, &kp) in keep.iter().enumerate() {
        if kp {
            em.ops.push(old_ops[k]);
            em.instrs.push(old_instrs[k]);
            em.imms.push(old_imms[k]);
        }
    }
    let mut rom_of: FastMap<(u8, u8, Vec<u64>), u32> = FastMap::default();
    let mut index_of: FastMap<Vec<(u32, u8, u8)>, u32> = FastMap::default();
    let mut shr_of: FastMap<(u32, u8, u8), u32> = FastMap::default();
    for (c, table) in m.cones.iter().zip(contents) {
        let dst = old_instrs[c.root as usize].dst;
        let w = em.widths[dst as usize];
        let mem = *rom_of
            .entry((c.bits as u8, w, table))
            .or_insert_with_key(|(bits, w, table)| {
                em.mems.push(MemDesc {
                    addr_bits: *bits,
                    width: *w,
                    init: table.iter().map(|v| v & mask(*w)).collect(),
                    writable: false,
                });
                (em.mems.len() - 1) as u32
            });
        let index = match index_of.get(&c.fields) {
            Some(&s) => s,
            None => {
                let nf = c.fields.len();
                let mut acc = NONE;
                let mut at = 0u32;
                for (j, &(s, lo, len)) in c.fields.iter().enumerate() {
                    let whole = lo == 0 && (len >= width[s as usize] || j + 1 == nf);
                    let field = if whole {
                        s
                    } else {
                        *shr_of.entry((s, lo, len)).or_insert_with(|| {
                            em.widths.push(len);
                            let t = (em.widths.len() - 1) as u32;
                            em.ops.push(Op::Shr);
                            em.instrs.push(Instr {
                                dst: t,
                                a: s,
                                b: lo as u32,
                                c: ZERO_SLOT,
                            });
                            em.imms.push(mask(len));
                            t
                        })
                    };
                    acc = if acc == NONE {
                        field
                    } else {
                        // (A whole top field may be wider than its bits:
                        // the read masks the address.)
                        let top = if whole { width[s as usize] } else { len };
                        let wn = (at + top as u32).min(64) as u8;
                        em.widths.push(wn);
                        let t = (em.widths.len() - 1) as u32;
                        em.ops.push(Op::Concat);
                        em.instrs.push(Instr {
                            dst: t,
                            a: acc,
                            b: field,
                            c: at,
                        });
                        em.imms.push(0);
                        t
                    };
                    at += len as u32;
                }
                index_of.insert(c.fields.clone(), acc);
                acc
            }
        };
        em.ops.push(Op::MemRead);
        em.instrs.push(Instr {
            dst,
            a: index,
            b: mem,
            c: ZERO_SLOT,
        });
        em.imms.push(0);
    }
    if !opts.debug {
        // Nets no longer computed leave the debug map.
        let mut written = vec![false; em.widths.len()];
        for i in &em.instrs {
            written[i.dst as usize] = true;
        }
        let gone = |s: u32| {
            let b = base(s);
            (b as usize) < n_slots && writer[b as usize] != NONE && !written[b as usize]
        };
        for map in &mut debug.net_slots {
            map.retain(|_, s| !gone(*s));
        }
        views.retain(|&(f, _)| !gone(f));
    }
}

#[cfg(test)]
mod tests;
