import re

with open("logi-sim/src/runner/naive.rs", "r") as f:
    content = f.read()

# Replace inspectable runner definition
content = content.replace("impl<'a> crate::runner::InspectableRunner for NaiveRunner<'a> {", "impl crate::runner::InspectableRunner for NaiveRunner {")

# To fix borrow errors, we will change evaluate_node to borrow node_ids
content = content.replace(
"""    fn tick(&mut self) {
        for (_, node) in &self.circuit.nodes {
            self.evaluate_node(node);
        }
        self.tick_count += 1;
    }""",
"""    fn tick(&mut self) {
        let node_ids: Vec<_> = self.circuit.nodes.keys().collect();
        for node_id in node_ids {
            self.evaluate_node(node_id);
        }
        self.tick_count += 1;
    }"""
)

# And evaluate_node needs to take a NodeId and perform lookups internally.
# But it's easier to just pass a clone of the Node if we can.
# Wait, Node has no clone.
# We can just change `evaluate_node` to work with node_id and then inline the `node = &self.circuit.nodes[node_id]` wherever needed.
# Since Rust allows `&mut self` to be borrowed if we only access disjoint fields. However, `self.input_value` and `self.set_output` only need `self.wire_values` and `self.circuit`. Wait, `set_output` mutates `self.wire_values`, but borrows `self.circuit`. This is fine because we don't mutate `self.circuit`.

# Let's replace the whole evaluate_node and related functions.

replacement = """
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
                phase,
            } => {
                let period = (*period_ticks).max(1) as u64;
                let elapsed = self.tick_count.wrapping_add(*phase as u64) % period;
                let val = elapsed < period / 2;
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
"""

start_idx = content.find("fn input_value")
end_idx = content.find("pub fn set_wire")

content = content[:start_idx] + replacement.lstrip() + "    " + content[end_idx:]

content = content.replace("""    fn set_input(&mut self, node: NodeId, pin: u8, value: bool) {
        let node = &self.circuit.nodes[node];
        // An Input node has 0 inputs and 1 output.
        // We want to force the value onto its output wires.
        self.set_output(node, pin as usize, value);
    }""", """    fn set_input(&mut self, node_id: NodeId, pin: u8, value: bool) {
        // An Input node has 0 inputs and 1 output.
        // We want to force the value onto its output wires.
        self.set_output(node_id, pin as usize, value);
    }""")

content = content.replace("""    fn output(&self, node: NodeId, pin: u8) -> bool {
        let node = &self.circuit.nodes[node];
        // An Output node has 1 input and 0 outputs.
        // We just read the wire connected to its input.
        self.input_value(node, pin as usize)
    }""", """    fn output(&self, node: NodeId, pin: u8) -> bool {
        // An Output node has 1 input and 0 outputs.
        // We just read the wire connected to its input.
        self.input_value(node, pin as usize)
    }""")

with open("logi-sim/src/runner/naive.rs", "w") as f:
    f.write(content)
