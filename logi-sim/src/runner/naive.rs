use crate::prelude::*;
use crate::runner::Runner;
use slotmap::SecondaryMap;

pub struct NaiveRunner<'a> {
    circuit: &'a Circuit,
    wire_values: SecondaryMap<WireId, bool>,
    tick_count: u64,
}

impl<'a> NaiveRunner<'a> {
    pub fn new(circuit: &'a Circuit) -> Self {
        let mut wire_values = SecondaryMap::new();

        for (wire, _) in &circuit.wires {
            wire_values.insert(wire, false);
        }

        Self {
            circuit,
            wire_values,
            tick_count: 0,
        }
    }
    fn input_value(&self, node: &Node, pin: usize) -> bool {
        node.inputs[pin]
            .wire
            .map(|w| self.wire_values[w])
            .unwrap_or(false)
    }
    fn set_output(&mut self, node: &Node, pin: usize, value: bool) {
        for wire in &node.outputs[pin].wires {
            self.wire_values[*wire] = value;
        }
    }
    fn evaluate_node(&mut self, node: &Node) {
        match &node.kind {
            NodeKind::Input | NodeKind::Output => return, // no evaluation needed
            NodeKind::And => {
                let val = self.input_value(node, 0) && self.input_value(node, 1);
                self.set_output(node, 0, val);
            }
            NodeKind::Or => {
                let val = self.input_value(node, 0) || self.input_value(node, 1);
                self.set_output(node, 0, val);
            }
            NodeKind::Not => {
                let val = !self.input_value(node, 0);
                self.set_output(node, 0, val);
            }
            NodeKind::Xor => {
                let val = self.input_value(node, 0) ^ self.input_value(node, 1);
                self.set_output(node, 0, val);
            }
            NodeKind::Nand => {
                let val = !(self.input_value(node, 0) && self.input_value(node, 1));
                self.set_output(node, 0, val);
            }
            NodeKind::Nor => {
                let val = !(self.input_value(node, 0) || self.input_value(node, 1));
                self.set_output(node, 0, val);
            }
            NodeKind::Xnor => {
                let val = !(self.input_value(node, 0) ^ self.input_value(node, 1));
                self.set_output(node, 0, val);
            }
            NodeKind::Buffer => {
                let val = self.input_value(node, 0);
                self.set_output(node, 0, val);
            }
            NodeKind::Mux => {
                let d0 = self.input_value(node, 0);
                let d1 = self.input_value(node, 1);
                let sel = self.input_value(node, 2);
                let val = if sel { d1 } else { d0 };
                self.set_output(node, 0, val);
            }
            NodeKind::Demux => {
                let input_signal = self.input_value(node, 0);
                let sel = self.input_value(node, 1);
                if sel {
                    self.set_output(node, 0, false);
                    self.set_output(node, 1, input_signal);
                } else {
                    self.set_output(node, 0, input_signal);
                    self.set_output(node, 1, false);
                }
            }
            NodeKind::TriStateBuffer => {
                let input_signal = self.input_value(node, 0);
                let enable = self.input_value(node, 1);
                if enable {
                    self.set_output(node, 0, input_signal);
                } else {
                    self.set_output(node, 0, false);
                }
            }
            NodeKind::Clock {
                period_ticks,
                phase,
            } => {
                let period = (*period_ticks).max(1) as u64;
                let elapsed = self.tick_count.wrapping_add(*phase as u64) % period;
                let val = elapsed < period / 2;
                self.set_output(node, 0, val);
            }

            NodeKind::SubCircuit(sub) => {
                let mut sub_runner = NaiveRunner::new(sub);

                // Map parent input pins to child Input nodes
                let input_nodes: Vec<NodeId> = sub
                    .nodes
                    .iter()
                    .filter(|(_, n)| matches!(n.kind, NodeKind::Input))
                    .map(|(id, _)| id)
                    .collect();

                for (pin_idx, &in_node_id) in input_nodes.iter().enumerate() {
                    if pin_idx < node.inputs.len() {
                        let val = self.input_value(node, pin_idx);
                        sub_runner.set_input(in_node_id, 0, val);
                    }
                }

                sub_runner.tick();

                // Map child Output nodes to parent output pins
                let output_nodes: Vec<NodeId> = sub
                    .nodes
                    .iter()
                    .filter(|(_, n)| matches!(n.kind, NodeKind::Output))
                    .map(|(id, _)| id)
                    .collect();

                for (pin_idx, &out_node_id) in output_nodes.iter().enumerate() {
                    if pin_idx < node.outputs.len() {
                        let val = sub_runner.output(out_node_id, 0);
                        self.set_output(node, pin_idx, val);
                    }
                }
            }
        }
    }

    pub fn set_wire(&mut self, wire: WireId, value: bool) {
        self.wire_values[wire] = value;
    }

    pub fn wire(&self, wire: WireId) -> bool {
        self.wire_values[wire]
    }
}

impl<'a> Runner for NaiveRunner<'a> {
    fn set_input(&mut self, node: NodeId, pin: u8, value: bool) {
        let node = &self.circuit.nodes[node];
        // An Input node has 0 inputs and 1 output.
        // We want to force the value onto its output wires.
        self.set_output(node, pin as usize, value);
    }

    fn tick(&mut self) {
        for (_, node) in &self.circuit.nodes {
            self.evaluate_node(node);
        }
        self.tick_count += 1;
    }

    fn output(&self, node: NodeId, pin: u8) -> bool {
        let node = &self.circuit.nodes[node];
        // An Output node has 1 input and 0 outputs.
        // We just read the wire connected to its input.
        self.input_value(node, pin as usize)
    }

    fn current_tick(&self) -> u64 {
        self.tick_count
    }
}

impl<'a> crate::runner::InspectableRunner for NaiveRunner<'a> {
    fn wire_value(&self, wire: WireId) -> bool {
        self.wire_values.get(wire).copied().unwrap_or(false)
    }

    fn for_each_wire(&self, f: &mut dyn FnMut(WireId, bool)) {
        for (wire, &val) in &self.wire_values {
            f(wire, val);
        }
    }
}
