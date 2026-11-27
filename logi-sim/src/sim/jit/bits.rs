//! Bits each slot can hold, for the native backend's choice of operand
//! widths (see `clif::Slots`).
//!
//! [`SlotBits::of`] extends `compile::spec::possible_bits` to every body
//! of the program (its own and each clock-phase variant) and to packed
//! words, as a union over the writers of each slot (a slot may have one
//! writer per body). These bound the *true* values of the slots, those
//! the general body computes. A body rewritten on demanded bits (a
//! known-bits or specialized fused body, `compile::spec`) can compute
//! other bits where no reader looks; the native code masks what it writes
//! to these bounds (which changes only such bits).

use crate::compile::spec::possible_of;
use crate::compile::{Block, Commit, Instr, Program, StateLoc, ZERO_SLOT};
use crate::netlist::mask;

const ALL: u64 = u64::MAX;

/// One body's code, borrowed.
struct Parts<'a> {
    blocks: &'a [Block],
    instrs: &'a [Instr],
    imms: &'a [u64],
    commits: &'a [Commit],
}

/// Per slot of a program: the bits it can hold.
#[derive(Debug, Clone)]
pub struct SlotBits {
    bits: Vec<u64>,
}

impl SlotBits {
    /// Every slot of `p` 64 bits wide (no narrowing).
    pub fn wide(p: &Program) -> Self {
        let n = p.slot_count as usize;
        SlotBits { bits: vec![ALL; n] }
    }

    /// The bits of every slot of `p`: root inputs and state elements to
    /// their widths (anything a caller can set), constants to their
    /// values, results to what their ops can produce from their operands'
    /// bits, packed words to anything; a fixpoint over the program's own
    /// body, its clock-phase variants and their commits. A slot nothing
    /// writes is taken as 64 bits.
    pub fn of(p: &Program) -> Self {
        let n = p.slot_count as usize;
        let mut bits = vec![0u64; n];
        let mut written = vec![false; n];
        let field = |bits: &mut Vec<u64>, slot: u32, width: u8| -> Option<usize> {
            let (real, shift, m) = p.resolve(slot)?;
            let r = real as usize;
            if r < n {
                bits[r] |= (mask(width) & m).checked_shl(u32::from(shift)).unwrap_or(0);
                Some(r)
            } else {
                None
            }
        };
        for port in &p.ports.inputs {
            if let Some(r) = field(&mut bits, port.slot, port.width) {
                written[r] = true;
            }
        }
        let mut keyed = vec![false; n];
        for key in &p.state_keys {
            if let StateLoc::Slot(s) = key.loc
                && let Some(r) = field(&mut bits, s, key.width)
            {
                written[r] = true;
                keyed[r] = true;
            }
        }
        for &(s, v) in &p.consts {
            bits[s as usize] |= v;
            written[s as usize] = true;
        }
        let variants = p.phases.as_ref().map_or(&[][..], |ph| &ph.variants[..]);
        let bodies: Vec<Parts<'_>> = std::iter::once(Parts {
            blocks: &p.blocks,
            instrs: &p.instrs,
            imms: &p.imms,
            commits: &p.commits,
        })
        .chain(variants.iter().map(|v| Parts {
            blocks: &v.blocks,
            instrs: &v.instrs,
            imms: &v.imms,
            commits: &v.commits,
        }))
        .collect();
        for b in &bodies {
            for c in b.commits {
                if let Commit::Copy { state, .. } = *c {
                    let s = state as usize;
                    written[s] = true;
                    if !keyed[s] {
                        // A state slot no state key describes: anything.
                        bits[s] = ALL;
                    }
                }
            }
        }
        // Packed words hold anything in their unused lanes; a scattered
        // lane is one bit.
        for pk in &p.packs {
            bits[pk.dst as usize] = ALL;
            written[pk.dst as usize] = true;
        }
        for x in &p.scatters {
            bits[x.slot as usize] |= 1;
            written[x.slot as usize] = true;
        }
        for b in &bodies {
            for blk in b.blocks.iter().filter(|k| !k.packed) {
                for k in blk.start..blk.start + blk.len {
                    written[b.instrs[k as usize].dst as usize] = true;
                }
            }
        }
        bits[ZERO_SLOT as usize] = 0;
        written[ZERO_SLOT as usize] = true;
        for (s, w) in written.iter().enumerate() {
            if !w {
                bits[s] = ALL;
            }
        }
        let mut done = false;
        for _ in 0..256 {
            let mut changed = false;
            for b in &bodies {
                for blk in b.blocks.iter().filter(|k| !k.packed) {
                    for k in blk.start..blk.start + blk.len {
                        let i = &b.instrs[k as usize];
                        let v = possible_of(blk.op, i, b.imms[k as usize], &bits, &p.mems);
                        let d = &mut bits[i.dst as usize];
                        changed |= *d | v != *d;
                        *d |= v;
                    }
                }
                for c in b.commits {
                    if let Commit::Copy { state, next } = *c {
                        let v = bits[next as usize];
                        let d = &mut bits[state as usize];
                        changed |= *d | v != *d;
                        *d |= v;
                    }
                }
            }
            if !changed {
                done = true;
                break;
            }
        }
        if !done {
            return Self::wide(p);
        }
        SlotBits { bits }
    }

    /// The bits `slot` can hold (every bit for a slot past the program's).
    pub fn bits(&self, slot: u32) -> u64 {
        self.bits.get(slot as usize).copied().unwrap_or(ALL)
    }

    /// The bits of every slot, indexed by slot.
    pub fn as_slice(&self) -> &[u64] {
        &self.bits
    }

    /// Whether `slot` holds at most 32 bits.
    pub fn narrow(&self, slot: u32) -> bool {
        self.bits(slot) >> 32 == 0
    }
}
