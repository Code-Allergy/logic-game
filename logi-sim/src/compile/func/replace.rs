//! Word implementations of matched definitions: the catalogue's functions
//! ([`Fun`], [`Word`]) and their lowering to a few primitive nodes
//! ([`Replacement`]) wired to the definition's ports.

use smallvec::{SmallVec, smallvec};

use crate::netlist::{NodeId, NodeKind, mask};

/// An operand of a replacement node, relative to the instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Ref {
    Zero,
    /// Input port (instance input pin).
    In(u16),
    /// Output port (instance output pin).
    Out(u16),
    /// A temporary (`Replacement::temps`).
    Temp(u16),
}

#[derive(Debug, Clone)]
pub(crate) struct RNode {
    pub kind: NodeKind,
    pub ins: SmallVec<[Ref; 3]>,
    pub outs: SmallVec<[Ref; 2]>,
    /// For a state node: its index in `Replacement::words`.
    pub state: Option<u16>,
}

/// One original state element held in a state word: element `state` (an
/// index into `Replacement::states`) is bits `off..off + width` of the
/// word.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Field {
    pub state: u16,
    pub off: u8,
    pub width: u8,
}

/// A definition's word implementation: primitive nodes over its ports and
/// temporaries. State nodes take the identity of the original state
/// elements (`states`, relative paths), so state keys, the reference's
/// view of state, and carry-over across recompiles are unchanged. A state
/// node may hold several original elements (`words`): each keeps its
/// identity as a bit field of the word (emit makes its state key a view).
#[derive(Debug, Clone, Default)]
pub(crate) struct Replacement {
    pub nodes: Vec<RNode>,
    pub temps: Vec<u8>,
    pub states: Vec<(Vec<NodeId>, NodeId)>,
    /// Per state node (`RNode::state`): the original elements it holds,
    /// low bits first.
    pub words: Vec<SmallVec<[Field; 1]>>,
    /// What it was matched as (diagnostics, reports).
    pub desc: String,
    /// A truth table (`super::matcher::table`) rather than a catalogue
    /// function: lowered only with `Options::tables`, and never used as a
    /// child's abstraction when analysing a parent (a parent's flat
    /// expands the child's gates instead).
    pub table: bool,
}

impl Replacement {
    fn temp(&mut self, w: u8) -> Ref {
        self.temps.push(w);
        Ref::Temp((self.temps.len() - 1) as u16)
    }
    fn node(&mut self, kind: NodeKind, ins: SmallVec<[Ref; 3]>, outs: SmallVec<[Ref; 2]>) {
        self.nodes.push(RNode {
            kind,
            ins,
            outs,
            state: None,
        });
    }
    pub fn is_sequential(&self) -> bool {
        !self.states.is_empty()
    }
    /// Instructions the replacement lowers to (an estimate: copies count).
    pub fn cost(&self) -> usize {
        self.nodes
            .iter()
            .map(|n| match n.kind {
                NodeKind::Add { .. } => n.outs.iter().filter(|o| **o != Ref::Zero).count(),
                NodeKind::Const { .. } => 0,
                _ => 1,
            })
            .sum()
    }
}

/// A 1-bit-or-word source: an input port, its complement, or a constant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Src {
    In(u16),
    NotIn(u16),
    Const(u64),
}

/// The 2-input bitwise ops.
pub(crate) const BITWISE: [fn(u8) -> NodeKind; 6] = [
    |w| NodeKind::And { w },
    |w| NodeKind::Or { w },
    |w| NodeKind::Xor { w },
    |w| NodeKind::Nand { w },
    |w| NodeKind::Nor { w },
    |w| NodeKind::Xnor { w },
];

/// A combinational word function of the input ports, for one output port.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Fun {
    Src(Src),
    /// `BITWISE[op](a, b)`.
    Bit(u8, u16, u16),
    /// `sel ? b : a`.
    Mux {
        sel: u16,
        a: u16,
        b: u16,
    },
    /// `a + (inv_b ? !b : b) + cin`: the sum (`carry` false) or carry out.
    Add {
        a: u16,
        b: u16,
        inv_b: bool,
        cin: Src,
        carry: bool,
    },
    Eq {
        a: u16,
        b: u16,
        neg: bool,
    },
    LtU {
        a: u16,
        b: u16,
        neg: bool,
    },
}

impl Fun {
    /// Evaluate on input port values (`w` the output width; `wi` the input
    /// port widths).
    pub fn eval(&self, ins: &[u64], wi: &[u8], w: u8) -> u64 {
        let src = |s: Src| match s {
            Src::In(p) => ins[p as usize],
            Src::NotIn(p) => !ins[p as usize] & mask(wi[p as usize]),
            Src::Const(c) => c,
        };
        match *self {
            Fun::Src(s) => src(s) & mask(w),
            Fun::Bit(op, a, b) => {
                let (x, y) = (ins[a as usize], ins[b as usize]);
                let v = match op {
                    0 => x & y,
                    1 => x | y,
                    2 => x ^ y,
                    3 => !(x & y),
                    4 => !(x | y),
                    _ => !(x ^ y),
                };
                v & mask(w)
            }
            Fun::Mux { sel, a, b } => {
                if ins[sel as usize] != 0 {
                    ins[b as usize]
                } else {
                    ins[a as usize]
                }
            }
            Fun::Add {
                a,
                b,
                inv_b,
                cin,
                carry,
            } => {
                let wa = wi[a as usize];
                let bv = if inv_b {
                    !ins[b as usize] & mask(wa)
                } else {
                    ins[b as usize]
                };
                let s = ins[a as usize] as u128 + bv as u128 + (src(cin) & 1) as u128;
                if carry {
                    ((s >> wa) & 1) as u64
                } else {
                    (s as u64) & mask(wa)
                }
            }
            Fun::Eq { a, b, neg } => ((ins[a as usize] == ins[b as usize]) != neg) as u64,
            Fun::LtU { a, b, neg } => ((ins[a as usize] < ins[b as usize]) != neg) as u64,
        }
    }

    /// Cost rank for choosing among proven candidates: bitwise forms first
    /// (word lifting can still group them), then the rest.
    pub fn rank(&self) -> u8 {
        match self {
            Fun::Src(Src::In(_)) | Fun::Src(Src::Const(_)) => 0,
            Fun::Src(_) => 1,
            Fun::Bit(..) => 2,
            Fun::Mux { .. } => 3,
            Fun::Eq { neg: false, .. } | Fun::LtU { neg: false, .. } => 4,
            Fun::Add { .. } => 5,
            _ => 6,
        }
    }

    /// The input bits output bit `bit` can depend on, as a bit set over
    /// the concatenated input ports (`off` the first bit of each port).
    pub fn support(&self, bit: u8, off: &[u32], wi: &[u8]) -> u128 {
        let one = |p: u16, i: u8| 1u128 << (off[p as usize] + i as u32);
        let all = |p: u16| {
            let w = wi[p as usize] as u32;
            (if w >= 128 {
                u128::MAX
            } else {
                (1u128 << w) - 1
            }) << off[p as usize]
        };
        let low = |p: u16, i: u8| {
            let n = i as u32 + 1;
            ((1u128 << n) - 1) << off[p as usize]
        };
        let src = |s: Src, i: u8| match s {
            Src::In(p) | Src::NotIn(p) => one(p, i),
            _ => 0,
        };
        let src_all = |s: Src| match s {
            Src::In(p) | Src::NotIn(p) => all(p),
            _ => 0,
        };
        match *self {
            Fun::Src(s) => src(s, bit),
            Fun::Bit(_, a, b) => one(a, bit) | one(b, bit),
            Fun::Mux { sel, a, b } => all(sel) | one(a, bit) | one(b, bit),
            Fun::Add {
                a, b, cin, carry, ..
            } => {
                if carry {
                    all(a) | all(b) | src_all(cin)
                } else {
                    low(a, bit) | low(b, bit) | src_all(cin)
                }
            }
            Fun::Eq { a, b, .. } | Fun::LtU { a, b, .. } => all(a) | all(b),
        }
    }
}

/// Lower a combinational match: `funs[j]` drives output port `j`.
pub(crate) fn lower_comb(funs: &[Fun], wi: &[u8], wo: &[u8]) -> Replacement {
    let mut r = Replacement::default();
    let mut done = vec![false; funs.len()];
    for j in 0..funs.len() {
        if done[j] {
            continue;
        }
        done[j] = true;
        let out = Ref::Out(j as u16);
        let w = wo[j];
        match funs[j] {
            Fun::Src(s) => {
                let x = src_ref(&mut r, s, w);
                r.node(
                    NodeKind::Merge {
                        widths: smallvec![w],
                    },
                    smallvec![x],
                    smallvec![out],
                );
            }
            Fun::Bit(op, a, b) => r.node(
                BITWISE[op as usize](w),
                smallvec![Ref::In(a), Ref::In(b)],
                smallvec![out],
            ),
            Fun::Mux { sel, a, b } => r.node(
                NodeKind::Mux { w },
                smallvec![Ref::In(sel), Ref::In(a), Ref::In(b)],
                smallvec![out],
            ),
            Fun::Add {
                a,
                b,
                inv_b,
                cin,
                carry,
            } => {
                let wa = wi[a as usize];
                // The other half of the same adder, if another output is it.
                let twin = (j + 1..funs.len()).find(|&k| {
                    !done[k]
                        && funs[k]
                            == Fun::Add {
                                a,
                                b,
                                inv_b,
                                cin,
                                carry: !carry,
                            }
                });
                let other = match twin {
                    Some(k) => {
                        done[k] = true;
                        Ref::Out(k as u16)
                    }
                    None => Ref::Zero,
                };
                let (sum, cout) = if carry { (other, out) } else { (out, other) };
                let bv = if inv_b {
                    let t = r.temp(wa);
                    r.node(NodeKind::Not { w: wa }, smallvec![Ref::In(b)], smallvec![t]);
                    t
                } else {
                    Ref::In(b)
                };
                let c = src_ref(&mut r, cin, 1);
                // An unused half still needs a destination.
                let sum = if sum == Ref::Zero { r.temp(wa) } else { sum };
                let cout = if cout == Ref::Zero { r.temp(1) } else { cout };
                r.node(
                    NodeKind::Add { w: wa },
                    smallvec![Ref::In(a), bv, c],
                    smallvec![sum, cout],
                );
            }
            Fun::Eq { a, b, neg } | Fun::LtU { a, b, neg } => {
                let wa = wi[a as usize];
                let kind = if matches!(funs[j], Fun::Eq { .. }) {
                    NodeKind::Eq { w: wa }
                } else {
                    NodeKind::LtU { w: wa }
                };
                if neg {
                    let t = r.temp(1);
                    r.node(kind, smallvec![Ref::In(a), Ref::In(b)], smallvec![t]);
                    r.node(NodeKind::Not { w: 1 }, smallvec![t], smallvec![out]);
                } else {
                    r.node(kind, smallvec![Ref::In(a), Ref::In(b)], smallvec![out]);
                }
            }
        }
    }
    r.desc = format!("{funs:?}");
    r
}

/// A source as an operand (complements and constants get a node).
fn src_ref(r: &mut Replacement, s: Src, w: u8) -> Ref {
    match s {
        Src::In(p) => Ref::In(p),
        Src::NotIn(p) => {
            let t = r.temp(w);
            r.node(NodeKind::Not { w }, smallvec![Ref::In(p)], smallvec![t]);
            t
        }
        Src::Const(0) => Ref::Zero,
        Src::Const(c) => {
            let t = r.temp(w);
            r.node(NodeKind::Const { w, value: c }, smallvec![], smallvec![t]);
            t
        }
    }
}

/// The enable of a state word: always (a `Delay`), or bit `bit` of input
/// port `port`, complemented when `neg`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) enum En {
    Always,
    Bit { port: u16, bit: u8, neg: bool },
}

/// A word a state word's data or an output reads from: an input port or
/// another state word (its current value).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) enum Base {
    Port(u16),
    Word(u16),
}

/// One state word: `next = en ? d : state` with `d` bits
/// `lo..lo + width` of `base`; it holds the original elements `fields`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Word {
    pub en: En,
    pub base: Base,
    pub lo: u8,
    pub width: u8,
    pub fields: SmallVec<[Field; 1]>,
}

/// A run of bits of an output port, low bits first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Run {
    Const {
        len: u8,
        value: u64,
    },
    /// Bits `lo..lo + len` of `base`, complemented when `neg`.
    Bits {
        base: Base,
        lo: u8,
        len: u8,
        neg: bool,
    },
}

impl Run {
    pub fn len(&self) -> u8 {
        match *self {
            Run::Const { len, .. } | Run::Bits { len, .. } => len,
        }
    }
}

/// `In(p)`, `In(p)[3]`, `In(p)[0..8]`, `r1`, `r1[4..8]`, `!...`: a slice
/// of a base, for [`Replacement::desc`].
pub(crate) fn slice_text(base: Base, lo: u8, len: u8, whole: u8, neg: bool) -> String {
    let b = match base {
        Base::Port(p) => format!("In({p})"),
        Base::Word(g) => format!("r{g}"),
    };
    let not = if neg { "!" } else { "" };
    if lo == 0 && len == whole {
        format!("{not}{b}")
    } else if len == 1 {
        format!("{not}{b}[{lo}]")
    } else {
        format!("{not}{b}[{lo}..{}]", lo + len)
    }
}

/// Lower a sequential match: each state word `words[g]` becomes one
/// `Register` (or `Delay`) of its width holding its original elements as
/// bit fields (identities `paths`); output port `j` is the concatenation
/// of `outs[j]`. `wi` and `wo` are the port widths.
pub(crate) fn lower_seq(
    words: &[Word],
    paths: Vec<(Vec<NodeId>, NodeId)>,
    outs: &[Vec<Run>],
    wi: &[u8],
    wo: &[u8],
) -> Replacement {
    let mut r = Replacement {
        states: paths,
        ..Default::default()
    };
    let whole = |b: Base| match b {
        Base::Port(p) => wi[p as usize],
        Base::Word(g) => words[g as usize].width,
    };
    // The state value of each word: an output port that is exactly it,
    // else a temporary.
    let mut port_done = vec![false; outs.len()];
    let mut wref: Vec<Ref> = Vec::with_capacity(words.len());
    for (g, w) in words.iter().enumerate() {
        let exact = |runs: &Vec<Run>| {
            matches!(runs.as_slice(), [Run::Bits { base: Base::Word(h), lo: 0, len, neg: false }]
                if *h as usize == g && *len == w.width)
        };
        match (0..outs.len()).find(|&j| !port_done[j] && exact(&outs[j])) {
            Some(j) => {
                port_done[j] = true;
                wref.push(Ref::Out(j as u16));
            }
            None => wref.push(r.temp(w.width)),
        }
    }
    // Bits `lo..lo + len` of `base` as an operand.
    let slice = |r: &mut Replacement, base: Base, lo: u8, len: u8, wref: &[Ref]| {
        let (x, w) = match base {
            Base::Port(p) => (Ref::In(p), wi[p as usize]),
            Base::Word(g) => (wref[g as usize], words[g as usize].width),
        };
        if lo == 0 && len == w {
            return x;
        }
        let t = r.temp(len);
        r.node(NodeKind::Split { w, lo, len }, smallvec![x], smallvec![t]);
        t
    };
    let mut text = Vec::with_capacity(words.len());
    for (g, w) in words.iter().enumerate() {
        let d = slice(&mut r, w.base, w.lo, w.width, &wref);
        let dtext = slice_text(w.base, w.lo, w.width, whole(w.base), false);
        let (kind, ins, t): (NodeKind, SmallVec<[Ref; 3]>, String) = match w.en {
            En::Always => (
                NodeKind::Delay { w: w.width },
                smallvec![d],
                format!("{}-bit Delay {{ d: {dtext} }}", w.width),
            ),
            En::Bit { port, bit, neg } => {
                let mut e = slice(&mut r, Base::Port(port), bit, 1, &wref);
                if neg {
                    let t = r.temp(1);
                    r.node(NodeKind::Not { w: 1 }, smallvec![e], smallvec![t]);
                    e = t;
                }
                let etext = slice_text(Base::Port(port), bit, 1, wi[port as usize], neg);
                (
                    NodeKind::Register { w: w.width },
                    smallvec![e, d],
                    format!("{}-bit Register {{ en: {etext}, d: {dtext} }}", w.width),
                )
            }
        };
        text.push(t);
        r.words.push(w.fields.clone());
        r.nodes.push(RNode {
            kind,
            ins,
            outs: smallvec![wref[g]],
            state: Some(g as u16),
        });
    }
    let mut otext = Vec::with_capacity(outs.len());
    for (j, runs) in outs.iter().enumerate() {
        let parts: Vec<String> = runs
            .iter()
            .map(|run| match *run {
                Run::Const { value, .. } => format!("{value}"),
                Run::Bits { base, lo, len, neg } => slice_text(base, lo, len, whole(base), neg),
            })
            .collect();
        otext.push(parts.join(" ++ "));
        if port_done[j] {
            continue;
        }
        let mut ops: SmallVec<[Ref; 3]> = SmallVec::new();
        let mut widths: SmallVec<[u8; 4]> = SmallVec::new();
        for run in runs {
            let x = match *run {
                Run::Const { len, value } => src_ref(&mut r, Src::Const(value), len),
                Run::Bits { base, lo, len, neg } => {
                    let x = slice(&mut r, base, lo, len, &wref);
                    if neg {
                        let t = r.temp(len);
                        r.node(NodeKind::Not { w: len }, smallvec![x], smallvec![t]);
                        t
                    } else {
                        x
                    }
                }
            };
            ops.push(x);
            widths.push(run.len());
        }
        debug_assert_eq!(widths.iter().map(|&w| w as u32).sum::<u32>(), wo[j] as u32);
        r.node(
            NodeKind::Merge { widths },
            ops,
            smallvec![Ref::Out(j as u16)],
        );
    }
    r.desc = format!(
        "state [{}], outputs [{}]",
        text.join(", "),
        otext.join(", ")
    );
    r
}
