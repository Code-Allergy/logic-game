//! Authoring IR for simulation v2 (design doc `docs/sim-v2-design.md`,
//! sections 3.3 and 4).
//!
//! A [`Design`] is a set of [`Definition`]s plus a root. A definition is a
//! graph of [`Node`]s connected by [`Net`]s. Every output pin owns exactly one
//! net, created with the node; fan-out is many input pins naming the same
//! [`NetId`]. Input pins are fixed-position `Option<NetId>` slots, so an
//! unconnected pin (`None`, reads 0) never shifts its neighbours.
//!
//! All mutation goes through [`Design`] methods, which keep nets, drivers,
//! instance pins and each definition's `revision` consistent. The fields are
//! public for reading; code that writes them directly should run
//! [`Design::validate`] afterwards.

mod builder;
mod kind;
mod validate;

#[cfg(test)]
mod tests;

pub use builder::Builder;
pub use kind::{
    CombDeps, MAX_ADDR_BITS, MAX_READ_PORTS, MAX_SCREEN_BITS, MAX_WIDTH, NodeKind, PinWidths,
    ScreenMode, mask,
};
pub use validate::ValidationError;

use slotmap::{SecondaryMap, SlotMap, new_key_type};
use smallvec::SmallVec;
use std::fmt;

new_key_type! {
    /// Identifies a [`Definition`] within a [`Design`].
    pub struct DefId;
    /// Identifies a [`Node`] within one [`Definition`].
    pub struct NodeId;
    /// Identifies a [`Net`] within one [`Definition`].
    pub struct NetId;
}

/// A node input pin: `(node, pin index)`. An editor "wire" is identified by
/// its sink pin.
pub type PinRef = (NodeId, u8);

/// A whole design: shared definitions plus the root definition.
#[derive(Debug, Clone)]
pub struct Design {
    pub defs: SlotMap<DefId, Definition>,
    pub root: DefId,
}

/// One circuit definition. Instances reference it by [`DefId`]; it is never
/// copied per instance.
#[derive(Debug, Clone)]
pub struct Definition {
    pub name: String,
    pub nodes: SlotMap<NodeId, Node>,
    pub nets: SlotMap<NetId, Net>,
    /// Input port nodes (`NodeKind::Input`), in port order.
    pub inputs: Vec<NodeId>,
    /// Output port nodes (`NodeKind::Output`), in port order.
    pub outputs: Vec<NodeId>,
    /// Bumped on every mutation.
    pub revision: u64,
    /// Optional node names (side table; no per-pin strings).
    pub names: SecondaryMap<NodeId, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    pub kind: NodeKind,
    /// `None` = unconnected = reads 0.
    pub inputs: SmallVec<[Option<NetId>; 4]>,
    /// Output pin `i` drives `outputs[i]`.
    pub outputs: SmallVec<[NetId; 2]>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Net {
    pub width: u8,
    /// The driving output pin.
    pub driver: (NodeId, u8),
}

/// Errors from the mutation API.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NetlistError {
    NoSuchDefinition(DefId),
    NoSuchNode(NodeId),
    NoSuchNet(NetId),
    /// The pin index is past the node's pin count.
    PinOutOfRange {
        node: NodeId,
        pin: u8,
        count: usize,
    },
    /// Driver and sink widths differ.
    WidthMismatch {
        net: NetId,
        net_width: u8,
        pin_width: u8,
    },
    /// The kind's parameters are invalid.
    BadParams {
        reason: String,
    },
    /// `NodeKind::Instance` must be added with [`Design::add_instance`].
    UseAddInstance,
    /// Instancing `child` inside `parent` would make a definition contain
    /// itself.
    InstanceRecursion {
        parent: DefId,
        child: DefId,
    },
    /// The definition is still instanced somewhere.
    DefinitionInUse {
        def: DefId,
        by: Vec<(DefId, NodeId)>,
    },
    /// The root definition cannot be removed.
    RemoveRoot,
}

impl fmt::Display for NetlistError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for NetlistError {}

pub type Result<T, E = NetlistError> = std::result::Result<T, E>;

// ─────────────────────────────────────────────────────────────────────────────
// Definition (read-only API plus private helpers)
// ─────────────────────────────────────────────────────────────────────────────

impl Definition {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            nodes: SlotMap::with_key(),
            nets: SlotMap::with_key(),
            inputs: Vec::new(),
            outputs: Vec::new(),
            revision: 0,
            names: SecondaryMap::new(),
        }
    }

    /// The net driven by output pin `pin` of `node`.
    pub fn output_net(&self, node: NodeId, pin: u8) -> Option<NetId> {
        self.nodes.get(node)?.outputs.get(pin as usize).copied()
    }

    /// The net connected to input pin `pin` of `node`, if any.
    pub fn input_net(&self, node: NodeId, pin: u8) -> Option<NetId> {
        *self.nodes.get(node)?.inputs.get(pin as usize)?
    }

    /// Every input pin connected to `net`. Linear in the definition size.
    pub fn sinks(&self, net: NetId) -> Vec<PinRef> {
        let mut out = Vec::new();
        for (id, node) in &self.nodes {
            for (pin, n) in node.inputs.iter().enumerate() {
                if *n == Some(net) {
                    out.push((id, pin as u8));
                }
            }
        }
        out
    }

    pub fn name_of(&self, node: NodeId) -> Option<&str> {
        self.names.get(node).map(String::as_str)
    }

    /// Find a node by its side-table name.
    pub fn find(&self, name: &str) -> Option<NodeId> {
        self.names
            .iter()
            .find(|(_, n)| n.as_str() == name)
            .map(|(id, _)| id)
    }

    /// Index of the named input port.
    pub fn input_port(&self, name: &str) -> Option<usize> {
        self.inputs
            .iter()
            .position(|&n| self.name_of(n) == Some(name))
    }

    /// Index of the named output port.
    pub fn output_port(&self, name: &str) -> Option<usize> {
        self.outputs
            .iter()
            .position(|&n| self.name_of(n) == Some(name))
    }

    /// Port widths `(inputs, outputs)` in port order.
    pub fn signature(&self) -> (PinWidths, PinWidths) {
        let width = |id: &NodeId| match self.nodes.get(*id).map(|n| &n.kind) {
            Some(NodeKind::Input { w }) | Some(NodeKind::Output { w }) => *w,
            _ => 0,
        };
        (
            self.inputs.iter().map(width).collect(),
            self.outputs.iter().map(width).collect(),
        )
    }

    fn insert_node(&mut self, kind: NodeKind, n_inputs: usize, out_widths: &[u8]) -> NodeId {
        let id = self.nodes.insert(Node {
            kind,
            inputs: SmallVec::from_elem(None, n_inputs),
            outputs: SmallVec::new(),
        });
        for (pin, &w) in out_widths.iter().enumerate() {
            let net = self.nets.insert(Net {
                width: w,
                driver: (id, pin as u8),
            });
            self.nodes[id].outputs.push(net);
        }
        match self.nodes[id].kind {
            NodeKind::Input { .. } => self.inputs.push(id),
            NodeKind::Output { .. } => self.outputs.push(id),
            _ => {}
        }
        self.revision += 1;
        id
    }

    /// Set every input pin reading `net` to unconnected.
    fn disconnect_sinks(&mut self, net: NetId) {
        for (_, node) in self.nodes.iter_mut() {
            for slot in node.inputs.iter_mut() {
                if *slot == Some(net) {
                    *slot = None;
                }
            }
        }
    }

    /// Remove output pin `pin` of `node` and its net, shifting later output
    /// pins down. Sinks of the removed net become unconnected.
    fn remove_output_pin(&mut self, node: NodeId, pin: usize) {
        let net = self.nodes[node].outputs.remove(pin);
        self.disconnect_sinks(net);
        self.nets.remove(net);
        for (i, &n) in self.nodes[node].outputs.iter().enumerate().skip(pin) {
            self.nets[n].driver.1 = i as u8;
        }
    }

    fn append_output_pin(&mut self, node: NodeId, width: u8) {
        let pin = self.nodes[node].outputs.len() as u8;
        let net = self.nets.insert(Net {
            width,
            driver: (node, pin),
        });
        self.nodes[node].outputs.push(net);
    }

    fn instances_of(&self, child: DefId) -> Vec<NodeId> {
        self.nodes
            .iter()
            .filter(|(_, n)| n.kind == NodeKind::Instance(child))
            .map(|(id, _)| id)
            .collect()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Design: construction and mutation
// ─────────────────────────────────────────────────────────────────────────────

impl Design {
    /// A design with a single, empty root definition.
    pub fn new(root_name: impl Into<String>) -> Self {
        let mut defs = SlotMap::with_key();
        let root = defs.insert(Definition::new(root_name));
        Self { defs, root }
    }

    pub fn root_def(&self) -> &Definition {
        &self.defs[self.root]
    }

    /// Primitive nodes of `def` with every instance expanded, ports not
    /// counted: the size of the design after flattening. Panics if `def`
    /// or a definition it instances does not exist, or on an instance
    /// cycle.
    pub fn flat_primitives(&self, def: DefId) -> u64 {
        fn count(d: &Design, def: DefId, memo: &mut SecondaryMap<DefId, u64>, depth: usize) -> u64 {
            assert!(depth <= d.defs.len(), "instance cycle");
            if let Some(&n) = memo.get(def) {
                return n;
            }
            let n = d.defs[def]
                .nodes
                .values()
                .map(|n| match n.kind {
                    NodeKind::Instance(child) => count(d, child, memo, depth + 1),
                    NodeKind::Input { .. } | NodeKind::Output { .. } => 0,
                    _ => 1,
                })
                .sum();
            memo.insert(def, n);
            n
        }
        count(self, def, &mut SecondaryMap::new(), 0)
    }

    pub fn def(&self, id: DefId) -> Result<&Definition> {
        self.defs.get(id).ok_or(NetlistError::NoSuchDefinition(id))
    }

    fn def_mut(&mut self, id: DefId) -> Result<&mut Definition> {
        self.defs
            .get_mut(id)
            .ok_or(NetlistError::NoSuchDefinition(id))
    }

    /// A builder for adding logic to `def`. Panics if `def` does not exist.
    pub fn builder(&mut self, def: DefId) -> Builder<'_> {
        Builder::new(self, def)
    }

    /// Add a new, empty definition.
    pub fn add_definition(&mut self, name: impl Into<String>) -> DefId {
        self.defs.insert(Definition::new(name))
    }

    /// Remove a definition that is not the root and is not instanced anywhere.
    pub fn remove_definition(&mut self, id: DefId) -> Result<Definition> {
        if id == self.root {
            return Err(NetlistError::RemoveRoot);
        }
        self.def(id)?;
        let by: Vec<_> = self
            .defs
            .iter()
            .flat_map(|(d, def)| def.instances_of(id).into_iter().map(move |n| (d, n)))
            .collect();
        if !by.is_empty() {
            return Err(NetlistError::DefinitionInUse { def: id, by });
        }
        Ok(self.defs.remove(id).expect("checked above"))
    }

    /// Pin widths `(inputs, outputs)` of a node, resolving instances.
    pub fn pin_widths(&self, def: DefId, node: NodeId) -> Result<(PinWidths, PinWidths)> {
        let n = self
            .def(def)?
            .nodes
            .get(node)
            .ok_or(NetlistError::NoSuchNode(node))?;
        match &n.kind {
            NodeKind::Instance(child) => Ok(self.def(*child)?.signature()),
            k => Ok(k.pins().expect("non-instance kinds have fixed pins")),
        }
    }

    /// Add a primitive node. Port nodes (`Input`/`Output`) are appended to
    /// the definition's port list, and a matching pin is appended to every
    /// instance of the definition.
    pub fn add_node(&mut self, def: DefId, kind: NodeKind) -> Result<NodeId> {
        if matches!(kind, NodeKind::Instance(_)) {
            return Err(NetlistError::UseAddInstance);
        }
        kind.check_params()
            .map_err(|reason| NetlistError::BadParams { reason })?;
        let (ins, outs) = kind.pins().expect("non-instance");
        let port = match kind {
            NodeKind::Input { .. } => Some((true, 0)),
            NodeKind::Output { w } => Some((false, w)),
            _ => None,
        };
        let id = self.def_mut(def)?.insert_node(kind, ins.len(), &outs);
        if let Some((is_input, w)) = port {
            self.for_each_instance_of(def, |parent, inst| {
                if is_input {
                    parent.nodes[inst].inputs.push(None);
                } else {
                    parent.append_output_pin(inst, w);
                }
            });
        }
        Ok(id)
    }

    /// Add an instance of `child` inside `parent`. Its pins follow `child`'s
    /// current ports, all unconnected.
    pub fn add_instance(&mut self, parent: DefId, child: DefId) -> Result<NodeId> {
        self.def(parent)?;
        self.def(child)?;
        if parent == child || self.def_reaches(child, parent) {
            return Err(NetlistError::InstanceRecursion { parent, child });
        }
        let (ins, outs) = self.defs[child].signature();
        Ok(self.defs[parent].insert_node(NodeKind::Instance(child), ins.len(), &outs))
    }

    /// Remove a node. Its output nets are deleted and their sinks become
    /// unconnected. Removing a port node removes that pin from every
    /// instance of the definition; the remaining pins keep their
    /// connections and their relative order.
    pub fn remove_node(&mut self, def: DefId, node: NodeId) -> Result<Node> {
        let d = self.def_mut(def)?;
        if !d.nodes.contains_key(node) {
            return Err(NetlistError::NoSuchNode(node));
        }
        let outs: SmallVec<[NetId; 2]> = d.nodes[node].outputs.clone();
        for net in outs {
            d.disconnect_sinks(net);
            d.nets.remove(net);
        }
        let removed = d.nodes.remove(node).expect("checked above");
        d.names.remove(node);
        d.revision += 1;
        let in_port = d.inputs.iter().position(|&n| n == node);
        let out_port = d.outputs.iter().position(|&n| n == node);
        if let Some(i) = in_port {
            d.inputs.remove(i);
            self.for_each_instance_of(def, |parent, inst| {
                if i < parent.nodes[inst].inputs.len() {
                    parent.nodes[inst].inputs.remove(i);
                }
            });
        }
        if let Some(i) = out_port {
            d_outputs_remove(self, def, i);
        }
        Ok(removed)
    }

    /// Connect input pin `sink` to `net`, replacing any previous connection.
    pub fn connect(&mut self, def: DefId, net: NetId, sink: PinRef) -> Result<()> {
        let (ins, _) = self.pin_widths(def, sink.0)?;
        let d = self.def_mut(def)?;
        let net_width = d.nets.get(net).ok_or(NetlistError::NoSuchNet(net))?.width;
        let pin_width = *ins
            .get(sink.1 as usize)
            .ok_or(NetlistError::PinOutOfRange {
                node: sink.0,
                pin: sink.1,
                count: ins.len(),
            })?;
        if net_width != pin_width {
            return Err(NetlistError::WidthMismatch {
                net,
                net_width,
                pin_width,
            });
        }
        let slot =
            d.nodes[sink.0]
                .inputs
                .get_mut(sink.1 as usize)
                .ok_or(NetlistError::PinOutOfRange {
                    node: sink.0,
                    pin: sink.1,
                    count: ins.len(),
                })?;
        *slot = Some(net);
        d.revision += 1;
        Ok(())
    }

    /// Connect output pin `src` to input pin `sink`.
    pub fn connect_pins(&mut self, def: DefId, src: PinRef, sink: PinRef) -> Result<()> {
        let d = self.def(def)?;
        let node = d.nodes.get(src.0).ok_or(NetlistError::NoSuchNode(src.0))?;
        let net = *node
            .outputs
            .get(src.1 as usize)
            .ok_or(NetlistError::PinOutOfRange {
                node: src.0,
                pin: src.1,
                count: node.outputs.len(),
            })?;
        self.connect(def, net, sink)
    }

    /// Disconnect input pin `sink`, returning the net it was connected to.
    pub fn disconnect(&mut self, def: DefId, sink: PinRef) -> Result<Option<NetId>> {
        let d = self.def_mut(def)?;
        let node = d
            .nodes
            .get_mut(sink.0)
            .ok_or(NetlistError::NoSuchNode(sink.0))?;
        let count = node.inputs.len();
        let slot = node
            .inputs
            .get_mut(sink.1 as usize)
            .ok_or(NetlistError::PinOutOfRange {
                node: sink.0,
                pin: sink.1,
                count,
            })?;
        let old = slot.take();
        d.revision += 1;
        Ok(old)
    }

    /// Set or clear a node's name in the side table.
    pub fn set_name(&mut self, def: DefId, node: NodeId, name: Option<String>) -> Result<()> {
        let d = self.def_mut(def)?;
        if !d.nodes.contains_key(node) {
            return Err(NetlistError::NoSuchNode(node));
        }
        match name {
            Some(n) => {
                d.names.insert(node, n);
            }
            None => {
                d.names.remove(node);
            }
        }
        d.revision += 1;
        Ok(())
    }

    /// Does `from` (transitively) instance `to`?
    pub fn def_reaches(&self, from: DefId, to: DefId) -> bool {
        let mut stack = vec![from];
        let mut seen = SecondaryMap::<DefId, ()>::new();
        while let Some(d) = stack.pop() {
            if d == to {
                return true;
            }
            if seen.insert(d, ()).is_some() {
                continue;
            }
            if let Some(def) = self.defs.get(d) {
                for node in def.nodes.values() {
                    if let NodeKind::Instance(c) = node.kind {
                        stack.push(c);
                    }
                }
            }
        }
        false
    }

    /// Definitions reachable from the root (root included), children before
    /// parents. Assumes no instance recursion.
    pub fn defs_bottom_up(&self) -> Vec<DefId> {
        let mut order = Vec::new();
        let mut state = SecondaryMap::<DefId, bool>::new(); // false = open, true = done
        let mut stack = vec![(self.root, false)];
        while let Some((d, expanded)) = stack.pop() {
            if expanded {
                if state.get(d) != Some(&true) {
                    state.insert(d, true);
                    order.push(d);
                }
                continue;
            }
            if state.contains_key(d) || !self.defs.contains_key(d) {
                continue;
            }
            state.insert(d, false);
            stack.push((d, true));
            for node in self.defs[d].nodes.values() {
                if let NodeKind::Instance(c) = node.kind
                    && !state.contains_key(c)
                {
                    stack.push((c, false));
                }
            }
        }
        order
    }

    fn for_each_instance_of(&mut self, child: DefId, mut f: impl FnMut(&mut Definition, NodeId)) {
        let parents: Vec<DefId> = self.defs.keys().collect();
        for p in parents {
            let insts = self.defs[p].instances_of(child);
            if insts.is_empty() {
                continue;
            }
            let parent = &mut self.defs[p];
            for inst in insts {
                f(parent, inst);
            }
            parent.revision += 1;
        }
    }
}

/// Remove output port `i` of `def` from every instance of it.
fn d_outputs_remove(design: &mut Design, def: DefId, i: usize) {
    design.defs[def].outputs.remove(i);
    design.for_each_instance_of(def, |parent, inst| {
        if i < parent.nodes[inst].outputs.len() {
            parent.remove_output_pin(inst, i);
        }
    });
}
