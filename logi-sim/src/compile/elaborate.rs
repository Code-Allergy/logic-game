//! Pass 2: elaboration. Expands the instance tree (iteratively, breadth
//! first) and gives every authored net in every instance a dense global
//! index. Port nets are unified with the parent's nets by union-find, so no
//! copy instructions exist for ports.

use slotmap::SecondaryMap;

use super::program::InstanceInfo;
use crate::netlist::{DefId, Design, NetId, NodeId, NodeKind};

pub(super) struct Elab<'d> {
    pub design: &'d Design,
    /// Instance 0 is the root; parents come before children.
    pub insts: Vec<InstanceInfo>,
    /// First global net index of each instance.
    pub net_base: Vec<u32>,
    /// Dense local index of each net, per definition.
    pub net_index: SecondaryMap<DefId, SecondaryMap<NetId, u32>>,
    /// Local index -> NetId, per definition.
    pub def_nets: SecondaryMap<DefId, Vec<NetId>>,
    pub n_nets: u32,
    /// Directed port-crossing edges `(from, to)` between global nets: parent
    /// net -> child input-port net, and child output-port net -> parent net.
    /// The loop check needs them; union-find loses direction.
    pub alias_edges: Vec<(u32, u32)>,
    uf: Vec<u32>,
}

impl<'d> Elab<'d> {
    pub fn new(design: &'d Design) -> Self {
        let mut net_index = SecondaryMap::new();
        let mut def_nets = SecondaryMap::new();
        for d in design.defs_bottom_up() {
            let nets: Vec<NetId> = design.defs[d].nets.keys().collect();
            let index = nets
                .iter()
                .enumerate()
                .map(|(i, &n)| (n, i as u32))
                .collect();
            net_index.insert(d, index);
            def_nets.insert(d, nets);
        }

        let root = design.root;
        let mut e = Elab {
            design,
            insts: vec![InstanceInfo {
                def: root,
                parent: None,
            }],
            net_base: vec![0],
            n_nets: def_nets[root].len() as u32,
            net_index,
            def_nets,
            alias_edges: Vec::new(),
            uf: Vec::new(),
        };

        let mut i = 0;
        while i < e.insts.len() {
            let def = &design.defs[e.insts[i].def];
            for (id, node) in &def.nodes {
                if let NodeKind::Instance(child) = node.kind {
                    e.insts.push(InstanceInfo {
                        def: child,
                        parent: Some((i as u32, id)),
                    });
                    e.net_base.push(e.n_nets);
                    e.n_nets = e
                        .n_nets
                        .checked_add(e.def_nets[child].len() as u32)
                        .expect("more than 2^32 nets");
                }
            }
            i += 1;
        }

        e.uf = (0..e.n_nets).collect();
        for child in 1..e.insts.len() {
            let (parent, node_id) = e.insts[child].parent.expect("non-root");
            let pdef = &design.defs[e.insts[parent as usize].def];
            let cdef = &design.defs[e.insts[child].def];
            let node = &pdef.nodes[node_id];
            for (pin, &port) in cdef.inputs.iter().enumerate() {
                let inner = cdef.nodes[port].outputs[0];
                if let Some(outer) = node.inputs[pin] {
                    let (o, i) = (e.global(parent, outer), e.global(child as u32, inner));
                    e.alias_edges.push((o, i));
                    e.union(o, i);
                }
            }
            for (pin, &port) in cdef.outputs.iter().enumerate() {
                let outer = node.outputs[pin];
                if let Some(inner) = cdef.nodes[port].inputs[0] {
                    let (i, o) = (e.global(child as u32, inner), e.global(parent, outer));
                    e.alias_edges.push((i, o));
                    e.union(i, o);
                }
            }
        }
        e
    }

    pub fn def_of(&self, inst: u32) -> DefId {
        self.insts[inst as usize].def
    }

    pub fn global(&self, inst: u32, net: NetId) -> u32 {
        let def = self.def_of(inst);
        self.net_base[inst as usize] + self.net_index[def][net]
    }

    /// `(instance, NetId)` of a global net.
    pub fn local(&self, g: u32) -> (u32, NetId) {
        // The last instance whose base is <= g contains it (instances with no
        // nets share a base with the next one).
        let inst = self.net_base.partition_point(|&b| b <= g) - 1;
        let def = self.def_of(inst as u32);
        (
            inst as u32,
            self.def_nets[def][(g - self.net_base[inst]) as usize],
        )
    }

    pub fn find(&mut self, mut x: u32) -> u32 {
        while self.uf[x as usize] != x {
            let p = self.uf[x as usize];
            self.uf[x as usize] = self.uf[p as usize];
            x = p;
        }
        x
    }

    fn union(&mut self, a: u32, b: u32) {
        let (ra, rb) = (self.find(a), self.find(b));
        if ra != rb {
            // Keep the smaller index as root: deterministic, and tends to
            // pick the outermost net.
            let (lo, hi) = if ra < rb { (ra, rb) } else { (rb, ra) };
            self.uf[hi as usize] = lo;
        }
    }

    /// Instance node path from the root to `inst`.
    pub fn path(&self, mut inst: u32) -> Vec<NodeId> {
        let mut path = Vec::new();
        while let Some((p, n)) = self.insts[inst as usize].parent {
            path.push(n);
            inst = p;
        }
        path.reverse();
        path
    }
}
