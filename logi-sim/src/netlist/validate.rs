//! Structural validation: widths, pin counts, net bookkeeping, ports and
//! instance recursion. Combinational loops are not checked here; that is the
//! compiler's (and the reference interpreter's) job.

use slotmap::SecondaryMap;

use super::{DefId, Design, NetId, NodeId, NodeKind};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidationError {
    /// `Design::root` names no definition.
    MissingRoot,
    /// The node's kind parameters are invalid.
    BadParams {
        def: DefId,
        node: NodeId,
        reason: String,
    },
    /// An instance names a definition that does not exist.
    MissingDefinition {
        def: DefId,
        node: NodeId,
        target: DefId,
    },
    /// The node has the wrong number of input or output pins for its kind
    /// (e.g. an instance whose definition's ports changed underneath it).
    PinCount {
        def: DefId,
        node: NodeId,
        expected_inputs: usize,
        inputs: usize,
        expected_outputs: usize,
        outputs: usize,
    },
    /// An input pin is connected to a net of a different width.
    WidthMismatch {
        def: DefId,
        node: NodeId,
        pin: u8,
        expected: u8,
        actual: u8,
    },
    /// An output pin's net has the wrong width.
    NetWidth {
        def: DefId,
        net: NetId,
        expected: u8,
        actual: u8,
    },
    /// An input pin names a net that does not exist in this definition.
    DanglingNet {
        def: DefId,
        node: NodeId,
        pin: u8,
        net: NetId,
    },
    /// A net's recorded driver does not drive it (or does not exist).
    BadDriver { def: DefId, net: NetId },
    /// A port list entry is missing, of the wrong kind, or duplicated, or a
    /// port node is missing from its port list.
    BadPort { def: DefId, node: NodeId },
    /// A definition contains itself, directly or through other definitions.
    /// `cycle` lists the definitions on the cycle.
    InstanceRecursion { cycle: Vec<DefId> },
}

impl Design {
    /// Check the whole design. Returns every problem found.
    pub fn validate(&self) -> Result<(), Vec<ValidationError>> {
        let mut errs = Vec::new();
        if !self.defs.contains_key(self.root) {
            errs.push(ValidationError::MissingRoot);
        }
        for (def_id, _) in &self.defs {
            self.validate_def(def_id, &mut errs);
        }
        if let Some(cycle) = self.find_instance_cycle() {
            errs.push(ValidationError::InstanceRecursion { cycle });
        }
        if errs.is_empty() { Ok(()) } else { Err(errs) }
    }

    fn validate_def(&self, def_id: DefId, errs: &mut Vec<ValidationError>) {
        use ValidationError as E;
        let def = &self.defs[def_id];

        for (id, node) in &def.nodes {
            if let Err(reason) = node.kind.check_params() {
                errs.push(E::BadParams {
                    def: def_id,
                    node: id,
                    reason,
                });
                continue;
            }
            let (ins, outs) = match &node.kind {
                NodeKind::Instance(target) => match self.defs.get(*target) {
                    Some(t) => t.signature(),
                    None => {
                        errs.push(E::MissingDefinition {
                            def: def_id,
                            node: id,
                            target: *target,
                        });
                        continue;
                    }
                },
                k => k.pins().expect("non-instance"),
            };
            if ins.len() != node.inputs.len() || outs.len() != node.outputs.len() {
                errs.push(E::PinCount {
                    def: def_id,
                    node: id,
                    expected_inputs: ins.len(),
                    inputs: node.inputs.len(),
                    expected_outputs: outs.len(),
                    outputs: node.outputs.len(),
                });
                continue;
            }
            for (pin, (slot, &w)) in node.inputs.iter().zip(ins.iter()).enumerate() {
                let Some(net) = *slot else { continue };
                match def.nets.get(net) {
                    None => errs.push(E::DanglingNet {
                        def: def_id,
                        node: id,
                        pin: pin as u8,
                        net,
                    }),
                    Some(n) if n.width != w => errs.push(E::WidthMismatch {
                        def: def_id,
                        node: id,
                        pin: pin as u8,
                        expected: w,
                        actual: n.width,
                    }),
                    Some(_) => {}
                }
            }
            for (pin, (&net, &w)) in node.outputs.iter().zip(outs.iter()).enumerate() {
                match def.nets.get(net) {
                    Some(n) if n.driver == (id, pin as u8) => {
                        if n.width != w {
                            errs.push(E::NetWidth {
                                def: def_id,
                                net,
                                expected: w,
                                actual: n.width,
                            });
                        }
                    }
                    _ => errs.push(E::BadDriver { def: def_id, net }),
                }
            }
        }

        for (net_id, net) in &def.nets {
            let ok = def
                .nodes
                .get(net.driver.0)
                .and_then(|n| n.outputs.get(net.driver.1 as usize))
                == Some(&net_id);
            if !ok {
                errs.push(E::BadDriver {
                    def: def_id,
                    net: net_id,
                });
            }
        }

        // Ports: every list entry is a port node of the right kind, listed
        // once, and every port node is listed.
        let mut listed = SecondaryMap::<NodeId, ()>::new();
        for (list, want_input) in [(&def.inputs, true), (&def.outputs, false)] {
            for &n in list {
                let kind_ok = match def.nodes.get(n).map(|n| &n.kind) {
                    Some(NodeKind::Input { .. }) => want_input,
                    Some(NodeKind::Output { .. }) => !want_input,
                    _ => false,
                };
                if !kind_ok || listed.insert(n, ()).is_some() {
                    errs.push(E::BadPort {
                        def: def_id,
                        node: n,
                    });
                }
            }
        }
        for (id, node) in &def.nodes {
            let is_port = matches!(node.kind, NodeKind::Input { .. } | NodeKind::Output { .. });
            if is_port && !listed.contains_key(id) {
                errs.push(E::BadPort {
                    def: def_id,
                    node: id,
                });
            }
        }
    }

    /// Iterative DFS over the instance graph; returns one cycle if any.
    fn find_instance_cycle(&self) -> Option<Vec<DefId>> {
        #[derive(Clone, Copy, PartialEq)]
        enum Mark {
            Open,
            Done,
        }
        let children = |d: DefId| -> Vec<DefId> {
            self.defs[d]
                .nodes
                .values()
                .filter_map(|n| match n.kind {
                    NodeKind::Instance(c) if self.defs.contains_key(c) => Some(c),
                    _ => None,
                })
                .collect()
        };
        let mut mark = SecondaryMap::<DefId, Mark>::new();
        for start in self.defs.keys() {
            if mark.contains_key(start) {
                continue;
            }
            // Stack of (def, its children, next child index); `path` mirrors it.
            let mut stack = vec![(start, children(start), 0usize)];
            mark.insert(start, Mark::Open);
            while let Some((d, kids, i)) = stack.last_mut() {
                if *i == kids.len() {
                    mark.insert(*d, Mark::Done);
                    stack.pop();
                    continue;
                }
                let c = kids[*i];
                *i += 1;
                match mark.get(c) {
                    Some(Mark::Open) => {
                        let pos = stack.iter().position(|(d, _, _)| *d == c).expect("open");
                        return Some(stack[pos..].iter().map(|(d, _, _)| *d).collect());
                    }
                    Some(Mark::Done) => {}
                    None => {
                        mark.insert(c, Mark::Open);
                        let k = children(c);
                        stack.push((c, k, 0));
                    }
                }
            }
        }
        None
    }
}
