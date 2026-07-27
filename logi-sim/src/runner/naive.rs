use crate::prelude::*;
use crate::runner::Runner;
use slotmap::SecondaryMap;

pub struct NaiveRunner {
    circuit: Circuit,
    wire_values: SecondaryMap<WireId, bool>,
    tick_count: u64,
}

impl NaiveRunner {
    pub fn new(circuit: Circuit) -> Self {
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
    fn input_value(&self, node_id: NodeId, pin: usize) -> bool {
        let node = &self.circuit.nodes[node_id];
        node.inputs[pin]
            .wire
            .map(|w| self.wire_values[w])
            .unwrap_or(false)
    }
    fn set_output(&mut self, node_id: NodeId, pin: usize, value: bool) {
        let mut wires = Vec::new();
        {
            let node = &self.circuit.nodes[node_id];
            if let Some(output) = node.outputs.get(pin) {
                wires.extend(output.wires.iter().copied());
            }
        }
        for wire in wires {
            self.wire_values[wire] = value;
        }
    }
    fn evaluate_node(&mut self, node_id: NodeId) {
        let node_kind = self.circuit.nodes[node_id].kind.clone();
        match &node_kind {
            NodeKind::Input | NodeKind::Output => return, // no evaluation needed
            NodeKind::And => {
                let val = self.input_value(node_id, 0) && self.input_value(node_id, 1);
                self.set_output(node_id, 0, val);
            }
            NodeKind::Or => {
                let val = self.input_value(node_id, 0) || self.input_value(node_id, 1);
                self.set_output(node_id, 0, val);
            }
            NodeKind::Not => {
                let val = !self.input_value(node_id, 0);
                self.set_output(node_id, 0, val);
            }
            NodeKind::Xor => {
                let val = self.input_value(node_id, 0) ^ self.input_value(node_id, 1);
                self.set_output(node_id, 0, val);
            }
            NodeKind::Nand => {
                let val = !(self.input_value(node_id, 0) && self.input_value(node_id, 1));
                self.set_output(node_id, 0, val);
            }
            NodeKind::Nor => {
                let val = !(self.input_value(node_id, 0) || self.input_value(node_id, 1));
                self.set_output(node_id, 0, val);
            }
            NodeKind::Xnor => {
                let val = !(self.input_value(node_id, 0) ^ self.input_value(node_id, 1));
                self.set_output(node_id, 0, val);
            }
            NodeKind::Buffer => {
                let val = self.input_value(node_id, 0);
                self.set_output(node_id, 0, val);
            }
            NodeKind::Mux => {
                let d0 = self.input_value(node_id, 0);
                let d1 = self.input_value(node_id, 1);
                let sel = self.input_value(node_id, 2);
                let val = if sel { d1 } else { d0 };
                self.set_output(node_id, 0, val);
            }
            NodeKind::Demux => {
                let input_signal = self.input_value(node_id, 0);
                let sel = self.input_value(node_id, 1);
                if sel {
                    self.set_output(node_id, 0, false);
                    self.set_output(node_id, 1, input_signal);
                } else {
                    self.set_output(node_id, 0, input_signal);
                    self.set_output(node_id, 1, false);
                }
            }
            NodeKind::TriStateBuffer => {
                let input_signal = self.input_value(node_id, 0);
                let enable = self.input_value(node_id, 1);
                if enable {
                    self.set_output(node_id, 0, input_signal);
                } else {
                    self.set_output(node_id, 0, false);
                }
            }
            NodeKind::Clock {
                period_ticks,
                duty_ticks,
                phase,
            } => {
                let period = (*period_ticks).max(1) as u64;
                let elapsed = self.tick_count.wrapping_add(*phase as u64) % period;
                let val = elapsed < (*duty_ticks).max(1) as u64;
                self.set_output(node_id, 0, val);
            }

            NodeKind::SubCircuit(sub) => {
                let mut sub_runner = NaiveRunner::new((**sub).clone());

                // Map parent input pins to child Input nodes
                let input_nodes: Vec<NodeId> = sub
                    .nodes
                    .iter()
                    .filter(|(_, n)| matches!(n.kind, NodeKind::Input))
                    .map(|(id, _)| id)
                    .collect();

                let node_inputs_len = self.circuit.nodes[node_id].inputs.len();
                let node_outputs_len = self.circuit.nodes[node_id].outputs.len();

                for (pin_idx, &in_node_id) in input_nodes.iter().enumerate() {
                    if pin_idx < node_inputs_len {
                        let val = self.input_value(node_id, pin_idx);
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
                    if pin_idx < node_outputs_len {
                        let val = sub_runner.output(out_node_id, 0);
                        self.set_output(node_id, pin_idx, val);
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

impl Runner for NaiveRunner {
    fn set_input(&mut self, node_id: NodeId, pin: u8, value: bool) {
        // An Input node has 0 inputs and 1 output.
        // We want to force the value onto its output wires.
        self.set_output(node_id, pin as usize, value);
    }

    fn tick(&mut self) {
        let node_ids: Vec<_> = self.circuit.nodes.keys().collect();
        for node_id in node_ids {
            self.evaluate_node(node_id);
        }
        self.tick_count += 1;
    }

    fn output(&self, node: NodeId, pin: u8) -> bool {
        // An Output node has 1 input and 0 outputs.
        // We just read the wire connected to its input.
        self.input_value(node, pin as usize)
    }

    fn current_tick(&self) -> u64 {
        self.tick_count
    }
}

impl crate::runner::InspectableRunner for NaiveRunner {
    fn wire_value(&self, wire: WireId) -> bool {
        self.wire_values.get(wire).copied().unwrap_or(false)
    }

    fn for_each_wire(&self, f: &mut dyn FnMut(WireId, bool)) {
        for (wire, &val) in &self.wire_values {
            f(wire, val);
        }
    }
}
