use crate::circuit::{BitPackedCircuit, NodeId, SimdOp, WireId};
use crate::runner::{InspectableRunner, Runner};

/// Evaluates a Level 3 `BitPackedCircuit` using 64-parallel bitwise CPU vector instructions.
pub struct SimdRunner {
    circuit: BitPackedCircuit,
    wire_values: Vec<u64>,
}

impl SimdRunner {
    pub fn new(circuit: BitPackedCircuit) -> Self {
        let wire_count = circuit.wire_count;
        Self {
            circuit,
            wire_values: vec![0u64; wire_count],
        }
    }

    /// Sets a 64-parallel bit vector for a specific input node & pin.
    pub fn set_input_u64(&mut self, node: NodeId, pin: u8, vector: u64) {
        if let Some(w_indices) = self.circuit.node_output_pins.get(&(node, pin)) {
            for &w_idx in w_indices {
                self.wire_values[w_idx] = vector;
            }
        }
    }

    /// Reads all 64 parallel run results for an output node & pin.
    pub fn output_u64(&self, node: NodeId, pin: u8) -> u64 {
        if let Some(&w_idx) = self.circuit.node_input_pins.get(&(node, pin)) {
            self.wire_values[w_idx]
        } else {
            0
        }
    }
}

impl Runner for SimdRunner {
    fn set_input(&mut self, node: NodeId, pin: u8, value: bool) {
        let bitmask = if value { u64::MAX } else { 0 };
        self.set_input_u64(node, pin, bitmask);
    }

    fn tick(&mut self) {
        // Tight linear loop over contiguous memory - 100% cache locality and SIMD auto-vectorizable
        for inst in &self.circuit.instructions {
            let in0 = self.wire_values[inst.in0 as usize];
            let in1 = self.wire_values[inst.in1 as usize];
            let out = match inst.op {
                SimdOp::And => in0 & in1,
                SimdOp::Or => in0 | in1,
                SimdOp::Not => !in0,
                SimdOp::Xor => in0 ^ in1,
            };
            for &out_w_idx in &inst.outputs {
                self.wire_values[out_w_idx as usize] = out;
            }
        }
    }


    fn output(&self, node: NodeId, pin: u8) -> bool {
        (self.output_u64(node, pin) & 1) != 0
    }
}

impl InspectableRunner for SimdRunner {
    fn wire_value(&self, wire: WireId) -> bool {
        if let Some(&w_idx) = self.circuit.wire_to_idx.get(&wire) {
            (self.wire_values[w_idx] & 1) != 0
        } else {
            false
        }
    }

    fn for_each_wire(&self, f: &mut dyn FnMut(WireId, bool)) {
        for (w_idx, &wire_id) in self.circuit.idx_to_wire.iter().enumerate() {
            f(wire_id, (self.wire_values[w_idx] & 1) != 0);
        }
    }
}
