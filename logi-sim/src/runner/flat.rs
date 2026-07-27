use crate::circuit::{FlattenedCircuit, NodeId, NodeKind, WireId};
use crate::runner::Runner;

/// Evaluates a Level 1 `FlattenedCircuit` using direct array index operations.
pub struct FlatRunner {
    circuit: FlattenedCircuit,
    wire_values: Vec<bool>,
    tick_count: u64,
}

impl FlatRunner {
    pub fn new(circuit: FlattenedCircuit) -> Self {
        let wire_count = circuit.wire_count;
        Self {
            circuit,
            wire_values: vec![false; wire_count],
            tick_count: 0,
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

    fn set_output_pin(&mut self, node_idx: usize, pin: usize, value: bool) {
        let node = &self.circuit.nodes[node_idx];
        if pin < node.outputs.len() {
            self.wire_values[node.outputs[pin]] = value;
        }
    }
}

impl Runner for FlatRunner {
    fn set_input(&mut self, node: NodeId, pin: u8, value: bool) {
        if let Some(w_indices) = self.circuit.node_output_pins.get(&(node, pin)) {
            for &w_idx in w_indices {
                self.wire_values[w_idx] = value;
            }
        }
    }

    fn tick(&mut self) {
        for n_idx in 0..self.circuit.nodes.len() {
            let kind = &self.circuit.nodes[n_idx].kind;
            match kind {
                NodeKind::Input { .. } | NodeKind::Output { .. } | NodeKind::SubCircuit(_) => continue,
                NodeKind::And => {
                    let out = self.input_value(n_idx, 0) && self.input_value(n_idx, 1);
                    self.set_output(n_idx, out);
                }
                NodeKind::Or => {
                    let out = self.input_value(n_idx, 0) || self.input_value(n_idx, 1);
                    self.set_output(n_idx, out);
                }
                NodeKind::Not => {
                    let out = !self.input_value(n_idx, 0);
                    self.set_output(n_idx, out);
                }
                NodeKind::Xor => {
                    let out = self.input_value(n_idx, 0) ^ self.input_value(n_idx, 1);
                    self.set_output(n_idx, out);
                }
                NodeKind::Nand => {
                    let out = !(self.input_value(n_idx, 0) && self.input_value(n_idx, 1));
                    self.set_output(n_idx, out);
                }
                NodeKind::Nor => {
                    let out = !(self.input_value(n_idx, 0) || self.input_value(n_idx, 1));
                    self.set_output(n_idx, out);
                }
                NodeKind::Xnor => {
                    let out = !(self.input_value(n_idx, 0) ^ self.input_value(n_idx, 1));
                    self.set_output(n_idx, out);
                }
                NodeKind::Buffer => {
                    let out = self.input_value(n_idx, 0);
                    self.set_output(n_idx, out);
                }
                NodeKind::Mux => {
                    let d0 = self.input_value(n_idx, 0);
                    let d1 = self.input_value(n_idx, 1);
                    let sel = self.input_value(n_idx, 2);
                    let out = if sel { d1 } else { d0 };
                    self.set_output(n_idx, out);
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
                    let out = if enable { sig } else { false };
                    self.set_output(n_idx, out);
                }
                NodeKind::Clock {
                    period_ticks,
                    phase,
                } => {
                    let period = (*period_ticks).max(1) as u64;
                    let elapsed = self.tick_count.wrapping_add(*phase as u64) % period;
                    let out = elapsed < period / 2;
                    self.set_output(n_idx, out);
                }
            }
        }

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

impl crate::runner::InspectableRunner for FlatRunner {
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
