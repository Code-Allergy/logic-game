//! Reference interpreter for simulation v2 (design doc sections 3 and 8).
//!
//! Deliberately simple and slow: it runs directly on the hierarchical
//! [`Design`] with no flattening and no optimization, and is the test oracle
//! for the compiled simulator.
//!
//! A tick is: commit state (RAM writes included), advance the tick count, then
//! settle by re-evaluating every node until nothing changes. Each settle is
//! bounded by the flattened node count; exceeding it is reported as
//! [`ReferenceError::DidNotSettle`]. Construction validates the design, checks
//! for combinational loops structurally (see below), and performs the tick-0
//! settle with all state at its reset value (0, or a RAM's `init`).
//!
//! Value-based settling cannot see every combinational loop (an OR loop
//! driven high settles fine), so construction also runs a direct structural
//! check, per definition, over the combinational edges listed in section
//! 3.3, treating each instance as the input→output paths of its definition.
//! Any cycle is [`ReferenceError::CombinationalLoop`].

#[cfg(test)]
mod tests;

use std::fmt;

use slotmap::SecondaryMap;
use smallvec::{SmallVec, smallvec};

use crate::netlist::{
    CombDeps, DefId, Definition, Design, NetId, NodeId, NodeKind, ValidationError, mask,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReferenceError {
    /// The design failed [`Design::validate`].
    Invalid(Vec<ValidationError>),
    /// A combinational cycle inside definition `def`. `nodes` are the nodes
    /// on the cycle, in order along it; instances on the cycle stand for a
    /// path through their definition.
    CombinationalLoop { def: DefId, nodes: Vec<NodeId> },
    /// Settling the instance at `path` exceeded its iteration bound. Should be
    /// unreachable once the structural loop check passes.
    DidNotSettle { path: Vec<NodeId> },
}

impl fmt::Display for ReferenceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for ReferenceError {}

/// Per-instance runtime state. One per instance in the hierarchy.
#[derive(Debug, Clone)]
struct Inst {
    def: DefId,
    nets: SecondaryMap<NetId, u64>,
    /// Committed state of Delay/Register/DLatch/SrLatch.
    state: SecondaryMap<NodeId, u64>,
    /// Next state, computed at the end of each settle.
    next: SecondaryMap<NodeId, u64>,
    mems: SecondaryMap<NodeId, Vec<u64>>,
    /// Pending RAM write `(addr, data)`, applied at commit.
    writes: SecondaryMap<NodeId, Option<(usize, u64)>>,
    children: SecondaryMap<NodeId, Inst>,
    /// Inputs of the last settle this tick; a settle with the same inputs is
    /// a no-op. Cleared at commit.
    settled_with: Option<Vec<u64>>,
}

impl Inst {
    fn new(design: &Design, def_id: DefId) -> Self {
        let def = &design.defs[def_id];
        let mut inst = Inst {
            def: def_id,
            nets: def.nets.keys().map(|n| (n, 0)).collect(),
            state: SecondaryMap::new(),
            next: SecondaryMap::new(),
            mems: SecondaryMap::new(),
            writes: SecondaryMap::new(),
            children: SecondaryMap::new(),
            settled_with: None,
        };
        for (id, node) in &def.nodes {
            match &node.kind {
                NodeKind::Delay { .. }
                | NodeKind::Register { .. }
                | NodeKind::DLatch { .. }
                | NodeKind::SrLatch => {
                    inst.state.insert(id, 0);
                    inst.next.insert(id, 0);
                }
                NodeKind::Ram { addr_bits, w, init } => {
                    let mut mem = vec![0u64; 1usize << addr_bits];
                    for (slot, v) in mem.iter_mut().zip(init.iter()) {
                        *slot = v & mask(*w);
                    }
                    inst.mems.insert(id, mem);
                    inst.writes.insert(id, None);
                }
                NodeKind::Instance(child) => {
                    inst.children.insert(id, Inst::new(design, *child));
                }
                _ => {}
            }
        }
        inst
    }

    fn val(&self, net: Option<NetId>) -> u64 {
        net.map_or(0, |n| self.nets[n])
    }

    fn commit(&mut self) {
        for (id, s) in self.state.iter_mut() {
            *s = self.next[id];
        }
        for (id, w) in self.writes.iter_mut() {
            if let Some((addr, data)) = w.take() {
                self.mems[id][addr] = data;
            }
        }
        for (_, child) in self.children.iter_mut() {
            child.commit();
        }
        self.settled_with = None;
    }
}

/// Shared, read-only context for a settle.
struct Ctx<'a> {
    design: &'a Design,
    tick: u64,
    /// Flattened node count per definition (the settle bound).
    flat: &'a SecondaryMap<DefId, usize>,
}

pub struct Reference {
    design: Design,
    root: Inst,
    inputs: Vec<u64>,
    tick: u64,
    flat: SecondaryMap<DefId, usize>,
}

impl Reference {
    /// Validate, loop-check and build the interpreter, performing the tick-0
    /// settle. The design is cloned.
    pub fn new(design: &Design) -> Result<Self, ReferenceError> {
        design.validate().map_err(ReferenceError::Invalid)?;
        check_loops(design)?;
        let order = design.defs_bottom_up();
        let mut flat = SecondaryMap::new();
        for &d in &order {
            let def = &design.defs[d];
            let n = def.nodes.len()
                + def
                    .nodes
                    .values()
                    .map(|n| match n.kind {
                        NodeKind::Instance(c) => flat[c],
                        _ => 0,
                    })
                    .sum::<usize>();
            flat.insert(d, n);
        }
        let mut r = Reference {
            root: Inst::new(design, design.root),
            inputs: vec![0; design.root_def().inputs.len()],
            design: design.clone(),
            tick: 0,
            flat,
        };
        r.settle()?;
        Ok(r)
    }

    pub fn design(&self) -> &Design {
        &self.design
    }

    pub fn tick_count(&self) -> u64 {
        self.tick
    }

    /// Set root input port `port`. Visible to logic at the next settle (the
    /// next [`Self::tick`]), and to state at the commit after that.
    pub fn set_input(&mut self, port: usize, value: u64) {
        let def = self.design.root_def();
        let w = match def.nodes[def.inputs[port]].kind {
            NodeKind::Input { w } => w,
            _ => unreachable!("validated"),
        };
        self.inputs[port] = value & mask(w);
    }

    /// Set a root input port by name. Panics if there is no such port.
    #[track_caller]
    pub fn set(&mut self, name: &str, value: u64) {
        let port = self
            .design
            .root_def()
            .input_port(name)
            .unwrap_or_else(|| panic!("no input port {name:?}"));
        self.set_input(port, value);
    }

    /// Value of root output port `port` after the last settle.
    pub fn output(&self, port: usize) -> u64 {
        let def = self.design.root_def();
        self.root.val(def.nodes[def.outputs[port]].inputs[0])
    }

    /// Root output port by name. Panics if there is no such port.
    #[track_caller]
    pub fn get(&self, name: &str) -> u64 {
        let port = self
            .design
            .root_def()
            .output_port(name)
            .unwrap_or_else(|| panic!("no output port {name:?}"));
        self.output(port)
    }

    /// All root outputs in port order.
    pub fn outputs(&self) -> Vec<u64> {
        (0..self.design.root_def().outputs.len())
            .map(|p| self.output(p))
            .collect()
    }

    fn inst(&self, path: &[NodeId]) -> Option<&Inst> {
        let mut inst = &self.root;
        for n in path {
            inst = inst.children.get(*n)?;
        }
        Some(inst)
    }

    /// Value of `net` in the instance reached by `path` (instance node ids
    /// from the root; empty for the root itself).
    pub fn net(&self, path: &[NodeId], net: NetId) -> Option<u64> {
        self.inst(path)?.nets.get(net).copied()
    }

    /// Committed state of a Delay/Register/DLatch/SrLatch.
    pub fn state(&self, path: &[NodeId], node: NodeId) -> Option<u64> {
        self.inst(path)?.state.get(node).copied()
    }

    /// Committed contents of a RAM.
    pub fn mem(&self, path: &[NodeId], node: NodeId) -> Option<&[u64]> {
        self.inst(path)?.mems.get(node).map(Vec::as_slice)
    }

    /// Commit, advance the tick count, settle.
    pub fn tick(&mut self) -> Result<(), ReferenceError> {
        self.root.commit();
        self.tick += 1;
        self.settle()
    }

    pub fn run(&mut self, ticks: u64) -> Result<(), ReferenceError> {
        for _ in 0..ticks {
            self.tick()?;
        }
        Ok(())
    }

    fn settle(&mut self) -> Result<(), ReferenceError> {
        let ctx = Ctx {
            design: &self.design,
            tick: self.tick,
            flat: &self.flat,
        };
        let mut path = Vec::new();
        settle(&ctx, &mut self.root, &self.inputs, &mut path)
    }
}

/// Settle one instance to a fixpoint given its input port values, then
/// compute next state for its state elements.
fn settle(
    ctx: &Ctx<'_>,
    inst: &mut Inst,
    inputs: &[u64],
    path: &mut Vec<NodeId>,
) -> Result<(), ReferenceError> {
    if inst.settled_with.as_deref() == Some(inputs) {
        return Ok(());
    }
    let def = &ctx.design.defs[inst.def];
    for (&port, &v) in def.inputs.iter().zip(inputs) {
        let net = def.nodes[port].outputs[0];
        inst.nets[net] = v;
    }

    let bound = ctx.flat[inst.def] + 1;
    let mut passes = 0;
    loop {
        let mut changed = false;
        for (id, node) in &def.nodes {
            let outs = eval(ctx, def, inst, id, path)?;
            for (&net, v) in node.outputs.iter().zip(outs) {
                if inst.nets[net] != v {
                    inst.nets[net] = v;
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
        passes += 1;
        if passes > bound {
            return Err(ReferenceError::DidNotSettle { path: path.clone() });
        }
    }

    compute_next(def, inst);
    inst.settled_with = Some(inputs.to_vec());
    Ok(())
}

/// Output values of one node from the current net values.
fn eval(
    ctx: &Ctx<'_>,
    def: &Definition,
    inst: &mut Inst,
    id: NodeId,
    path: &mut Vec<NodeId>,
) -> Result<SmallVec<[u64; 2]>, ReferenceError> {
    use NodeKind::*;
    let node = &def.nodes[id];
    let i = |k: usize| inst.val(node.inputs[k]);
    let b = |v: bool| v as u64;
    let out: SmallVec<[u64; 2]> = match &node.kind {
        // Input nets are set from the port values; Output has no outputs.
        Input { .. } => smallvec![inst.nets[node.outputs[0]]],
        Output { .. } => smallvec![],
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
            smallvec![if neg { (a | !mask(*from)) & mask(*to) } else { a }]
        }
        Clock { period, duty, phase } => {
            let t = (ctx.tick + *phase as u64) % *period as u64;
            smallvec![b(t < *duty as u64)]
        }
        Rom { w, contents, .. } => {
            smallvec![contents.get(i(0) as usize).copied().unwrap_or(0) & mask(*w)]
        }
        Delay { .. } | Register { .. } | SrLatch => smallvec![inst.state[id]],
        DLatch { .. } => smallvec![if i(0) != 0 { i(1) } else { inst.state[id] }],
        Ram { .. } => smallvec![inst.mems[id][i(0) as usize]],
        Instance(child) => {
            let child_inputs: Vec<u64> = (0..node.inputs.len()).map(i).collect();
            let child_def = &ctx.design.defs[*child];
            let child_inst = inst.children.get_mut(id).expect("built with design");
            path.push(id);
            settle(ctx, child_inst, &child_inputs, path)?;
            path.pop();
            child_def
                .outputs
                .iter()
                .map(|&o| child_inst.val(child_def.nodes[o].inputs[0]))
                .collect()
        }
    };
    Ok(out)
}

fn compute_next(def: &Definition, inst: &mut Inst) {
    for (id, node) in &def.nodes {
        let i = |k: usize| inst.val(node.inputs[k]);
        let next = match &node.kind {
            NodeKind::Delay { .. } => i(0),
            NodeKind::Register { .. } | NodeKind::DLatch { .. } => {
                if i(0) != 0 { i(1) } else { inst.state[id] }
            }
            NodeKind::SrLatch => {
                if i(1) != 0 {
                    0
                } else if i(0) != 0 {
                    1
                } else {
                    inst.state[id]
                }
            }
            NodeKind::Ram { .. } => {
                let write = (i(1) != 0).then(|| (i(0) as usize, i(2)));
                inst.writes[id] = write;
                continue;
            }
            _ => continue,
        };
        inst.next[id] = next;
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Structural loop check
// ─────────────────────────────────────────────────────────────────────────────

/// Per definition, which input ports reach which output ports combinationally:
/// `paths[i][j]`.
type PortPaths = Vec<Vec<bool>>;

/// Check every definition reachable from the root for combinational cycles,
/// children first. Assumes the design validated.
pub fn check_loops(design: &Design) -> Result<(), ReferenceError> {
    let mut paths: SecondaryMap<DefId, PortPaths> = SecondaryMap::new();
    for d in design.defs_bottom_up() {
        let p = check_def(design, d, &paths)?;
        paths.insert(d, p);
    }
    Ok(())
}

/// Does output `q` of `node` depend combinationally on input `p`?
fn depends(node_kind: &NodeKind, p: usize, q: usize, paths: &SecondaryMap<DefId, PortPaths>) -> bool {
    match node_kind.comb_deps() {
        CombDeps::All => true,
        CombDeps::None => false,
        CombDeps::Pins(pins) => pins.contains(&(p as u8)),
        CombDeps::Instance(c) => paths[c][p][q],
    }
}

fn check_def(
    design: &Design,
    def_id: DefId,
    paths: &SecondaryMap<DefId, PortPaths>,
) -> Result<PortPaths, ReferenceError> {
    let def = &design.defs[def_id];
    // Net graph: an edge net_in -> net_out for every combinational
    // input->output pin pair.
    let mut succ: SecondaryMap<NetId, Vec<NetId>> = def.nets.keys().map(|n| (n, Vec::new())).collect();
    for node in def.nodes.values() {
        for (p, net_in) in node.inputs.iter().enumerate() {
            let Some(net_in) = net_in else { continue };
            for (q, &net_out) in node.outputs.iter().enumerate() {
                if depends(&node.kind, p, q, paths) {
                    succ[*net_in].push(net_out);
                }
            }
        }
    }

    // Iterative DFS with colours; on a back edge, report the cycle.
    #[derive(Clone, Copy, PartialEq)]
    enum C {
        Grey,
        Black,
    }
    let mut colour: SecondaryMap<NetId, C> = SecondaryMap::new();
    for start in def.nets.keys() {
        if colour.contains_key(start) {
            continue;
        }
        colour.insert(start, C::Grey);
        let mut stack: Vec<(NetId, usize)> = vec![(start, 0)];
        while let Some(&mut (net, ref mut k)) = stack.last_mut() {
            if *k == succ[net].len() {
                colour.insert(net, C::Black);
                stack.pop();
                continue;
            }
            let next = succ[net][*k];
            *k += 1;
            match colour.get(next) {
                Some(C::Grey) => {
                    let pos = stack.iter().position(|(n, _)| *n == next).expect("grey on stack");
                    let nodes = stack[pos..]
                        .iter()
                        .map(|(n, _)| def.nets[*n].driver.0)
                        .collect();
                    return Err(ReferenceError::CombinationalLoop { def: def_id, nodes });
                }
                Some(C::Black) => {}
                None => {
                    colour.insert(next, C::Grey);
                    stack.push((next, 0));
                }
            }
        }
    }

    // Port-to-port reachability for parents.
    let out_nets: Vec<Option<NetId>> = def
        .outputs
        .iter()
        .map(|&o| def.nodes[o].inputs[0])
        .collect();
    let mut result = Vec::with_capacity(def.inputs.len());
    for &port in &def.inputs {
        let start = def.nodes[port].outputs[0];
        let mut seen: SecondaryMap<NetId, ()> = SecondaryMap::new();
        let mut stack = vec![start];
        seen.insert(start, ());
        while let Some(n) = stack.pop() {
            for &m in &succ[n] {
                if seen.insert(m, ()).is_none() {
                    stack.push(m);
                }
            }
        }
        result.push(
            out_nets
                .iter()
                .map(|n| n.is_some_and(|n| seen.contains_key(n)))
                .collect(),
        );
    }
    Ok(result)
}
