//! Word implementations of matched definitions: the catalogue's functions
//! ([`Fun`], [`Seq`]) and their lowering to a few primitive nodes
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
    /// For a state node: its index in `Replacement::states`.
    pub state: Option<u16>,
}

/// A definition's word implementation: primitive nodes over its ports and
/// temporaries. State nodes take the identity of the original state
/// elements (`states`, relative paths), so state keys, the reference's
/// view of state, and carry-over across recompiles are unchanged.
#[derive(Debug, Clone, Default)]
pub(crate) struct Replacement {
    pub nodes: Vec<RNode>,
    pub temps: Vec<u8>,
    pub states: Vec<(Vec<NodeId>, NodeId)>,
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
    /// Sequential only: state element `k`, or its complement.
    State(u16),
    NotState(u16),
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
            Src::State(_) | Src::NotState(_) => unreachable!("combinational"),
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

/// One state element's tick-level behaviour: `next = en ? d : state`
/// (`en` `None` means always: a `Delay`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Seq {
    pub en: Option<Src>,
    pub d: Src,
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
        Src::State(_) | Src::NotState(_) => unreachable!("combinational source"),
    }
}

/// Lower a sequential match: state element `k` (width `ws[k]`, identity
/// `paths[k]`) becomes a `Register` (or `Delay`) per `seqs[k]`; output
/// port `j` is `outs[j]`.
pub(crate) fn lower_seq(
    seqs: &[Seq],
    ws: &[u8],
    paths: Vec<(Vec<NodeId>, NodeId)>,
    outs: &[Src],
    wo: &[u8],
) -> Replacement {
    let mut r = Replacement {
        states: paths,
        ..Default::default()
    };
    // The state value of each element: an output port that is exactly it,
    // else a temporary.
    let mut sref: Vec<Ref> = Vec::with_capacity(seqs.len());
    let mut port_done = vec![false; outs.len()];
    for (k, &w) in ws.iter().enumerate() {
        match outs.iter().position(|&o| o == Src::State(k as u16)) {
            Some(j) => {
                port_done[j] = true;
                sref.push(Ref::Out(j as u16));
            }
            None => sref.push(r.temp(w)),
        }
    }
    let operand = |r: &mut Replacement, s: Src, w: u8, sref: &[Ref]| match s {
        Src::State(m) => sref[m as usize],
        Src::NotState(m) => {
            let t = r.temp(w);
            r.node(
                NodeKind::Not { w },
                smallvec![sref[m as usize]],
                smallvec![t],
            );
            t
        }
        other => src_ref(r, other, w),
    };
    for (k, seq) in seqs.iter().enumerate() {
        let w = ws[k];
        let d = operand(&mut r, seq.d, w, &sref);
        let (kind, ins): (NodeKind, SmallVec<[Ref; 3]>) = match seq.en {
            None => (NodeKind::Delay { w }, smallvec![d]),
            Some(en) => {
                let e = operand(&mut r, en, 1, &sref);
                (NodeKind::Register { w }, smallvec![e, d])
            }
        };
        r.nodes.push(RNode {
            kind,
            ins,
            outs: smallvec![sref[k]],
            state: Some(k as u16),
        });
    }
    for (j, &o) in outs.iter().enumerate() {
        if port_done[j] {
            continue;
        }
        let x = operand(&mut r, o, wo[j], &sref);
        r.node(
            NodeKind::Merge {
                widths: smallvec![wo[j]],
            },
            smallvec![x],
            smallvec![Ref::Out(j as u16)],
        );
    }
    r.desc = format!("state {seqs:?}, outputs {outs:?}");
    r
}
