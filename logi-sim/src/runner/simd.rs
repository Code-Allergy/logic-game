use crate::circuit::{BitPackedCircuit, NodeId, SimdOp, WireId};
use crate::runner::{InspectableRunner, Runner};
use wide::u64x4;

/// Evaluates a Level 3 `BitPackedCircuit` using guaranteed 256-bit hardware SIMD vector registers (`wide::u64x4`).
pub struct SimdRunner {
    circuit: BitPackedCircuit,
    wire_values: Vec<u64x4>,
}

impl SimdRunner {
    pub fn new(circuit: BitPackedCircuit) -> Self {
        let wire_count = circuit.wire_count;
        Self {
            circuit,
            wire_values: vec![u64x4::ZERO; wire_count],
        }
    }

    /// Sets a 256-bit SIMD vector (`u64x4`) for a specific input node & pin.
    pub fn set_input_u64x4(&mut self, node: NodeId, pin: u8, vector: u64x4) {
        if let Some(w_indices) = self.circuit.node_output_pins.get(&(node, pin)) {
            for &w_idx in w_indices {
                self.wire_values[w_idx] = vector;
            }
        }
    }

    /// Reads the 256-bit SIMD vector (`u64x4`) for an output node & pin.
    pub fn output_u64x4(&self, node: NodeId, pin: u8) -> u64x4 {
        if let Some(&w_idx) = self.circuit.node_input_pins.get(&(node, pin)) {
            self.wire_values[w_idx]
        } else {
            u64x4::ZERO
        }
    }

    /// Sets a 64-bit vector for the first SIMD lane of an input node & pin.
    pub fn set_input_u64(&mut self, node: NodeId, pin: u8, val: u64) {
        let arr = [val, 0, 0, 0];
        self.set_input_u64x4(node, pin, u64x4::from(arr));
    }

    /// Reads a 64-bit vector from the first SIMD lane of an output node & pin.
    pub fn output_u64(&self, node: NodeId, pin: u8) -> u64 {
        let arr: [u64; 4] = self.output_u64x4(node, pin).into();
        arr[0]
    }
}

impl Runner for SimdRunner {
    fn set_input(&mut self, node: NodeId, pin: u8, value: bool) {
        let bitmask = if value { u64::MAX } else { 0 };
        self.set_input_u64x4(node, pin, u64x4::splat(bitmask));
    }

    fn tick(&mut self) {
        // Tight 256-bit SIMD vector execution loop over contiguous memory
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
        let arr: [u64; 4] = self.output_u64x4(node, pin).into();
        (arr[0] & 1) != 0
    }
}

impl InspectableRunner for SimdRunner {
    fn wire_value(&self, wire: WireId) -> bool {
        if let Some(&w_idx) = self.circuit.wire_to_idx.get(&wire) {
            let arr: [u64; 4] = self.wire_values[w_idx].into();
            (arr[0] & 1) != 0
        } else {
            false
        }
    }

    fn for_each_wire(&self, f: &mut dyn FnMut(WireId, bool)) {
        for (w_idx, &wire_id) in self.circuit.idx_to_wire.iter().enumerate() {
            let arr: [u64; 4] = self.wire_values[w_idx].into();
            f(wire_id, (arr[0] & 1) != 0);
        }
    }
}
