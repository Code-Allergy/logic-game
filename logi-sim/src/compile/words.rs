//! Word-level cleanup (issue #117, stage 3).
//!
//! Runs after word lifting (or after simplify when lifting is off), before
//! levelizing. Lifting and function matching leave word ops joined by
//! adapters: slices (`Shr`) and concatenations (`Concat`) at the 1-, 8- and
//! 16-bit boundaries of the player's subcircuits, and state split into
//! one word per subcircuit (two 8-bit state words for a 16-bit register
//! built from two 8-bit registers). This pass rebuilds the instruction list
//! as a hash-consed expression graph, applying exact word-level rewrites as
//! it goes, then emits only what the program needs.
//!
//! **Rewrites** (each exact on values; see the tests):
//! - constant folding, copies, `Mux` with a constant select or equal arms,
//!   `x & x`, `x | x`, `x ^ x`, a double complement, and a select that is a
//!   1-bit complement (`Mux(!s, a, b)` is `Mux(s, b, a)`);
//! - **slices**: a full-width slice is its operand; a slice of a slice is
//!   one slice; a slice of a concatenation is the part it selects; a slice
//!   of a constant is a constant;
//! - **Split→Merge round trips**: a concatenation of consecutive slices of
//!   one word is one slice of it (or the word itself);
//! - **merging across bit ranges**: a concatenation of two `Mux`es with the
//!   same select (or two bitwise ops of the same kind), directly or through
//!   slices, is one wider op of the concatenated arms, used only when every
//!   arm's concatenation simplifies away (so it never adds instructions).
//!   The two narrow results are then bit fields of the wide one, and are
//!   read from it.
//! - **shifts** (`Options::shifts`): a concatenation whose low part is
//!   constant zero is one [`Op::Shl`] of the high part (through a slice:
//!   `{x[14:0], 1'b0}` is `(x << 1) & 0xffff`); a slice of such a shift
//!   is a slice of its operand, or zero;
//! - **barrel shifters** (`Options::barrels`): `Mux(v[i], y, y << 2^i)`
//!   truncated to the width `w`, where `y` is a word of at most `w` bits
//!   (`i == 0`) or `ShlV(x, v[0..i])` (masked to `w` bits), is
//!   `ShlV(.., v[0..=i])`. The amount has at most 6 bits (the stage's
//!   shift `2^i` is below 64), so `ShlV`'s `& 63` never applies, and a
//!   shift by `w` or more gives 0, as the muxes do.
//! - **state merging**: state words that something reads only as parts of
//!   one concatenation (`Concat(A, B)` with `A`, `B` committed by `Copy`)
//!   are merged into one state word whose next value is the concatenation
//!   of theirs; merging repeats while the rewrites expose new pairs (the
//!   next values of merged slaves concatenate their masters). The old state
//!   elements become bit fields (views) of the merged word, so state keys,
//!   `StateSnapshot`s and probes are unchanged.
//!
//! **Emission.** Only nodes that commits, root outputs, probes, views and
//! (under `Options::debug`) authored nets need are emitted. A net whose
//! node has no slot is read as a bit field of a node that has one (a
//! view, [`Program::views`]) when it is one; otherwise it gets its slot
//! under `debug` and is dropped like dead code otherwise.
//!
//! The pass keeps its result only when it has fewer instructions plus
//! commits than its input (with and without state merging, the cheaper).
//! It is linear in the program size per round, with a few rounds.

use super::emit::Emitter;
use super::lift::Seg;
use super::program::*;
use super::simplify::fold;
use super::{FastMap, FastSet, Options};
use crate::netlist::mask;

const NONE: u32 = u32::MAX;
/// Rounds of state merging (each may expose new pairs).
const MAX_ROUNDS: usize = 4;
/// Depth of nested op merging.
const MAX_DEPTH: u32 = 6;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Kind {
    /// A slot no instruction writes (input, state, zero): `a`.
    Leaf,
    /// The value `imm`.
    Const,
    Op(Op),
}

/// One node. For `Op`, `a`, `b`, `c` are node ids where the op takes a
/// slot and literals otherwise. Every node's value fits in `w` bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct Node {
    kind: Kind,
    a: u32,
    b: u32,
    c: u32,
    imm: u64,
    w: u8,
}

/// Bits `lo..lo + len` of a node equal bits `toff..` of node `target`.
#[derive(Debug, Clone, Copy)]
struct Piece {
    lo: u8,
    len: u8,
    target: u32,
    toff: u8,
}

/// The expression graph of one attempt.
struct Graph<'a> {
    nodes: Vec<Node>,
    cons: FastMap<Node, u32>,
    /// Per node: bit fields known to equal bit fields of other nodes.
    pieces: FastMap<u32, Vec<Piece>>,
    mems: &'a [MemDesc],
    /// Per slot: is it a `Commit::Copy` state slot?
    copy_state: &'a [bool],
    /// Merged word slot -> (first, last) old state slot in it.
    group_ends: FastMap<u32, (u32, u32)>,
    /// Adjacent state pairs `(A, B)` seen concatenated as `Concat(A, B,
    /// width(A))` (merge candidates).
    pairs: Vec<(u32, u32)>,
    /// `Options::shifts`.
    shifts: bool,
    /// `Options::barrels` (with `shifts`).
    barrels: bool,
}

/// The bit field `(lo, len)` an `Shl` mask keeps, if it is one run of
/// ones at or above the shift `b` (so every kept bit is a bit of the
/// operand, none a shifted-in zero).
fn shl_field(b: u32, imm: u64) -> Option<(u32, u32)> {
    let lo = imm.trailing_zeros();
    let len = imm.count_ones();
    (imm != 0 && imm == mask(len as u8) << lo && lo >= b).then_some((lo, len))
}

impl Graph<'_> {
    fn node(&self, n: u32) -> &Node {
        &self.nodes[n as usize]
    }

    fn w(&self, n: u32) -> u8 {
        self.nodes[n as usize].w
    }

    fn intern(&mut self, n: Node) -> u32 {
        if let Some(&id) = self.cons.get(&n) {
            return id;
        }
        let id = self.nodes.len() as u32;
        self.nodes.push(n);
        self.cons.insert(n, id);
        id
    }

    fn leaf(&mut self, slot: u32, w: u8) -> u32 {
        self.intern(Node {
            kind: Kind::Leaf,
            a: slot,
            b: 0,
            c: 0,
            imm: 0,
            w,
        })
    }

    fn konst(&mut self, v: u64, w: u8) -> u32 {
        let w = w.max(1);
        self.intern(Node {
            kind: Kind::Const,
            a: 0,
            b: 0,
            c: 0,
            imm: v & mask(w),
            w,
        })
    }

    fn cval(&self, n: u32) -> Option<u64> {
        let x = self.node(n);
        (x.kind == Kind::Const).then_some(x.imm)
    }

    /// `(base, offset, length)` if `n` is a slice of another node.
    fn as_slice(&self, n: u32) -> (u32, u8, u8) {
        let x = self.node(n);
        if x.kind == Kind::Op(Op::Shr) {
            (x.a, x.b as u8, x.imm.count_ones() as u8)
        } else {
            (n, 0, x.w)
        }
    }

    /// The node for `op` with these operands and immediate, `w` bits wide,
    /// after the rewrites in the module docs.
    fn mk(&mut self, op: Op, a: u32, b: u32, c: u32, imm: u64, w: u8) -> u32 {
        let (sa, sb, sc) = op.slot_operands();
        if op != Op::Clock {
            let all_const = (!sa || self.cval(a).is_some())
                && (!sb || self.cval(b).is_some())
                && (!sc || self.cval(c).is_some());
            if all_const {
                let ins = Instr { dst: 0, a, b, c };
                let cv = |n: u32| self.cval(n);
                if let Some(v) = fold(op, &ins, imm, &cv, self.mems) {
                    return self.konst(v, w);
                }
            }
        }
        match op {
            Op::Copy => return a,
            Op::Shl if imm == 0 => return self.konst(0, w),
            // A slice (the graph's `Shr` nodes are slices: a mask of low
            // ones).
            Op::Shl if b == 0 && imm == mask(imm.count_ones() as u8) => {
                return self.mk(Op::Shr, a, c, 0, imm, w);
            }
            Op::ShlV => {
                if let Some(k) = self.cval(b) {
                    return self.mk(Op::Shl, a, (k & 63) as u32, 0, imm, w);
                }
                if self.cval(a) == Some(0) {
                    return self.konst(0, w);
                }
            }
            Op::Shr if imm == mask(imm.count_ones() as u8) && imm != 0 => {
                return self.slice(a, b, imm.count_ones());
            }
            Op::Concat => return self.concat(a, b, c, w, 0),
            Op::Mux => {
                if let Some(s) = self.cval(a) {
                    return if s != 0 { c } else { b };
                }
                if b == c {
                    return b;
                }
                let sel = *self.node(a);
                if sel.kind == Kind::Op(Op::Not) && sel.imm == 1 && self.w(sel.a) == 1 {
                    return self.mk(Op::Mux, sel.a, c, b, 0, w);
                }
                if self.barrels
                    && let Some(r) = self.barrel(a, b, c, w)
                {
                    return r;
                }
            }
            Op::And | Op::Or if a == b => return a,
            Op::Xor if a == b => return self.konst(0, w),
            Op::And => {
                for (x, y) in [(a, b), (b, a)] {
                    match self.cval(x) {
                        Some(0) => return self.konst(0, w),
                        Some(m) if m & mask(self.w(y)) == mask(self.w(y)) => return y,
                        _ => {}
                    }
                }
            }
            Op::Or | Op::Xor => {
                for (x, y) in [(a, b), (b, a)] {
                    if self.cval(x) == Some(0) {
                        return y;
                    }
                }
            }
            Op::Not => {
                let x = *self.node(a);
                if x.kind == Kind::Op(Op::Not)
                    && x.imm == imm
                    && self.w(x.a) <= imm.count_ones() as u8
                {
                    return x.a;
                }
            }
            _ => {}
        }
        let (mut a, mut b) = (a, b);
        if matches!(
            op,
            Op::And
                | Op::Or
                | Op::Xor
                | Op::Nand
                | Op::Nor
                | Op::Xnor
                | Op::Eq
                | Op::AddSum
                | Op::AddCarry
        ) && a > b
        {
            std::mem::swap(&mut a, &mut b);
        }
        let n = Node {
            kind: Kind::Op(op),
            a,
            b,
            c,
            imm,
            w,
        };
        if op == Op::Clock {
            // Never merged (as simplify's CSE).
            self.nodes.push(n);
            return (self.nodes.len() - 1) as u32;
        }
        self.intern(n)
    }

    /// Bits `off..off + len` of node `x`.
    fn slice(&mut self, x: u32, off: u32, len: u32) -> u32 {
        let nx = *self.node(x);
        let wx = nx.w as u32;
        if off >= wx || len == 0 {
            return self.konst(0, 1);
        }
        let len = len.min(wx - off);
        if off == 0 && len == wx {
            return x;
        }
        match nx.kind {
            Kind::Const => return self.konst((nx.imm >> off) & mask(len as u8), len as u8),
            Kind::Op(Op::Shr) => {
                let l2 = nx.imm.count_ones();
                return self.slice(nx.a, nx.b + off, len.min(l2.saturating_sub(off)));
            }
            Kind::Op(Op::Concat) if (self.w(nx.a) as u32) <= nx.c => {
                if off + len <= nx.c {
                    return self.slice(nx.a, off, len);
                }
                if off >= nx.c {
                    return self.slice(nx.b, off - nx.c, len);
                }
            }
            Kind::Op(Op::Shl) => {
                if let Some((lo, flen)) = shl_field(nx.b, nx.imm) {
                    if off + len <= lo || off >= lo + flen {
                        return self.konst(0, len as u8);
                    }
                    if off >= lo && off + len <= lo + flen {
                        return self.slice(nx.a, off - nx.b + nx.c, len);
                    }
                }
            }
            _ => {}
        }
        self.intern(Node {
            kind: Kind::Op(Op::Shr),
            a: x,
            b: off,
            c: 0,
            imm: mask(len as u8),
            w: len as u8,
        })
    }

    /// One stage of a barrel shifter (see the module docs):
    /// `Mux(sel, y, t)`, `w` bits wide, as one `ShlV`, if `sel` is bit `i`
    /// of a word `v`, `t` is `(y << 2^i) & mask(w)` (an `Shl` of `y` or of
    /// the word `y` is a slice of), and `y` is a word of
    /// at most `w` bits (`i == 0`) or `ShlV(x, v[0..i])` masked to `w`
    /// bits.
    fn barrel(&mut self, sel: u32, y: u32, t: u32, w: u8) -> Option<u32> {
        let nt = *self.node(t);
        let wy = self.w(y);
        // `t` shifts `y`, or the word `y` is a bit field of (from its
        // offset), keeping exactly the bits of `y << k` below `w`.
        let (yb, yo, _) = self.as_slice(y);
        if nt.kind != Kind::Op(Op::Shl) || nt.a != yb || nt.c != yo as u32 || wy > w {
            return None;
        }
        let k = nt.b;
        if !k.is_power_of_two() || nt.imm != (mask(wy) << k) & mask(w) {
            return None;
        }
        let (v, i, len) = self.as_slice(sel);
        if len != 1 || u32::from(i) != k.trailing_zeros() {
            return None;
        }
        debug_assert!(i <= 5, "a literal shift is below 64");
        if i == 0 {
            return Some(self.mk(Op::ShlV, y, sel, 0, mask(w), w));
        }
        let ny = *self.node(y);
        if ny.kind == Kind::Op(Op::ShlV) && ny.imm == mask(w) {
            let (va, ia, la) = self.as_slice(ny.b);
            if va == v && ia == 0 && la == i && self.w(ny.b) == i {
                let amt = self.slice(v, 0, u32::from(i) + 1);
                return Some(self.mk(Op::ShlV, ny.a, amt, 0, mask(w), w));
            }
        }
        None
    }

    /// `p | q << c`, `w` bits wide.
    fn concat(&mut self, p: u32, q: u32, c: u32, w: u8, depth: u32) -> u32 {
        if c >= 64 || self.cval(q) == Some(0) {
            return p;
        }
        // A zero low part: a shift of the high part (through a slice of
        // it, or a shift).
        if self.shifts && self.cval(p) == Some(0) && (self.w(p) as u32) <= c {
            let nq = *self.node(q);
            let (a, b, sh, keep) = match nq.kind {
                Kind::Op(Op::Shl) if nq.b + c < 64 => (nq.a, nq.b + c, nq.c, nq.imm),
                Kind::Op(Op::Shr) => (nq.a, c, nq.b, nq.imm),
                _ => (q, c, 0, mask(nq.w)),
            };
            let imm = (keep << c) & mask(w);
            return self.mk(Op::Shl, a, b, sh, imm, w);
        }
        let (bp, op_, lp) = self.as_slice(p);
        let (bq, oq, lq) = self.as_slice(q);
        // Consecutive slices of one word.
        if bp == bq
            && oq as u32 == op_ as u32 + c
            && (lp as u32 == c || (lp as u32) < c && op_ as u32 + lp as u32 >= self.w(bp) as u32)
        {
            return self.slice(bp, op_ as u32, c + lq as u32);
        }
        if depth < MAX_DEPTH
            && let Some(r) = self.merge_ops(p, q, c, depth)
        {
            return r;
        }
        // A pair of whole state words: a merge candidate.
        let (np, nq) = (*self.node(p), *self.node(q));
        if np.kind == Kind::Leaf && nq.kind == Kind::Leaf && np.w as u32 == c {
            // A merged word stands for its first and last members.
            let last = self.group_ends.get(&np.a).map_or(np.a, |e| e.1);
            let first = self.group_ends.get(&nq.a).map_or(nq.a, |e| e.0);
            let state = |s: u32| self.copy_state.get(s as usize).copied().unwrap_or(false);
            if state(last) && state(first) && last != first {
                self.pairs.push((last, first));
            }
        }
        self.intern(Node {
            kind: Kind::Op(Op::Concat),
            a: p,
            b: q,
            c,
            imm: 0,
            w,
        })
    }

    /// `x` as a bitwise op or mux over the same bit positions: `(op, the
    /// select (for Mux), value operands, imm)`, through a slice.
    fn as_op(&mut self, x: u32) -> Option<(Op, u32, [u32; 2], u64)> {
        let (base, off, len) = self.as_slice(x);
        let nb = *self.node(base);
        let Kind::Op(op) = nb.kind else {
            return None;
        };
        let sliced = base != x;
        let arm = |g: &mut Self, n: u32| {
            if sliced {
                g.slice(n, off as u32, len as u32)
            } else {
                n
            }
        };
        match op {
            Op::Mux => {
                let (b, c) = (arm(self, nb.b), arm(self, nb.c));
                Some((op, nb.a, [b, c], 0))
            }
            Op::And | Op::Or | Op::Xor => {
                let (a, b) = (arm(self, nb.a), arm(self, nb.b));
                Some((op, NONE, [a, b], 0))
            }
            Op::Not | Op::Nand | Op::Nor | Op::Xnor => {
                let wi = nb.imm.count_ones();
                if nb.imm != mask(wi as u8) || (sliced && off as u32 + len as u32 > wi) {
                    return None;
                }
                let imm = if sliced { mask(len) } else { nb.imm };
                let a = arm(self, nb.a);
                let b = if op == Op::Not { NONE } else { arm(self, nb.b) };
                Some((op, NONE, [a, b], imm))
            }
            _ => None,
        }
    }

    /// `Concat(P, Q, c)` of two ops of one kind as one op of concatenated
    /// arms, if every arm's concatenation simplifies away.
    fn merge_ops(&mut self, p: u32, q: u32, c: u32, depth: u32) -> Option<u32> {
        let (op, sp, ap, ip) = self.as_op(p)?;
        let (oq, sq, aq, iq) = self.as_op(q)?;
        if op != oq || sp != sq {
            return None;
        }
        let wq = self.w(q) as u32;
        let w = (c + wq).min(64) as u8;
        let fits = |g: &Self, n: u32| n == NONE || g.w(n) as u32 <= c;
        let mut imm = 0;
        match op {
            Op::Mux => {}
            Op::And | Op::Or | Op::Xor => {
                if !(fits(self, ap[0]) && fits(self, ap[1])) {
                    return None;
                }
            }
            _ => {
                // The complement ops: P's mask must be exactly `c` bits.
                if ip != mask(c as u8) || !(fits(self, ap[0]) && fits(self, ap[1])) {
                    return None;
                }
                imm = mask((c + iq.count_ones()).min(64) as u8);
            }
        }
        let mut arms = [NONE; 2];
        for k in 0..2 {
            if ap[k] == NONE {
                continue;
            }
            let wk = (c + self.w(aq[k]) as u32).min(64) as u8;
            let r = self.concat(ap[k], aq[k], c, wk, depth + 1);
            if matches!(self.node(r).kind, Kind::Op(Op::Concat | Op::Shl)) {
                // Still an instruction: the merge would add it.
                return None;
            }
            arms[k] = r;
        }
        let r = match op {
            Op::Mux => self.mk(Op::Mux, sp, arms[0], arms[1], 0, w),
            Op::Not => self.mk(Op::Not, arms[0], ZERO_SLOT, ZERO_SLOT, imm, w),
            _ => self.mk(op, arms[0], arms[1], ZERO_SLOT, imm, w),
        };
        // P and Q are bit fields of the result (when they do not overlap).
        if self.w(p) as u32 <= c {
            let (bp, op_, lp) = self.as_slice(p);
            self.add_piece(bp, op_, lp, r, 0);
            let (bq, oq_, lq) = self.as_slice(q);
            self.add_piece(bq, oq_, lq, r, c as u8);
        }
        Some(r)
    }

    fn add_piece(&mut self, base: u32, lo: u8, len: u8, target: u32, toff: u8) {
        if base == target || !matches!(self.node(base).kind, Kind::Op(_)) {
            return;
        }
        self.pieces.entry(base).or_default().push(Piece {
            lo,
            len,
            target,
            toff,
        });
    }

    /// A piece of `n` covering `off..off + len`.
    fn piece(&self, n: u32, off: u8, len: u8) -> Option<(u32, u8)> {
        let v = self.pieces.get(&n)?;
        v.iter()
            .find(|p| p.lo <= off && off as u32 + len as u32 <= p.lo as u32 + p.len as u32)
            .map(|p| (p.target, p.toff + (off - p.lo)))
    }
}

/// What one attempt produces, before it is applied.
struct Attempt {
    ops: Vec<Op>,
    instrs: Vec<Instr>,
    imms: Vec<u64>,
    /// Constants added.
    consts: Vec<(u32, u64)>,
    commits: Vec<Commit>,
    /// Widths of the slots added (numbered from the input's slot count).
    widths: Vec<u8>,
    /// New slot (plain or view) of each old slot or view, or `NONE`.
    remap: Vec<u32>,
    /// New slot of each state key's slot: the state word, or a view of it.
    key_remap: FastMap<u32, u32>,
    /// Slot of each root output.
    port_slots: Vec<u32>,
    views: Vec<(u32, Seg)>,
}

impl Attempt {
    fn cost(&self) -> usize {
        self.instrs.len() + self.commits.len()
    }
}

/// Run the pass on `em` (see the module docs). `views` are the views word
/// lifting made; the result replaces them.
pub(super) fn clean(
    em: &mut Emitter,
    ports: &mut PortMap,
    debug: &mut DebugMap,
    probe_slots: &[u32],
    views: Vec<(u32, Seg)>,
    opts: &Options,
) -> Vec<(u32, Seg)> {
    let n_slots = em.widths.len();
    let mut copy_state = vec![false; n_slots];
    for c in &em.commits {
        if let Commit::Copy { state, .. } = *c {
            copy_state[state as usize] = true;
        }
    }
    let (_, order) = super::emit::levelize(&em.ops, &em.instrs, n_slots);
    let view_of: FastMap<u32, Seg> = views.iter().copied().collect();
    // The bits a state word can hold: its next value's, and those a state
    // key can write (word lifting gives a bank of `n` lanes a wider
    // word). Bits above are always zero.
    let mut value_w = em.widths.clone();
    for c in &em.commits {
        if let Commit::Copy { state, next } = *c {
            value_w[state as usize] = em.widths[state as usize].min(em.widths[next as usize]);
        }
    }
    for key in &em.state_keys {
        if let StateLoc::Slot(s) = key.loc {
            let (base, end) = match view_of.get(&s) {
                Some(seg) => (seg.base, seg.off + seg.len),
                None => (s, em.widths[s as usize]),
            };
            let v = &mut value_w[base as usize];
            *v = (*v).max(end.min(em.widths[base as usize]));
        }
    }
    let keep_nets = opts.debug || !opts.dce;
    let input = Input {
        shifts: opts.shifts,
        barrels: opts.shifts && opts.barrels,
        em,
        ports,
        debug,
        probe_slots,
        view_of: &view_of,
        order: &order,
        copy_state: &copy_state,
        value_w: &value_w,
        keep_nets,
    };

    // Round 0: no merging; later rounds merge the pairs seen so far.
    let mut links: FastMap<u32, u32> = FastMap::default();
    let mut prev: FastMap<u32, u32> = FastMap::default();
    let (mut best, pairs) = attempt(&input, &[]);
    let mut seen_pairs = pairs;
    for _ in 0..MAX_ROUNDS {
        let mut new = false;
        for &(a, b) in &seen_pairs {
            if links.contains_key(&a) || prev.contains_key(&b) {
                continue;
            }
            // No cycles: B's chain must not lead back to A.
            let mut x = b;
            let mut cycle = false;
            while let Some(&y) = links.get(&x) {
                if y == a {
                    cycle = true;
                    break;
                }
                x = y;
            }
            if cycle || x == a {
                continue;
            }
            links.insert(a, b);
            prev.insert(b, a);
            new = true;
        }
        if !new {
            break;
        }
        let groups = chains(&links, &prev, input.value_w);
        if groups.is_empty() {
            break;
        }
        let (att, pairs) = attempt(&input, &groups);
        seen_pairs = pairs;
        if att.cost() < best.cost() {
            best = att;
        }
    }
    if best.cost() >= em_cost(input.em) {
        return views;
    }
    apply(em, ports, debug, best)
}

fn em_cost(em: &Emitter) -> usize {
    em.instrs.len() + em.commits.len()
}

/// Maximal chains of linked state slots that fit in one word.
fn chains(links: &FastMap<u32, u32>, prev: &FastMap<u32, u32>, widths: &[u8]) -> Vec<Vec<u32>> {
    let mut heads: Vec<u32> = links
        .keys()
        .copied()
        .filter(|a| !prev.contains_key(a))
        .collect();
    heads.sort_unstable();
    let mut out = Vec::new();
    for h in heads {
        let mut chain = vec![h];
        let mut w = widths[h as usize] as u32;
        let mut x = h;
        while let Some(&y) = links.get(&x) {
            let wy = widths[y as usize] as u32;
            if w + wy > 64 {
                if chain.len() > 1 {
                    out.push(std::mem::take(&mut chain));
                } else {
                    chain.clear();
                }
                chain.push(y);
                w = wy;
            } else {
                chain.push(y);
                w += wy;
            }
            x = y;
        }
        if chain.len() > 1 {
            out.push(chain);
        }
    }
    out
}

struct Input<'a> {
    em: &'a Emitter,
    ports: &'a PortMap,
    debug: &'a DebugMap,
    probe_slots: &'a [u32],
    view_of: &'a FastMap<u32, Seg>,
    order: &'a [u32],
    copy_state: &'a [bool],
    /// Per slot: the bits its value can have (narrower than its width for
    /// some state words).
    value_w: &'a [u8],
    keep_nets: bool,
    shifts: bool,
    barrels: bool,
}

/// Build, rewrite and emit once, merging the state slots of each group in
/// `groups` into one word. Returns the attempt and the state pairs seen.
fn attempt(inp: &Input<'_>, groups: &[Vec<u32>]) -> (Attempt, Vec<(u32, u32)>) {
    let em = inp.em;
    let n_slots = em.widths.len();
    let mut g = Graph {
        nodes: Vec::with_capacity(em.instrs.len() * 2 + 16),
        cons: FastMap::default(),
        pieces: FastMap::default(),
        mems: &em.mems,
        copy_state: inp.copy_state,
        group_ends: FastMap::default(),
        pairs: Vec::new(),
        shifts: inp.shifts,
        barrels: inp.barrels,
    };
    let mut widths_new: Vec<u8> = Vec::new();

    // Merged state words.
    // Per merged old slot: (group, offset).
    let mut merged_at: FastMap<u32, (u32, u8)> = FastMap::default();
    let mut group_slot: Vec<u32> = Vec::new();
    for (k, grp) in groups.iter().enumerate() {
        let w: u32 = grp.iter().map(|&s| inp.value_w[s as usize] as u32).sum();
        widths_new.push(w as u8);
        let s = (n_slots + widths_new.len() - 1) as u32;
        group_slot.push(s);
        g.group_ends.insert(s, (grp[0], *grp.last().unwrap()));
        let mut off = 0u32;
        for &x in grp {
            merged_at.insert(x, (k as u32, off as u8));
            off += inp.value_w[x as usize] as u32;
        }
    }

    // Nodes of slots.
    let mut node_of = vec![NONE; n_slots];
    node_of[ZERO_SLOT as usize] = g.konst(0, 1);
    for &(s, v) in &em.consts {
        node_of[s as usize] = g.konst(v, em.widths[s as usize]);
    }
    let mut written = vec![false; n_slots];
    for ins in &em.instrs {
        written[ins.dst as usize] = true;
    }
    let leaf_of = |g: &mut Graph<'_>, s: u32| -> u32 {
        if let Some(&(k, off)) = merged_at.get(&s) {
            let gs = group_slot[k as usize];
            let gw = groups[k as usize]
                .iter()
                .map(|&x| inp.value_w[x as usize] as u32)
                .sum::<u32>() as u8;
            let l = g.leaf(gs, gw);
            g.slice(l, off as u32, inp.value_w[s as usize] as u32)
        } else {
            g.leaf(s, inp.value_w[s as usize])
        }
    };
    for s in 0..n_slots {
        if node_of[s] == NONE && !written[s] && !inp.view_of.contains_key(&(s as u32)) {
            node_of[s] = leaf_of(&mut g, s as u32);
        }
    }
    for &k in inp.order {
        let k = k as usize;
        let op = em.ops[k];
        let ins = em.instrs[k];
        let (sa, sb, sc) = op.slot_operands();
        let m = |s: u32, is: bool| if is { node_of[s as usize] } else { s };
        let (a, b, c) = (m(ins.a, sa), m(ins.b, sb), m(ins.c, sc));
        debug_assert!(!sa || a != NONE);
        let n = g.mk(op, a, b, c, em.imms[k], em.widths[ins.dst as usize]);
        node_of[ins.dst as usize] = n;
    }
    let node = |s: u32| -> u32 { node_of[s as usize] };

    // Commits: merged groups commit the concatenation of their nexts.
    let mut commits: Vec<(Commit, u32)> = Vec::new(); // (commit, next node or NONE)
    for c in &em.commits {
        match *c {
            // A merged word commits once, below.
            Commit::Copy { state, .. } if merged_at.contains_key(&state) => {}
            Commit::Copy { state, next } => {
                commits.push((Commit::Copy { state, next: NONE }, node(next)))
            }
            other => commits.push((other, NONE)),
        }
    }
    let next_of: FastMap<u32, u32> = em
        .commits
        .iter()
        .filter_map(|c| match *c {
            Commit::Copy { state, next } => Some((state, next)),
            _ => None,
        })
        .collect();
    for (k, grp) in groups.iter().enumerate() {
        let mut acc = NONE;
        let mut off = 0u32;
        for &x in grp {
            let nx = node(next_of[&x]);
            let wx = inp.value_w[x as usize] as u32;
            acc = if acc == NONE {
                nx
            } else {
                g.concat(acc, nx, off, (off + wx) as u8, 0)
            };
            off += wx;
        }
        commits.push((
            Commit::Copy {
                state: group_slot[k],
                next: NONE,
            },
            acc,
        ));
    }

    // ── Emission ──────────────────────────────────────────────────────────
    let mut em_out = Emission {
        home: FastMap::default(),
        redirect: FastMap::default(),
        stack: Vec::new(),
        zero: node_of[ZERO_SLOT as usize],
    };
    // Home slots: an original destination of the same width.
    let mut home_pref: FastMap<u32, u32> = FastMap::default();
    for &k in inp.order {
        let d = em.instrs[k as usize].dst;
        let n = node_of[d as usize];
        if g.node(n).w == em.widths[d as usize] {
            home_pref.entry(n).or_insert(d);
        }
    }
    let mut commit_out = Vec::with_capacity(commits.len());
    for &(c, n) in &commits {
        if let Commit::Copy { state, .. } = c {
            em_out.need(&mut g, n);
            commit_out.push((Commit::Copy { state, next: NONE }, n));
        } else {
            for s in c.reads() {
                em_out.need(&mut g, node(s));
            }
            commit_out.push((c, NONE));
        }
    }
    for p in &inp.ports.outputs {
        em_out.need(&mut g, node(p.slot));
    }
    // Bit fields that must stay readable: probes, state keys, and (with
    // `keep_nets`) every authored net.
    let field = |s: u32| -> Option<(u32, u8, u8)> {
        if let Some(seg) = inp.view_of.get(&s) {
            let n = node_of[seg.base as usize];
            (n != NONE).then_some((n, seg.off, seg.len))
        } else {
            let n = node_of[s as usize];
            (n != NONE).then(|| (n, 0, em.widths[s as usize]))
        }
    };
    let mut required: Vec<u32> = inp.probe_slots.to_vec();
    for key in &em.state_keys {
        if let StateLoc::Slot(s) = key.loc {
            required.push(s);
        }
    }
    if inp.keep_nets {
        for map in &inp.debug.net_slots {
            required.extend(map.values().copied());
        }
    }
    for &s in &required {
        if let Some((n, off, len)) = field(s)
            && em_out.locate(&g, n, off, len).is_none()
        {
            em_out.need(&mut g, n);
        }
    }

    // Slots of the emitted nodes.
    let mut sl = Slots {
        n_slots,
        widths_new,
        consts_new: Vec::new(),
        const_slot: FastMap::default(),
    };
    for &(s, v) in &em.consts {
        sl.const_slot.entry((v, em.widths[s as usize])).or_insert(s);
    }
    let mut materialized: Vec<u32> = em_out.home.keys().copied().collect();
    materialized.sort_unstable();
    for &n in &materialized {
        let s = match home_pref.get(&n) {
            Some(&d) => d,
            None => sl.new_slot(g.w(n)),
        };
        em_out.home.insert(n, s);
    }
    let mut ops = Vec::with_capacity(materialized.len());
    let mut instrs = Vec::with_capacity(materialized.len());
    let mut imms = Vec::with_capacity(materialized.len());
    for &n in &materialized {
        let x = *g.node(n);
        let Kind::Op(op) = x.kind else { unreachable!() };
        let (sa, sb, sc) = op.slot_operands();
        let mut o = |v: u32, is: bool| if is { sl.slot_of(&g, &em_out, v) } else { v };
        let (a, b, c) = (o(x.a, sa), o(x.b, sb), o(x.c, sc));
        let dst = em_out.home[&n];
        ops.push(op);
        instrs.push(Instr { dst, a, b, c });
        imms.push(x.imm);
    }
    // Commits read instruction destinations only.
    let is_dst: FastSet<u32> = instrs.iter().map(|i| i.dst).collect();
    let mut copy_of: FastMap<u32, u32> = FastMap::default();
    let mut copies: Vec<(u32, u32)> = Vec::new();
    let mut read_slot = |sl: &mut Slots, n: u32| -> u32 {
        let s = sl.slot_of(&g, &em_out, n);
        if is_dst.contains(&s) {
            return s;
        }
        *copy_of.entry(s).or_insert_with(|| {
            let t = sl.new_slot(g.w(n));
            copies.push((t, s));
            t
        })
    };
    let mut commits_final = Vec::with_capacity(commit_out.len());
    for (c, n) in commit_out {
        commits_final.push(match c {
            Commit::Copy { state, .. } => Commit::Copy {
                state,
                next: read_slot(&mut sl, n),
            },
            other => other.map_reads(|s| read_slot(&mut sl, node(s))),
        });
    }
    for &(t, s) in &copies {
        ops.push(Op::Copy);
        instrs.push(Instr {
            dst: t,
            a: s,
            b: ZERO_SLOT,
            c: ZERO_SLOT,
        });
        imms.push(0);
    }

    // New slots of old slots and views: plain slots or views.
    let mut remap = vec![NONE; n_slots];
    let mut views: Vec<(u32, Seg)> = Vec::new();
    let mut view_slot: FastMap<Seg, u32> = FastMap::default();
    let mut referenced: Vec<u32> = Vec::new();
    for map in &inp.debug.net_slots {
        referenced.extend(map.values().copied());
    }
    referenced.extend(inp.probe_slots.iter().copied());
    for key in &em.state_keys {
        if let StateLoc::Slot(s) = key.loc {
            referenced.push(s);
        }
    }
    referenced.sort_unstable();
    referenced.dedup();
    for s in referenced {
        if (s as usize) >= n_slots {
            continue;
        }
        let Some((n, off, len)) = field(s) else {
            continue;
        };
        let Some((n2, off2, len)) = em_out.locate(&g, n, off, len) else {
            continue;
        };
        let x2 = *g.node(n2);
        remap[s as usize] = if x2.kind == Kind::Const {
            // A constant bit field: a constant slot.
            sl.konst((x2.imm >> off2) & mask(len), len)
        } else {
            let real = sl.slot_of(&g, &em_out, n2);
            if off2 == 0 && len >= sl.width(em, real) {
                real
            } else {
                let seg = Seg {
                    base: real,
                    off: off2,
                    len,
                };
                *view_slot.entry(seg).or_insert_with(|| {
                    let f = sl.new_slot(len);
                    views.push((f, seg));
                    f
                })
            }
        };
    }
    // State keys name the state word itself, never an emitted copy of
    // its bits (which `locate` may find first, as for a slice of a merged
    // word that is also read alone): a write through a key, such as
    // `Sim::import_state`, must reach the state, or the next settle
    // recomputes the copy from the old state.
    let mut key_remap: FastMap<u32, u32> = FastMap::default();
    for key in &em.state_keys {
        let StateLoc::Slot(s) = key.loc else {
            continue;
        };
        let (base, off, len) = match inp.view_of.get(&s) {
            Some(seg) => (seg.base, seg.off, seg.len),
            None => (s, 0, key.width),
        };
        let (base, off) = match merged_at.get(&base) {
            Some(&(k, o)) => (group_slot[k as usize], o + off),
            None => (base, off),
        };
        let new = if off == 0 && len >= sl.width(em, base) {
            base
        } else {
            let seg = Seg { base, off, len };
            *view_slot.entry(seg).or_insert_with(|| {
                let f = sl.new_slot(len);
                views.push((f, seg));
                f
            })
        };
        key_remap.insert(s, new);
    }
    let port_slots: Vec<u32> = inp
        .ports
        .outputs
        .iter()
        .map(|p| sl.slot_of(&g, &em_out, node(p.slot)))
        .collect();
    let pairs = std::mem::take(&mut g.pairs);
    (
        Attempt {
            ops,
            instrs,
            imms,
            consts: sl.consts_new,
            commits: commits_final,
            widths: sl.widths_new,
            remap,
            key_remap,
            port_slots,
            views,
        },
        pairs,
    )
}

/// Slots an attempt adds.
struct Slots {
    n_slots: usize,
    widths_new: Vec<u8>,
    consts_new: Vec<(u32, u64)>,
    const_slot: FastMap<(u64, u8), u32>,
}

impl Slots {
    fn new_slot(&mut self, w: u8) -> u32 {
        self.widths_new.push(w);
        (self.n_slots + self.widths_new.len() - 1) as u32
    }

    fn width(&self, em: &Emitter, s: u32) -> u8 {
        if (s as usize) < self.n_slots {
            em.widths[s as usize]
        } else {
            self.widths_new[s as usize - self.n_slots]
        }
    }

    /// A slot holding the constant `v`.
    fn konst(&mut self, v: u64, w: u8) -> u32 {
        if v == 0 {
            return ZERO_SLOT;
        }
        if let Some(&s) = self.const_slot.get(&(v, w)) {
            return s;
        }
        let s = self.new_slot(w);
        self.consts_new.push((s, v));
        self.const_slot.insert((v, w), s);
        s
    }

    /// The slot of node `n` (emitted, a leaf or a constant).
    fn slot_of(&mut self, g: &Graph<'_>, e: &Emission, n: u32) -> u32 {
        let n = e.redirect.get(&n).copied().unwrap_or(n);
        let x = g.node(n);
        match x.kind {
            Kind::Leaf => x.a,
            Kind::Const => self.konst(x.imm, x.w),
            Kind::Op(_) => e.home[&n],
        }
    }
}

/// Which nodes get a slot.
struct Emission {
    /// Materialized nodes (slots assigned after the walk).
    home: FastMap<u32, u32>,
    /// A node read through another (a bit field of a merged op).
    redirect: FastMap<u32, u32>,
    stack: Vec<u32>,
    /// The constant-zero node.
    zero: u32,
}

impl Emission {
    /// Give `n` (and what it reads) a slot.
    fn need(&mut self, g: &mut Graph<'_>, n: u32) {
        self.stack.push(n);
        while let Some(n) = self.stack.pop() {
            let x = *g.node(n);
            let Kind::Op(op) = x.kind else { continue };
            if self.home.contains_key(&n) || self.redirect.contains_key(&n) {
                continue;
            }
            // A node that is a whole bit field of a merged op reads it.
            if let Some((t, toff)) = g.piece(n, 0, x.w)
                && self.home.contains_key(&t)
            {
                let r = g.slice(t, toff as u32, x.w as u32);
                if r != n && !self.redirect.contains_key(&r) {
                    self.redirect.insert(n, r);
                    self.stack.push(r);
                    continue;
                }
            }
            self.home.insert(n, NONE);
            let (sa, sb, sc) = op.slot_operands();
            for (is, v) in [(sa, x.a), (sb, x.b), (sc, x.c)] {
                if is {
                    self.stack.push(v);
                }
            }
        }
    }

    /// The node holding bits `off..off + len` of `n` in a slot, and their
    /// offset in it, if there is one without new instructions.
    fn locate(&self, g: &Graph<'_>, mut n: u32, mut off: u8, mut len: u8) -> Option<(u32, u8, u8)> {
        for _ in 0..64 {
            if let Some(&r) = self.redirect.get(&n) {
                n = r;
                continue;
            }
            let x = g.node(n);
            if off >= x.w {
                // Past the value's width: zero.
                return Some((self.zero, 0, 1));
            }
            len = len.min(x.w - off);
            match x.kind {
                Kind::Leaf | Kind::Const => return Some((n, off, len)),
                _ if self.home.contains_key(&n) => return Some((n, off, len)),
                Kind::Op(Op::Shr) => {
                    off = off.checked_add(x.b as u8)?;
                    n = x.a;
                    continue;
                }
                Kind::Op(Op::Concat) if (g.w(x.a) as u32) <= x.c => {
                    if off as u32 + len as u32 <= x.c {
                        n = x.a;
                        continue;
                    }
                    if off as u32 >= x.c {
                        off -= x.c as u8;
                        n = x.b;
                        continue;
                    }
                }
                Kind::Op(Op::Shl) => {
                    if let Some((lo, flen)) = shl_field(x.b, x.imm) {
                        let (o, l) = (off as u32, len as u32);
                        if o + l <= lo || o >= lo + flen {
                            return Some((self.zero, 0, 1));
                        }
                        if o >= lo && o + l <= lo + flen {
                            off = (o - x.b + x.c).try_into().ok()?;
                            n = x.a;
                            continue;
                        }
                    }
                }
                _ => {}
            }
            let (t, toff) = g.piece(n, off, len)?;
            n = t;
            off = toff;
        }
        None
    }
}

/// Replace `em`'s program by the attempt's; returns the new views.
fn apply(
    em: &mut Emitter,
    ports: &mut PortMap,
    debug: &mut DebugMap,
    att: Attempt,
) -> Vec<(u32, Seg)> {
    let remap = att.remap;
    em.widths.extend_from_slice(&att.widths);
    em.ops = att.ops;
    em.instrs = att.instrs;
    em.imms = att.imms;
    em.consts.extend_from_slice(&att.consts);
    em.commits = att.commits;
    // Constants nothing reads any more stay (they cost nothing per tick).
    for map in &mut debug.net_slots {
        map.retain(|_, s| {
            let r = remap.get(*s as usize).copied().unwrap_or(NONE);
            *s = r;
            r != NONE
        });
    }
    for key in &mut em.state_keys {
        if let StateLoc::Slot(s) = &mut key.loc {
            let r = att.key_remap.get(s).copied().unwrap_or(remap[*s as usize]);
            debug_assert_ne!(r, NONE, "a state key resolves");
            *s = r;
        }
    }
    for (p, &s) in ports.outputs.iter_mut().zip(&att.port_slots) {
        p.slot = s;
    }
    att.views
}

#[cfg(test)]
mod tests;
