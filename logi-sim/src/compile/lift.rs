//! Bus-aligned word lifting (issue #117, stage 1).
//!
//! Runs after simplify, before levelizing, on the lowered instruction list.
//! It turns bit-sliced logic back into word operations:
//!
//! - **Bit labels.** Every slot produced by a bit slice (`Shr`) or a
//!   concatenation (`Concat`) is described as a list of segments of *base*
//!   words: `(base, offset, length)`. Slices of concatenations of slices
//!   compose, so a bus split into bits, passed through instance ports and
//!   merged again is seen as bits of the original word.
//! - **Groups.** 1-bit bitwise instructions (`Not`, `And`, `Or`, `Xor`,
//!   `Nand`, `Nor`, `Xnor`, `Mux`, `Switch`) with the same op form a group
//!   when, for every operand, lane `i` reads bit `i + delta` of one word
//!   (`Word`), the same 1-bit slot in every lane (`Bcast`, such as a mux
//!   select or a register enable), or a constant. The lane is the bit
//!   position, so no gathers are ever needed. A group's result is a new
//!   word whose bit `i` is lane `i`; its lanes are labelled as bits of it,
//!   so groups chain and concatenations of lanes resolve to the word.
//!   Ranks are processed in order; an operand that is a bit of a word can
//!   be read either way, and each instruction takes the reading shared by
//!   the most instructions of its rank.
//! - **State banks.** 1-bit state elements (`Commit::Copy`, e.g. `Delay`,
//!   `Register`, `DLatch`) whose outputs are concatenated into a bus, or
//!   that a group reads lane-wise, become one multi-bit state word. Labels
//!   are then recomputed with the bank's bits known, until no new bank
//!   appears (at most [`MAX_ROUNDS`] rounds). A bank's next value is built
//!   from runs of lanes that are consecutive bits of one word (one slice
//!   each), so it is exact whatever the lifting decisions.
//! - **Cost.** A group is lifted only when one word op, its operand
//!   adapters and the bit extracts that unlifted readers still need cost no
//!   more than its scalar instructions (iterated to a fixed point).
//! - **Rewrite.** Lifted instructions, and slices and concatenations whose
//!   label is valid, disappear. A slot that something still reads as a
//!   plain slot is rebuilt from its label (one `Shr`, an alias, or a short
//!   `Concat` chain). A net nothing reads becomes a **view** in the debug
//!   map: bits `shift..shift + width` of a real slot (see
//!   [`Program::views`]); state keys of banked elements are views too, so
//!   probes, `Sim::read_net` and state export/import keep working.
//!
//! Each round is linear in the instruction count (hash maps keyed by
//! operand descriptors); there are at most [`MAX_ROUNDS`] rounds.

use smallvec::{SmallVec, smallvec};

use super::Options;
use super::emit::{Emitter, levelize};
use super::program::*;
use crate::netlist::mask;

const NONE: u32 = u32::MAX;

/// The pass's maps: `foldhash` (the default SipHash dominated the pass's
/// time).
type Map<K, V> = super::FastMap<K, V>;
/// Label rounds (each may add state banks).
const MAX_ROUNDS: usize = 6;
/// Times the rounds are rerun after rejecting unprofitable state banks.
const MAX_ATTEMPTS: usize = 3;
/// Passes of the group cost model.
const MAX_COST_PASSES: usize = 4;

/// Bits `off..off + len` of slot `base`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) struct Seg {
    pub base: u32,
    pub off: u8,
    pub len: u8,
}

type Segs = SmallVec<[Seg; 2]>;

/// How lane `i` of a group reads one operand.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum Opd {
    /// Not a slot operand.
    None,
    /// Bit `i + delta` of `base`.
    Word(u32, u8),
    /// The same 1-bit slot.
    Bcast(u32),
    /// A constant bit per lane.
    Const,
    /// A different 1-bit state slot per lane (a state bank candidate;
    /// never lifted).
    Var,
}

type Key = (Op, [Opd; 3]);

/// One way to read an operand: its descriptor, its payload (a constant
/// bit or a `Var` slot), the bit it reads (for `Word`) and the slot's
/// fan-out (for `Word`).
type OperandOption = (Opd, u32, u8, u32);

struct Group {
    op: Op,
    opds: [Opd; 3],
    width: u8,
    word: u32,
    occupied: u64,
    /// `(lane, instruction)`.
    members: Vec<(u8, u32)>,
    /// Per operand: the constant bits.
    consts: [u64; 3],
    /// Per operand: `(lane, state slot)` of `Var` operands.
    vars: [Vec<(u8, u32)>; 3],
}

impl Group {
    fn has_var(&self) -> bool {
        self.opds.contains(&Opd::Var)
    }
}

/// A multi-bit state word replacing 1-bit state elements.
struct Bank {
    slot: u32,
    width: u8,
    /// `(bit, old state slot)`.
    lanes: Vec<(u8, u32)>,
}

/// The labels and groups of one round.
struct Round {
    labels: Vec<Segs>,
    /// Per slot: the `Concat` instructions building it (0 for anything
    /// else).
    chain: Vec<u16>,
    groups: Vec<Group>,
    /// Per instruction: `(group, lane)` or `(NONE, 0)`.
    member: Vec<(u32, u8)>,
}

/// One-bit bitwise ops that can be lifted.
fn liftable_op(op: Op) -> bool {
    matches!(
        op,
        Op::Not | Op::And | Op::Or | Op::Xor | Op::Nand | Op::Nor | Op::Xnor | Op::Mux | Op::Switch
    )
}

fn commutative(op: Op) -> bool {
    matches!(
        op,
        Op::And | Op::Or | Op::Xor | Op::Nand | Op::Nor | Op::Xnor
    )
}

/// Append `s` to `segs`, merging it with the last segment when contiguous.
fn push_seg(segs: &mut Segs, s: Seg) {
    if let Some(last) = segs.last_mut()
        && last.base == s.base
        && last.off as u32 + last.len as u32 == s.off as u32
    {
        last.len += s.len;
        return;
    }
    segs.push(s);
}

/// Bits `lo..lo + len` of a value described by `segs`.
fn sub_segs(segs: &[Seg], lo: u32, len: u32) -> Segs {
    let mut out = Segs::new();
    let (mut pos, end) = (0u32, lo + len);
    for s in segs {
        let (a, b) = (pos, pos + s.len as u32);
        let (x, y) = (a.max(lo), b.min(end));
        if x < y {
            push_seg(
                &mut out,
                Seg {
                    base: s.base,
                    off: (s.off as u32 + x - a) as u8,
                    len: (y - x) as u8,
                },
            );
        }
        pos = b;
    }
    out
}

struct Ctx<'a> {
    ops: &'a [Op],
    instrs: &'a [Instr],
    imms: &'a [u64],
    /// Instructions sorted by rank, and the rank boundaries.
    sorted: Vec<u32>,
    spans: Vec<(usize, usize)>,
    cval: Vec<Option<u64>>,
    /// Per slot: the next-value slot of a 1-bit `Commit::Copy` state.
    state_next: Vec<u32>,
    /// Per slot: liftable 1-bit instructions reading it.
    fanout: Vec<u32>,
}

impl Ctx<'_> {
    fn is_state1(&self, s: u32) -> bool {
        self.state_next.get(s as usize).is_some_and(|&n| n != NONE)
    }
}

pub(super) fn lift(
    em: &mut Emitter,
    ports: &mut PortMap,
    debug: &mut DebugMap,
    probe_slots: &[u32],
    opts: &Options,
) -> Vec<(u32, Seg)> {
    let n_slots = em.widths.len();
    let (ranks, order) = levelize(&em.ops, &em.instrs, n_slots);
    let mut sorted = order;
    sorted.sort_by_key(|&k| ranks[k as usize]);
    let mut spans = Vec::new();
    let mut lo = 0;
    for k in 1..=sorted.len() {
        if k == sorted.len() || ranks[sorted[k] as usize] != ranks[sorted[lo] as usize] {
            spans.push((lo, k));
            lo = k;
        }
    }
    let mut cval = vec![None; n_slots];
    cval[ZERO_SLOT as usize] = Some(0);
    for &(s, v) in &em.consts {
        cval[s as usize] = Some(v);
    }
    let mut state_next = vec![NONE; n_slots];
    for c in &em.commits {
        if let Commit::Copy { state, next } = *c
            && em.widths[state as usize] == 1
        {
            state_next[state as usize] = next;
        }
    }
    let mut fanout = vec![0u32; n_slots];
    for (k, ins) in em.instrs.iter().enumerate() {
        let op = em.ops[k];
        if liftable_op(op) && em.widths[ins.dst as usize] == 1 {
            let (sa, sb, sc) = op.slot_operands();
            for (is, s) in [(sa, ins.a), (sb, ins.b), (sc, ins.c)] {
                if is {
                    fanout[s as usize] += 1;
                }
            }
        }
    }
    let ops = std::mem::take(&mut em.ops);
    let instrs = std::mem::take(&mut em.instrs);
    let imms = std::mem::take(&mut em.imms);
    let ctx = Ctx {
        ops: &ops,
        instrs: &instrs,
        imms: &imms,
        sorted,
        spans,
        cval,
        state_next,
        fanout,
    };

    // ── Rounds: labels and groups, growing the state banks ────────────────
    // Banks that cost more than their 1-bit commits are rejected and the
    // rounds rerun without them (at most `MAX_ATTEMPTS` times).
    let mut rejected = vec![false; n_slots];
    let mut attempt = 0;
    let (
        banks,
        bank_of,
        labels,
        chain,
        groups,
        member,
        group_of_word,
        lifted,
        bank_lane_commit,
        real,
    ) = loop {
        attempt += 1;
        let mut banks: Vec<Bank> = Vec::new();
        let mut bank_of: Map<u32, (u32, u8)> = Map::default();
        let mut rounds = 0;
        let round = loop {
            rounds += 1;
            let r = run_round(&ctx, &mut em.widths, &banks, &rejected);
            let mut new = false;
            if rounds < MAX_ROUNDS {
                for (width, lanes) in proposals(&ctx, &r, &em.widths, &bank_of, &rejected) {
                    let slot = em.widths.len() as u32;
                    em.widths.push(width);
                    let b = banks.len() as u32;
                    for &(lane, s) in &lanes {
                        bank_of.insert(s, (b, lane));
                    }
                    banks.push(Bank { slot, width, lanes });
                    new = true;
                }
            }
            if !new {
                break r;
            }
        };
        let Round {
            labels,
            chain,
            groups,
            member,
        } = round;

        // ── Which groups to lift ───────────────────────────────────────────────
        let mut group_of_word: Map<u32, usize> = Map::default();
        for (g, gr) in groups.iter().enumerate() {
            group_of_word.insert(gr.word, g);
        }
        let mut bad = vec![false; groups.len()];
        let mut lifted = vec![false; groups.len()];
        let bank_lane_commit: Vec<bool> = {
            let mut v = vec![false; em.widths.len()];
            for bank in &banks {
                for &(_, s) in &bank.lanes {
                    v[s as usize] = true;
                }
            }
            v
        };
        // Each pass of the cost model unlifts a layer of groups; a few
        // passes settle it (whatever is left lifted is still exact).
        let mut passes = 0;
        let real = loop {
            passes += 1;
            for g in 0..groups.len() {
                let gr = &groups[g];
                lifted[g] = !bad[g]
                    && gr.members.len() >= 2
                    && !gr.has_var()
                    && gr.opds.iter().all(|o| match *o {
                        Opd::Word(b, _) => group_of_word.get(&b).is_none_or(|&h| lifted[h]),
                        _ => true,
                    });
            }
            let valid = |b: u32| group_of_word.get(&b).is_none_or(|&h| lifted[h]);
            let real = real_reads(
                &ctx,
                em,
                ports,
                debug,
                probe_slots,
                opts,
                &labels,
                &chain,
                &groups,
                &member,
                &lifted,
                &banks,
                &bank_lane_commit,
                &valid,
            );
            let mut changed = false;
            for (g, gr) in groups.iter().enumerate() {
                if !lifted[g] {
                    continue;
                }
                let lanes = gr.members.len();
                let mut cost = if matches!(gr.op, Op::Mux) && !matches!(gr.opds[0], Opd::Bcast(_)) {
                    4
                } else {
                    1
                };
                for (p, o) in gr.opds.iter().enumerate() {
                    match *o {
                        Opd::Word(b, d) if d != 0 || em.widths[b as usize] != gr.width => cost += 1,
                        Opd::Bcast(_) if !(p == 0 && matches!(gr.op, Op::Mux | Op::Switch)) => {
                            cost += 1
                        }
                        _ => {}
                    }
                }
                cost += gr
                    .members
                    .iter()
                    .filter(|&&(_, k)| real[instrs[k as usize].dst as usize])
                    .count();
                if cost > lanes {
                    bad[g] = true;
                    changed = true;
                }
            }
            if !changed || passes >= MAX_COST_PASSES {
                break real;
            }
        };
        let mut bad_bank = false;
        if attempt < MAX_ATTEMPTS {
            for bank in &banks {
                // Structure only: how the next value would be built if
                // every group it reads were lifted.
                let runs = bank_runs(&ctx, &labels, bank, &|_| true);
                let slices = runs
                    .iter()
                    .filter(|r| !(r.2 == 0 && r.3 == em.widths[r.1 as usize]))
                    .count();
                let extracts = bank
                    .lanes
                    .iter()
                    .filter(|&&(_, s)| real[s as usize])
                    .count();
                if 2 * runs.len() > bank.lanes.len()
                    || 1 + slices + runs.len() + extracts > 2 * bank.lanes.len()
                {
                    for &(_, s) in &bank.lanes {
                        rejected[s as usize] = true;
                    }
                    bad_bank = true;
                }
            }
        }
        if !bad_bank {
            break (
                banks,
                bank_of,
                labels,
                chain,
                groups,
                member,
                group_of_word,
                lifted,
                bank_lane_commit,
                real,
            );
        }
    };
    let valid = |b: u32| group_of_word.get(&b).is_none_or(|&h| lifted[h]);
    let absorbed: Vec<bool> = (0..instrs.len())
        .map(|k| {
            let (g, _) = member[k];
            (g != NONE && lifted[g as usize])
                || (matches!(ops[k], Op::Shr | Op::Concat | Op::Copy)
                    && label_ok(&labels, &chain, &em.widths, instrs[k].dst, &valid))
        })
        .collect();

    // ── Rewrite ────────────────────────────────────────────────────────────
    let mut out = Out {
        ops: Vec::with_capacity(ops.len()),
        instrs: Vec::with_capacity(instrs.len()),
        imms: Vec::with_capacity(imms.len()),
        dsts: Map::default(),
        adapters: Map::default(),
    };
    for k in 0..instrs.len() {
        if !absorbed[k] {
            out.push(ops[k], instrs[k], imms[k]);
        }
    }
    // Group word ops.
    let mut const_words: Map<(u64, u8), u32> = Map::default();
    for (g, gr) in groups.iter().enumerate() {
        if !lifted[g] {
            continue;
        }
        let w = gr.width;
        let mut operand =
            |p: usize, out: &mut Out, widths: &mut Vec<u8>, em_consts: &mut Vec<(u32, u64)>| {
                match gr.opds[p] {
                    Opd::None => ZERO_SLOT,
                    Opd::Word(b, d) => out.adapter(widths, b, d as i32, w, false),
                    Opd::Bcast(x) => {
                        if p == 0 && matches!(gr.op, Op::Mux | Op::Switch) {
                            x
                        } else {
                            out.sext(widths, x, w)
                        }
                    }
                    Opd::Const => {
                        let v = gr.consts[p] & mask(w);
                        *const_words.entry((v, w)).or_insert_with(|| {
                            let s = widths.len() as u32;
                            widths.push(w);
                            em_consts.push((s, v));
                            s
                        })
                    }
                    Opd::Var => unreachable!("groups with Var operands are never lifted"),
                }
            };
        let a = operand(0, &mut out, &mut em.widths, &mut em.consts);
        let b = operand(1, &mut out, &mut em.widths, &mut em.consts);
        let c = operand(2, &mut out, &mut em.widths, &mut em.consts);
        let z = ZERO_SLOT;
        let bitwise_sel = !matches!(gr.opds[0], Opd::Bcast(_));
        let ins = |dst, a, b, c| Instr { dst, a, b, c };
        match gr.op {
            Op::Mux if bitwise_sel => {
                // (a & c) | (!a & b)
                let t1 = new_slot(&mut em.widths, w);
                out.push(Op::And, ins(t1, a, c, z), 0);
                let t2 = new_slot(&mut em.widths, w);
                out.push(Op::Not, ins(t2, a, z, z), mask(w));
                let t3 = new_slot(&mut em.widths, w);
                out.push(Op::And, ins(t3, t2, b, z), 0);
                out.push(Op::Or, ins(gr.word, t1, t3, z), 0);
            }
            Op::Switch if bitwise_sel => out.push(Op::And, ins(gr.word, a, b, z), 0),
            op => {
                let imm = match op {
                    Op::Not | Op::Nand | Op::Nor | Op::Xnor => mask(w),
                    _ => 0,
                };
                out.push(op, ins(gr.word, a, b, c), imm);
            }
        }
    }
    // Bank commits.
    let mut commit_written = vec![false; em.widths.len()];
    let mut new_commits = Vec::with_capacity(em.commits.len());
    for c in &em.commits {
        match *c {
            Commit::Copy { state, .. } if bank_lane_commit[state as usize] => {}
            Commit::Copy { state, .. } => {
                commit_written[state as usize] = true;
                new_commits.push(*c);
            }
            other => new_commits.push(other),
        }
    }
    for bank in &banks {
        commit_written[bank.slot as usize] = true;
    }
    for bank in &banks {
        let runs = bank_runs(&ctx, &labels, bank, &valid);
        let mut acc: Option<(u32, u32)> = None; // (slot, width)
        for &(lane, base, off, len) in &runs {
            let piece = if off == 0 && len == em.widths[base as usize] {
                base
            } else {
                let t = new_slot(&mut em.widths, len);
                out.push(
                    Op::Shr,
                    Instr {
                        dst: t,
                        a: base,
                        b: off as u32,
                        c: ZERO_SLOT,
                    },
                    mask(len),
                );
                t
            };
            acc = Some(match acc {
                None if lane == 0 => (piece, len as u32),
                prev => {
                    let w = lane as u32 + len as u32;
                    let dst = new_slot(&mut em.widths, w as u8);
                    out.push(
                        Op::Concat,
                        Instr {
                            dst,
                            a: prev.map_or(ZERO_SLOT, |p| p.0),
                            b: piece,
                            c: lane as u32,
                        },
                        0,
                    );
                    (dst, w)
                }
            });
        }
        let (mut next, _) = acc.expect("a bank has lanes");
        // A commit reads only slots that instructions write.
        if !out_dsts_contains(&out, next, &group_of_word, &lifted) {
            let t = new_slot(&mut em.widths, bank.width);
            out.push(
                Op::Copy,
                Instr {
                    dst: t,
                    a: next,
                    b: ZERO_SLOT,
                    c: ZERO_SLOT,
                },
                0,
            );
            next = t;
        }
        new_commits.push(Commit::Copy {
            state: bank.slot,
            next,
        });
    }
    if commit_written.len() < em.widths.len() {
        commit_written.resize(em.widths.len(), false);
    }
    let mut commit_read = vec![false; em.widths.len()];
    for c in &new_commits {
        for s in c.reads() {
            commit_read[s as usize] = true;
        }
    }

    // Materialize labelled slots that are still read as plain slots.
    let mut repl: Vec<u32> = (0..em.widths.len() as u32).collect();
    let mut labelled_slots: Vec<u32> = Vec::new();
    for k in 0..instrs.len() {
        if absorbed[k] {
            labelled_slots.push(instrs[k].dst);
        }
    }
    for bank in &banks {
        labelled_slots.extend(bank.lanes.iter().map(|&(_, s)| s));
    }
    let mut materialized = vec![false; em.widths.len()];
    for &s in &labelled_slots {
        if !real[s as usize] {
            continue;
        }
        materialized[s as usize] = true;
        let segs = &labels[s as usize];
        if let [seg] = segs.as_slice() {
            let bw = em.widths[seg.base as usize];
            if seg.off == 0 && seg.len == bw {
                if commit_read[s as usize] && commit_written[seg.base as usize] {
                    out.push(
                        Op::Copy,
                        Instr {
                            dst: s,
                            a: seg.base,
                            b: ZERO_SLOT,
                            c: ZERO_SLOT,
                        },
                        0,
                    );
                } else {
                    repl[s as usize] = seg.base;
                }
            } else {
                out.push(
                    Op::Shr,
                    Instr {
                        dst: s,
                        a: seg.base,
                        b: seg.off as u32,
                        c: ZERO_SLOT,
                    },
                    mask(seg.len),
                );
            }
            continue;
        }
        let mut acc = NONE;
        let mut shift = 0u32;
        for (i, seg) in segs.iter().enumerate() {
            let piece = if seg.off == 0 && seg.len == em.widths[seg.base as usize] {
                seg.base
            } else {
                let t = new_slot(&mut em.widths, seg.len);
                out.push(
                    Op::Shr,
                    Instr {
                        dst: t,
                        a: seg.base,
                        b: seg.off as u32,
                        c: ZERO_SLOT,
                    },
                    mask(seg.len),
                );
                t
            };
            if i == 0 {
                acc = piece;
            } else {
                let dst = if i + 1 == segs.len() {
                    s
                } else {
                    new_slot(&mut em.widths, (shift + seg.len as u32) as u8)
                };
                out.push(
                    Op::Concat,
                    Instr {
                        dst,
                        a: acc,
                        b: piece,
                        c: shift,
                    },
                    0,
                );
                acc = dst;
            }
            shift += seg.len as u32;
        }
    }
    let resolve = |s: u32| repl.get(s as usize).copied().unwrap_or(s);

    // Views for everything labelled and not materialized.
    let mut views: Vec<(u32, Seg)> = Vec::new();
    let mut view_of: Map<Seg, u32> = Map::default();
    let mut view = |seg: Seg, widths: &mut Vec<u8>| -> u32 {
        *view_of.entry(seg).or_insert_with(|| {
            let f = widths.len() as u32;
            widths.push(seg.len);
            views.push((f, seg));
            f
        })
    };
    let mut is_labelled = vec![false; em.widths.len()];
    for &s in &labelled_slots {
        is_labelled[s as usize] = true;
    }
    for map in &mut debug.net_slots {
        let mut drop = Vec::new();
        for (net, s) in map.iter_mut() {
            let x = *s as usize;
            if x < is_labelled.len() && is_labelled[x] && !materialized[x] {
                match labels[x].as_slice() {
                    [seg] => *s = view(*seg, &mut em.widths),
                    _ => drop.push(net),
                }
            } else {
                *s = resolve(*s);
            }
        }
        for net in drop {
            map.remove(net);
        }
    }
    for key in &mut em.state_keys {
        if let StateLoc::Slot(s) = key.loc
            && let Some(&(b, lane)) = bank_of.get(&s)
            && bank_lane_commit[s as usize]
        {
            let bank = &banks[b as usize];
            key.loc = StateLoc::Slot(view(
                Seg {
                    base: bank.slot,
                    off: lane,
                    len: 1,
                },
                &mut em.widths,
            ));
        }
    }
    for (ins, &op) in out.instrs.iter_mut().zip(&out.ops) {
        let (sa, sb, sc) = op.slot_operands();
        if sa {
            ins.a = resolve(ins.a);
        }
        if sb {
            ins.b = resolve(ins.b);
        }
        if sc {
            ins.c = resolve(ins.c);
        }
    }
    for c in &mut new_commits {
        *c = c.map_reads(resolve);
    }
    for p in &mut ports.outputs {
        p.slot = resolve(p.slot);
    }
    em.commits = new_commits;
    em.ops = out.ops;
    em.instrs = out.instrs;
    em.imms = out.imms;

    if opts.dce && !opts.debug {
        dce(em, ports, debug, probe_slots, &resolve, &mut views);
    }
    views
}

fn new_slot(widths: &mut Vec<u8>, w: u8) -> u32 {
    widths.push(w);
    (widths.len() - 1) as u32
}

struct Out {
    ops: Vec<Op>,
    instrs: Vec<Instr>,
    imms: Vec<u64>,
    /// Destinations written so far.
    dsts: Map<u32, ()>,
    /// `(base, delta, width, need an instruction) -> slot`, and
    /// `(slot, u32::MAX, width)` for broadcasts.
    adapters: Map<(u32, i32, u8, bool), u32>,
}

impl Out {
    fn push(&mut self, op: Op, ins: Instr, imm: u64) {
        self.dsts.insert(ins.dst, ());
        self.ops.push(op);
        self.instrs.push(ins);
        self.imms.push(imm);
    }

    /// A slot holding `(base >> delta) & mask(w)` (`base << -delta` for a
    /// negative `delta`). `base` itself when that is the same value, unless
    /// `need` asks for an instruction's destination.
    fn adapter(&mut self, widths: &mut Vec<u8>, base: u32, delta: i32, w: u8, need: bool) -> u32 {
        if delta == 0 && widths[base as usize] == w && !need {
            return base;
        }
        if let Some(&s) = self.adapters.get(&(base, delta, w, true)) {
            return s;
        }
        let z = ZERO_SLOT;
        let s = new_slot(widths, w);
        if delta >= 0 {
            if delta == 0 && widths[base as usize] == w {
                self.push(
                    Op::Copy,
                    Instr {
                        dst: s,
                        a: base,
                        b: z,
                        c: z,
                    },
                    0,
                );
            } else {
                self.push(
                    Op::Shr,
                    Instr {
                        dst: s,
                        a: base,
                        b: delta as u32,
                        c: z,
                    },
                    mask(w),
                );
            }
        } else {
            let up = (-delta) as u32;
            let tw = (widths[base as usize] as u32 + up).min(64) as u8;
            let t = new_slot(widths, tw);
            self.push(
                Op::Concat,
                Instr {
                    dst: t,
                    a: z,
                    b: base,
                    c: up,
                },
                0,
            );
            self.push(
                Op::Shr,
                Instr {
                    dst: s,
                    a: t,
                    b: 0,
                    c: z,
                },
                mask(w),
            );
        }
        self.adapters.insert((base, delta, w, true), s);
        s
    }

    /// `w` copies of 1-bit slot `x`.
    fn sext(&mut self, widths: &mut Vec<u8>, x: u32, w: u8) -> u32 {
        let key = (x, i32::MIN, w, true);
        if let Some(&s) = self.adapters.get(&key) {
            return s;
        }
        let s = new_slot(widths, w);
        self.push(
            Op::Sext,
            Instr {
                dst: s,
                a: x,
                b: 1,
                c: ZERO_SLOT,
            },
            mask(w),
        );
        self.adapters.insert(key, s);
        s
    }
}

fn seg_of(labels: &[Segs], widths: &[u8], s: u32) -> Segs {
    match labels.get(s as usize) {
        Some(l) if !l.is_empty() => l.clone(),
        _ => smallvec![Seg {
            base: s,
            off: 0,
            len: widths[s as usize],
        }],
    }
}

/// One labelling round with the state banks `banks`.
fn run_round(ctx: &Ctx<'_>, widths: &mut Vec<u8>, banks: &[Bank], rejected: &[bool]) -> Round {
    let mut labels: Vec<Segs> = vec![Segs::new(); widths.len()];
    let mut chain: Vec<u16> = vec![0; widths.len()];
    for bank in banks {
        for &(lane, s) in &bank.lanes {
            labels[s as usize] = smallvec![Seg {
                base: bank.slot,
                off: lane,
                len: 1,
            }];
        }
    }
    let mut groups: Vec<Group> = Vec::new();
    let mut group_of: Map<Key, u32> = Map::default();
    let mut member = vec![(NONE, 0u8); ctx.instrs.len()];
    let mut count: Map<Key, u32> = Map::default();
    // Per candidate instruction of a rank: its variants.
    struct Variant {
        key: Key,
        lane: u8,
        width: u8,
        words: u8,
        vars: u8,
        /// Every `Word` operand at the lane's own bit (delta 0).
        aligned: bool,
        /// Readers (among liftable 1-bit instructions) of the operands
        /// read as `Word`: fewer is likelier a bus bit than a shared one.
        fanout: u32,
        /// Per operand: the constant bit or the `Var` slot.
        payload: [u32; 3],
    }
    let mut cands: Vec<(u32, SmallVec<[Variant; 4]>)> = Vec::new();

    for &(lo, hi) in &ctx.spans {
        cands.clear();
        count.clear();
        for &k in &ctx.sorted[lo..hi] {
            let k = k as usize;
            let op = ctx.ops[k];
            let ins = ctx.instrs[k];
            let dw = widths[ins.dst as usize];
            match op {
                Op::Copy => {
                    labels[ins.dst as usize] = seg_of(&labels, widths, ins.a);
                }
                Op::Shr => {
                    let len = dw as u32;
                    if ctx.imms[k] == mask(dw) && ins.b + len <= widths[ins.a as usize] as u32 {
                        let segs = seg_of(&labels, widths, ins.a);
                        labels[ins.dst as usize] = sub_segs(&segs, ins.b, len);
                    }
                }
                Op::Concat => {
                    let (wa, wb) = (widths[ins.a as usize] as u32, widths[ins.b as usize] as u32);
                    chain[ins.dst as usize] =
                        (1 + chain[ins.a as usize] as u32 + chain[ins.b as usize] as u32)
                            .min(u16::MAX as u32) as u16;
                    if wa == ins.c && wa + wb == dw as u32 {
                        let mut segs = seg_of(&labels, widths, ins.a);
                        for s in seg_of(&labels, widths, ins.b) {
                            push_seg(&mut segs, s);
                        }
                        labels[ins.dst as usize] = segs;
                    }
                }
                _ if liftable_op(op) && dw == 1 => {
                    let (sa, sb, sc) = op.slot_operands();
                    let slots = [(sa, ins.a), (sb, ins.b), (sc, ins.c)];
                    if slots.iter().any(|&(is, s)| is && widths[s as usize] != 1) {
                        continue;
                    }
                    // Options per operand: (descriptor, payload).
                    let mut opts: [SmallVec<[OperandOption; 2]>; 3] = Default::default();
                    for (p, &(is, x)) in slots.iter().enumerate() {
                        if !is {
                            opts[p].push((Opd::None, 0, 0, 0));
                            continue;
                        }
                        if let Some(v) = ctx.cval.get(x as usize).copied().flatten() {
                            opts[p].push((Opd::Const, (v & 1) as u32, 0, 0));
                            continue;
                        }
                        let seg = seg_of(&labels, widths, x)[0];
                        if widths[seg.base as usize] > 1 {
                            opts[p].push((
                                Opd::Word(seg.base, 0),
                                0,
                                seg.off,
                                ctx.fanout[x as usize],
                            ));
                            opts[p].push((Opd::Bcast(x), 0, 0, 0));
                        } else {
                            let canon = seg.base;
                            opts[p].push((Opd::Bcast(canon), 0, 0, 0));
                            if ctx.is_state1(canon) && !rejected[canon as usize] {
                                opts[p].push((Opd::Var, canon, 0, 0));
                            }
                        }
                    }
                    let mut vars_out: SmallVec<[Variant; 4]> = SmallVec::new();
                    for &o0 in &opts[0] {
                        for &o1 in &opts[1] {
                            for &o2 in &opts[2] {
                                let sel = [o0, o1, o2];
                                let lane = sel
                                    .iter()
                                    .filter(|o| matches!(o.0, Opd::Word(..)))
                                    .map(|o| o.2)
                                    .min();
                                let Some(lane) = lane else { continue };
                                let mut width = 64u8;
                                let mut opds = [Opd::None; 3];
                                let mut payload = [0u32; 3];
                                let (mut words, mut vars) = (0u8, 0u8);
                                let (mut aligned, mut fanout) = (true, 0u32);
                                for (p, &(o, pl, j, fan)) in sel.iter().enumerate() {
                                    opds[p] = match o {
                                        Opd::Word(b, _) => {
                                            let d = j - lane;
                                            width = width.min(widths[b as usize] - d);
                                            words += 1;
                                            aligned &= d == 0;
                                            fanout += fan;
                                            Opd::Word(b, d)
                                        }
                                        Opd::Var => {
                                            vars += 1;
                                            o
                                        }
                                        o => o,
                                    };
                                    payload[p] = pl;
                                }
                                if commutative(op) && opds[1] < opds[0] {
                                    opds.swap(0, 1);
                                    payload.swap(0, 1);
                                }
                                vars_out.push(Variant {
                                    key: (op, opds),
                                    lane,
                                    width,
                                    words,
                                    vars,
                                    aligned,
                                    fanout,
                                    payload,
                                });
                            }
                        }
                    }
                    for v in &vars_out {
                        *count.entry(v.key).or_default() += 1;
                    }
                    cands.push((k as u32, vars_out));
                }
                _ => {}
            }
        }
        for (k, variants) in cands.drain(..) {
            // Most instructions of the rank or the group so far, then
            // (for an instruction alone in its rank) all operands at the
            // same bit, shared bits read as broadcasts, no state bank
            // candidates, the most words.
            type Score = (u32, bool, i64, i32, u8);
            let mut best: Option<(&Variant, Score)> = None;
            for v in &variants {
                let existing = group_of.get(&v.key).map(|&g| &groups[g as usize]);
                if existing.is_some_and(|g| g.occupied >> v.lane & 1 == 1) {
                    continue;
                }
                let score = count[&v.key] + existing.map_or(0, |g| g.members.len() as u32);
                let rank = (
                    score,
                    v.aligned,
                    -(v.fanout as i64),
                    -(v.vars as i32),
                    v.words,
                );
                if best.as_ref().is_none_or(|b| rank > b.1) {
                    best = Some((v, rank));
                }
            }
            let Some((v, _)) = best else { continue };
            let g = *group_of.entry(v.key).or_insert_with(|| {
                let word = widths.len() as u32;
                widths.push(v.width);
                labels.push(Segs::new());
                chain.push(0);
                groups.push(Group {
                    op: v.key.0,
                    opds: v.key.1,
                    width: v.width,
                    word,
                    occupied: 0,
                    members: Vec::new(),
                    consts: [0; 3],
                    vars: Default::default(),
                });
                (groups.len() - 1) as u32
            });
            let gr = &mut groups[g as usize];
            gr.occupied |= 1 << v.lane;
            gr.members.push((v.lane, k));
            for p in 0..3 {
                match gr.opds[p] {
                    Opd::Const => gr.consts[p] |= (v.payload[p] as u64 & 1) << v.lane,
                    Opd::Var => gr.vars[p].push((v.lane, v.payload[p])),
                    _ => {}
                }
            }
            member[k as usize] = (g, v.lane);
            labels[ctx.instrs[k as usize].dst as usize] = smallvec![Seg {
                base: gr.word,
                off: v.lane,
                len: 1,
            }];
        }
    }
    Round {
        labels,
        chain,
        groups,
        member,
    }
}

/// Whether slot `s` is rebuilt from its label: every base exists, and a
/// label of several segments costs no more instructions (a slice per
/// partial segment and the concatenations) than the `Concat`s it replaces.
fn label_ok(
    labels: &[Segs],
    chain: &[u16],
    widths: &[u8],
    s: u32,
    valid: &dyn Fn(u32) -> bool,
) -> bool {
    let Some(l) = labels.get(s as usize) else {
        return false;
    };
    if l.is_empty() || !l.iter().all(|seg| valid(seg.base)) {
        return false;
    }
    if l.len() == 1 {
        return true;
    }
    let slices = l
        .iter()
        .filter(|seg| !(seg.off == 0 && seg.len == widths[seg.base as usize]))
        .count();
    slices + l.len() - 1 <= chain.get(s as usize).copied().unwrap_or(0) as usize
}

/// New state banks suggested by a round: 1-bit state slots concatenated
/// into one bus, or read lane-wise by a group (`Var` operands).
fn proposals(
    ctx: &Ctx<'_>,
    r: &Round,
    widths: &[u8],
    bank_of: &Map<u32, (u32, u8)>,
    rejected: &[bool],
) -> Vec<(u8, Vec<(u8, u32)>)> {
    let mut claimed: Map<u32, ()> = Map::default();
    let free = |s: u32, claimed: &Map<u32, ()>| {
        ctx.is_state1(s)
            && !rejected[s as usize]
            && !bank_of.contains_key(&s)
            && !claimed.contains_key(&s)
    };
    let mut out = Vec::new();
    // Whole merges (a concatenation that does not continue into another
    // one), innermost first: a register's own output bus.
    let mut continued: Map<u32, ()> = Map::default();
    for (k, ins) in ctx.instrs.iter().enumerate() {
        if ctx.ops[k] == Op::Concat {
            continued.insert(ins.a, ());
        }
    }
    for &k in ctx.sorted.iter() {
        let k = k as usize;
        if ctx.ops[k] != Op::Concat || continued.contains_key(&ctx.instrs[k].dst) {
            continue;
        }
        let dst = ctx.instrs[k].dst;
        let segs = &r.labels[dst as usize];
        let mut lanes = Vec::new();
        let mut pos = 0u32;
        for s in segs {
            if s.len == 1
                && s.off == 0
                && free(s.base, &claimed)
                && !lanes.iter().any(|&(_, x)| x == s.base)
            {
                lanes.push((pos as u8, s.base));
            }
            pos += s.len as u32;
        }
        if lanes.len() >= 2 {
            for &(_, s) in &lanes {
                claimed.insert(s, ());
            }
            out.push((widths[dst as usize], lanes));
        }
    }
    for g in &r.groups {
        if g.members.len() < 2 {
            continue;
        }
        for vars in &g.vars {
            let mut lanes: Vec<(u8, u32)> = Vec::new();
            for &(lane, s) in vars {
                if free(s, &claimed) && !lanes.iter().any(|&(_, x)| x == s) {
                    lanes.push((lane, s));
                }
            }
            if lanes.len() >= 2 {
                for &(_, s) in &lanes {
                    claimed.insert(s, ());
                }
                out.push((g.width, lanes));
            }
        }
    }
    out
}

/// A bank's next value as runs `(lane, base, offset, length)`: lanes
/// `lane..lane + length` are bits `offset..` of `base`. A lane whose next
/// state has no valid one-bit label reads its own slot.
fn bank_runs(
    ctx: &Ctx<'_>,
    labels: &[Segs],
    bank: &Bank,
    valid: &dyn Fn(u32) -> bool,
) -> Vec<(u8, u32, u8, u8)> {
    let mut lanes = bank.lanes.clone();
    lanes.sort_unstable();
    let mut runs: Vec<(u8, u32, u8, u8)> = Vec::new();
    for (lane, s) in lanes {
        let n = ctx.state_next[s as usize];
        let (base, off) = match labels.get(n as usize).map(|l| l.as_slice()) {
            Some([seg]) if valid(seg.base) => (seg.base, seg.off),
            _ => (n, 0),
        };
        if let Some(last) = runs.last_mut()
            && last.1 == base
            && last.0 as u32 + last.3 as u32 == lane as u32
            && last.2 as u32 + last.3 as u32 == off as u32
        {
            last.3 += 1;
            continue;
        }
        runs.push((lane, base, off, 1));
    }
    runs
}

/// Whether `s` is written by an instruction of `out` that is known now:
/// a lifted group word, or (scanning) any emitted instruction.
fn out_dsts_contains(out: &Out, s: u32, group_of_word: &Map<u32, usize>, lifted: &[bool]) -> bool {
    group_of_word.get(&s).is_some_and(|&g| lifted[g]) || out.dsts.contains_key(&s)
}

/// Per slot: read as a plain slot after the rewrite, for the given lifted
/// groups.
#[allow(clippy::too_many_arguments)]
fn real_reads(
    ctx: &Ctx<'_>,
    em: &Emitter,
    ports: &PortMap,
    debug: &DebugMap,
    probe_slots: &[u32],
    opts: &Options,
    labels: &[Segs],
    chain: &[u16],
    groups: &[Group],
    member: &[(u32, u8)],
    lifted: &[bool],
    banks: &[Bank],
    bank_lane_commit: &[bool],
    valid: &dyn Fn(u32) -> bool,
) -> Vec<bool> {
    let n = em.widths.len();
    let mut real = vec![false; n];
    let label_valid = |s: u32| label_ok(labels, chain, &em.widths, s, valid);
    for (k, ins) in ctx.instrs.iter().enumerate() {
        let op = ctx.ops[k];
        let (g, _) = member[k];
        if (g != NONE && lifted[g as usize])
            || (matches!(op, Op::Shr | Op::Concat | Op::Copy) && label_valid(ins.dst))
        {
            continue;
        }
        let (sa, sb, sc) = op.slot_operands();
        for (is, s) in [(sa, ins.a), (sb, ins.b), (sc, ins.c)] {
            if is {
                real[s as usize] = true;
            }
        }
    }
    for (g, gr) in groups.iter().enumerate() {
        if !lifted[g] {
            continue;
        }
        for o in gr.opds {
            match o {
                Opd::Word(b, _) => real[b as usize] = true,
                Opd::Bcast(x) => real[x as usize] = true,
                _ => {}
            }
        }
    }
    for c in &em.commits {
        match *c {
            Commit::Copy { state, .. } if bank_lane_commit[state as usize] => {}
            _ => {
                for s in c.reads() {
                    real[s as usize] = true;
                }
            }
        }
    }
    for bank in banks {
        for (_, base, _, _) in bank_runs(ctx, labels, bank, valid) {
            real[base as usize] = true;
        }
    }
    for p in &ports.outputs {
        real[p.slot as usize] = true;
    }
    if !opts.debug {
        for &s in probe_slots {
            real[s as usize] = true;
        }
    } else {
        // Multi-segment nets cannot be views: keep them as slots.
        for map in &debug.net_slots {
            for (_, &s) in map {
                if labels.get(s as usize).is_some_and(|l| l.len() > 1) {
                    real[s as usize] = true;
                }
            }
        }
    }
    real
}

/// Dead-code elimination after lifting (without `Options::debug`).
fn dce(
    em: &mut Emitter,
    ports: &PortMap,
    debug: &mut DebugMap,
    probe_slots: &[u32],
    resolve: &dyn Fn(u32) -> u32,
    views: &mut Vec<(u32, Seg)>,
) {
    let n = em.widths.len();
    let mut producer = vec![NONE; n];
    for (k, ins) in em.instrs.iter().enumerate() {
        producer[ins.dst as usize] = k as u32;
    }
    let view_base: Map<u32, u32> = views.iter().map(|&(f, s)| (f, s.base)).collect();
    let mut live = vec![false; n];
    let mut stack = Vec::new();
    let mark = |s: u32, live: &mut Vec<bool>, stack: &mut Vec<u32>| {
        let s = view_base.get(&s).copied().unwrap_or(s);
        if !live[s as usize] {
            live[s as usize] = true;
            stack.push(s);
        }
    };
    for p in &ports.outputs {
        mark(p.slot, &mut live, &mut stack);
    }
    for c in &em.commits {
        for s in c.reads() {
            mark(s, &mut live, &mut stack);
        }
    }
    for &s in probe_slots {
        let s = resolve(s);
        mark(s, &mut live, &mut stack);
    }
    while let Some(s) = stack.pop() {
        let p = producer[s as usize];
        if p == NONE {
            continue;
        }
        let ins = em.instrs[p as usize];
        let (sa, sb, sc) = em.ops[p as usize].slot_operands();
        for (is, x) in [(sa, ins.a), (sb, ins.b), (sc, ins.c)] {
            if is {
                mark(x, &mut live, &mut stack);
            }
        }
    }
    let mut w = 0;
    for k in 0..em.instrs.len() {
        if live[em.instrs[k].dst as usize] {
            em.instrs[w] = em.instrs[k];
            em.ops[w] = em.ops[k];
            em.imms[w] = em.imms[k];
            w += 1;
        }
    }
    em.instrs.truncate(w);
    em.ops.truncate(w);
    em.imms.truncate(w);
    em.consts.retain(|&(s, _)| live[s as usize]);
    let mut has_value = vec![false; n];
    has_value[ZERO_SLOT as usize] = true;
    for p in &ports.inputs {
        has_value[p.slot as usize] = true;
    }
    for &(s, _) in &em.consts {
        has_value[s as usize] = true;
    }
    for c in &em.commits {
        if let Commit::Copy { state, .. } = *c {
            has_value[state as usize] = true;
        }
    }
    for ins in &em.instrs {
        has_value[ins.dst as usize] = true;
    }
    let ok = |s: u32| has_value[view_base.get(&s).copied().unwrap_or(s) as usize];
    for map in &mut debug.net_slots {
        map.retain(|_, s| ok(*s));
    }
    // State keys of banks always have a value (banks are state).
    views.retain(|&(_, s)| has_value[s.base as usize]);
}
