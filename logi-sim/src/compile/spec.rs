//! State-keyed specialization, compile side (issue #117; the design is in
//! `docs/adaptive-spec.md`).
//!
//! A fused-run body (`phases::prune` of a clock-phase variant) is
//! specialized on the values of a few *key slots*: state or values the
//! body computes, read once per tick before the body runs. Under the
//! assumption the key holds, the logic reading the key folds like the
//! clocks of a phase variant. This module holds the pure analyses and the
//! rewrite; the runtime (key dispatch, hit counters, building bodies in
//! the background) is not here.
//!
//! - [`possible_bits`]: per slot, the bits that can ever be 1 (port and
//!   state widths, constants, through every op). Sound for any state a
//!   caller can set, pokes and imports included.
//! - [`SpecContext::demanded`]: per slot, the bits some reader of the body
//!   observes (backwards from the commits). A key needs only the key
//!   slot's demanded bits: [`SpecContext::support`].
//! - [`SpecContext::specialize`]: the body under an assumption of known
//!   bits ([`Known`]) for some slots, with constant folding, mux
//!   selection, identities and dead-code elimination down to what the
//!   commits read.
//! - [`SpecContext::candidates`] and [`SpecContext::gain`]: automatic key
//!   candidates and the operations a key value folds away.
//!
//! # Exactness
//!
//! The rewrite reasons about the values the general body computes (its
//! *true* values). Every slot the specialized body writes agrees with
//! the true value on the slot's demanded bits, which is all its readers
//! observe; commits demand every bit, so the state they apply is exact.
//! The rules that make that hold:
//!
//! - a value is a constant when its demanded bits are all known; the
//!   constant is the known value *restricted to the demanded bits* (so
//!   the body does not depend on knowledge of bits nobody reads, which is
//!   what makes keying on [`SpecContext::support`] give the same body as
//!   keying on the whole slot);
//! - a value aliases another slot only where the two agree on every
//!   demanded bit, and only when the other slot is demanded at least as
//!   much (so the other slot is itself exact there);
//! - an instruction that stays reads operands that are exact on their
//!   demanded bits, which is all the operation's demanded result bits
//!   depend on (the transfer functions of [`SpecContext::demanded`]).
//!
//! Known bits propagate forwards ([`transfer`]); each transfer only uses
//! knowledge of an operand's demanded bits for the result's demanded
//! bits, so the decisions, too, depend on demanded knowledge only. The
//! unit tests check both claims exhaustively on small designs: the
//! specialized body equals the general one on every state satisfying the
//! key, and keying on the support yields the same body as keying on the
//! whole slot.
//!
//! Slots written by the key's *prefix* (the cone that computes a key slot
//! the body itself computes, run before the body) are taken as computed:
//! the specialized body neither recomputes nor folds them.

use super::{Block, Body, Commit, FastMap, Instr, MemDesc, Op, Program, StateLoc, ZERO_SLOT};
use crate::netlist::mask;

/// Every bit.
pub const ALL: u64 = u64::MAX;

const NONE: u32 = u32::MAX;

/// Bits of a value known at specialization time: `zeros` known 0, `ones`
/// known 1 (disjoint).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct Known {
    pub zeros: u64,
    pub ones: u64,
}

impl Known {
    /// Nothing known.
    pub const UNKNOWN: Known = Known { zeros: 0, ones: 0 };

    /// Every bit known: `v`.
    pub fn constant(v: u64) -> Self {
        Known { zeros: !v, ones: v }
    }

    /// The bits `mask` known, with the values they have in `value`.
    pub fn bits(mask: u64, value: u64) -> Self {
        Known {
            zeros: mask & !value,
            ones: mask & value,
        }
    }

    /// The known bits.
    pub fn mask(self) -> u64 {
        self.zeros | self.ones
    }

    /// Whether every bit of `bits` is known.
    pub fn knows(self, bits: u64) -> bool {
        bits & !self.mask() == 0
    }

    /// Both facts (an assumption added to what is derived).
    pub fn union(self, o: Known) -> Known {
        Known {
            zeros: self.zeros | o.zeros,
            ones: self.ones | o.ones,
        }
    }

    /// What holds for either (a mux with an unknown select).
    pub fn meet(self, o: Known) -> Known {
        Known {
            zeros: self.zeros & o.zeros,
            ones: self.ones & o.ones,
        }
    }

    /// Also known: every bit outside `possible` is 0.
    pub fn within(self, possible: u64) -> Known {
        Known {
            zeros: self.zeros | !possible,
            ones: self.ones & possible,
        }
    }

    /// Whether some bit is known 1 (the value is nonzero).
    fn nonzero(self) -> bool {
        self.ones != 0
    }

    /// Whether every bit is known 0.
    fn zero(self) -> bool {
        self.zeros == ALL
    }

    fn not(self, imm: u64) -> Known {
        Known {
            zeros: self.ones | !imm,
            ones: self.zeros & imm,
        }
    }

    fn bit(v: bool) -> Known {
        Known::constant(v as u64)
    }

    /// A 1-bit result nothing is known of.
    const ONE_BIT: Known = Known { zeros: !1, ones: 0 };
}

/// The bits of `x` up to and including its highest set bit.
fn low_mask(x: u64) -> u64 {
    match x.leading_zeros() {
        64 => 0,
        lz => ALL >> lz,
    }
}

/// Number of low bits known, from bit 0 up.
fn known_low(k: Known) -> u32 {
    k.mask().trailing_ones()
}

/// Per slot of `p`, the bits that can be 1 in any state: root inputs and
/// state elements to their widths (anything a caller can set), constants
/// to their values, and every instruction result to what its op can
/// produce from its operands' possible bits. A fixpoint over the
/// program's own body and commits (a state slot can hold whatever its
/// next value can). For a packed program every slot is [`ALL`].
pub fn possible_bits(p: &Program) -> Vec<u64> {
    let n = p.slot_count as usize;
    if !p.packs.is_empty() {
        return vec![ALL; n];
    }
    let mut pos = vec![0u64; n];
    let field = |pos: &mut Vec<u64>, slot: u32, width: u8| {
        if let Some((real, shift, m)) = p.resolve(slot)
            && (real as usize) < n
        {
            pos[real as usize] |= (mask(width) & m).checked_shl(u32::from(shift)).unwrap_or(0);
        }
    };
    for port in &p.ports.inputs {
        field(&mut pos, port.slot, port.width);
    }
    let mut keyed = vec![false; n];
    for key in &p.state_keys {
        if let StateLoc::Slot(s) = key.loc {
            field(&mut pos, s, key.width);
            if let Some((real, _, _)) = p.resolve(s)
                && (real as usize) < n
            {
                keyed[real as usize] = true;
            }
        }
    }
    for c in &p.commits {
        if let Commit::Copy { state, .. } = *c
            && !keyed[state as usize]
        {
            // A state slot no state key describes: anything.
            pos[state as usize] = ALL;
        }
    }
    for &(s, v) in &p.consts {
        pos[s as usize] |= v;
    }
    pos[ZERO_SLOT as usize] = 0;
    let ops = op_of(&p.blocks, p.instrs.len());
    for _ in 0..128 {
        for (k, i) in p.instrs.iter().enumerate() {
            pos[i.dst as usize] = possible_of(ops[k], i, p.imms[k], &pos, &p.mems);
        }
        let mut changed = false;
        for c in &p.commits {
            if let Commit::Copy { state, next } = *c {
                let grown = pos[state as usize] | pos[next as usize];
                changed |= grown != pos[state as usize];
                pos[state as usize] = grown;
            }
        }
        if !changed {
            return pos;
        }
    }
    vec![ALL; n]
}

/// Per instruction index, the op of its block.
fn op_of(blocks: &[Block], len: usize) -> Vec<Op> {
    let mut ops = vec![Op::Copy; len];
    for b in blocks {
        for k in b.start..b.start + b.len {
            ops[k as usize] = b.op;
        }
    }
    ops
}

/// The bits an instruction's result can have, from its operands'.
fn possible_of(op: Op, i: &Instr, imm: u64, pos: &[u64], mems: &[MemDesc]) -> u64 {
    let a = pos[i.a as usize];
    let b = || pos[i.b as usize];
    let c = || pos[i.c as usize];
    match op {
        Op::Copy => a,
        Op::Not | Op::Nand | Op::Nor | Op::Xnor | Op::AddSum => imm,
        Op::And => a & b(),
        Op::Or | Op::Xor => a | b(),
        Op::Mux => b() | c(),
        Op::Switch => b(),
        Op::AddCarry
        | Op::Eq
        | Op::LtU
        | Op::ReduceOr
        | Op::ReduceAnd
        | Op::ReduceXor
        | Op::Clock
        | Op::SrNext => 1,
        Op::Shr => a.checked_shr(i.b).unwrap_or(0) & imm,
        Op::Concat => a | b().checked_shl(i.c).unwrap_or(0),
        Op::Sext => {
            if i.b >= 1 && (a >> (i.b - 1)) & 1 == 1 {
                a | (imm & !mask(i.b as u8))
            } else {
                a
            }
        }
        Op::MemRead => mems.get(i.b as usize).map_or(ALL, |m| mask(m.width)),
    }
}

/// The known bits of an instruction's result from its operands'
/// (`k(slot)`), its own possible bits applied by the caller. `phase` is
/// the tick residue of the body's clock-phase variant, if any (a `Clock`
/// is then a constant).
pub fn transfer(
    op: Op,
    i: &Instr,
    imm: u64,
    k: &dyn Fn(u32) -> Known,
    mems: &[MemDesc],
    phase: Option<u64>,
) -> Known {
    let a = || k(i.a);
    let b = || k(i.b);
    let c = || k(i.c);
    let full = |x: Known| x.mask() == ALL;
    match op {
        Op::Copy => a(),
        Op::Not => a().not(imm),
        Op::And => {
            let (x, y) = (a(), b());
            Known {
                zeros: x.zeros | y.zeros,
                ones: x.ones & y.ones,
            }
        }
        Op::Or => {
            let (x, y) = (a(), b());
            Known {
                zeros: x.zeros & y.zeros,
                ones: x.ones | y.ones,
            }
        }
        Op::Xor => xor(a(), b()),
        Op::Nand => transfer(Op::And, i, imm, k, mems, phase).not(imm),
        Op::Nor => transfer(Op::Or, i, imm, k, mems, phase).not(imm),
        Op::Xnor => xor(a(), b()).not(imm),
        Op::Mux => {
            let s = a();
            if s.nonzero() {
                c()
            } else if s.zero() {
                b()
            } else {
                b().meet(c())
            }
        }
        Op::Switch => {
            let s = a();
            if s.nonzero() {
                b()
            } else if s.zero() {
                Known::constant(0)
            } else {
                Known {
                    zeros: b().zeros,
                    ones: 0,
                }
            }
        }
        Op::AddSum => {
            let (x, y, z) = (a(), b(), c());
            let n = known_low(x).min(known_low(y)).min(known_low(z));
            let low = if n >= 64 { ALL } else { mask(n as u8) };
            let v = x.ones.wrapping_add(y.ones).wrapping_add(z.ones) & imm;
            Known {
                zeros: (!v & low) | !imm,
                ones: v & low,
            }
        }
        Op::AddCarry => {
            let (x, y, z) = (a(), b(), c());
            if full(x) && full(y) && full(z) {
                let s = x.ones as u128 + y.ones as u128 + z.ones as u128;
                Known::bit((s >> imm) & 1 == 1)
            } else {
                Known::ONE_BIT
            }
        }
        Op::Eq => {
            let (x, y) = (a(), b());
            if x.mask() & y.mask() & (x.ones ^ y.ones) != 0 {
                Known::bit(false)
            } else if full(x) && full(y) {
                Known::bit(x.ones == y.ones)
            } else {
                Known::ONE_BIT
            }
        }
        Op::LtU => {
            let (x, y) = (a(), b());
            if full(x) && full(y) {
                Known::bit(x.ones < y.ones)
            } else {
                Known::ONE_BIT
            }
        }
        Op::ReduceOr => {
            let x = a();
            if x.nonzero() {
                Known::bit(true)
            } else if x.zero() {
                Known::bit(false)
            } else {
                Known::ONE_BIT
            }
        }
        Op::ReduceAnd => {
            // `a == imm`.
            let x = a();
            if x.zeros & imm != 0 || x.ones & !imm != 0 {
                Known::bit(false)
            } else if full(x) {
                Known::bit(true)
            } else {
                Known::ONE_BIT
            }
        }
        Op::ReduceXor => {
            let x = a();
            if full(x) {
                Known::bit(x.ones.count_ones() & 1 == 1)
            } else {
                Known::ONE_BIT
            }
        }
        Op::Shr => {
            let x = a();
            let sh = |v: u64| v.checked_shr(i.b).unwrap_or(0);
            Known {
                zeros: sh(x.zeros) | !sh(ALL) | !imm,
                ones: sh(x.ones) & imm,
            }
        }
        Op::Concat => {
            let (x, y) = (a(), b());
            let sh = |v: u64| v.checked_shl(i.c).unwrap_or(0);
            let low = if i.c >= 64 { ALL } else { mask(i.c as u8) };
            Known {
                zeros: x.zeros & (sh(y.zeros) | low),
                ones: x.ones | sh(y.ones),
            }
        }
        Op::Sext => {
            let x = a();
            if i.b == 0 || i.b > 64 {
                return Known::UNKNOWN;
            }
            let low = mask(i.b as u8);
            let sign = 1u64 << (i.b - 1);
            if x.ones & sign != 0 {
                Known {
                    zeros: (x.zeros & low) | !imm,
                    ones: (x.ones | (imm & !low)) & imm,
                }
            } else if x.zeros & sign != 0 {
                x
            } else {
                Known {
                    zeros: (x.zeros & low) | !imm,
                    ones: x.ones & low,
                }
            }
        }
        Op::Clock => match phase {
            Some(t) if i.a != 0 => {
                Known::bit((t + u64::from(i.c)) % u64::from(i.a) < u64::from(i.b))
            }
            _ => Known::ONE_BIT,
        },
        Op::MemRead => {
            let Some(m) = mems.get(i.b as usize) else {
                return Known::UNKNOWN;
            };
            let width = Known {
                zeros: !mask(m.width),
                ones: 0,
            };
            if m.writable {
                return width;
            }
            let x = a();
            let amask = mask(m.addr_bits);
            let free = amask & !x.mask();
            if free.count_ones() > ROM_ENUM_BITS {
                return width;
            }
            // Every address the known bits allow: what their words share.
            let word = |addr: u64| m.init.get(addr as usize).copied().unwrap_or(0);
            let base = x.ones & amask;
            let mut out = Known::constant(word(base));
            let mut sub = free;
            while sub != 0 {
                out = out.meet(Known::constant(word(base | sub)));
                sub = (sub - 1) & free;
            }
            out.within(mask(m.width))
        }
        Op::SrNext => {
            let (s, r, q) = (a(), b(), c());
            if r.nonzero() {
                Known::bit(false)
            } else if r.zero() && s.nonzero() {
                Known::bit(true)
            } else if r.zero() && s.zero() {
                q
            } else {
                Known::ONE_BIT
            }
        }
    }
}

/// Most unknown address bits a ROM read is enumerated over (the words the
/// remaining addresses can read are met).
pub const ROM_ENUM_BITS: u32 = 8;

fn xor(x: Known, y: Known) -> Known {
    let m = x.mask() & y.mask();
    let v = x.ones ^ y.ones;
    Known {
        zeros: !v & m,
        ones: v & m,
    }
}

/// A slot's value in a specialized body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Val {
    /// Computed by the slot's own (rewritten) instruction.
    Own,
    /// Equal on its demanded bits to another slot (which is not an
    /// alias).
    Alias(u32),
    /// Equal on its demanded bits to a constant (restricted to them).
    Const(u64),
}

/// An instruction operand: a slot, or a constant and the slot it came
/// from (to compute it there when no constant slot holds it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Opnd {
    Slot(u32),
    Const(u64, u32),
}

/// One fused-run body prepared for specialization: its demanded bits,
/// writers and ranks, shared by every key value it is specialized on.
#[derive(Debug, Clone)]
pub struct SpecContext {
    body: Body,
    possible: Vec<u64>,
    demanded: Vec<u64>,
    /// The part of `demanded` some decision reads.
    decided: Vec<u64>,
    /// Per slot, the body instruction writing it (`NONE`: none).
    writer: Vec<u32>,
    /// Per body instruction: its op and rank.
    op_rank: Vec<(Op, u32)>,
    /// The constant slot holding each constant value.
    home: FastMap<u64, u32>,
    phase: Option<u64>,
}

/// An automatic key candidate (see [`SpecContext::candidates`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub slot: u32,
    pub class: KeyClass,
    /// Bits of the slot the body reads (the key's mask).
    pub support: u64,
    /// Instructions of the prefix computing the slot (0 for a state or
    /// input slot).
    pub prefix_ops: usize,
    /// Every value the masked key can take, when the analysis knows them
    /// (a ROM word: its distinct contents; a narrow key: every value).
    pub values: Option<Vec<u64>>,
}

/// Why a slot is a key candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum KeyClass {
    /// A word read from a ROM (a microcode word): few distinct values.
    RomWord,
    /// A 1-bit comparison of state (a counter's region flag).
    Flag,
    /// A state word that holds its value unless enabled, or that this
    /// body does not write at all (a control register).
    Register,
}

/// Widest masked key whose every value [`SpecContext::candidates`]
/// enumerates.
pub const ENUM_BITS: u32 = 8;

/// Most distinct values of a ROM word candidate listed.
pub const ROM_VALUES: usize = 4096;

impl SpecContext {
    /// Prepare `body` (a fused-run body: unpacked, unguarded) of `p`,
    /// with `possible` from [`possible_bits`] and the tick residue of its
    /// clock-phase variant (`None` without phases). `None` for a packed
    /// or guarded body.
    pub fn new(p: &Program, body: &Body, possible: &[u64], phase: Option<u64>) -> Option<Self> {
        if body.blocks.iter().any(|b| b.packed) || !body.guards.is_empty() {
            return None;
        }
        let n = p.slot_count as usize;
        let mut writer = vec![NONE; n];
        let mut op_rank = vec![(Op::Copy, 0); body.instrs.len()];
        for b in &body.blocks {
            for k in b.start..b.start + b.len {
                writer[body.instrs[k as usize].dst as usize] = k;
                op_rank[k as usize] = (b.op, b.rank);
            }
        }
        let mut home = FastMap::default();
        home.insert(0, ZERO_SLOT);
        for &(s, v) in &p.consts {
            home.entry(v).or_insert(s);
        }
        let (demanded, decided) = demanded(p, body, &op_rank);
        Some(SpecContext {
            body: body.clone(),
            possible: possible.to_vec(),
            demanded,
            decided,
            writer,
            op_rank,
            home,
            phase,
        })
    }

    /// The body this context specializes.
    pub fn body(&self) -> &Body {
        &self.body
    }

    /// Per slot, the bits some reader in the body observes.
    pub fn demanded(&self) -> &[u64] {
        &self.demanded
    }

    /// The bits of `slot` the body observes and that can be 1: keying on
    /// just these gives the same body as keying on the whole value.
    pub fn exact_support(&self, slot: u32) -> u64 {
        self.demanded[slot as usize] & self.possible[slot as usize]
    }

    /// The bits of `slot` a key on it needs: the [`Self::exact_support`]
    /// bits some decision reads. Leaving out the others (bits that only
    /// travel to a commit as data, such as a register's hold path) loses
    /// at most the folding of that data into constants.
    pub fn support(&self, slot: u32) -> u64 {
        self.exact_support(slot) & self.decided[slot as usize]
    }

    /// Whether the body computes `slot` (a key on it needs a prefix).
    pub fn computes(&self, slot: u32) -> bool {
        self.writer[slot as usize] != NONE
    }

    /// The prefix computing `keys`: the body's instructions in their
    /// cone, without commits, and which slots it writes. Empty when every
    /// key is a state, input or constant slot.
    pub fn prefix(&self, keys: &[u32]) -> (Body, Vec<bool>) {
        let n = self.writer.len();
        let mut live = vec![false; self.body.instrs.len()];
        let mut work: Vec<u32> = keys.to_vec();
        while let Some(s) = work.pop() {
            let w = self.writer[s as usize];
            if w == NONE || live[w as usize] {
                continue;
            }
            live[w as usize] = true;
            let i = self.body.instrs[w as usize];
            let (sa, sb, sc) = self.op_rank[w as usize].0.slot_operands();
            for (is, x) in [(sa, i.a), (sb, i.b), (sc, i.c)] {
                if is {
                    work.push(x);
                }
            }
        }
        let mut pre = vec![false; n];
        let mut out = Body::default();
        for b in &self.body.blocks {
            let start = out.instrs.len() as u32;
            for k in b.start..b.start + b.len {
                if live[k as usize] {
                    let i = self.body.instrs[k as usize];
                    pre[i.dst as usize] = true;
                    out.instrs.push(i);
                    out.imms.push(self.body.imms[k as usize]);
                }
            }
            let len = out.instrs.len() as u32 - start;
            if len > 0 {
                out.blocks.push(Block { start, len, ..*b });
            }
        }
        (out, pre)
    }

    /// The body for settles in which every `(slot, known)` of `assume`
    /// holds (state and input slots at the start of the settle, computed
    /// slots once computed), and in which the slots `pre` marks (a
    /// [`Self::prefix`]'s) already hold their values. Its commits apply
    /// the same state and memory writes as the body's whenever the
    /// assumption holds; it has no guards (guard it afterwards).
    pub fn specialize(&self, p: &Program, assume: &[(u32, Known)], pre: &[bool]) -> Body {
        let body = &self.body;
        let n = self.writer.len();
        let is_pre = |s: u32| pre.get(s as usize).copied().unwrap_or(false);
        let mut assumed: FastMap<u32, Known> = FastMap::default();
        for &(s, k) in assume {
            let e = assumed.entry(s).or_default();
            *e = e.union(k);
        }
        let mut kn: Vec<Known> = self
            .possible
            .iter()
            .map(|&m| Known::UNKNOWN.within(m))
            .collect();
        let mut val = vec![Val::Own; n];
        for &(s, v) in &p.consts {
            kn[s as usize] = Known::constant(v);
            val[s as usize] = Val::Const(v);
        }
        kn[ZERO_SLOT as usize] = Known::constant(0);
        val[ZERO_SLOT as usize] = Val::Const(0);
        // A constant once its demanded bits are known, restricted to them.
        let constant = |k: Known, d: u64| k.knows(d).then_some(Val::Const(k.ones & d));
        for (&s, &k) in &assumed {
            if self.writer[s as usize] == NONE {
                let k = kn[s as usize].union(k);
                kn[s as usize] = k;
                if let Some(v) = constant(k, self.demanded[s as usize]) {
                    val[s as usize] = v;
                }
            }
        }
        let opnd = |val: &[Val], s: u32| match val[s as usize] {
            Val::Own => Opnd::Slot(s),
            Val::Alias(t) => Opnd::Slot(t),
            Val::Const(c) => Opnd::Const(c, s),
        };
        let mut own: Vec<[Opnd; 3]> = vec![[Opnd::Slot(ZERO_SLOT); 3]; body.instrs.len()];
        for (idx, i) in body.instrs.iter().enumerate() {
            let (op, _) = self.op_rank[idx];
            let imm = body.imms[idx];
            let d = i.dst;
            let dm = self.demanded[d as usize];
            let (sa, sb, sc) = op.slot_operands();
            let pick = |is: bool, s: u32| {
                if is {
                    opnd(&val, s)
                } else {
                    Opnd::Slot(ZERO_SLOT)
                }
            };
            let ops = [pick(sa, i.a), pick(sb, i.b), pick(sc, i.c)];
            own[idx] = ops;
            let mut k = if is_pre(d) {
                Known::UNKNOWN
            } else {
                transfer(op, i, imm, &|s| kn[s as usize], &p.mems, self.phase)
            };
            k = k.within(self.possible[d as usize]);
            if let Some(a) = assumed.get(&d) {
                k = k.union(*a);
            }
            kn[d as usize] = k;
            if is_pre(d) {
                // Computed by the prefix: its slot holds the value.
                val[d as usize] = constant(k, dm).unwrap_or(Val::Own);
                continue;
            }
            if let Some(v) = constant(k, dm) {
                val[d as usize] = v;
                continue;
            }
            // Aliases: `d` equals `t` on its demanded bits, and `t` is
            // exact on them.
            let alias =
                |t: Opnd| -> Option<Val> {
                    match t {
                        Opnd::Slot(t) => (dm & !self.demanded[t as usize] == 0)
                            .then_some(if t == d { Val::Own } else { Val::Alias(t) }),
                        Opnd::Const(c, _) => Some(Val::Const(c & dm)),
                    }
                };
            let pa = self.possible[i.a as usize];
            let pb = || self.possible[i.b as usize];
            let ka = kn[i.a as usize];
            let kb = || kn[i.b as usize];
            let v = match op {
                Op::Copy => alias(ops[0]),
                Op::Mux => {
                    let s = ka;
                    if s.nonzero() {
                        alias(ops[2])
                    } else if s.zero() || ops[1] == ops[2] {
                        alias(ops[1])
                    } else {
                        None
                    }
                }
                Op::Switch if ka.nonzero() => alias(ops[1]),
                // `a & b` is `a` where `b` is 1 wherever `a` can be.
                Op::And if (dm & pa) & !kb().ones == 0 => alias(ops[0]),
                Op::And if (dm & pb()) & !ka.ones == 0 => alias(ops[1]),
                Op::Or | Op::Xor if dm & !kb().zeros == 0 => alias(ops[0]),
                Op::Or | Op::Xor if dm & !ka.zeros == 0 => alias(ops[1]),
                Op::Concat
                    if dm
                        & !(kb().zeros.checked_shl(i.c).unwrap_or(0) | mask(i.c.min(64) as u8))
                        == 0 =>
                {
                    alias(ops[0])
                }
                _ => None,
            };
            if let Some(v) = v {
                val[d as usize] = v;
            }
        }

        // Commits: dropped when they would change nothing.
        let mut commits: Vec<Commit> = Vec::with_capacity(body.commits.len());
        let mut reads: Vec<[Opnd; 3]> = Vec::with_capacity(body.commits.len());
        for c in &body.commits {
            let r = c.reads().map(|s| opnd(&val, s));
            let off = |o: Opnd| matches!(o, Opnd::Const(0, _));
            let keep = match *c {
                Commit::Copy { state, .. } => {
                    let ks = kn[state as usize];
                    r[0] != Opnd::Slot(state)
                        && !matches!(r[0], Opnd::Const(v, _)
                            if ks.knows(self.possible[state as usize]) && ks.ones == v)
                }
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
        let mut work: Vec<u32> = Vec::new();
        for (c, r) in commits.iter_mut().zip(&reads) {
            let orig = c.reads();
            let mut j = 0;
            *c = c.map_reads(|_| {
                let (o, s) = (r[j], orig[j]);
                j += 1;
                match o {
                    Opnd::Slot(t) if !written[t as usize] => {
                        work.push(t);
                        t
                    }
                    Opnd::Const(v, _) if self.home.contains_key(&v) => self.home[&v],
                    _ => {
                        // The original slot, written by this settle (a
                        // commit reads no slot a commit writes).
                        work.push(s);
                        s
                    }
                }
            });
        }

        // Emit what the commits need.
        let mut need = vec![false; n];
        let mut out: Vec<(u32, Op, u32, Instr, u64)> = Vec::new();
        while let Some(s) = work.pop() {
            if need[s as usize] {
                continue;
            }
            need[s as usize] = true;
            let w = self.writer[s as usize];
            if w == NONE || is_pre(s) {
                continue; // state, input, constant, or computed by the prefix
            }
            let (op, rank) = self.op_rank[w as usize];
            let orig = body.instrs[w as usize];
            let imm = body.imms[w as usize];
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
            let copy = |a: u32| {
                (
                    Op::Copy,
                    Instr {
                        dst: s,
                        a,
                        b: 0,
                        c: 0,
                    },
                )
            };
            let (op, ins) = match val[s as usize] {
                Val::Alias(t) => copy(slot_of(Opnd::Slot(t), &mut work)),
                Val::Const(v) if self.home.contains_key(&v) => copy(self.home[&v]),
                Val::Own | Val::Const(_) => {
                    // The instruction over its rewritten operands (for a
                    // constant with no slot of its own, they compute it).
                    let (sa, sb, sc) = op.slot_operands();
                    let ops = own[w as usize];
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
            let imm = if op == Op::Copy { 0 } else { imm };
            out.push((rank, op, w, ins, imm));
        }
        out.sort_unstable_by_key(|&(rank, op, w, _, _)| (rank, op, w));
        let mut result = Body {
            commits,
            ..Body::default()
        };
        for (rank, op, _, ins, imm) in out {
            match result.blocks.last_mut() {
                Some(b) if b.rank == rank && b.op == op => b.len += 1,
                _ => result.blocks.push(Block {
                    op,
                    rank,
                    start: result.instrs.len() as u32,
                    len: 1,
                    packed: false,
                }),
            }
            result.instrs.push(ins);
            result.imms.push(imm);
        }
        result
    }

    /// Operations a settle saves when the key `fields` holds `value`
    /// (packed as [`KeyLayout::pack`] packs it): the body's operations,
    /// minus the prefix's and the specialized body's. Negative when the
    /// prefix costs more than the folding saves.
    pub fn gain(&self, p: &Program, layout: &KeyLayout, value: u64) -> i64 {
        let slots: Vec<u32> = layout.fields.iter().map(|f| f.slot).collect();
        let (prefix, pre) = self.prefix(&slots);
        let body = self.specialize(p, &layout.assume(value), &pre);
        self.body.ops() as i64 - prefix.ops() as i64 - body.ops() as i64
    }

    /// The body a key miss runs: the general body without the
    /// instructions `pre` marks (the prefix already computed them), so a
    /// miss costs nothing but the dispatch.
    pub fn fallback(&self, p: &Program, pre: &[bool]) -> Body {
        self.specialize(p, &[], pre)
    }

    /// `layout` without the key bits that do not pay: repeatedly drop the
    /// bit whose loss is smallest, while that loss (operations per settle,
    /// averaged over `samples` weighted by their counts, as
    /// [`Self::specialize`] counts them) is below `min_loss`. Fewer bits
    /// mean fewer distinct keys, so bodies built in one scene keep hitting
    /// in another (a title screen's `PPUMASK` against a game's, which
    /// differ only in the left-column clipping bits). Any sub-mask is
    /// exact; this only trades folding for reach.
    pub fn prune_bits(
        &self,
        p: &Program,
        layout: &KeyLayout,
        samples: &[(u64, u64)],
        min_loss: f64,
    ) -> KeyLayout {
        let slots: Vec<u32> = layout.fields.iter().map(|f| f.slot).collect();
        let pre = self.prefix(&slots).1;
        // Samples as assumptions, so they survive dropping bits.
        let assumed: Vec<(Vec<(u32, Known)>, u64)> = samples
            .iter()
            .map(|&(k, n)| (layout.assume(k), n))
            .collect();
        let total = samples.iter().map(|s| s.1).sum::<u64>().max(1) as f64;
        let cost = |fields: &[KeyField]| -> f64 {
            let mut sum = 0.0;
            for (a, n) in &assumed {
                let restricted: Vec<(u32, Known)> = a
                    .iter()
                    .zip(fields)
                    .map(|(&(s, k), f)| (s, Known::bits(k.mask() & f.mask, k.ones)))
                    .collect();
                sum += *n as f64 * self.specialize(p, &restricted, &pre).ops() as f64;
            }
            sum / total
        };
        let mut fields = layout.fields.clone();
        let mut now = cost(&fields);
        loop {
            let mut best: Option<(f64, usize, u64)> = None;
            for (i, f) in fields.iter().enumerate() {
                let mut m = f.mask;
                while m != 0 {
                    let bit = m.isolate_lowest_one();
                    m &= m - 1;
                    let mut t = fields.clone();
                    t[i].mask &= !bit;
                    let c = cost(&t);
                    if best.is_none_or(|b| c < b.0) {
                        best = Some((c, i, bit));
                    }
                }
            }
            match best {
                Some((c, i, bit)) if c - now < min_loss => {
                    fields[i].mask &= !bit;
                    now = c;
                }
                _ => break,
            }
        }
        fields.retain(|f| f.mask != 0);
        KeyLayout::new(fields)
    }

    /// Key candidates of this body, found by the shape of the logic:
    ///
    /// - [`KeyClass::RomWord`]: the result of a read from a ROM;
    /// - [`KeyClass::Flag`]: a 1-bit comparison (`Eq`, `LtU`, a
    ///   reduction) computed from state alone (no root input, no RAM);
    /// - [`KeyClass::Register`]: a state slot the body reads that it
    ///   either does not write, or writes through a mux one arm of which
    ///   is the state itself (a register with an enable).
    ///
    /// Only slots with a nonzero [`Self::support`] are listed. Choosing
    /// among them needs the values they take, which is the runtime's
    /// part (see the design doc); [`Self::gain`] estimates each value.
    pub fn candidates(&self, p: &Program) -> Vec<Candidate> {
        let body = &self.body;
        let n = self.writer.len();
        // Slots computed from state and constants alone.
        let mut is_input = vec![false; n];
        for port in &p.ports.inputs {
            if let Some((real, _, _)) = p.resolve(port.slot)
                && (real as usize) < n
            {
                is_input[real as usize] = true;
            }
        }
        let mut dynamic = is_input; // depends on a root input or a RAM
        let mut out = Vec::new();
        let mut seen = vec![false; n];
        for (idx, i) in body.instrs.iter().enumerate() {
            let (op, _) = self.op_rank[idx];
            let (sa, sb, sc) = op.slot_operands();
            let mut dyn_ =
                op == Op::MemRead && p.mems.get(i.b as usize).is_some_and(|m| m.writable);
            for (is, x) in [(sa, i.a), (sb, i.b), (sc, i.c)] {
                dyn_ |= is && dynamic[x as usize];
            }
            dynamic[i.dst as usize] = dyn_;
            let d = i.dst;
            let support = self.support(d);
            if support == 0 || seen[d as usize] {
                continue;
            }
            let class = match op {
                Op::MemRead if p.mems.get(i.b as usize).is_some_and(|m| !m.writable) => {
                    Some(KeyClass::RomWord)
                }
                Op::Eq | Op::LtU | Op::ReduceOr | Op::ReduceAnd | Op::ReduceXor if !dyn_ => {
                    Some(KeyClass::Flag)
                }
                _ => None,
            };
            if let Some(class) = class {
                seen[d as usize] = true;
                out.push(Candidate {
                    slot: d,
                    class,
                    support,
                    prefix_ops: self.prefix(&[d]).0.instrs.len(),
                    values: self.values(p, d, class, support),
                });
            }
        }
        // Registers.
        let mut written = vec![false; n];
        let mut held = vec![false; n];
        for c in &body.commits {
            if let Commit::Copy { state, next } = *c {
                written[state as usize] = true;
                let w = self.writer[next as usize];
                if w != NONE {
                    let i = body.instrs[w as usize];
                    let (op, _) = self.op_rank[w as usize];
                    held[state as usize] = op == Op::Mux && (i.b == state || i.c == state);
                }
            }
        }
        let mut states: Vec<u32> = p
            .state_keys
            .iter()
            .filter_map(|k| match k.loc {
                StateLoc::Slot(s) => p.resolve(s).map(|r| r.0),
                StateLoc::Mem(_) => None,
            })
            .filter(|&s| (s as usize) < n)
            .collect();
        states.sort_unstable();
        states.dedup();
        for s in states {
            let support = self.support(s);
            if support == 0 || seen[s as usize] || (written[s as usize] && !held[s as usize]) {
                continue;
            }
            seen[s as usize] = true;
            out.push(Candidate {
                slot: s,
                class: KeyClass::Register,
                support,
                prefix_ops: 0,
                values: self.values(p, s, KeyClass::Register, support),
            });
        }
        out
    }

    /// The values a masked key on `slot` can take, when known statically.
    fn values(&self, p: &Program, slot: u32, class: KeyClass, support: u64) -> Option<Vec<u64>> {
        if class == KeyClass::RomWord {
            let w = self.writer[slot as usize];
            let m = &p.mems[self.body.instrs[w as usize].b as usize];
            let mut v: Vec<u64> = m.init.iter().map(|&x| x & support).collect();
            if m.init.len() < 1usize << m.addr_bits {
                v.push(0);
            }
            v.sort_unstable();
            v.dedup();
            return (v.len() <= ROM_VALUES).then_some(v);
        }
        if support.count_ones() > ENUM_BITS {
            return None;
        }
        // Every subset of the support.
        let mut v = vec![0];
        let mut sub = support;
        while sub != 0 {
            v.push(sub);
            sub = (sub - 1) & support;
        }
        v.sort_unstable();
        Some(v)
    }
}

/// Demanded bits per slot of `body`: `.0` what its commits read (every
/// bit), pulled back through each instruction to the bits of its
/// operands the demanded bits of its result depend on; `.1` the part of
/// that some *decision* reads: a select, a comparison, a reduction, a
/// carry, a memory address, or a bitwise `And`/`Or` (a known 0 or 1
/// there can make the other operand dead). Bits demanded only on their
/// way to a commit as data (a register's hold path, a value copied into
/// a state) are in `.0` but not in `.1`: knowing them folds nothing.
fn demanded(p: &Program, body: &Body, op_rank: &[(Op, u32)]) -> (Vec<u64>, Vec<u64>) {
    let n = p.slot_count as usize;
    let mut dm = vec![0u64; n];
    let mut dd = vec![0u64; n];
    for c in &body.commits {
        for s in c.reads() {
            dm[s as usize] = ALL;
        }
        // Whether a state copy changes anything reads the state too.
        if let Commit::Copy { state, .. } = *c {
            dm[state as usize] = ALL;
        }
    }
    for g in &body.guards {
        for c in &g.conds {
            dm[c.slot as usize] = ALL;
            dd[c.slot as usize] = ALL;
        }
    }
    let mut cst: FastMap<u32, u64> = FastMap::default();
    for &(s, v) in &p.consts {
        cst.insert(s, v);
    }
    cst.insert(ZERO_SLOT, 0);
    // Readers come after writers: walk the body backwards.
    for (idx, i) in body.instrs.iter().enumerate().rev() {
        let (op, _) = op_rank[idx];
        let imm = body.imms[idx];
        // Not masked by the possible bits: a reader observes them even
        // where the general body has a 0 there, and an alias need not.
        let d = dm[i.dst as usize];
        if d == 0 {
            continue;
        }
        let full = pull(op, i, imm, d, &cst);
        let decision: [bool; 3] = match op {
            Op::Mux | Op::Switch | Op::ReduceOr | Op::ReduceAnd | Op::ReduceXor | Op::MemRead => {
                [true, false, false]
            }
            Op::Eq | Op::LtU | Op::SrNext => [true, true, false],
            Op::AddCarry | Op::And | Op::Or | Op::Nand | Op::Nor => [true, true, true],
            _ => [false; 3],
        };
        let data = pull(op, i, imm, dd[i.dst as usize], &cst);
        let (sa, sb, sc) = op.slot_operands();
        for (k, (is, x)) in [(sa, i.a), (sb, i.b), (sc, i.c)].into_iter().enumerate() {
            if is {
                dm[x as usize] |= full[k];
                dd[x as usize] |= if decision[k] { full[k] } else { data[k] };
            }
        }
    }
    (dm, dd)
}

/// The demanded bits of an instruction's operands, given its result's.
fn pull(op: Op, i: &Instr, imm: u64, d: u64, cst: &FastMap<u32, u64>) -> [u64; 3] {
    if d == 0 {
        return [0; 3];
    }
    match op {
        Op::Copy => [d, 0, 0],
        Op::Not | Op::Nand | Op::Nor | Op::Xnor => [d & imm, d & imm, 0],
        Op::Xor => [d, d, 0],
        // A constant operand: `x & k` needs `x` only where `k` is 1,
        // `x | k` only where `k` is 0.
        Op::And => [
            d & cst.get(&i.b).copied().unwrap_or(ALL),
            d & cst.get(&i.a).copied().unwrap_or(ALL),
            0,
        ],
        Op::Or => [
            d & !cst.get(&i.b).copied().unwrap_or(0),
            d & !cst.get(&i.a).copied().unwrap_or(0),
            0,
        ],
        Op::Mux => [ALL, d, d],
        Op::Switch => [ALL, d, 0],
        Op::AddSum => {
            let l = low_mask(d);
            [l, l, l]
        }
        Op::AddCarry | Op::Eq | Op::LtU => [ALL, ALL, ALL],
        Op::ReduceOr | Op::ReduceAnd | Op::ReduceXor | Op::MemRead => [ALL, 0, 0],
        Op::Shr => [(d & imm).checked_shl(i.b).unwrap_or(0), 0, 0],
        Op::Concat => [d, d.checked_shr(i.c).unwrap_or(0), 0],
        Op::Sext => {
            let low = if i.b >= 64 { ALL } else { mask(i.b as u8) };
            let sign = if d & !low != 0 && (1..=64).contains(&i.b) {
                1u64 << (i.b - 1)
            } else {
                0
            };
            [d | sign, 0, 0]
        }
        Op::SrNext => [ALL, ALL, d],
        Op::Clock => [0, 0, 0],
    }
}

/// One field of a key: the bits `mask` of `slot`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeyField {
    pub slot: u32,
    pub mask: u64,
}

/// How key fields pack into one key word: field 0's masked bits in the
/// low bits (compressed, in bit order), then field 1's, and so on.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct KeyLayout {
    pub fields: Vec<KeyField>,
}

impl KeyLayout {
    pub fn new(fields: Vec<KeyField>) -> Self {
        KeyLayout { fields }
    }

    /// Bits in a key.
    pub fn width(&self) -> u32 {
        self.fields.iter().map(|f| f.mask.count_ones()).sum()
    }

    /// The key of the slot values `slots` (indexed by slot).
    pub fn pack(&self, slots: &[u64]) -> u64 {
        let mut key = 0u64;
        let mut at = 0;
        for f in &self.fields {
            key |= pext(slots[f.slot as usize], f.mask)
                .checked_shl(at)
                .unwrap_or(0);
            at += f.mask.count_ones();
        }
        key
    }

    /// The assumption a key value makes: each field's masked bits.
    pub fn assume(&self, key: u64) -> Vec<(u32, Known)> {
        let mut at = 0;
        self.fields
            .iter()
            .map(|f| {
                let v = pdep(key.checked_shr(at).unwrap_or(0), f.mask);
                at += f.mask.count_ones();
                (f.slot, Known::bits(f.mask, v))
            })
            .collect()
    }
}

/// The bits of `x` at the set bits of `m`, packed into the low bits.
pub fn pext(x: u64, m: u64) -> u64 {
    let (mut out, mut bit, mut m) = (0u64, 0u32, m);
    while m != 0 {
        let low = m.isolate_lowest_one();
        if x & low != 0 {
            out |= 1 << bit;
        }
        bit += 1;
        m &= m - 1;
    }
    out
}

/// The low bits of `x` spread to the set bits of `m` (inverse of
/// [`pext`]).
pub fn pdep(x: u64, m: u64) -> u64 {
    let (mut out, mut bit, mut m) = (0u64, 0u32, m);
    while m != 0 {
        let low = m.isolate_lowest_one();
        if (x >> bit) & 1 != 0 {
            out |= low;
        }
        bit += 1;
        m &= m - 1;
    }
    out
}

#[cfg(test)]
mod tests;
