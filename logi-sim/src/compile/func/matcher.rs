//! Function matching: fingerprint a flattened definition by random
//! simulation, match each output against the catalogue, then prove the
//! match before it may be used.
//!
//! # Catalogue
//!
//! Per output port, over the input ports (every role assignment, i.e.
//! every port permutation that fits the widths):
//! constants, pass-through and complement, the six 2-input bitwise ops,
//! `Mux` (any width, 1-bit select), `Add` sum or carry out (with or without
//! an inverted `b`, i.e. subtraction, and carry in 0, 1, a 1-bit port or
//! its complement), `Eq`, `LtU` and their complements. Bitwise forms rank
//! first, so word lifting can still group them.
//!
//! Sequential definitions (state elements built from gates, `Delay`s,
//! registers ...): each state element `k` must behave, tick by tick, as
//! `next = en ? d : state` with `en` always, a bit of an input port (a
//! 1-bit port, or one bit of a wider one, such as a decoded enable bus) or
//! its complement, and `d` a slice of an input port or of another state
//! element as wide as `k` (bit `k` of a `D` bus); each output bit must be
//! a state bit, an input bit, a complement of either or a constant. The
//! elements are then grouped into state words (`group`): elements with
//! the same enable loading consecutive slices of one port (or of one word
//! already formed) in order, and read by the outputs only in that order,
//! become one `Register` (or `Delay`) of their combined width, each
//! element keeping its identity as a bit field of it. Eight 1-bit
//! registers on bits of `D` become one 8-bit register; a word of
//! master-slave flip-flops one master word and one slave word.
//!
//! # Proofs
//!
//! A [`Prover`] decides a candidate for good. Two exist:
//! - [`Exhaustive`]: every input (and state) combination, up to
//!   [`EXHAUSTIVE_BITS`] bits;
//! - [`Sliced`]: per output bit, the structural support of that bit in the
//!   definition plus the candidate's own support, enumerated exhaustively
//!   with every other input held at 0. Both functions depend on no other
//!   input bit, so agreeing there is agreeing everywhere. This proves wide
//!   bit-sliced chips (a 16-bit mux) but not wide carry chains.
//!
//! Sequential matches have their own two ([`SeqProver`]):
//! [`SeqExhaustive`] (every input and state combination, small
//! definitions) and [`SeqSliced`]: per state element bit and output bit,
//! the structural support of the circuit's next value (output) bit, with
//! state values as leaves, plus the candidate's own support (its state
//! bit, enable bit and data bit), enumerated with every other input and
//! state bit held at 0. A register bit's support is three bits however
//! wide the register, so 16- and 32-bit registers and register files
//! prove in a few evaluations per bit.
//!
//! A SAT or BDD prover would implement the same traits.

use smallvec::SmallVec;

use super::flat::Flat;
use super::replace::{
    BITWISE, Base, En, Field, Fun, Replacement, Run, Src, Word, lower_comb, lower_seq,
};
use crate::netlist::{NodeKind, mask};

/// Most input (plus state) bits proven by enumeration.
pub(crate) const EXHAUSTIVE_BITS: u32 = 20;

/// Most undecided candidate combinations tried per definition.
const MAX_ATTEMPTS: usize = 64;

/// Random fingerprint vectors (plus all-zero and all-one).
const SAMPLES: usize = 64;

/// How a match was proven.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Proof {
    /// Every combination of this many input and state bits.
    Exhaustive { bits: u32 },
    /// Per output bit, over at most this many support bits.
    Sliced { max_support: u32 },
    /// A carry chain of this many adders.
    Chained { adders: u32 },
    /// A truth table of every combination of this many input bits.
    Table { bits: u32 },
}

/// The result of checking one assignment of candidates.
pub(crate) enum Verdict {
    Proven(Proof),
    /// Output `j`'s candidate is wrong.
    Refuted(usize),
    /// The prover cannot decide.
    Unknown,
}

/// A way to prove a combinational match (`funs[j]` for output `j`).
pub(crate) trait Prover {
    fn prove(&self, flat: &Flat, funs: &[Fun]) -> Verdict;
}

pub(crate) struct Exhaustive;
pub(crate) struct Sliced;
/// Composition: an output that is a carry chain of proven adders (word
/// `Add` steps, e.g. matched 4-bit adder instances) over consecutive
/// slices of two ports, merged in order, is their wide sum (and the last
/// carry its carry out). Exact by the definition of addition; no
/// enumeration.
pub(crate) struct Chained;

struct Ports {
    wi: Vec<u8>,
    wo: Vec<u8>,
    off: Vec<u32>,
    bits: u32,
}

impl Ports {
    fn new(flat: &Flat) -> Self {
        let wi: Vec<u8> = flat
            .inputs
            .iter()
            .map(|&v| flat.widths[v as usize])
            .collect();
        let wo: Vec<u8> = flat
            .outputs
            .iter()
            .map(|&v| flat.widths[v as usize])
            .collect();
        let mut off = Vec::with_capacity(wi.len());
        let mut bits = 0u32;
        for &w in &wi {
            off.push(bits);
            bits += w as u32;
        }
        Ports { wi, wo, off, bits }
    }

    /// Port values of a bit vector over the concatenated ports.
    fn split(&self, x: u128, ins: &mut [u64]) {
        for (p, v) in ins.iter_mut().enumerate() {
            *v = (x >> self.off[p]) as u64 & mask(self.wi[p]);
        }
    }
}

impl Prover for Exhaustive {
    fn prove(&self, flat: &Flat, funs: &[Fun]) -> Verdict {
        let ports = Ports::new(flat);
        if ports.bits > EXHAUSTIVE_BITS {
            return Verdict::Unknown;
        }
        let mut vals = vec![0u64; flat.values()];
        let mut ins = vec![0u64; ports.wi.len()];
        for x in 0..1u128 << ports.bits {
            ports.split(x, &mut ins);
            flat.eval(&ins, &[], &mut vals);
            for (j, f) in funs.iter().enumerate() {
                if flat.output(&vals, j) != f.eval(&ins, &ports.wi, ports.wo[j]) {
                    return Verdict::Refuted(j);
                }
            }
        }
        Verdict::Proven(Proof::Exhaustive { bits: ports.bits })
    }
}

impl Prover for Sliced {
    fn prove(&self, flat: &Flat, funs: &[Fun]) -> Verdict {
        let ports = Ports::new(flat);
        if ports.bits > 128 {
            return Verdict::Unknown;
        }
        let Some(support) = bit_support(flat, &ports) else {
            return Verdict::Unknown;
        };
        let mut vals = vec![0u64; flat.values()];
        let mut ins = vec![0u64; ports.wi.len()];
        let mut max_support = 0;
        for (j, f) in funs.iter().enumerate() {
            let out = &support[flat.outputs[j] as usize];
            for bit in 0..ports.wo[j] {
                let s = out.get(bit as usize).copied().unwrap_or(0)
                    | f.support(bit, &ports.off, &ports.wi);
                let n = s.count_ones();
                if n > EXHAUSTIVE_BITS {
                    return Verdict::Unknown;
                }
                max_support = max_support.max(n);
                let positions: SmallVec<[u32; 24]> =
                    (0..128).filter(|&b| s >> b & 1 == 1).collect();
                for k in 0..1u64 << n {
                    let mut x = 0u128;
                    for (q, &b) in positions.iter().enumerate() {
                        x |= (((k >> q) & 1) as u128) << b;
                    }
                    ports.split(x, &mut ins);
                    flat.eval(&ins, &[], &mut vals);
                    let got = flat.output(&vals, j) >> bit & 1;
                    let want = f.eval(&ins, &ports.wi, ports.wo[j]) >> bit & 1;
                    if got != want {
                        return Verdict::Refuted(j);
                    }
                }
            }
        }
        Verdict::Proven(Proof::Sliced { max_support })
    }
}

impl Prover for Chained {
    fn prove(&self, flat: &Flat, funs: &[Fun]) -> Verdict {
        let mut producer = vec![u32::MAX; flat.values()];
        for (k, st) in flat.steps.iter().enumerate() {
            for &o in &st.outs {
                producer[o as usize] = k as u32;
            }
        }
        let step = |v: u32| flat.steps.get(producer[v as usize] as usize);
        let width = |v: u32| flat.widths[v as usize];
        // Is `v` bits `lo..lo + len` of input port `p`?
        let slice_of = |v: u32, p: u16, lo: u8, len: u8| {
            let port = flat.inputs[p as usize];
            if v == port {
                return lo == 0 && len == width(port);
            }
            matches!(step(v), Some(st) if st.ins[0] == port
                && st.kind == NodeKind::Split { w: width(port), lo, len })
        };
        let mut adders = 0;
        for (j, f) in funs.iter().enumerate() {
            let Fun::Add {
                a,
                b,
                inv_b: false,
                cin,
                carry,
            } = *f
            else {
                return Verdict::Unknown;
            };
            let mut c = match cin {
                Src::Const(0) => super::flat::ZERO,
                Src::In(p) => flat.inputs[p as usize],
                _ => return Verdict::Unknown,
            };
            let w = width(flat.inputs[a as usize]);
            let mut pos = 0u8;
            let mut sums = Vec::new();
            while pos < w {
                // The adder whose carry in is `c` and whose operands are
                // the next slices of `a` and `b`.
                let next = flat.steps.iter().find(|st| {
                    matches!(st.kind, NodeKind::Add { w: len }
                        if st.ins[2] == c
                            && pos + len <= w
                            && slice_of(st.ins[0], a, pos, len)
                            && slice_of(st.ins[1], b, pos, len))
                });
                let Some(st) = next else {
                    return Verdict::Unknown;
                };
                let NodeKind::Add { w: len } = st.kind else {
                    unreachable!()
                };
                sums.push((st.outs[0], len));
                c = st.outs[1];
                pos += len;
            }
            adders += sums.len() as u32;
            let out = flat.outputs[j];
            let ok = if carry {
                out == c
            } else if sums.len() == 1 {
                out == sums[0].0
            } else {
                matches!(step(out), Some(st) if matches!(&st.kind, NodeKind::Merge { widths }
                    if widths.len() == sums.len()
                        && widths.iter().zip(&sums).all(|(&w, &(_, l))| w == l))
                    && st.ins.iter().zip(&sums).all(|(&i, &(s, _))| i == s))
            };
            if !ok {
                return Verdict::Unknown;
            }
        }
        Verdict::Proven(Proof::Chained { adders })
    }
}

/// A set of leaf bits (input port bits, and for a flat with state, state
/// bits), as words of 64.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct BitSet(SmallVec<[u64; 2]>);

impl BitSet {
    fn new(words: usize) -> Self {
        BitSet(smallvec::smallvec![0; words])
    }
    fn one(words: usize, b: u32) -> Self {
        let mut s = Self::new(words);
        s.insert(b);
        s
    }
    fn insert(&mut self, b: u32) {
        self.0[(b / 64) as usize] |= 1 << (b % 64);
    }
    fn union(&mut self, o: &BitSet) {
        for (a, b) in self.0.iter_mut().zip(&o.0) {
            *a |= b;
        }
    }
    fn len(&self) -> u32 {
        self.0.iter().map(|w| w.count_ones()).sum()
    }
    /// The members, ascending.
    fn members(&self) -> impl Iterator<Item = u32> + '_ {
        self.0.iter().enumerate().flat_map(|(k, &w)| {
            (0..64u32)
                .filter(move |&b| w >> b & 1 == 1)
                .map(move |b| k as u32 * 64 + b)
        })
    }
}

/// Per value and bit, the leaf bits it structurally depends on (an
/// over-approximation of its semantic support). `leaves` gives the first
/// leaf bit of each leaf value (input ports, state values); `nbits` is the
/// number of leaf bits.
fn support_sets(flat: &Flat, leaves: &[(u32, u32)], nbits: u32) -> Vec<Vec<BitSet>> {
    let words = nbits.div_ceil(64).max(1) as usize;
    let mut sup: Vec<Vec<BitSet>> = flat
        .widths
        .iter()
        .map(|&w| vec![BitSet::new(words); w as usize])
        .collect();
    for &(v, first) in leaves {
        for (i, b) in sup[v as usize].iter_mut().enumerate() {
            *b = BitSet::one(words, first + i as u32);
        }
    }
    let empty = BitSet::new(words);
    let all = |sup: &Vec<Vec<BitSet>>, v: u32| {
        let mut s = BitSet::new(words);
        for b in &sup[v as usize] {
            s.union(b);
        }
        s
    };
    let bit = |sup: &Vec<Vec<BitSet>>, v: u32, i: usize| -> BitSet {
        sup[v as usize]
            .get(i)
            .cloned()
            .unwrap_or_else(|| empty.clone())
    };
    let or = |mut a: BitSet, b: &BitSet| {
        a.union(b);
        a
    };
    for step in &flat.steps {
        use NodeKind::*;
        let ins = &step.ins;
        let outs = &step.outs;
        let w0 = flat.widths[outs[0] as usize] as usize;
        let mut res: Vec<Vec<BitSet>> = match &step.kind {
            Const { .. } => vec![vec![empty.clone(); w0]],
            Not { .. } => vec![sup[ins[0] as usize].clone()],
            And { .. } | Or { .. } | Xor { .. } | Nand { .. } | Nor { .. } | Xnor { .. } => {
                vec![
                    (0..w0)
                        .map(|i| or(bit(&sup, ins[0], i), &bit(&sup, ins[1], i)))
                        .collect(),
                ]
            }
            Mux { .. } => {
                let s = all(&sup, ins[0]);
                vec![
                    (0..w0)
                        .map(|i| or(or(s.clone(), &bit(&sup, ins[1], i)), &bit(&sup, ins[2], i)))
                        .collect(),
                ]
            }
            Switch { .. } => {
                let s = all(&sup, ins[0]);
                vec![
                    (0..w0)
                        .map(|i| or(s.clone(), &bit(&sup, ins[1], i)))
                        .collect(),
                ]
            }
            Add { w } => {
                let mut acc = all(&sup, ins[2]);
                let mut sum = Vec::with_capacity(*w as usize);
                for i in 0..*w as usize {
                    acc.union(&bit(&sup, ins[0], i));
                    acc.union(&bit(&sup, ins[1], i));
                    sum.push(acc.clone());
                }
                vec![sum, vec![acc]]
            }
            Split { lo, len, .. } => {
                vec![
                    (0..*len as usize)
                        .map(|i| bit(&sup, ins[0], *lo as usize + i))
                        .collect(),
                ]
            }
            Merge { widths } => {
                let mut v = Vec::with_capacity(w0);
                for (k, &w) in widths.iter().enumerate() {
                    for i in 0..w as usize {
                        v.push(bit(&sup, ins[k], i));
                    }
                }
                vec![v]
            }
            Extend { from, to, signed } => {
                let top = bit(&sup, ins[0], *from as usize - 1);
                vec![
                    (0..*to as usize)
                        .map(|i| {
                            if i < *from as usize {
                                bit(&sup, ins[0], i)
                            } else if *signed {
                                top.clone()
                            } else {
                                empty.clone()
                            }
                        })
                        .collect(),
                ]
            }
            _ => {
                let mut s = BitSet::new(words);
                for &v in ins {
                    s.union(&all(&sup, v));
                }
                outs.iter()
                    .map(|&o| vec![s.clone(); flat.widths[o as usize] as usize])
                    .collect()
            }
        };
        for (k, &o) in outs.iter().enumerate() {
            sup[o as usize] = std::mem::take(&mut res[k]);
        }
    }
    sup
}

/// Per value and bit, the input bits it structurally depends on. `None`
/// for a flat with state.
fn bit_support(flat: &Flat, ports: &Ports) -> Option<Vec<Vec<u128>>> {
    if !flat.states.is_empty() {
        return None;
    }
    let leaves: Vec<(u32, u32)> = flat
        .inputs
        .iter()
        .zip(&ports.off)
        .map(|(&v, &o)| (v, o))
        .collect();
    let sets = support_sets(flat, &leaves, ports.bits);
    Some(
        sets.into_iter()
            .map(|bits| {
                bits.into_iter()
                    .map(|b| {
                        b.0.iter()
                            .take(2)
                            .enumerate()
                            .fold(0u128, |a, (k, &w)| a | (w as u128) << (64 * k))
                    })
                    .collect()
            })
            .collect(),
    )
}

/// A small deterministic generator for fingerprints.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

/// Sample input vectors: all zero, all one, then random.
fn samples(wi: &[u8], n: usize) -> Vec<Vec<u64>> {
    let mut rng = Rng(0x2545_F491_4F6C_DD1D);
    let mut out = vec![
        vec![0; wi.len()],
        wi.iter().map(|&w| mask(w)).collect::<Vec<_>>(),
    ];
    for _ in 0..n {
        out.push(wi.iter().map(|&w| rng.next() & mask(w)).collect());
    }
    out
}

/// Every catalogue candidate for output `j` of width `w`.
fn candidates(wi: &[u8], w: u8, constant: Option<u64>) -> Vec<Fun> {
    let mut v = Vec::new();
    let n = wi.len() as u16;
    if let Some(c) = constant {
        v.push(Fun::Src(Src::Const(c)));
    }
    let same = |p: u16| wi[p as usize] == w;
    for p in 0..n {
        if same(p) {
            v.push(Fun::Src(Src::In(p)));
            v.push(Fun::Src(Src::NotIn(p)));
        }
    }
    for a in 0..n {
        for b in a + 1..n {
            if same(a) && same(b) {
                for op in 0..BITWISE.len() as u8 {
                    v.push(Fun::Bit(op, a, b));
                }
            }
        }
    }
    for sel in 0..n {
        if wi[sel as usize] != 1 {
            continue;
        }
        for a in 0..n {
            for b in 0..n {
                if a != b && a != sel && b != sel && same(a) && same(b) {
                    v.push(Fun::Mux { sel, a, b });
                }
            }
        }
    }
    let mut cins = vec![Src::Const(0), Src::Const(1)];
    for c in 0..n {
        if wi[c as usize] == 1 {
            cins.push(Src::In(c));
            cins.push(Src::NotIn(c));
        }
    }
    for a in 0..n {
        for b in 0..n {
            if a == b || wi[a as usize] != wi[b as usize] {
                continue;
            }
            let wa = wi[a as usize];
            for carry in [false, true] {
                if (carry && w != 1) || (!carry && wa != w) {
                    continue;
                }
                for inv_b in [false, true] {
                    if !inv_b && b < a {
                        continue;
                    }
                    for &cin in &cins {
                        if matches!(cin, Src::In(c) | Src::NotIn(c) if c == a || c == b) {
                            continue;
                        }
                        v.push(Fun::Add {
                            a,
                            b,
                            inv_b,
                            cin,
                            carry,
                        });
                    }
                }
            }
            if w == 1 {
                for neg in [false, true] {
                    if a < b {
                        v.push(Fun::Eq { a, b, neg });
                    }
                    v.push(Fun::LtU { a, b, neg });
                }
            }
        }
    }
    v.sort_by_key(Fun::rank);
    v
}

/// Match a stateless flat. `gate_cost` is the flat's own instruction
/// estimate; a match must be cheaper.
pub(crate) fn match_comb(flat: &Flat, gate_cost: usize) -> Result<(Replacement, Proof), String> {
    let ports = Ports::new(flat);
    if flat.outputs.is_empty() {
        return Err("no outputs".into());
    }
    let vecs = samples(&ports.wi, SAMPLES);
    let mut vals = vec![0u64; flat.values()];
    let outs: Vec<Vec<u64>> = vecs
        .iter()
        .map(|ins| {
            flat.eval(ins, &[], &mut vals);
            (0..flat.outputs.len())
                .map(|j| flat.output(&vals, j))
                .collect()
        })
        .collect();
    let mut survivors: Vec<Vec<Fun>> = Vec::new();
    for j in 0..flat.outputs.len() {
        let w = ports.wo[j];
        let c0 = outs[0][j];
        let constant = outs.iter().all(|o| o[j] == c0).then_some(c0);
        let s: Vec<Fun> = candidates(&ports.wi, w, constant)
            .into_iter()
            .filter(|f| {
                vecs.iter()
                    .zip(&outs)
                    .all(|(ins, o)| f.eval(ins, &ports.wi, w) == o[j])
            })
            .collect();
        if s.is_empty() {
            return Err(format!("output {j}: no catalogue function fits"));
        }
        survivors.push(s);
    }
    let mut pick = vec![0usize; survivors.len()];
    let mut attempts = 0;
    // Slices first: far fewer cases whenever output bits have small
    // supports (a mux, a bitwise op, a 4-bit adder's low bits).
    let provers: [&dyn Prover; 3] = [&Sliced, &Exhaustive, &Chained];
    loop {
        let funs: Vec<Fun> = pick.iter().zip(&survivors).map(|(&k, s)| s[k]).collect();
        let mut refuted = None;
        let mut proof = None;
        for p in provers {
            match p.prove(flat, &funs) {
                Verdict::Proven(pr) => {
                    proof = Some(pr);
                    break;
                }
                Verdict::Refuted(j) => {
                    refuted = Some(j);
                    break;
                }
                Verdict::Unknown => {}
            }
        }
        if let Some(pr) = proof {
            let r = lower_comb(&funs, &ports.wi, &ports.wo);
            if r.cost() >= gate_cost {
                return Err(format!("no cheaper: {} vs {gate_cost}", r.cost()));
            }
            return Ok((r, pr));
        }
        match refuted {
            Some(j) => {
                pick[j] += 1;
                if pick[j] == survivors[j].len() {
                    return Err(format!("output {j}: every fingerprint match refuted"));
                }
            }
            None => {
                // Undecided: try the next combination of survivors (a
                // bounded odometer), since another may be provable.
                attempts += 1;
                let mut j = 0;
                loop {
                    if j == pick.len() || attempts > MAX_ATTEMPTS {
                        return Err(format!("unproven: {funs:?}"));
                    }
                    pick[j] += 1;
                    if pick[j] < survivors[j].len() {
                        break;
                    }
                    pick[j] = 0;
                    j += 1;
                }
            }
        }
    }
}

/// Most input and state bits the sequential exhaustive prover enumerates
/// (small definitions: a latch, a flip-flop); larger ones are proven per
/// slice.
pub(crate) const SEQ_EXHAUSTIVE_BITS: u32 = 10;

/// Where a state element's data comes from (element terms): an input port
/// or another state element.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) enum ElBase {
    Port(u16),
    State(u16),
}

/// A candidate for one state element of width `w`: `next = en ? d :
/// state`, `d` bits `lo..lo + w` of `base`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ElemSeq {
    pub en: En,
    pub base: ElBase,
    pub lo: u8,
}

/// A candidate for one output bit: a constant, a bit of an input port or
/// of a state element, or a complement of either.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OBit {
    Const(bool),
    Port { p: u16, i: u8, neg: bool },
    State { k: u16, i: u8, neg: bool },
}

/// The shape of a flat with state: its ports, its state elements and its
/// leaf bits (input port bits, then state bits).
pub(crate) struct SeqShape {
    ports: Ports,
    ws: Vec<u8>,
    /// First leaf bit of each state element.
    soff: Vec<u32>,
    /// Output port and bit of each output bit item.
    obits: Vec<(u16, u8)>,
    /// What each leaf bit is: `(state?, port or element, bit)`.
    leaf: Vec<(bool, u16, u8)>,
}

impl SeqShape {
    fn new(flat: &Flat) -> Self {
        let ports = Ports::new(flat);
        let ws: Vec<u8> = flat.states.iter().map(|s| s.width).collect();
        let mut leaf = Vec::new();
        for (p, &w) in ports.wi.iter().enumerate() {
            leaf.extend((0..w).map(|i| (false, p as u16, i)));
        }
        let mut soff = Vec::with_capacity(ws.len());
        for (k, &w) in ws.iter().enumerate() {
            soff.push(leaf.len() as u32);
            leaf.extend((0..w).map(|i| (true, k as u16, i)));
        }
        let obits = ports
            .wo
            .iter()
            .enumerate()
            .flat_map(|(j, &w)| (0..w).map(move |b| (j as u16, b)))
            .collect();
        SeqShape {
            ports,
            ws,
            soff,
            obits,
            leaf,
        }
    }

    fn bits(&self) -> u32 {
        self.leaf.len() as u32
    }

    fn en(&self, en: En, ins: &[u64]) -> bool {
        match en {
            En::Always => true,
            En::Bit { port, bit, neg } => (ins[port as usize] >> bit & 1 == 1) != neg,
        }
    }

    /// The candidate's next value of element `k`.
    fn want(&self, k: usize, c: ElemSeq, ins: &[u64], st: &[u64]) -> u64 {
        if !self.en(c.en, ins) {
            return st[k];
        }
        let src = match c.base {
            ElBase::Port(p) => ins[p as usize],
            ElBase::State(m) => st[m as usize],
        };
        (src >> c.lo) & mask(self.ws[k])
    }

    fn obit(&self, c: OBit, ins: &[u64], st: &[u64]) -> u64 {
        match c {
            OBit::Const(v) => v as u64,
            OBit::Port { p, i, neg } => (ins[p as usize] >> i & 1) ^ neg as u64,
            OBit::State { k, i, neg } => (st[k as usize] >> i & 1) ^ neg as u64,
        }
    }

    /// The leaf bits the candidate for bit `i` of element `k` reads.
    fn elem_support(&self, k: usize, i: u8, c: ElemSeq, s: &mut BitSet) {
        s.insert(self.soff[k] + i as u32);
        if let En::Bit { port, bit, .. } = c.en {
            s.insert(self.ports.off[port as usize] + bit as u32);
        }
        s.insert(match c.base {
            ElBase::Port(p) => self.ports.off[p as usize] + (c.lo + i) as u32,
            ElBase::State(m) => self.soff[m as usize] + (c.lo + i) as u32,
        });
    }

    fn obit_support(&self, c: OBit, s: &mut BitSet) {
        match c {
            OBit::Const(_) => {}
            OBit::Port { p, i, .. } => s.insert(self.ports.off[p as usize] + i as u32),
            OBit::State { k, i, .. } => s.insert(self.soff[k as usize] + i as u32),
        }
    }

    /// Port and state values with the leaf bits `bits` set and every
    /// other bit 0.
    fn assign(&self, bits: impl Iterator<Item = u32>, ins: &mut [u64], st: &mut [u64]) {
        ins.fill(0);
        st.fill(0);
        for b in bits {
            let (state, x, i) = self.leaf[b as usize];
            let v = if state {
                &mut st[x as usize]
            } else {
                &mut ins[x as usize]
            };
            *v |= 1 << i;
        }
    }
}

/// A way to prove a sequential match: `elems[k]` for state element `k`
/// and `obits[b]` for output bit item `b`. Items are numbered state
/// elements first, then output bits (`Verdict::Refuted`).
pub(crate) trait SeqProver {
    fn prove(&self, flat: &Flat, shape: &SeqShape, elems: &[ElemSeq], obits: &[OBit]) -> Verdict;
}

/// Whether the circuit (its values) and a candidate (from the input and
/// state values) agree on one assignment.
type Agree<'a> = dyn Fn(&[u64], &[u64], &[u64]) -> bool + 'a;

/// Every combination of input and state bits, up to `max_bits`.
pub(crate) struct SeqExhaustive {
    max_bits: u32,
}

/// Per state element bit and output bit, the structural support of the
/// circuit's next value (output) bit plus the candidate's own support,
/// enumerated exhaustively with every other input and state bit held at 0.
/// Neither function depends on any other bit, so agreeing there is
/// agreeing everywhere. A register's bit has a support of three bits (its
/// state, its enable, its data), however wide the register.
pub(crate) struct SeqSliced;

impl SeqProver for SeqExhaustive {
    fn prove(&self, flat: &Flat, shape: &SeqShape, elems: &[ElemSeq], obits: &[OBit]) -> Verdict {
        let bits = shape.bits();
        if bits > self.max_bits {
            return Verdict::Unknown;
        }
        let ns = elems.len();
        let mut vals = vec![0u64; flat.values()];
        let mut ins = vec![0u64; shape.ports.wi.len()];
        let mut st = vec![0u64; ns];
        for x in 0..1u64 << bits {
            shape.assign((0..bits).filter(|&b| x >> b & 1 == 1), &mut ins, &mut st);
            flat.eval(&ins, &st, &mut vals);
            for (k, &c) in elems.iter().enumerate() {
                if flat.next(&vals, k) != shape.want(k, c, &ins, &st) {
                    return Verdict::Refuted(k);
                }
            }
            for (b, &c) in obits.iter().enumerate() {
                let (j, i) = shape.obits[b];
                if flat.output(&vals, j as usize) >> i & 1 != shape.obit(c, &ins, &st) {
                    return Verdict::Refuted(ns + b);
                }
            }
        }
        Verdict::Proven(Proof::Exhaustive { bits })
    }
}

impl SeqProver for SeqSliced {
    fn prove(&self, flat: &Flat, shape: &SeqShape, elems: &[ElemSeq], obits: &[OBit]) -> Verdict {
        let mut leaves: Vec<(u32, u32)> = flat
            .inputs
            .iter()
            .zip(&shape.ports.off)
            .map(|(&v, &o)| (v, o))
            .collect();
        leaves.extend(
            flat.states
                .iter()
                .zip(&shape.soff)
                .map(|(s, &o)| (s.value, o)),
        );
        let sup = support_sets(flat, &leaves, shape.bits());
        let words = shape.bits().div_ceil(64).max(1) as usize;
        let ns = elems.len();
        let mut vals = vec![0u64; flat.values()];
        let mut ins = vec![0u64; shape.ports.wi.len()];
        let mut st = vec![0u64; ns];
        let mut max_support = 0;
        // Enumerate every assignment of `s`; `check` says whether the
        // circuit and the candidate agree on it.
        let mut each = |s: &BitSet, vals: &mut Vec<u64>, check: &Agree<'_>| -> Option<bool> {
            let n = s.len();
            if n > EXHAUSTIVE_BITS {
                return None;
            }
            max_support = max_support.max(n);
            let positions: SmallVec<[u32; 24]> = s.members().collect();
            for x in 0..1u64 << n {
                shape.assign(
                    positions
                        .iter()
                        .enumerate()
                        .filter(|&(q, _)| x >> q & 1 == 1)
                        .map(|(_, &b)| b),
                    &mut ins,
                    &mut st,
                );
                flat.eval(&ins, &st, vals);
                if !check(vals, &ins, &st) {
                    return Some(false);
                }
            }
            Some(true)
        };
        for (k, &c) in elems.iter().enumerate() {
            let el = &flat.states[k];
            for i in 0..shape.ws[k] {
                // What the circuit's next value bit `i` can read.
                let mut s = BitSet::new(words);
                let bit = |v: u32, s: &mut BitSet| {
                    if let Some(b) = sup[v as usize].get(i as usize) {
                        s.union(b);
                    }
                };
                let all = |v: u32, s: &mut BitSet| {
                    for b in &sup[v as usize] {
                        s.union(b);
                    }
                };
                match el.kind {
                    NodeKind::Delay { .. } => bit(el.ins[0], &mut s),
                    NodeKind::Register { .. } | NodeKind::DLatch { .. } => {
                        all(el.ins[0], &mut s);
                        bit(el.ins[1], &mut s);
                        s.insert(shape.soff[k] + i as u32);
                    }
                    _ => {
                        for &v in &el.ins {
                            all(v, &mut s);
                        }
                        s.insert(shape.soff[k] + i as u32);
                    }
                }
                shape.elem_support(k, i, c, &mut s);
                let ok = each(&s, &mut vals, &|vals, ins, st| {
                    flat.next(vals, k) >> i & 1 == shape.want(k, c, ins, st) >> i & 1
                });
                match ok {
                    None => return Verdict::Unknown,
                    Some(false) => return Verdict::Refuted(k),
                    Some(true) => {}
                }
            }
        }
        for (b, &c) in obits.iter().enumerate() {
            let (j, i) = shape.obits[b];
            let out = flat.outputs[j as usize];
            let mut s = sup[out as usize]
                .get(i as usize)
                .cloned()
                .unwrap_or_else(|| BitSet::new(words));
            shape.obit_support(c, &mut s);
            let ok = each(&s, &mut vals, &|vals, ins, st| {
                flat.output(vals, j as usize) >> i & 1 == shape.obit(c, ins, st)
            });
            match ok {
                None => return Verdict::Unknown,
                Some(false) => return Verdict::Refuted(ns + b),
                Some(true) => {}
            }
        }
        Verdict::Proven(Proof::Sliced { max_support })
    }
}

/// Fingerprint results evaluated so far (see `match_seq`).
struct Fingerprints {
    vals: Vec<u64>,
    nexts: Vec<Vec<u64>>,
    outs: Vec<Vec<u64>>,
}

impl Fingerprints {
    /// Next values and outputs at sample `x`, evaluating samples up to it.
    fn at(&mut self, flat: &Flat, vecs: &[Vec<u64>], nwi: usize, x: usize) -> (&[u64], &[u64]) {
        while self.nexts.len() <= x {
            let (ins, st) = vecs[self.nexts.len()].split_at(nwi);
            flat.eval(ins, st, &mut self.vals);
            self.nexts
                .push((0..st.len()).map(|k| flat.next(&self.vals, k)).collect());
            self.outs.push(
                (0..flat.outputs.len())
                    .map(|j| flat.output(&self.vals, j))
                    .collect(),
            );
        }
        (&self.nexts[x], &self.outs[x])
    }
}

/// An enable, for reasons: `In(1)`, `!In(1)`, `In(2)[3]`.
fn en_text(en: En, wi: &[u8]) -> String {
    match en {
        En::Always => "always".into(),
        En::Bit { port, bit, neg } => {
            super::replace::slice_text(Base::Port(port), bit, 1, wi[port as usize], neg)
        }
    }
}

/// A state element's kind and width, for reasons.
fn elem_text(flat: &Flat, k: usize) -> String {
    let s = &flat.states[k];
    let kind = match s.kind {
        NodeKind::Delay { .. } => "Delay",
        NodeKind::Register { .. } => "Register",
        NodeKind::DLatch { .. } => "DLatch",
        NodeKind::SrLatch => "SrLatch",
        _ => "state",
    };
    format!("state element {k} ({}-bit {kind})", s.width)
}

/// Match a flat with state, tick by tick (see the module docs): find each
/// state element's `en ? d : state` and each output bit's source by
/// fingerprints, prove them, then group the elements into state words.
pub(crate) fn match_seq(flat: &Flat) -> Result<(Replacement, Proof), String> {
    let shape = SeqShape::new(flat);
    let ws = &shape.ws;
    let ns = ws.len();
    let wi = &shape.ports.wi;
    let ni = wi.len() as u16;
    // Fingerprints: random input and state values (plus all 0, all 1),
    // evaluated as the checks first need them (checks stop at the first
    // sample that refutes a candidate, so a definition that is no register
    // usually needs only a few).
    let mut widths: Vec<u8> = wi.clone();
    widths.extend_from_slice(ws);
    let vecs = samples(&widths, SAMPLES);
    let nwi = wi.len();
    fn cut(v: &[u64], n: usize) -> (&[u64], &[u64]) {
        v.split_at(n)
    }
    let lazy = std::cell::RefCell::new(Fingerprints {
        vals: vec![0u64; flat.values()],
        nexts: Vec::with_capacity(vecs.len()),
        outs: Vec::with_capacity(vecs.len()),
    });
    // The next value of every element at sample `x`.
    let next_at = |x: usize, k: usize| lazy.borrow_mut().at(flat, &vecs, nwi, x).0[k];
    let mut ens = vec![En::Always];
    for p in 0..ni {
        for bit in 0..wi[p as usize] {
            for neg in [false, true] {
                ens.push(En::Bit { port: p, bit, neg });
            }
        }
    }
    // Random samples first: all-0 and all-1 tell few candidates apart.
    let order: Vec<usize> = (2..vecs.len()).chain(0..2).collect();
    const QUICK_HOLDS: usize = 8;
    let mut elem_cands: Vec<Vec<ElemSeq>> = Vec::with_capacity(ns);
    for k in 0..ns {
        let w = ws[k];
        let bases: Vec<(ElBase, u8)> = (0..ni)
            .filter(|&p| wi[p as usize] >= w)
            .map(|p| (ElBase::Port(p), wi[p as usize]))
            .chain(
                (0..ns as u16)
                    .filter(|&m| m as usize != k && ws[m as usize] >= w)
                    .map(|m| (ElBase::State(m), ws[m as usize])),
            )
            .collect();
        let mut v = Vec::new();
        // An enable it held under until no data source was left.
        let mut held = None;
        for &en in &ens {
            // A quick look first: does it hold where the enable is off
            // (until it has been off a few times)?
            let mut off = 0;
            let plausible = order.iter().all(|&x| {
                if off >= QUICK_HOLDS {
                    return true;
                }
                let (ins, st) = cut(&vecs[x], nwi);
                if shape.en(en, ins) {
                    return true;
                }
                off += 1;
                next_at(x, k) == st[k]
            });
            if !plausible {
                continue;
            }
            let mut ds: Vec<ElemSeq> = bases
                .iter()
                .flat_map(|&(base, bw)| (0..=bw - w).map(move |lo| ElemSeq { en, base, lo }))
                .collect();
            // Sample by sample: where the enable is off the element holds,
            // where it is on it loads `d`.
            let mut holds = true;
            for &x in &order {
                if ds.is_empty() {
                    break;
                }
                let (ins, st) = cut(&vecs[x], nwi);
                let n = next_at(x, k);
                if !shape.en(en, ins) {
                    if n != st[k] {
                        holds = false;
                        break;
                    }
                    continue;
                }
                ds.retain(|&c| shape.want(k, c, ins, st) == n);
            }
            if holds && en != En::Always {
                held.get_or_insert(en);
            }
            if holds {
                v.extend(ds);
            }
        }
        if v.is_empty() {
            let what = elem_text(flat, k);
            return Err(match held {
                None => format!(
                    "{what}: not a register: no input bit (or its complement) enables it, \
                     and it is not a Delay of input or state bits"
                ),
                Some(en) => format!(
                    "{what}: not a register: enabled by {}, it loads no slice of an input \
                     or another state element",
                    en_text(en, wi)
                ),
            });
        }
        elem_cands.push(v);
    }
    let outs: Vec<Vec<u64>> = (0..vecs.len())
        .map(|x| lazy.borrow_mut().at(flat, &vecs, nwi, x).1.to_vec())
        .collect();
    // Candidates per output bit.
    let mut obit_cands: Vec<Vec<OBit>> = Vec::with_capacity(shape.obits.len());
    for &(j, i) in &shape.obits {
        let got = |o: &Vec<u64>| o[j as usize] >> i & 1;
        let c0 = got(&outs[0]);
        let mut all: Vec<OBit> = Vec::new();
        if outs.iter().all(|o| got(o) == c0) {
            all.push(OBit::Const(c0 == 1));
        }
        for neg in [false, true] {
            for k in 0..ns as u16 {
                for b in 0..ws[k as usize] {
                    all.push(OBit::State { k, i: b, neg });
                }
            }
            for p in 0..ni {
                for b in 0..wi[p as usize] {
                    all.push(OBit::Port { p, i: b, neg });
                }
            }
        }
        let v: Vec<OBit> = all
            .into_iter()
            .filter(|&c| {
                vecs.iter().zip(&outs).all(|(x, o)| {
                    let (ins, st) = cut(x, nwi);
                    shape.obit(c, ins, st) == got(o)
                })
            })
            .collect();
        if v.is_empty() {
            return Err(format!(
                "output {j} bit {i}: not a state bit, an input bit or a constant"
            ));
        }
        obit_cands.push(v);
    }
    // Prove: the first candidates, the next of any refuted.
    let item_text = |item: usize| {
        if item < ns {
            elem_text(flat, item)
        } else {
            let (j, i) = shape.obits[item - ns];
            format!("output {j} bit {i}")
        }
    };
    let mut pick = vec![0usize; ns + shape.obits.len()];
    let provers: [&dyn SeqProver; 2] = [
        &SeqExhaustive {
            max_bits: SEQ_EXHAUSTIVE_BITS,
        },
        &SeqSliced,
    ];
    let (elems, obits, proof) = loop {
        let elems: Vec<ElemSeq> = (0..ns).map(|k| elem_cands[k][pick[k]]).collect();
        let obits: Vec<OBit> = (0..shape.obits.len())
            .map(|b| obit_cands[b][pick[ns + b]])
            .collect();
        let mut verdict = Verdict::Unknown;
        for p in provers {
            verdict = p.prove(flat, &shape, &elems, &obits);
            if !matches!(verdict, Verdict::Unknown) {
                break;
            }
        }
        match verdict {
            Verdict::Proven(pr) => break (elems, obits, pr),
            Verdict::Refuted(item) => {
                pick[item] += 1;
                let n = if item < ns {
                    elem_cands[item].len()
                } else {
                    obit_cands[item - ns].len()
                };
                if pick[item] == n {
                    return Err(format!(
                        "{}: every fingerprint match refuted",
                        item_text(item)
                    ));
                }
            }
            Verdict::Unknown => {
                return Err(format!(
                    "unproven: a state or output bit depends on more than {EXHAUSTIVE_BITS} \
                     input and state bits"
                ));
            }
        }
    };
    let (words, runs) = group(&shape, &elems, &obits);
    let paths = flat
        .states
        .iter()
        .map(|s| (s.path.clone(), s.node))
        .collect();
    Ok((lower_seq(&words, paths, &runs, wi, &shape.ports.wo), proof))
}

/// Group proven state elements into state words, and output bits into
/// runs of them.
///
/// Elements with the same enable whose data are consecutive slices of one
/// input port, in order, form one word, as do elements with the same
/// enable whose data are consecutive slices of one word already formed
/// (the slaves of a word of master-slave flip-flops). Two neighbours stay
/// in one word only if no output reads them out of order: wherever an
/// output reads the last bit of the first, the next bit of that output (if
/// any) is the first bit of the second, with the same polarity, and
/// conversely. So a word is read only as a whole or a slice of it, and a
/// near miss (a crossed data bit, a reversed output, one bit on another
/// enable) splits into exact smaller words. Elements whose sources form a
/// cycle (a ring of states) become words of their own. Each word is at
/// most 64 bits.
fn group(shape: &SeqShape, elems: &[ElemSeq], obits: &[OBit]) -> (Vec<Word>, Vec<Vec<Run>>) {
    let ws = &shape.ws;
    let ns = elems.len();
    // What each output read of an element's last bit is followed by, and
    // each read of its first bit preceded by: `(polarity, neighbour)`.
    let mut after: Vec<Vec<(bool, Option<OBit>)>> = vec![Vec::new(); ns];
    let mut before: Vec<Vec<(bool, Option<OBit>)>> = vec![Vec::new(); ns];
    for (b, &c) in obits.iter().enumerate() {
        let OBit::State { k, i, neg } = c else {
            continue;
        };
        let j = shape.obits[b].0;
        let at = |d: isize| {
            let x = b as isize + d;
            (x >= 0 && (x as usize) < obits.len() && shape.obits[x as usize].0 == j)
                .then(|| obits[x as usize])
        };
        if i + 1 == ws[k as usize] {
            after[k as usize].push((neg, at(1)));
        }
        if i == 0 {
            before[k as usize].push((neg, at(-1)));
        }
    }
    let linkable = |e: usize, f: usize| {
        let last = ws[e] - 1;
        after[e].iter().all(|&(neg, n)| match n {
            None => true,
            Some(x) => {
                x == OBit::State {
                    k: f as u16,
                    i: 0,
                    neg,
                }
            }
        }) && before[f].iter().all(|&(neg, p)| match p {
            None => true,
            Some(x) => {
                x == OBit::State {
                    k: e as u16,
                    i: last,
                    neg,
                }
            }
        })
    };
    const NONE: u16 = u16::MAX;
    let mut gid = vec![NONE; ns];
    let mut off = vec![0u8; ns];
    let mut members: Vec<Vec<usize>> = Vec::new();
    // An element's data source in word terms, once its source is placed.
    let source = |k: usize, gid: &[u16], off: &[u8]| -> Option<(Base, u8)> {
        match elems[k].base {
            ElBase::Port(p) => Some((Base::Port(p), elems[k].lo)),
            ElBase::State(m) => {
                let g = gid[m as usize];
                (g != NONE).then(|| (Base::Word(g), off[m as usize] + elems[k].lo))
            }
        }
    };
    while gid.contains(&NONE) {
        let mut ready: Vec<(En, Base, u8, usize)> = (0..ns)
            .filter(|&k| gid[k] == NONE)
            .filter_map(|k| source(k, &gid, &off).map(|(b, lo)| (elems[k].en, b, lo, k)))
            .collect();
        if ready.is_empty() {
            // A cycle of state sources: break it with a word of its own.
            let k = gid.iter().position(|&g| g == NONE).expect("unplaced");
            gid[k] = members.len() as u16;
            members.push(vec![k]);
            continue;
        }
        ready.sort();
        let mut run: Vec<usize> = Vec::new();
        let mut width = 0u32;
        let mut prev: Option<(En, Base, u8, usize)> = None;
        let mut close = |run: &mut Vec<usize>, gid: &mut Vec<u16>, off: &mut Vec<u8>| {
            let g = members.len() as u16;
            let mut at = 0u8;
            for &k in run.iter() {
                gid[k] = g;
                off[k] = at;
                at += ws[k];
            }
            members.push(std::mem::take(run));
        };
        for &(en, base, lo, k) in &ready {
            let joins = prev.is_some_and(|(pen, pbase, plo, pk)| {
                pen == en
                    && pbase == base
                    && plo as u32 + ws[pk] as u32 == lo as u32
                    && width + ws[k] as u32 <= 64
                    && linkable(pk, k)
            });
            if !joins && !run.is_empty() {
                close(&mut run, &mut gid, &mut off);
                width = 0;
            }
            run.push(k);
            width += ws[k] as u32;
            prev = Some((en, base, lo, k));
        }
        close(&mut run, &mut gid, &mut off);
    }
    let words: Vec<Word> = members
        .iter()
        .map(|m| {
            let (base, lo) = source(m[0], &gid, &off).expect("placed");
            Word {
                en: elems[m[0]].en,
                base,
                lo,
                width: m.iter().map(|&k| ws[k]).sum(),
                fields: m
                    .iter()
                    .map(|&k| Field {
                        state: k as u16,
                        off: off[k],
                        width: ws[k],
                    })
                    .collect(),
            }
        })
        .collect();
    // Output bits as runs.
    let mut runs: Vec<Vec<Run>> = vec![Vec::new(); shape.ports.wo.len()];
    for (b, &c) in obits.iter().enumerate() {
        let j = shape.obits[b].0 as usize;
        let rs = &mut runs[j];
        let (base, bit, neg) = match c {
            OBit::Const(v) => {
                if let Some(Run::Const { len, value }) = rs.last_mut() {
                    *value |= (v as u64) << *len;
                    *len += 1;
                } else {
                    rs.push(Run::Const {
                        len: 1,
                        value: v as u64,
                    });
                }
                continue;
            }
            OBit::Port { p, i, neg } => (Base::Port(p), i, neg),
            OBit::State { k, i, neg } => (Base::Word(gid[k as usize]), off[k as usize] + i, neg),
        };
        match rs.last_mut() {
            Some(Run::Bits {
                base: b0,
                lo,
                len,
                neg: n0,
            }) if *b0 == base && *n0 == neg && *lo + *len == bit => *len += 1,
            _ => rs.push(Run::Bits {
                base,
                lo: bit,
                len: 1,
                neg,
            }),
        }
    }
    (words, runs)
}

/// Most input bits a truth table is built for (`2^12` words, 32 KiB).
pub(crate) const TABLE_BITS: u32 = 12;

/// Fewest input bits a truth table is built for. Smaller definitions
/// (2- to 4-bit decoders) are cheap as gates, and as gates they let word
/// lifting and state merging see through them: on the gate `cpu16`,
/// tabling its 2- to 4-bit decoders, which drive the register banks'
/// enables, blocks merging their registers (77 commits become 445, 335
/// instructions 1,102), while tabling its 5-bit decoder saves 33
/// instructions.
pub(crate) const MIN_TABLE_BITS: u32 = 5;

/// A stateless flat with [`MIN_TABLE_BITS`] to [`TABLE_BITS`] input bits
/// and at most 64
/// output bits as a truth table (issue #117, stage 4): the input ports
/// concatenated into an index (port 0 lowest), one ROM read, and one slice
/// per output port when there are several. Built by evaluating the flat
/// on every input combination, so it is exact by construction. `None` when
/// the flat is too wide, or the table would not cost fewer instructions
/// than `gate_cost` (counting the index concatenation and the output
/// slices).
pub(crate) fn table(flat: &Flat, gate_cost: usize) -> Option<(Replacement, Proof)> {
    use crate::compile::func::replace::Ref;
    use smallvec::smallvec;
    let ports = Ports::new(flat);
    let (nin, nout) = (ports.wi.len(), ports.wo.len());
    if !flat.states.is_empty()
        || nin == 0
        || nout == 0
        || !(MIN_TABLE_BITS..=TABLE_BITS).contains(&ports.bits)
    {
        return None;
    }
    let wsum: u32 = ports.wo.iter().map(|&w| w as u32).sum();
    if wsum > 64 {
        return None;
    }
    let cost = (nin - 1) + 1 + if nout > 1 { nout } else { 0 };
    if cost >= gate_cost {
        return None;
    }
    let mut contents = Vec::with_capacity(1 << ports.bits);
    let mut vals = vec![0u64; flat.values()];
    let mut ins = vec![0u64; nin];
    for x in 0..1u128 << ports.bits {
        ports.split(x, &mut ins);
        flat.eval(&ins, &[], &mut vals);
        let mut word = 0u64;
        let mut at = 0u32;
        for (j, &w) in ports.wo.iter().enumerate() {
            word |= (flat.output(&vals, j) & mask(w)) << at;
            at += w as u32;
        }
        contents.push(word);
    }
    let mut r = Replacement {
        table: true,
        desc: format!("table of {} input bits", ports.bits),
        ..Replacement::default()
    };
    let index = if nin == 1 {
        Ref::In(0)
    } else {
        r.temps.push(ports.bits as u8);
        let t = Ref::Temp((r.temps.len() - 1) as u16);
        r.nodes.push(super::replace::RNode {
            kind: NodeKind::Merge {
                widths: ports.wi.iter().copied().collect(),
            },
            ins: (0..nin as u16).map(Ref::In).collect(),
            outs: smallvec![t],
            state: None,
        });
        t
    };
    let word = if nout == 1 {
        Ref::Out(0)
    } else {
        r.temps.push(wsum as u8);
        Ref::Temp((r.temps.len() - 1) as u16)
    };
    r.nodes.push(super::replace::RNode {
        kind: NodeKind::Rom {
            addr_bits: ports.bits as u8,
            w: wsum as u8,
            contents: contents.into(),
        },
        ins: smallvec![index],
        outs: smallvec![word],
        state: None,
    });
    if nout > 1 {
        let mut lo = 0u8;
        for (j, &len) in ports.wo.iter().enumerate() {
            r.nodes.push(super::replace::RNode {
                kind: NodeKind::Split {
                    w: wsum as u8,
                    lo,
                    len,
                },
                ins: smallvec![word],
                outs: smallvec![Ref::Out(j as u16)],
                state: None,
            });
            lo += len;
        }
    }
    Some((r, Proof::Table { bits: ports.bits }))
}
