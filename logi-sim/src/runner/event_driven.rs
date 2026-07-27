use crate::circuit::{EventDrivenCircuit, NodeId, NodeKind, WireId};
use crate::runner::{InspectableRunner, Runner};
use std::collections::VecDeque;

/// Evaluates a Level 2 `EventDrivenCircuit` using an event queue of active gates.
pub struct EventDrivenRunner {
    circuit: EventDrivenCircuit,
    wire_values: Vec<bool>,
    dirty_queue: VecDeque<usize>,
    in_queue: Vec<bool>,
    /// Node indices for `NodeKind::Clock` nodes, which are time-driven rather than
    /// purely event-driven and must be re-evaluated every tick.
    clock_nodes: Vec<usize>,
    tick_count: u64,
}

impl EventDrivenRunner {
    pub fn new(circuit: EventDrivenCircuit) -> Self {
        let wire_count = circuit.wire_count;
        let node_count = circuit.nodes.len();

        let mut dirty_queue = VecDeque::with_capacity(node_count);
        let mut in_queue = vec![true; node_count];

        // Seed initial queue with all non-input/non-output logic nodes once on startup
        for (n_idx, node) in circuit.nodes.iter().enumerate() {
            if !matches!(node.kind, NodeKind::Input { .. } | NodeKind::Output { .. }) {
                dirty_queue.push_back(n_idx);
            } else {
                in_queue[n_idx] = false;
            }
        }

        let clock_nodes: Vec<usize> = circuit
            .nodes
            .iter()
            .enumerate()
            .filter(|(_, node)| matches!(node.kind, NodeKind::Clock { .. }))
            .map(|(n_idx, _)| n_idx)
            .collect();

        let mut runner = Self {
            circuit,
            wire_values: vec![false; wire_count],
            dirty_queue,
            in_queue,
            clock_nodes,
            tick_count: 0,
        };

        // Run initial drain to establish steady-state logic level outputs. This does not
        // count as an elapsed tick.
        runner.drain_queue();
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

    fn set_node_outputs(&mut self, n_idx: usize, value: bool) {
        let outputs = &self.circuit.nodes[n_idx].outputs;
        for &out_w_idx in outputs {
            if self.wire_values[out_w_idx] != value {
                self.wire_values[out_w_idx] = value;
                for &listener_n_idx in &self.circuit.wire_listeners[out_w_idx] {
                    Self::enqueue_node_static(
                        &mut self.in_queue,
                        &mut self.dirty_queue,
                        listener_n_idx,
                    );
                }
            }
        }
    }

    fn set_output_pin(&mut self, n_idx: usize, pin: usize, value: bool) {
        let outputs = &self.circuit.nodes[n_idx].outputs;
        if pin < outputs.len() {
            let out_w_idx = outputs[pin];
            if self.wire_values[out_w_idx] != value {
                self.wire_values[out_w_idx] = value;
                for &listener_n_idx in &self.circuit.wire_listeners[out_w_idx] {
                    Self::enqueue_node_static(
                        &mut self.in_queue,
                        &mut self.dirty_queue,
                        listener_n_idx,
                    );
                }
            }
        }
    }

    fn process_node_outputs(&mut self, n_idx: usize) {
        let kind = &self.circuit.nodes[n_idx].kind;
        match kind {
            NodeKind::Input { .. } | NodeKind::Output { .. } | NodeKind::SubCircuit(_) => {}
            NodeKind::And => {
                let val = self.input_value(n_idx, 0) && self.input_value(n_idx, 1);
                self.set_node_outputs(n_idx, val);
            }
            NodeKind::Or => {
                let val = self.input_value(n_idx, 0) || self.input_value(n_idx, 1);
                self.set_node_outputs(n_idx, val);
            }
            NodeKind::Not => {
                let val = !self.input_value(n_idx, 0);
                self.set_node_outputs(n_idx, val);
            }
            NodeKind::Xor => {
                let val = self.input_value(n_idx, 0) ^ self.input_value(n_idx, 1);
                self.set_node_outputs(n_idx, val);
            }
            NodeKind::Nand => {
                let val = !(self.input_value(n_idx, 0) && self.input_value(n_idx, 1));
                self.set_node_outputs(n_idx, val);
            }
            NodeKind::Nor => {
                let val = !(self.input_value(n_idx, 0) || self.input_value(n_idx, 1));
                self.set_node_outputs(n_idx, val);
            }
            NodeKind::Xnor => {
                let val = !(self.input_value(n_idx, 0) ^ self.input_value(n_idx, 1));
                self.set_node_outputs(n_idx, val);
            }
            NodeKind::Buffer => {
                let val = self.input_value(n_idx, 0);
                self.set_node_outputs(n_idx, val);
            }
            NodeKind::Mux => {
                let d0 = self.input_value(n_idx, 0);
                let d1 = self.input_value(n_idx, 1);
                let sel = self.input_value(n_idx, 2);
                let val = if sel { d1 } else { d0 };
                self.set_node_outputs(n_idx, val);
            }
            NodeKind::Demux => {
                let sig = self.input_value(n_idx, 0);
                let sel = self.input_value(n_idx, 1);
                self.set_output_pin(n_idx, 0, if !sel { sig } else { false });
                self.set_output_pin(n_idx, 1, if sel { sig } else { false });
            }
            NodeKind::TriStateBuffer => {
                let sig = self.input_value(n_idx, 0);
                let enable = self.input_value(n_idx, 1);
                let val = if enable { sig } else { false };
                self.set_node_outputs(n_idx, val);
            }
            NodeKind::Clock {
                period_ticks,
                phase,
            } => {
                let period = (*period_ticks).max(1) as u64;
                let elapsed = self.tick_count.wrapping_add(*phase as u64) % period;
                let val = elapsed < period / 2;
                self.set_node_outputs(n_idx, val);
            }
        }
    }

    /// Drains the dirty queue, propagating changes until it settles (or a safety cap is hit).
    /// Does not advance `tick_count`.
    fn drain_queue(&mut self) {
        let mut iterations = 0;
        while let Some(n_idx) = self.dirty_queue.pop_front() {
            self.in_queue[n_idx] = false;
            self.process_node_outputs(n_idx);

            iterations += 1;
            if iterations > 10_000 {
                break;
            }
        }
    }
}

impl Runner for EventDrivenRunner {
    fn set_input(&mut self, node: NodeId, pin: u8, value: bool) {
        if let Some(w_indices) = self.circuit.node_output_pins.get(&(node, pin)) {
            for &w_idx in w_indices {
                if self.wire_values[w_idx] != value {
                    self.wire_values[w_idx] = value;
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
        // Clock/time-driven nodes don't depend on other wires changing, so they need to be
        // explicitly re-queued every tick in order to react to the new elapsed tick count.
        for &n_idx in &self.clock_nodes {
            Self::enqueue_node_static(&mut self.in_queue, &mut self.dirty_queue, n_idx);
        }

        self.drain_queue();
        self.tick_count += 1;
    }

    fn output(&self, node: NodeId, pin: u8) -> bool {
        if let Some(&w_idx) = self.circuit.node_input_pins.get(&(node, pin)) {
            self.wire_values[w_idx]
        } else {
            false
        }
    }

    fn current_tick(&self) -> u64 {
        self.tick_count
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
