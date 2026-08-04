use crate::circuit::{FlattenedCircuit, NodeId, NodeKind, WireId};
use crate::runner::Runner;

/// Evaluates a Level 1 `FlattenedCircuit` using direct array index operations.
pub struct FlatRunner {
    circuit: FlattenedCircuit,
    wire_values: Vec<bool>,
    previous_clock: Vec<bool>,
    tick_count: u64,
}

impl FlatRunner {
    pub fn new(circuit: FlattenedCircuit) -> Self {
        let wire_count = circuit.wire_count;
        let node_count = circuit.nodes.len();
        Self {
            circuit,
            wire_values: vec![false; wire_count],
            previous_clock: vec![false; node_count],
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

    fn set_logical_output_pin(&mut self, node_idx: usize, pin: usize, value: bool) {
        if let Some(wires) = self.circuit.nodes[node_idx].output_pins.get(pin) {
            for &wire in wires {
                self.wire_values[wire] = value;
            }
        }
    }

    fn evaluate_counter(&mut self, node_idx: usize, bits: usize) {
        let clock = self.input_value(node_idx, 0);
        let reset = self.input_value(node_idx, 1);
        let falling = self.previous_clock[node_idx] && !clock;
        let pins = self.circuit.nodes[node_idx].output_pins.clone();
        let observable = self.circuit.idx_to_wire.len();
        let mut carry = falling;
        for pin in pins.iter().take(bits) {
            let old = pin
                .iter()
                .find(|&&wire| wire < observable)
                .or_else(|| pin.first())
                .map(|&wire| self.wire_values[wire])
                .unwrap_or(false);
            let next = if reset { false } else { old ^ carry };
            carry &= old;
            for &wire in pin {
                self.wire_values[wire] = next;
            }
        }
        self.previous_clock[node_idx] = clock;
    }

    fn evaluate_adder(&mut self, node_idx: usize, bits: usize, has_carry: bool) {
        let mut carry = has_carry && self.input_value(node_idx, bits * 2);
        for bit in 0..bits {
            let a = self.input_value(node_idx, bit);
            let b = self.input_value(node_idx, bits + bit);
            self.set_logical_output_pin(node_idx, bit, a ^ b ^ carry);
            carry = (a && b) || (carry && (a ^ b));
        }
        self.set_logical_output_pin(node_idx, bits, carry);
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
        for executable_idx in 0..self.circuit.executable_nodes.len() {
            let n_idx = self.circuit.executable_nodes[executable_idx];
            let kind = self.circuit.nodes[n_idx].kind.clone();
            match &kind {
                NodeKind::Input | NodeKind::Output | NodeKind::SubCircuit(_) => continue,
                NodeKind::BusInput { .. } | NodeKind::BusOutput { .. } => continue,
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
                NodeKind::Counter8 => {
                    self.evaluate_counter(n_idx, 8);
                }
                NodeKind::Counter64 => {
                    self.evaluate_counter(n_idx, 64);
                }
                NodeKind::Adder { bits, carry_in } => {
                    self.evaluate_adder(n_idx, usize::from(*bits), *carry_in);
                }
                NodeKind::Clock {
                    period_ticks,
                    duty_ticks,
                    phase,
                } => {
                    let period = (*period_ticks).max(1) as u64;
                    let elapsed = self.tick_count.wrapping_add(*phase as u64) % period;
                    let out = elapsed < (*duty_ticks).max(1) as u64;
                    self.set_output(n_idx, out);
                }
                NodeKind::BusExpand { bits } => {
                    let value = self.input_value(n_idx, 0);
                    for bit in 0..usize::from(*bits) {
                        self.set_logical_output_pin(n_idx, bit, value);
                    }
                }
                NodeKind::BusReduceOr { bits } => {
                    let mut out = false;
                    for bit in 0..usize::from(*bits) {
                        out |= self.input_value(n_idx, bit);
                    }
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

    fn execution_state(&self) -> Vec<bool> {
        let mut state = self.wire_values.clone();
        state.extend_from_slice(&self.previous_clock);
        state
    }

    fn restore_execution_state(&mut self, state: &[bool]) -> bool {
        if state.len() != self.wire_values.len() + self.previous_clock.len() {
            return false;
        }
        let (wires, clocks) = state.split_at(self.wire_values.len());
        self.wire_values.copy_from_slice(wires);
        self.previous_clock.copy_from_slice(clocks);
        true
    }

    fn restore_wire_value(&mut self, wire: WireId, value: bool) {
        if let Some(&index) = self.circuit.wire_to_idx.get(&wire) {
            self.wire_values[index] = value;
        }
    }

    fn set_current_tick(&mut self, tick: u64) {
        self.tick_count = tick;
    }
}
