//! A small flattened evaluator for one definition: the definition with
//! every instance expanded (or, for analysis, a matched combinational
//! child replaced by its proven word implementation), as a list of
//! primitive steps in topological order over local values.
//!
//! It evaluates one tick of the definition in isolation: given input port
//! values and the value of every state element, the value of every net,
//! every output and every state element's next value. The semantics are
//! the reference interpreter's (`crate::reference`) node by node; the
//! tests check the two against each other.
//!
//! Uses: fingerprinting and proving matches (`super::matcher`), and
//! reading the internal nets of a substituted instance (`Sim::read` of a
//! shadow slot; see [`crate::compile::Shadow`]).

use std::sync::Arc;

use slotmap::SecondaryMap;
use smallvec::{SmallVec, smallvec};

use super::super::FastMap;
use super::replace::{Ref, Replacement};
use crate::netlist::{DefId, Design, NetId, NodeId, NodeKind, mask};

/// Value 0 always reads 0 (unconnected pins, undriven nets).
pub(crate) const ZERO: u32 = 0;

#[derive(Debug, Clone)]
pub(crate) struct Step {
    pub kind: NodeKind,
    pub ins: SmallVec<[u32; 4]>,
    pub outs: SmallVec<[u32; 2]>,
}

/// One state element.
#[derive(Debug, Clone)]
pub(crate) struct StateEl {
    pub kind: NodeKind,
    /// Instance path relative to the flattened definition, and the node.
    pub path: Vec<NodeId>,
    pub node: NodeId,
    pub ins: SmallVec<[u32; 2]>,
    /// The value holding the state: the output net of a `Delay`,
    /// `Register` or `SrLatch`; a hidden value for a `DLatch` (whose output
    /// is a step).
    pub value: u32,
    pub width: u8,
}

/// A flattened definition. See the module docs.
#[derive(Debug, Clone, Default)]
pub struct Flat {
    pub(crate) widths: Vec<u8>,
    pub(crate) inputs: Vec<u32>,
    pub(crate) outputs: Vec<u32>,
    pub(crate) steps: Vec<Step>,
    pub(crate) states: Vec<StateEl>,
    /// `(relative instance path, net) -> value` (inspection flats only).
    pub(crate) nets: FastMap<(Vec<NodeId>, NetId), u32>,
}

/// Why a definition cannot be flattened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NoFlat {
    /// Clock, ROM, RAM or screen: not a function of the ports and state.
    Unsupported,
    TooLarge,
    /// A combinational cycle (the compile reports it).
    Cycle,
}

/// Flatten `root`. `abstr` gives the word implementation to use in place
/// of a child definition (combinational replacements only), or `None` to
/// expand it. `want_nets` records every authored net's value. At most
/// `max_steps` steps.
pub(crate) fn build(
    design: &Design,
    root: DefId,
    abstr: &dyn Fn(DefId) -> Option<Arc<Replacement>>,
    want_nets: bool,
    max_steps: usize,
) -> Result<Flat, NoFlat> {
    struct Inst {
        def: DefId,
        parent: Option<(usize, NodeId)>,
        base: u32,
        repl: Option<Arc<Replacement>>,
    }
    let mut net_index: SecondaryMap<DefId, SecondaryMap<NetId, u32>> = SecondaryMap::new();
    let mut index_of = |d: DefId| -> SecondaryMap<NetId, u32> {
        if let Some(m) = net_index.get(d) {
            return m.clone();
        }
        let m: SecondaryMap<NetId, u32> = design.defs[d]
            .nets
            .keys()
            .enumerate()
            .map(|(i, n)| (n, i as u32))
            .collect();
        net_index.insert(d, m.clone());
        m
    };
    let mut indices: Vec<SecondaryMap<NetId, u32>> = Vec::new();
    let mut insts = vec![Inst {
        def: root,
        parent: None,
        base: 1,
        repl: None,
    }];
    indices.push(index_of(root));
    let mut n = 1 + design.defs[root].nets.len() as u32;
    let mut prims = 0usize;
    let mut i = 0;
    while i < insts.len() {
        if insts[i].repl.is_some() {
            i += 1;
            continue;
        }
        let def = &design.defs[insts[i].def];
        for (id, node) in &def.nodes {
            match &node.kind {
                NodeKind::Instance(child) => {
                    let repl = abstr(*child);
                    let base = n;
                    if let Some(r) = &repl {
                        prims += r.nodes.len();
                        n += r.temps.len() as u32;
                        indices.push(SecondaryMap::new());
                    } else {
                        n += design.defs[*child].nets.len() as u32;
                        indices.push(index_of(*child));
                    }
                    insts.push(Inst {
                        def: *child,
                        parent: Some((i, id)),
                        base,
                        repl,
                    });
                }
                NodeKind::Clock { .. }
                | NodeKind::Rom { .. }
                | NodeKind::Ram { .. }
                | NodeKind::Ram2 { .. }
                | NodeKind::Screen { .. } => return Err(NoFlat::Unsupported),
                NodeKind::Input { .. } | NodeKind::Output { .. } => {}
                _ => prims += 1,
            }
        }
        if prims > max_steps {
            return Err(NoFlat::TooLarge);
        }
        i += 1;
    }
    let global = |inst: usize, net: NetId| insts[inst].base + indices[inst][net];

    // Union-find over raw values: port nets join their outer nets.
    let mut uf: Vec<u32> = (0..n).collect();
    fn find(uf: &mut [u32], mut x: u32) -> u32 {
        while uf[x as usize] != x {
            let p = uf[x as usize];
            uf[x as usize] = uf[p as usize];
            x = p;
        }
        x
    }
    let union = |uf: &mut Vec<u32>, a: u32, b: u32| {
        let (ra, rb) = (find(uf, a), find(uf, b));
        if ra != rb {
            let (lo, hi) = if ra < rb { (ra, rb) } else { (rb, ra) };
            uf[hi as usize] = lo;
        }
    };
    for c in 1..insts.len() {
        if insts[c].repl.is_some() {
            continue;
        }
        let (p, node) = insts[c].parent.expect("non-root");
        let pnode = &design.defs[insts[p].def].nodes[node];
        let cdef = &design.defs[insts[c].def];
        for (pin, &port) in cdef.inputs.iter().enumerate() {
            if let Some(outer) = pnode.inputs[pin] {
                let inner = cdef.nodes[port].outputs[0];
                union(&mut uf, global(p, outer), global(c, inner));
            }
        }
        for (pin, &port) in cdef.outputs.iter().enumerate() {
            if let Some(inner) = cdef.nodes[port].inputs[0] {
                union(&mut uf, global(c, inner), global(p, pnode.outputs[pin]));
            }
        }
    }
    // Dense values per class; value 0 is ZERO.
    let mut dense = vec![u32::MAX; n as usize];
    let mut widths: Vec<u8> = vec![1];
    let mut val = |uf: &mut Vec<u32>, widths: &mut Vec<u8>, g: u32, w: u8| -> u32 {
        let r = find(uf, g) as usize;
        if dense[r] == u32::MAX {
            dense[r] = widths.len() as u32;
            widths.push(w);
        }
        dense[r]
    };

    let mut flat = Flat::default();
    let rdef = &design.defs[root];
    for &port in &rdef.inputs {
        let net = rdef.nodes[port].outputs[0];
        let v = val(&mut uf, &mut widths, global(0, net), rdef.nets[net].width);
        flat.inputs.push(v);
    }
    let mut steps: Vec<Step> = Vec::new();
    for (k, inst) in insts.iter().enumerate() {
        if let Some(r) = &inst.repl {
            let (p, node) = inst.parent.expect("non-root");
            let pdef = &design.defs[insts[p].def];
            let pnode = &pdef.nodes[node];
            let mut temps = Vec::with_capacity(r.temps.len());
            for (t, &w) in r.temps.iter().enumerate() {
                temps.push(val(&mut uf, &mut widths, inst.base + t as u32, w));
            }
            let mut rv = |uf: &mut Vec<u32>, widths: &mut Vec<u8>, rf: Ref| -> u32 {
                match rf {
                    Ref::Zero => ZERO,
                    Ref::In(pin) => pnode.inputs[pin as usize].map_or(ZERO, |net| {
                        val(uf, widths, global(p, net), pdef.nets[net].width)
                    }),
                    Ref::Out(pin) => {
                        let net = pnode.outputs[pin as usize];
                        val(uf, widths, global(p, net), pdef.nets[net].width)
                    }
                    Ref::Temp(t) => temps[t as usize],
                }
            };
            for rn in &r.nodes {
                debug_assert!(!rn.kind.is_state());
                let ins = rn
                    .ins
                    .iter()
                    .map(|&x| rv(&mut uf, &mut widths, x))
                    .collect();
                let outs = rn
                    .outs
                    .iter()
                    .map(|&x| rv(&mut uf, &mut widths, x))
                    .collect();
                steps.push(Step {
                    kind: rn.kind.clone(),
                    ins,
                    outs,
                });
            }
            continue;
        }
        let def = &design.defs[inst.def];
        let path_of = |mut i: usize| {
            let mut path = Vec::new();
            while let Some((p, node)) = insts[i].parent {
                path.push(node);
                i = p;
            }
            path.reverse();
            path
        };
        let path = if want_nets || def.nodes.values().any(|n| n.kind.is_state()) {
            path_of(k)
        } else {
            Vec::new()
        };
        if want_nets {
            for (net, info) in &def.nets {
                let v = val(&mut uf, &mut widths, global(k, net), info.width);
                flat.nets.insert((path.clone(), net), v);
            }
        }
        for (id, node) in &def.nodes {
            let ins: SmallVec<[u32; 4]> = node
                .inputs
                .iter()
                .map(|x| {
                    x.map_or(ZERO, |net| {
                        val(&mut uf, &mut widths, global(k, net), def.nets[net].width)
                    })
                })
                .collect();
            let outs: SmallVec<[u32; 2]> = node
                .outputs
                .iter()
                .map(|&net| val(&mut uf, &mut widths, global(k, net), def.nets[net].width))
                .collect();
            match &node.kind {
                NodeKind::Input { .. } | NodeKind::Output { .. } | NodeKind::Instance(_) => {}
                NodeKind::Delay { w } | NodeKind::Register { w } => {
                    flat.states.push(StateEl {
                        kind: node.kind.clone(),
                        path: path.clone(),
                        node: id,
                        ins: ins.iter().copied().collect(),
                        value: outs[0],
                        width: *w,
                    });
                }
                NodeKind::SrLatch => flat.states.push(StateEl {
                    kind: node.kind.clone(),
                    path: path.clone(),
                    node: id,
                    ins: ins.iter().copied().collect(),
                    value: outs[0],
                    width: 1,
                }),
                NodeKind::DLatch { w } => {
                    let hidden = widths.len() as u32;
                    widths.push(*w);
                    flat.states.push(StateEl {
                        kind: node.kind.clone(),
                        path: path.clone(),
                        node: id,
                        ins: ins.iter().copied().collect(),
                        value: hidden,
                        width: *w,
                    });
                    let mut ins = ins;
                    ins.push(hidden);
                    steps.push(Step {
                        kind: node.kind.clone(),
                        ins,
                        outs,
                    });
                }
                kind => steps.push(Step {
                    kind: kind.clone(),
                    ins,
                    outs,
                }),
            }
        }
    }
    for &port in &rdef.outputs {
        let v = rdef.nodes[port].inputs[0].map_or(ZERO, |net| {
            val(&mut uf, &mut widths, global(0, net), rdef.nets[net].width)
        });
        flat.outputs.push(v);
    }

    // Topological order (Kahn).
    let nv = widths.len();
    let mut producer = vec![u32::MAX; nv];
    for (k, s) in steps.iter().enumerate() {
        for &o in &s.outs {
            producer[o as usize] = k as u32;
        }
    }
    let mut indeg = vec![0u32; steps.len()];
    let mut readers: Vec<SmallVec<[u32; 4]>> = vec![SmallVec::new(); steps.len()];
    for (k, s) in steps.iter().enumerate() {
        for &x in &s.ins {
            let p = producer[x as usize];
            if p != u32::MAX {
                indeg[k] += 1;
                readers[p as usize].push(k as u32);
            }
        }
    }
    let mut queue: Vec<u32> = (0..steps.len() as u32)
        .filter(|&k| indeg[k as usize] == 0)
        .collect();
    let mut order = Vec::with_capacity(steps.len());
    while let Some(k) = queue.pop() {
        order.push(k);
        for &c in &readers[k as usize] {
            indeg[c as usize] -= 1;
            if indeg[c as usize] == 0 {
                queue.push(c);
            }
        }
    }
    if order.len() != steps.len() {
        return Err(NoFlat::Cycle);
    }
    let mut slots: Vec<Option<Step>> = steps.into_iter().map(Some).collect();
    flat.steps = order
        .iter()
        .map(|&k| slots[k as usize].take().expect("once"))
        .collect();
    flat.widths = widths;
    Ok(flat)
}

impl Flat {
    /// Number of values (the length of the `vals` buffer of [`Flat::eval`]).
    pub fn values(&self) -> usize {
        self.widths.len()
    }

    /// Total state bits.
    pub(crate) fn state_bits(&self) -> u32 {
        self.states.iter().map(|s| s.width as u32).sum()
    }

    /// Evaluate one settle: every value from the input port values and the
    /// state values (both masked to their widths). `vals` has
    /// [`Flat::values`] entries.
    pub fn eval(&self, inputs: &[u64], state: &[u64], vals: &mut [u64]) {
        vals.fill(0);
        for (&v, &x) in self.inputs.iter().zip(inputs) {
            vals[v as usize] = x & mask(self.widths[v as usize]);
        }
        for (s, &x) in self.states.iter().zip(state) {
            vals[s.value as usize] = x & mask(s.width);
        }
        for step in &self.steps {
            let i = |k: usize| vals[step.ins[k] as usize];
            let out = eval_kind(&step.kind, &i);
            for (&o, v) in step.outs.iter().zip(out) {
                vals[o as usize] = v;
            }
        }
    }

    /// Output port values after [`Flat::eval`].
    pub(crate) fn output(&self, vals: &[u64], port: usize) -> u64 {
        vals[self.outputs[port] as usize]
    }

    /// Next value of state element `k` after [`Flat::eval`].
    pub(crate) fn next(&self, vals: &[u64], k: usize) -> u64 {
        let s = &self.states[k];
        let i = |j: usize| vals[s.ins[j] as usize];
        let cur = vals[s.value as usize];
        match s.kind {
            NodeKind::Delay { .. } => i(0),
            NodeKind::Register { .. } | NodeKind::DLatch { .. } => {
                if i(0) != 0 {
                    i(1)
                } else {
                    cur
                }
            }
            NodeKind::SrLatch => {
                if i(1) != 0 {
                    0
                } else if i(0) != 0 {
                    1
                } else {
                    cur
                }
            }
            _ => unreachable!("only these are flattened"),
        }
    }
}

/// Output values of one combinational primitive (and of a `DLatch`, whose
/// last operand is its state), exactly as the reference interpreter.
pub(crate) fn eval_kind(kind: &NodeKind, i: &dyn Fn(usize) -> u64) -> SmallVec<[u64; 2]> {
    use NodeKind::*;
    let b = |v: bool| v as u64;
    match kind {
        Const { w, value } => smallvec![value & mask(*w)],
        Not { w } => smallvec![!i(0) & mask(*w)],
        And { .. } => smallvec![i(0) & i(1)],
        Or { .. } => smallvec![i(0) | i(1)],
        Xor { .. } => smallvec![i(0) ^ i(1)],
        Nand { w } => smallvec![!(i(0) & i(1)) & mask(*w)],
        Nor { w } => smallvec![!(i(0) | i(1)) & mask(*w)],
        Xnor { w } => smallvec![!(i(0) ^ i(1)) & mask(*w)],
        Mux { .. } => smallvec![if i(0) != 0 { i(2) } else { i(1) }],
        Switch { .. } => smallvec![if i(0) != 0 { i(1) } else { 0 }],
        Add { w } => {
            let s = i(0) as u128 + i(1) as u128 + i(2) as u128;
            smallvec![(s as u64) & mask(*w), ((s >> *w) & 1) as u64]
        }
        Eq { .. } => smallvec![b(i(0) == i(1))],
        LtU { .. } => smallvec![b(i(0) < i(1))],
        ReduceOr { .. } => smallvec![b(i(0) != 0)],
        ReduceAnd { w } => smallvec![b(i(0) == mask(*w))],
        ReduceXor { .. } => smallvec![(i(0).count_ones() & 1) as u64],
        Split { lo, len, .. } => smallvec![(i(0) >> lo) & mask(*len)],
        Merge { widths } => {
            let mut v = 0u64;
            let mut shift = 0u32;
            for (k, &w) in widths.iter().enumerate() {
                v |= (i(k) & mask(w)) << shift;
                shift += w as u32;
            }
            smallvec![v]
        }
        Extend { from, to, signed } => {
            let a = i(0);
            let neg = *signed && (a >> (from - 1)) & 1 == 1;
            smallvec![if neg {
                (a | !mask(*from)) & mask(*to)
            } else {
                a
            }]
        }
        DLatch { .. } => smallvec![if i(0) != 0 { i(1) } else { i(2) }],
        other => unreachable!("not a flattened step: {other:?}"),
    }
}
