use crate::circuit::{EventDrivenCircuit, NodeId, NodeKind, WireId};
use crate::runner::{InspectableRunner, Runner};
use std::collections::VecDeque;

/// Evaluates a Level 2 `EventDrivenCircuit` using an event queue of active gates.
pub struct EventDrivenRunner {
    circuit: EventDrivenCircuit,
    wire_values: Vec<bool>,
    dirty_queue: VecDeque<usize>,
    in_queue: Vec<bool>,
}

impl EventDrivenRunner {
    pub fn new(circuit: EventDrivenCircuit) -> Self {
        let wire_count = circuit.wire_count;
        let node_count = circuit.nodes.len();

        let mut dirty_queue = VecDeque::with_capacity(node_count);
        let mut in_queue = vec![true; node_count];

        // Seed initial queue with all non-input/non-output logic nodes once on startup
        for (n_idx, node) in circuit.nodes.iter().enumerate() {
            if !matches!(node.kind, NodeKind::Input | NodeKind::Output) {
                dirty_queue.push_back(n_idx);
            } else {
                in_queue[n_idx] = false;
            }
        }

        let mut runner = Self {
            circuit,
            wire_values: vec![false; wire_count],
            dirty_queue,
            in_queue,
        };

        // Run initial tick to establish steady-state logic level outputs
        runner.tick();
        runner
    }

    #[inline]
    fn enqueue_node_static(in_queue: &mut [bool], dirty_queue: &mut VecDeque<usize>, n_idx: usize) {
        if n_idx < in_queue.len() && !in_queue[n_idx] {
            in_queue[n_idx] = true;
            dirty_queue.push_back(n_idx);
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

    fn evaluate_node(&self, node_idx: usize) -> bool {
        let kind = &self.circuit.nodes[node_idx].kind;
        match kind {
            NodeKind::Input | NodeKind::Output | NodeKind::SubCircuit(_) => false,
            NodeKind::And => self.input_value(node_idx, 0) && self.input_value(node_idx, 1),
            NodeKind::Or => self.input_value(node_idx, 0) || self.input_value(node_idx, 1),
            NodeKind::Not => !self.input_value(node_idx, 0),
            NodeKind::Xor => self.input_value(node_idx, 0) ^ self.input_value(node_idx, 1),
        }

    }
}

impl Runner for EventDrivenRunner {
    fn set_input(&mut self, node: NodeId, pin: u8, value: bool) {
        if let Some(w_indices) = self.circuit.node_output_pins.get(&(node, pin)) {
            for &w_idx in w_indices {
                if self.wire_values[w_idx] != value {
                    self.wire_values[w_idx] = value;
                    // Queue all nodes listening to this wire
                    for &listener_n_idx in &self.circuit.wire_listeners[w_idx] {
                        Self::enqueue_node_static(
                            &mut self.in_queue,
                            &mut self.dirty_queue,
                            listener_n_idx,
                        );
                    }
                }
            }
        }
    }

    fn tick(&mut self) {
        // Drain dirty queue until signal propagation stabilizes
        let mut iterations = 0;
        while let Some(n_idx) = self.dirty_queue.pop_front() {
            self.in_queue[n_idx] = false;
            let new_output = self.evaluate_node(n_idx);

            // Update outputs and enqueue listeners if output wire state toggled
            let outputs = &self.circuit.nodes[n_idx].outputs;
            for &out_w_idx in outputs {
                if self.wire_values[out_w_idx] != new_output {
                    self.wire_values[out_w_idx] = new_output;
                    for &listener_n_idx in &self.circuit.wire_listeners[out_w_idx] {
                        Self::enqueue_node_static(
                            &mut self.in_queue,
                            &mut self.dirty_queue,
                            listener_n_idx,
                        );
                    }
                }
            }

            iterations += 1;
            if iterations > 10_000 {
                // Safety guard against infinite feedback loops per tick
                break;
            }
        }
    }



    fn output(&self, node: NodeId, pin: u8) -> bool {
        if let Some(&w_idx) = self.circuit.node_input_pins.get(&(node, pin)) {
            self.wire_values[w_idx]
        } else {
            false
        }
    }
}

impl InspectableRunner for EventDrivenRunner {
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
