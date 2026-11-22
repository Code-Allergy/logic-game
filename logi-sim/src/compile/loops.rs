//! Pass 4: combinational loop check. Iterative Tarjan SCC over the
//! elaborated net graph.
//!
//! Vertices are global nets (before unification, so port crossings keep
//! their direction). Edges are every combinational input->output pin pair of
//! a primitive (section 3.3: everything except Delay/Register/SrLatch inputs
//! and RAM we/wdata), plus port-crossing edges. A strongly connected
//! component with more than one net, or a net with an edge to itself, is a
//! loop.

use super::elaborate::Elab;
use super::program::NodePath;
use crate::netlist::{CombDeps, NodeKind};

/// Each loop as the node paths on it, sorted. A loop's nodes are the
/// primitives driving its nets. A loop made only of wires through instance
/// ports, with no primitive on it, is reported as the instance nodes it
/// crosses.
pub(super) fn find_loops(e: &Elab<'_>) -> Vec<Vec<NodePath>> {
    let n = e.n_nets as usize;

    // CSR adjacency.
    let mut edges: Vec<(u32, u32)> = e.alias_edges.clone();
    for inst in 0..e.insts.len() as u32 {
        let def = &e.design.defs[e.def_of(inst)];
        for node in def.nodes.values() {
            if matches!(node.kind, NodeKind::Instance(_)) {
                continue;
            }
            let deps = node.kind.comb_deps();
            for (pin, net_in) in node.inputs.iter().enumerate() {
                let Some(net_in) = *net_in else { continue };
                let comb = match deps {
                    CombDeps::All => true,
                    CombDeps::None => false,
                    CombDeps::Pins(pins) => pins.contains(&(pin as u8)),
                    CombDeps::Instance(_) => unreachable!(),
                };
                if comb {
                    let from = e.global(inst, net_in);
                    for &out in &node.outputs {
                        edges.push((from, e.global(inst, out)));
                    }
                }
            }
        }
    }
    let mut start = vec![0u32; n + 1];
    for &(f, _) in &edges {
        start[f as usize + 1] += 1;
    }
    for i in 0..n {
        start[i + 1] += start[i];
    }
    let mut adj = vec![0u32; edges.len()];
    let mut fill = start.clone();
    for &(f, t) in &edges {
        adj[fill[f as usize] as usize] = t;
        fill[f as usize] += 1;
    }
    drop(edges);

    // Iterative Tarjan.
    const UNSEEN: u32 = u32::MAX;
    let mut index = vec![UNSEEN; n];
    let mut low = vec![0u32; n];
    let mut on_stack = vec![false; n];
    let mut stack: Vec<u32> = Vec::new();
    let mut call: Vec<(u32, u32)> = Vec::new(); // (vertex, next edge position)
    let mut next_index = 0u32;
    let mut loops = Vec::new();

    for root in 0..n as u32 {
        if index[root as usize] != UNSEEN {
            continue;
        }
        call.push((root, start[root as usize]));
        index[root as usize] = next_index;
        low[root as usize] = next_index;
        next_index += 1;
        stack.push(root);
        on_stack[root as usize] = true;

        while let Some(&mut (v, ref mut pos)) = call.last_mut() {
            let vu = v as usize;
            if *pos < start[vu + 1] {
                let w = adj[*pos as usize];
                *pos += 1;
                let wu = w as usize;
                if index[wu] == UNSEEN {
                    index[wu] = next_index;
                    low[wu] = next_index;
                    next_index += 1;
                    stack.push(w);
                    on_stack[wu] = true;
                    call.push((w, start[wu]));
                } else if on_stack[wu] {
                    low[vu] = low[vu].min(index[wu]);
                }
                continue;
            }
            call.pop();
            if let Some(&(parent, _)) = call.last() {
                low[parent as usize] = low[parent as usize].min(low[vu]);
            }
            if low[vu] == index[vu] {
                let mut scc = Vec::new();
                loop {
                    let w = stack.pop().expect("tarjan stack");
                    on_stack[w as usize] = false;
                    scc.push(w);
                    if w == v {
                        break;
                    }
                }
                let self_loop =
                    scc.len() == 1 && adj[start[vu] as usize..start[vu + 1] as usize].contains(&v);
                if scc.len() > 1 || self_loop {
                    loops.push(describe(e, &scc));
                }
            }
        }
    }
    loops
}

/// Node paths for the nets of one SCC.
fn describe(e: &Elab<'_>, scc: &[u32]) -> Vec<NodePath> {
    let mut prims = Vec::new();
    let mut crossings = Vec::new();
    for &g in scc {
        let (inst, net) = e.local(g);
        let def = &e.design.defs[e.def_of(inst)];
        let driver = def.nets[net].driver.0;
        match def.nodes[driver].kind {
            NodeKind::Instance(_) => crossings.push(NodePath {
                instances: e.path(inst),
                node: driver,
            }),
            NodeKind::Input { .. } => {
                if let Some((parent, node)) = e.insts[inst as usize].parent {
                    crossings.push(NodePath {
                        instances: e.path(parent),
                        node,
                    });
                }
            }
            _ => prims.push(NodePath {
                instances: e.path(inst),
                node: driver,
            }),
        }
    }
    let mut out = if prims.is_empty() { crossings } else { prims };
    out.sort();
    out.dedup();
    out
}
