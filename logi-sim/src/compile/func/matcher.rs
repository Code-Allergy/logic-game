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
//! registers ...): each state element must behave, tick by tick, as
//! `next = en ? d : state` with `en` a 1-bit port, its complement or
//! always, and `d` an input port or another state element; each output
//! must be a state element, an input port, a complement of either or a
//! constant. Then the element becomes a `Register` (or `Delay`) with the
//! original element's identity.
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
//! A SAT or BDD prover would implement the same trait.

use smallvec::SmallVec;

use super::flat::Flat;
use super::replace::{BITWISE, Fun, Replacement, Seq, Src, lower_comb, lower_seq};
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

/// Per value and bit, the input bits it structurally depends on (an
/// over-approximation of its semantic support). `None` for a flat with
/// state.
fn bit_support(flat: &Flat, ports: &Ports) -> Option<Vec<Vec<u128>>> {
    if !flat.states.is_empty() {
        return None;
    }
    let mut sup: Vec<Vec<u128>> = flat
        .widths
        .iter()
        .map(|&w| vec![0u128; w as usize])
        .collect();
    for (p, &v) in flat.inputs.iter().enumerate() {
        for (i, b) in sup[v as usize].iter_mut().enumerate() {
            *b = 1u128 << (ports.off[p] + i as u32);
        }
    }
    let all = |sup: &Vec<Vec<u128>>, v: u32| sup[v as usize].iter().fold(0u128, |a, &b| a | b);
    let bit = |sup: &Vec<Vec<u128>>, v: u32, i: usize| sup[v as usize].get(i).copied().unwrap_or(0);
    for step in &flat.steps {
        use NodeKind::*;
        let ins = &step.ins;
        let outs = &step.outs;
        let w0 = flat.widths[outs[0] as usize] as usize;
        let mut res: Vec<Vec<u128>> = match &step.kind {
            Const { .. } => vec![vec![0; w0]],
            Not { .. } => vec![sup[ins[0] as usize].clone()],
            And { .. } | Or { .. } | Xor { .. } | Nand { .. } | Nor { .. } | Xnor { .. } => {
                vec![
                    (0..w0)
                        .map(|i| bit(&sup, ins[0], i) | bit(&sup, ins[1], i))
                        .collect(),
                ]
            }
            Mux { .. } => {
                let s = all(&sup, ins[0]);
                vec![
                    (0..w0)
                        .map(|i| s | bit(&sup, ins[1], i) | bit(&sup, ins[2], i))
                        .collect(),
                ]
            }
            Switch { .. } => {
                let s = all(&sup, ins[0]);
                vec![(0..w0).map(|i| s | bit(&sup, ins[1], i)).collect()]
            }
            Add { w } => {
                let c = all(&sup, ins[2]);
                let mut acc = c;
                let mut sum = Vec::with_capacity(*w as usize);
                for i in 0..*w as usize {
                    acc |= bit(&sup, ins[0], i) | bit(&sup, ins[1], i);
                    sum.push(acc);
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
                                top
                            } else {
                                0
                            }
                        })
                        .collect(),
                ]
            }
            _ => {
                let s = ins.iter().fold(0u128, |a, &v| a | all(&sup, v));
                outs.iter()
                    .map(|&o| vec![s; flat.widths[o as usize] as usize])
                    .collect()
            }
        };
        for (k, &o) in outs.iter().enumerate() {
            sup[o as usize] = std::mem::take(&mut res[k]);
        }
    }
    Some(sup)
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

/// Match a flat with state, tick by tick (see the module docs).
pub(crate) fn match_seq(flat: &Flat) -> Result<(Replacement, Proof), String> {
    let ports = Ports::new(flat);
    let ws: Vec<u8> = flat.states.iter().map(|s| s.width).collect();
    let bits = ports.bits + flat.state_bits();
    if bits > EXHAUSTIVE_BITS {
        return Err(format!("{bits} input and state bits"));
    }
    let ns = ws.len() as u16;
    let ni = ports.wi.len() as u16;
    // Candidates per state element and per output.
    let mut seq_cands: Vec<Vec<Seq>> = Vec::new();
    for k in 0..ns {
        let w = ws[k as usize];
        let mut ds: Vec<Src> = (0..ni)
            .filter(|&p| ports.wi[p as usize] == w)
            .map(Src::In)
            .collect();
        ds.extend(
            (0..ns)
                .filter(|&m| m != k && ws[m as usize] == w)
                .map(Src::State),
        );
        let mut ens: Vec<Option<Src>> = vec![None];
        for p in (0..ni).filter(|&p| ports.wi[p as usize] == 1) {
            ens.push(Some(Src::In(p)));
            ens.push(Some(Src::NotIn(p)));
        }
        let mut v = Vec::new();
        for &en in &ens {
            for &d in &ds {
                v.push(Seq { en, d });
            }
        }
        seq_cands.push(v);
    }
    let mut out_cands: Vec<Vec<Src>> = Vec::new();
    for j in 0..ports.wo.len() {
        let w = ports.wo[j];
        let mut v = Vec::new();
        for k in (0..ns).filter(|&k| ws[k as usize] == w) {
            v.push(Src::State(k));
            v.push(Src::NotState(k));
        }
        for p in (0..ni).filter(|&p| ports.wi[p as usize] == w) {
            v.push(Src::In(p));
            v.push(Src::NotIn(p));
        }
        v.push(Src::Const(0));
        out_cands.push(v);
    }
    let src = |s: Src, ins: &[u64], st: &[u64], w: u8| match s {
        Src::In(p) => ins[p as usize],
        Src::NotIn(p) => !ins[p as usize] & mask(w),
        Src::State(k) => st[k as usize],
        Src::NotState(k) => !st[k as usize] & mask(w),
        Src::Const(c) => c,
    };
    // Every combination of inputs and state: filter the candidates.
    let mut vals = vec![0u64; flat.values()];
    let mut ins = vec![0u64; ports.wi.len()];
    let mut st = vec![0u64; ws.len()];
    for x in 0..1u64 << bits {
        ports.split(x as u128, &mut ins);
        let mut shift = ports.bits;
        for (k, s) in st.iter_mut().enumerate() {
            *s = (x >> shift) & mask(ws[k]);
            shift += ws[k] as u32;
        }
        flat.eval(&ins, &st, &mut vals);
        for (k, cands) in seq_cands.iter_mut().enumerate() {
            let next = flat.next(&vals, k);
            let w = ws[k];
            cands.retain(|c| {
                let en = c.en.is_none_or(|e| src(e, &ins, &st, 1) & 1 == 1);
                let want = if en { src(c.d, &ins, &st, w) } else { st[k] };
                want == next
            });
            if cands.is_empty() {
                return Err(format!("state element {k}: not a register"));
            }
        }
        for (j, cands) in out_cands.iter_mut().enumerate() {
            let got = flat.output(&vals, j);
            cands.retain(|&c| src(c, &ins, &st, ports.wo[j]) == got);
            if cands.is_empty() {
                return Err(format!("output {j}: not a state element or port"));
            }
        }
    }
    let seqs: Vec<Seq> = seq_cands.iter().map(|c| c[0]).collect();
    let outs: Vec<Src> = out_cands.iter().map(|c| c[0]).collect();
    let paths = flat
        .states
        .iter()
        .map(|s| (s.path.clone(), s.node))
        .collect();
    Ok((
        lower_seq(&seqs, &ws, paths, &outs, &ports.wo),
        Proof::Exhaustive { bits },
    ))
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
        for j in 0..nout {
            word |= (flat.output(&vals, j) & mask(ports.wo[j])) << at;
            at += ports.wo[j] as u32;
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
        for j in 0..nout {
            r.nodes.push(super::replace::RNode {
                kind: NodeKind::Split {
                    w: wsum as u8,
                    lo,
                    len: ports.wo[j],
                },
                ins: smallvec![word],
                outs: smallvec![Ref::Out(j as u16)],
                state: None,
            });
            lo += ports.wo[j];
        }
    }
    Some((r, Proof::Table { bits: ports.bits }))
}
