//! Clock-phase specialization (issue #117, stage 4).
//!
//! A [`Op::Clock`] reads `((tick + phase) % period) < duty`: a function of
//! the tick count alone. With `L` the least common multiple of every
//! clock's period, the program a settle runs depends only on `tick % L`.
//! [`specialize`] builds one [`Body`] per residue with every clock folded
//! to its constant there, and simplifies what that exposes:
//!
//! - a `Mux` on a constant select becomes its taken arm, so a latch whose
//!   enable is constant 0 computes nothing (its next value is its state),
//!   and one whose enable is constant 1 reads its data directly;
//! - a commit whose next value is the state itself is dropped (it would
//!   copy an unchanged value), and one whose next value is another slot
//!   reads that slot directly;
//! - a RAM write, masked write or clear whose enable is constant 0 is
//!   dropped (it would not be taken);
//! - constant folding (`simplify::fold`) and the usual identities
//!   (`x & 0`, `x | 0`, `x ^ 0`, a `Mux` with equal arms, ...);
//! - dead-code elimination down to what the variant must produce.
//!
//! # What a variant must produce
//!
//! Variants share every slot with the unspecialized program, so the debug
//! map, views, shadows, ports, state keys and snapshots are unchanged.
//! After each settle, every slot the program exposes holds the value the
//! unspecialized program would put there:
//!
//! - every slot in the debug map (through views to their words), so
//!   `read_net`, probes and `DebugMap` lookups are unchanged under any
//!   options;
//! - root input and output ports, state slots, constants;
//! - every input and state slot of a [`super::Shadow`];
//! - whatever the variant's own commits read.
//!
//! An exposed slot whose value became a copy of another slot, or a
//! constant, is still written (one `Copy`, or the original instruction).
//! Slots nothing exposes (the next-state temporaries of registers) are
//! written only where a commit of the same variant reads them; in other
//! variants they keep stale values nobody reads.
//!
//! # Commits
//!
//! The commit at the start of tick `t + 1` applies the next values that
//! the settle of tick `t` computed, so it belongs to the variant of `t`.
//! A commit reads only slots no commit of its variant writes (the
//! invariant `Sim::commit` relies on): a next value that became another
//! state slot written in the same variant is copied into its own next
//! slot during the settle instead, as before.
//!
//! Every emitted instruction keeps the rank of the instruction that wrote
//! its slot before, and reads only slots of lower rank, so the variant's
//! blocks are the original `(rank, op)` order.

use super::{Block, Body, Commit, Instr, Op, Phases, Program, StateLoc, ZERO_SLOT};
use super::{FastMap, simplify};

/// Most variants a program may have (the least common multiple of its
/// clock periods).
pub const MAX_PHASES: u64 = 8;

/// A slot's value in one variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Val {
    /// The slot holds its own value: not an instruction result, or an
    /// instruction that stays (its rewritten form is in `own`).
    Own,
    /// The same value as another slot (which is `Own`).
    Alias(u32),
    /// A constant (held by the slot itself once written).
    Const(u64),
}

/// An instruction operand in a variant: a slot, or a constant whose
/// origin slot computes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Opnd {
    Slot(u32),
    Const(u64, u32),
}

/// The clock-phase variants of `p`, or `None` when it has no clock, the
/// clocks' common period exceeds [`MAX_PHASES`], the layout is not plain
/// (packed words or thread partitions), or specializing saves nothing.
pub fn specialize(p: &Program) -> Option<Phases> {
    if !p.packs.is_empty() || !p.block_parts.is_empty() {
        return None;
    }
    let mut period = 1u64;
    let mut any = false;
    for b in p.blocks.iter().filter(|b| b.op == Op::Clock) {
        for i in &p.instrs[b.start as usize..(b.start + b.len) as usize] {
            any = true;
            let a = i.a as u64;
            period = period / gcd(period, a) * a;
            if period > MAX_PHASES {
                return None;
            }
        }
    }
    if !any {
        return None;
    }
    let exposed = exposed(p);
    let spec = Spec::new(p);
    let variants: Vec<Body> = (0..period).map(|k| spec.variant(p, &exposed, k)).collect();
    let base = p.instrs.len() + p.commits.len();
    let total: usize = variants.iter().map(Body::ops).sum();
    if total >= base * period as usize {
        return None;
    }
    Some(Phases {
        period: period as u32,
        variants,
    })
}

fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

/// Slots every variant must keep correct (see the module docs).
fn exposed(p: &Program) -> Vec<bool> {
    let n = p.slot_count as usize;
    let mut out = vec![false; n];
    let mut mark = |s: u32| {
        if let Some((real, _, _)) = p.resolve(s)
            && (real as usize) < n
        {
            out[real as usize] = true;
        }
    };
    mark(ZERO_SLOT);
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
    for port in p.ports.inputs.iter().chain(&p.ports.outputs) {
        mark(port.slot);
    }
    for key in &p.state_keys {
        if let StateLoc::Slot(s) = key.loc {
            mark(s);
        }
    }
    for &(s, _) in &p.consts {
        mark(s);
    }
    out
}

/// Per-program facts shared by every variant.
struct Spec {
    /// Per slot: the instruction writing it (`u32::MAX` for none).
    writer: Vec<u32>,
    /// Per instruction: its op and rank.
    op_rank: Vec<(Op, u32)>,
    /// Constant slot holding each constant value (0 is the zero slot).
    home: FastMap<u64, u32>,
}

const NONE: u32 = u32::MAX;

impl Spec {
    fn new(p: &Program) -> Self {
        let mut writer = vec![NONE; p.slot_count as usize];
        let mut op_rank = vec![(Op::Copy, 0); p.instrs.len()];
        for b in &p.blocks {
            for k in b.start..b.start + b.len {
                writer[p.instrs[k as usize].dst as usize] = k;
                op_rank[k as usize] = (b.op, b.rank);
            }
        }
        let mut home = FastMap::default();
        home.insert(0, ZERO_SLOT);
        for &(s, v) in &p.consts {
            home.entry(v).or_insert(s);
        }
        Spec {
            writer,
            op_rank,
            home,
        }
    }

    /// The variant for ticks `t` with `t % period == k`.
    fn variant(&self, p: &Program, exposed: &[bool], k: u64) -> Body {
        let n = p.slot_count as usize;
        let mut val = vec![Val::Own; n];
        // Rewritten operands of every instruction that stays `Own`.
        let mut own: Vec<[Opnd; 3]> = vec![[Opnd::Slot(ZERO_SLOT); 3]; p.instrs.len()];
        for &(s, v) in &p.consts {
            val[s as usize] = Val::Const(v);
        }
        let opnd = |val: &[Val], s: u32| match val[s as usize] {
            Val::Own => Opnd::Slot(s),
            Val::Alias(t) => Opnd::Slot(t),
            Val::Const(c) => Opnd::Const(c, s),
        };
        let of = |o: Opnd, s: u32| match o {
            Opnd::Slot(t) if t == s => Val::Own,
            Opnd::Slot(t) => Val::Alias(t),
            Opnd::Const(c, _) => Val::Const(c),
        };
        for b in &p.blocks {
            let (sa, sb, sc) = b.op.slot_operands();
            for idx in b.start..b.start + b.len {
                let i = p.instrs[idx as usize];
                let imm = p.imms[idx as usize];
                let d = i.dst;
                let pick = |is: bool, s: u32| {
                    if is {
                        opnd(&val, s)
                    } else {
                        Opnd::Slot(ZERO_SLOT)
                    }
                };
                let ops = [pick(sa, i.a), pick(sb, i.b), pick(sc, i.c)];
                let cst = |o: Opnd| match o {
                    Opnd::Const(c, _) => Some(c),
                    Opnd::Slot(_) => None,
                };
                let used = [sa, sb, sc];
                let all_const = ops.iter().zip(used).all(|(o, u)| !u || cst(*o).is_some());
                let v = if b.op == Op::Clock {
                    let t = (k + i.c as u64) % i.a as u64;
                    Val::Const((t < i.b as u64) as u64)
                } else if all_const
                    && let Some(c) = simplify::fold(
                        b.op,
                        &i,
                        imm,
                        &|s| {
                            if s == i.a && sa {
                                cst(ops[0])
                            } else if s == i.b && sb {
                                cst(ops[1])
                            } else if s == i.c && sc {
                                cst(ops[2])
                            } else {
                                None
                            }
                        },
                        &p.mems,
                    )
                {
                    Val::Const(c)
                } else {
                    let [a, bb, c] = ops;
                    let zero = |o: Opnd| cst(o) == Some(0);
                    match b.op {
                        Op::Copy => of(a, d),
                        Op::Mux if cst(a).is_some() => {
                            of(if cst(a) != Some(0) { c } else { bb }, d)
                        }
                        Op::Mux if same(bb, c) => of(bb, d),
                        Op::Switch if cst(a) == Some(0) => Val::Const(0),
                        Op::Switch if cst(a).is_some() => of(bb, d),
                        Op::And | Op::Switch if zero(a) || zero(bb) => Val::Const(0),
                        Op::Or | Op::Xor if zero(a) => of(bb, d),
                        Op::Or | Op::Xor if zero(bb) => of(a, d),
                        Op::Concat if zero(bb) => of(a, d),
                        _ => Val::Own,
                    }
                };
                if v == Val::Own {
                    own[idx as usize] = ops;
                }
                val[d as usize] = v;
            }
        }

        // Commits.
        let mut commits: Vec<Commit> = Vec::with_capacity(p.commits.len());
        let mut reads: Vec<[Opnd; 3]> = Vec::with_capacity(p.commits.len());
        for c in &p.commits {
            let r = c.reads().map(|s| opnd(&val, s));
            let off = |o: Opnd| matches!(o, Opnd::Const(0, _));
            let keep = match *c {
                Commit::Copy { state, .. } => r[0] != Opnd::Slot(state),
                Commit::RamWrite { .. } | Commit::MemClear { .. } => !off(r[0]),
                Commit::RamWriteMasked { .. } => !off(r[0]),
            };
            if keep {
                commits.push(*c);
                reads.push(r);
            }
        }
        let mut written = vec![false; n];
        for c in &commits {
            if let Commit::Copy { state, .. } = *c {
                written[state as usize] = true;
            }
        }

        // What must be written, and what each commit reads.
        let mut need = vec![false; n];
        let mut work: Vec<u32> = Vec::new();
        for (s, &e) in exposed.iter().enumerate() {
            if e {
                work.push(s as u32);
            }
        }
        for (c, r) in commits.iter_mut().zip(&reads) {
            let orig = c.reads();
            let mut k = 0;
            *c = c.map_reads(|_| {
                let (o, s) = (r[k], orig[k]);
                k += 1;
                match o {
                    Opnd::Slot(t) if !written[t as usize] => {
                        work.push(t);
                        t
                    }
                    Opnd::Const(v, _) if self.home.contains_key(&v) => self.home[&v],
                    _ => {
                        // Keep the original slot, written by this settle.
                        work.push(s);
                        s
                    }
                }
            });
        }

        // Emit: every needed slot that an instruction writes, as its value
        // in this variant says.
        let mut out: Vec<(u32, Op, u32, Instr, u64)> = Vec::new();
        while let Some(s) = work.pop() {
            if need[s as usize] {
                continue;
            }
            need[s as usize] = true;
            let w = self.writer[s as usize];
            if w == NONE {
                continue; // a port, constant or state slot
            }
            let (op, rank) = self.op_rank[w as usize];
            let orig = p.instrs[w as usize];
            let imm = p.imms[w as usize];
            let slot_of = |o: Opnd, work: &mut Vec<u32>| match o {
                Opnd::Slot(t) => {
                    work.push(t);
                    t
                }
                Opnd::Const(v, origin) => match self.home.get(&v) {
                    Some(&h) => h,
                    None => {
                        work.push(origin);
                        origin
                    }
                },
            };
            let (op, ins) = match val[s as usize] {
                Val::Alias(t) => (
                    Op::Copy,
                    Instr {
                        dst: s,
                        a: slot_of(Opnd::Slot(t), &mut work),
                        b: 0,
                        c: 0,
                    },
                ),
                Val::Const(v) if self.home.contains_key(&v) => (
                    Op::Copy,
                    Instr {
                        dst: s,
                        a: self.home[&v],
                        b: 0,
                        c: 0,
                    },
                ),
                Val::Const(_) if op == Op::Clock => (op, orig),
                Val::Own | Val::Const(_) => {
                    // The original instruction, over this variant's
                    // operands (for a constant, they evaluate to it).
                    let (sa, sb, sc) = op.slot_operands();
                    let ops = if val[s as usize] == Val::Own {
                        own[w as usize]
                    } else {
                        // Only slot fields: a literal field (a width or a
                        // shift of `Sext`, `Shr`, `Concat`) is kept as is.
                        let pick = |is: bool, s: u32| {
                            if is {
                                opnd(&val, s)
                            } else {
                                Opnd::Slot(ZERO_SLOT)
                            }
                        };
                        [pick(sa, orig.a), pick(sb, orig.b), pick(sc, orig.c)]
                    };
                    let mut ins = orig;
                    if sa {
                        ins.a = slot_of(ops[0], &mut work);
                    }
                    if sb {
                        ins.b = slot_of(ops[1], &mut work);
                    }
                    if sc {
                        ins.c = slot_of(ops[2], &mut work);
                    }
                    (op, ins)
                }
            };
            // `Copy` ignores its immediate.
            let imm = if op == Op::Copy { 0 } else { imm };
            out.push((rank, op, w, ins, imm));
        }
        out.sort_unstable_by_key(|&(rank, op, w, _, _)| (rank, op, w));
        let mut body = Body {
            commits,
            ..Body::default()
        };
        for (rank, op, _, ins, imm) in out {
            match body.blocks.last_mut() {
                Some(b) if b.rank == rank && b.op == op => b.len += 1,
                _ => body.blocks.push(Block {
                    op,
                    rank,
                    start: body.instrs.len() as u32,
                    len: 1,
                    packed: false,
                }),
            }
            body.instrs.push(ins);
            body.imms.push(imm);
        }
        body
    }
}

fn same(a: Opnd, b: Opnd) -> bool {
    match (a, b) {
        (Opnd::Slot(x), Opnd::Slot(y)) => x == y,
        (Opnd::Const(x, _), Opnd::Const(y, _)) => x == y,
        _ => false,
    }
}

/// The settle of `body` restricted to what its commits read (issue #117,
/// tick fusion): within `Sim::run`, every tick but the last is observed by
/// nothing but the commit that follows it, so its settle only needs the
/// commits' cone. `None` when that saves nothing, or for a packed body.
pub fn prune(p: &Program, body: &Body) -> Option<Body> {
    if body.blocks.iter().any(|b| b.packed) {
        return None;
    }
    let n = p.slot_count as usize;
    let mut writer = vec![NONE; n];
    for (k, i) in body.instrs.iter().enumerate() {
        writer[i.dst as usize] = k as u32;
    }
    let mut op_of = vec![Op::Copy; body.instrs.len()];
    for b in &body.blocks {
        for k in b.start..b.start + b.len {
            op_of[k as usize] = b.op;
        }
    }
    let mut live = vec![false; body.instrs.len()];
    let mut work: Vec<u32> = body.commits.iter().flat_map(|c| c.reads()).collect();
    while let Some(s) = work.pop() {
        let w = writer[s as usize];
        if w == NONE || live[w as usize] {
            continue;
        }
        live[w as usize] = true;
        let i = body.instrs[w as usize];
        let (sa, sb, sc) = op_of[w as usize].slot_operands();
        for (is, x) in [(sa, i.a), (sb, i.b), (sc, i.c)] {
            if is {
                work.push(x);
            }
        }
    }
    if live.iter().all(|&l| l) {
        return None;
    }
    let mut out = Body {
        commits: body.commits.clone(),
        ..Body::default()
    };
    for b in &body.blocks {
        let start = out.instrs.len() as u32;
        for k in b.start..b.start + b.len {
            if live[k as usize] {
                out.instrs.push(body.instrs[k as usize]);
                out.imms.push(body.imms[k as usize]);
            }
        }
        let len = out.instrs.len() as u32 - start;
        if len > 0 {
            out.blocks.push(Block { start, len, ..*b });
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests;
