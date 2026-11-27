//! Lane packing (issue #41, [`super::Options::pack`]).
//!
//! Runs after levelizing and the locality order, on the old (pre-renumber)
//! slot numbering. Within each `(rank, op)` run, the 1-bit instructions of a
//! packable op (every slot operand and the result 1 bit wide) are grouped
//! into words of up to 64 lanes; each word becomes one [`Pack`] with a new
//! 64-bit destination slot.
//!
//! - **Lane choice.** An instruction prefers the lane its first packed
//!   operand already sits in. Instructions preferring the same source word
//!   are placed at those lanes when at least [`ALIGN_MIN`] of them do, so
//!   that whole operand words are read with one load (`PackOperand::base`).
//!   Everything else fills the remaining lanes in order.
//! - **Operands.** Per operand, the source word covering the most lanes in
//!   place is the base, the bit read by the most remaining lanes (at least
//!   two) is splatted, and the rest is gathered bit by bit. Reads of the
//!   zero slot are dropped.
//! - **Boundaries.** A packed net that something reads as a plain slot (a
//!   scalar instruction, a commit or a root output) keeps its slot and is
//!   scattered there when its word is computed. Any other packed net lives
//!   only in its lane; the debug map points at its lane slot.
//!
//! Words with fewer than `pack_min` lanes are not packed.

use super::program::*;

/// Fewest instructions preferring one source word for their lanes to be
/// honoured.
const ALIGN_MIN: usize = 4;

/// Whether instruction `ins` can live in a lane.
pub(super) fn packable(op: Op, ins: &Instr, widths: &[u8]) -> bool {
    use Op::*;
    if !matches!(
        op,
        Copy | Not | And | Or | Xor | Nand | Nor | Xnor | Mux | Switch
    ) {
        return false;
    }
    let (sa, sb, sc) = op.slot_operands();
    widths[ins.dst as usize] == 1
        && [(sa, ins.a), (sb, ins.b), (sc, ins.c)]
            .iter()
            .all(|&(is, s)| !is || widths[s as usize] == 1)
}

/// One planned word: its op, new destination slot and `(lane, instr)`.
pub(super) struct Word {
    pub op: Op,
    pub dst: u32,
    pub members: Vec<(u8, u32)>,
}

/// Lane placement for every packed instruction.
pub(super) struct Plan {
    pub words: Vec<Word>,
    /// Per old slot: read as a plain slot whatever gets packed (by a commit,
    /// a root output or an instruction that cannot be packed).
    pub sink_read: Vec<bool>,
    /// Per old slot: `word << 6 | lane` of the packed instruction writing
    /// it, or `NONE`.
    pub lane_of: Vec<u32>,
}

pub(super) const NONE: u32 = u32::MAX;

impl Plan {
    pub fn new(slots: usize) -> Self {
        Plan {
            words: Vec::new(),
            sink_read: vec![false; slots],
            lane_of: vec![NONE; slots],
        }
    }

    /// Source of reading old slot `s` as one bit: `(slot, bit)`.
    fn source(&self, s: u32) -> (u32, u8) {
        match self.lane_of[s as usize] {
            NONE => (s, 0),
            x => (self.words[(x >> 6) as usize].dst, (x & 63) as u8),
        }
    }

    /// Pack the packable instructions `run` (one op, one rank, execution
    /// order) into words. Returns the instructions left scalar. New word
    /// slots are allocated through `new_slot`.
    pub fn pack_run(
        &mut self,
        op: Op,
        run: &[u32],
        instrs: &[Instr],
        mode: super::PackMode,
        min: usize,
        mut new_slot: impl FnMut() -> u32,
    ) -> Vec<u32> {
        if run.len() < min.max(1) {
            return run.to_vec();
        }
        // Preferred (source word, lane) of each instruction.
        let pref = |k: u32| -> Option<(u32, u8)> {
            let ins = &instrs[k as usize];
            let order = match op {
                Op::Mux => [ins.c, ins.b, ins.a],
                _ => [ins.a, ins.b, ins.c],
            };
            let (sa, sb, sc) = op.slot_operands();
            let is = match op {
                Op::Mux => [sc, sb, sa],
                _ => [sa, sb, sc],
            };
            order
                .iter()
                .zip(is)
                .filter(|&(_, is)| is)
                .map(|(&s, _)| self.lane_of[s as usize])
                .find(|&x| x != NONE)
                .map(|x| (x >> 6, (x & 63) as u8))
        };
        let mut count = super::FastMap::<u32, usize>::default();
        let prefs: Vec<Option<(u32, u8)>> = run.iter().map(|&k| pref(k)).collect();
        for p in prefs.iter().flatten() {
            *count.entry(p.0).or_default() += 1;
        }

        // Dense: the fewest words that hold the run. Buckets of instructions
        // preferring one source word go, largest first, to the word with the
        // most of their lanes free; everything else fills free lanes.
        let n_words = run.len().div_ceil(64);
        let mut words: Vec<(u64, Vec<(u8, u32)>)> = vec![(0, Vec::new()); n_words];
        let mut buckets: Vec<(u32, Vec<(u8, u32)>)> = Vec::new();
        let mut bucket_of = super::FastMap::<u32, usize>::default();
        let mut free = Vec::new();
        for (&k, p) in run.iter().zip(&prefs) {
            match p {
                Some((src, lane)) if count[src] >= ALIGN_MIN => {
                    let i = *bucket_of.entry(*src).or_insert_with(|| {
                        buckets.push((*src, Vec::new()));
                        buckets.len() - 1
                    });
                    buckets[i].1.push((*lane, k));
                }
                _ => free.push(k),
            }
        }
        buckets.sort_by_key(|(src, m)| (std::cmp::Reverse(m.len()), *src));
        for (_, members) in buckets {
            let want = members.iter().fold(0u64, |m, &(l, _)| m | 1 << l);
            let w = (0..n_words)
                .max_by_key(|&w| ((want & !words[w].0).count_ones(), std::cmp::Reverse(w)))
                .unwrap();
            for (lane, k) in members {
                let bit = 1u64 << lane;
                if words[w].0 & bit == 0 {
                    words[w].0 |= bit;
                    words[w].1.push((lane, k));
                } else {
                    free.push(k);
                }
            }
        }
        // Fill free lanes in execution order.
        let pos: super::FastMap<u32, usize> =
            run.iter().enumerate().map(|(i, &k)| (k, i)).collect();
        free.sort_by_key(|k| pos[k]);
        let mut free = free.into_iter();
        'fill: for w in &mut words {
            while w.0 != u64::MAX {
                let Some(k) = free.next() else { break 'fill };
                let lane = (!w.0).trailing_zeros() as u8;
                w.0 |= 1 << lane;
                w.1.push((lane, k));
            }
        }
        debug_assert!(free.next().is_none());

        let mut scalar = Vec::new();
        for (lanes, mut members) in words {
            members.sort_unstable();
            if (lanes.count_ones() as usize) < min
                || (mode == super::PackMode::Aligned && !self.pays(op, &members, instrs))
            {
                scalar.extend(members.iter().map(|&(_, k)| k));
                continue;
            }
            let w = self.words.len() as u32;
            for &(lane, k) in &members {
                self.lane_of[instrs[k as usize].dst as usize] = w << 6 | lane as u32;
            }
            self.words.push(Word {
                op,
                dst: new_slot(),
                members,
            });
        }
        // Keep execution order for what stays scalar.
        scalar.sort_by_key(|k| pos[k]);
        scalar
    }

    /// Cost model for [`super::PackMode::Aligned`], in tenths of a
    /// nanosecond as measured on the 1-bit gate benchmarks with everything
    /// in cache: a scalar 1-bit instruction ~0.9 ns, a packed word ~3 ns
    /// plus ~0.3 ns per base or splat term, ~1 ns per gathered lane and
    /// ~0.9 ns per scattered lane (a store, like the scalar instruction).
    /// Only scatters already certain
    /// (`sink_read`) are counted.
    fn pays(&self, op: Op, members: &[(u8, u32)], instrs: &[Instr]) -> bool {
        let (sa, sb, sc) = op.slot_operands();
        let mut cost = 30;
        for &(_, k) in members {
            cost += 9 * self.sink_read[instrs[k as usize].dst as usize] as usize;
        }
        for (is, pick) in [
            (sa, (|i: &Instr| i.a) as fn(&Instr) -> u32),
            (sb, |i: &Instr| i.b),
            (sc, |i: &Instr| i.c),
        ] {
            if is {
                let (o, gathers) = self.describe(members, instrs, pick);
                cost += 3 * ((o.keep != 0) as usize + (o.splat_mask != 0) as usize);
                cost += 10 * gathers.len();
            }
        }
        cost < 9 * members.len()
    }

    /// Operand descriptor for reading `pick(instr)` in every lane of a word
    /// with `members`, and its gathers (`gather_start`/`gather_len` unset).
    fn describe(
        &self,
        members: &[(u8, u32)],
        instrs: &[Instr],
        pick: fn(&Instr) -> u32,
    ) -> (PackOperand, Vec<Gather>) {
        use super::FastMap as HashMap;
        let srcs: Vec<(u8, (u32, u8))> = members
            .iter()
            .map(|&(lane, k)| (lane, self.source(pick(&instrs[k as usize]))))
            .filter(|&(_, (s, _))| s != ZERO_SLOT)
            .collect();
        let mut o = PackOperand::default();
        // Base: the word covering the most lanes in place.
        let mut in_place = HashMap::<u32, u64>::default();
        for &(lane, (s, bit)) in &srcs {
            if bit == lane && self.is_word(s) {
                *in_place.entry(s).or_default() |= 1 << lane;
            }
        }
        if let Some((&s, &m)) = in_place
            .iter()
            .max_by_key(|&(&s, &m)| (m.count_ones(), std::cmp::Reverse(s)))
        {
            o.base = s;
            o.keep = m;
        }
        let rest: Vec<_> = srcs
            .into_iter()
            .filter(|&(lane, (s, bit))| !(o.keep != 0 && s == o.base && bit == lane))
            .collect();
        // Splat: the bit read by the most remaining lanes (at least two).
        let mut by_src = HashMap::<(u32, u8), u64>::default();
        for &(lane, src) in &rest {
            *by_src.entry(src).or_default() |= 1 << lane;
        }
        if let Some((&(s, bit), &m)) = by_src
            .iter()
            .filter(|&(_, m)| m.count_ones() >= 2)
            .max_by_key(|&(&src, &m)| (m.count_ones(), std::cmp::Reverse(src)))
        {
            o.splat = s;
            o.splat_bit = bit;
            o.splat_mask = m;
        }
        let gathers = rest
            .into_iter()
            .filter(|&(lane, _)| o.splat_mask & (1 << lane) == 0)
            .map(|(lane, (slot, bit))| Gather { slot, bit, lane })
            .collect();
        (o, gathers)
    }

    /// Build the packs once every word is planned. `scalar_read[s]` says
    /// whether old slot `s` is read as a plain slot (by a scalar
    /// instruction, a commit or a root output).
    pub fn build(
        &self,
        instrs: &[Instr],
        scalar_read: &[bool],
    ) -> (Vec<Pack>, Vec<Gather>, Vec<Scatter>) {
        let mut packs = Vec::with_capacity(self.words.len());
        let mut gathers = Vec::new();
        let mut scatters = Vec::new();
        for word in &self.words {
            let (sa, sb, sc) = word.op.slot_operands();
            let mut operand = |pick: fn(&Instr) -> u32| {
                let (mut o, g) = self.describe(&word.members, instrs, pick);
                o.gather_start = gathers.len() as u32;
                o.gather_len = g.len() as u32;
                gathers.extend(g);
                o
            };
            let a = if sa {
                operand(|i| i.a)
            } else {
                PackOperand::default()
            };
            let b = if sb {
                operand(|i| i.b)
            } else {
                PackOperand::default()
            };
            let c = if sc {
                operand(|i| i.c)
            } else {
                PackOperand::default()
            };
            let scatter_start = scatters.len() as u32;
            let mut lanes = 0u64;
            for &(lane, k) in &word.members {
                lanes |= 1 << lane;
                let dst = instrs[k as usize].dst;
                if scalar_read[dst as usize] {
                    scatters.push(Scatter { slot: dst, lane });
                }
            }
            packs.push(Pack {
                dst: word.dst,
                lanes,
                a,
                b,
                c,
                scatter_start,
                scatter_len: scatters.len() as u32 - scatter_start,
            });
        }
        (packs, gathers, scatters)
    }

    fn is_word(&self, s: u32) -> bool {
        // Word slots are allocated after every original slot.
        s as usize >= self.lane_of.len()
    }
}
