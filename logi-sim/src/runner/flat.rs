use crate::circuit::{FlattenedCircuit, NodeId, NodeKind, WireId};
use crate::runner::Runner;

/// Evaluates a Level 1 `FlattenedCircuit` using direct array index operations.
pub struct FlatRunner {
    circuit: FlattenedCircuit,
    wire_values: Vec<bool>,
}

impl FlatRunner {
    pub fn new(circuit: FlattenedCircuit) -> Self {
        let wire_count = circuit.wire_count;
        Self {
            circuit,
            wire_values: vec![false; wire_count],
        }
    }

    fn input_value(&self, node_idx: usize, pin: usize) -> bool {
        let node = &self.circuit.nodes[node_idx];
        if pin < node.inputs.len() {
            self.wire_values[node.inputs[pin]]
        } else {
            false
        }
    }

    fn set_output(&mut self, node_idx: usize, value: bool) {
        let node = &self.circuit.nodes[node_idx];
        for &wire_idx in &node.outputs {
            self.wire_values[wire_idx] = value;
        }
    }
}

impl Runner for FlatRunner {
    fn set_input(&mut self, node: NodeId, pin: u8, value: bool) {
        if let Some(&w_idx) = self
            .circuit
            .node_output_pins
            .get(&(node, pin))
            .and_then(|v| v.first())
        {
            self.wire_values[w_idx] = value;
        }
    }

    fn tick(&mut self) {
        for n_idx in 0..self.circuit.nodes.len() {
            let kind = &self.circuit.nodes[n_idx].kind;
            let output = match kind {
                NodeKind::Input | NodeKind::Output => continue,
                NodeKind::And => self.input_value(n_idx, 0) && self.input_value(n_idx, 1),
                NodeKind::Or => self.input_value(n_idx, 0) || self.input_value(n_idx, 1),
                NodeKind::Not => !self.input_value(n_idx, 0),
            };
            self.set_output(n_idx, output);
        }
    }

    fn output(&self, node: NodeId, pin: u8) -> bool {
        if let Some(&w_idx) = self.circuit.node_input_pins.get(&(node, pin)) {
            self.wire_values[w_idx]
        } else {
            false
        }
    }

    fn wire_value(&self, wire: WireId) -> bool {
        if let Some(&w_idx) = self.circuit.wire_to_idx.get(&wire) {
            self.wire_values[w_idx]
        } else {
            false
        }
    }

    fn for_each_wire(&self, f: &mut dyn FnMut(WireId, bool)) {
        for (w_idx, &wire_id) in self.circuit.idx_to_wire.iter().enumerate() {
            f(wire_id, self.wire_values[w_idx]);
        }
    }
}
