use crate::prelude::*;
use crate::runner::Runner;
use slotmap::SecondaryMap;

pub struct NaiveRunner<'a> {
    circuit: &'a Circuit,
    wire_values: SecondaryMap<WireId, bool>,
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
        let output = match node.kind {
            NodeKind::Input | NodeKind::Output => return, // no evaluation needed
            NodeKind::And => self.input_value(node, 0) && self.input_value(node, 1),

            NodeKind::Or => self.input_value(node, 0) || self.input_value(node, 1),

            NodeKind::Not => !self.input_value(node, 0),
        };

        self.set_output(node, 0, output);
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
    }

    fn output(&self, node: NodeId, pin: u8) -> bool {
        let node = &self.circuit.nodes[node];
        // An Output node has 1 input and 0 outputs.
        // We just read the wire connected to its input.
        self.input_value(node, pin as usize)
    }
}
