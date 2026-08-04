use crate::circuit::{
    BitPackedCircuit, NodeId, SimdIndex, SimdOp, SimdProgram, SimdPrograms, WireId,
};
use crate::runner::{InspectableRunner, Runner};
use wide::u64x4;

pub struct SimdRunner {
    circuit: BitPackedCircuit,
    wire_values: Vec<u64x4>,
    tick_count: u64,
    previous_clock: Vec<u64x4>,
}

impl SimdRunner {
    pub fn new(circuit: BitPackedCircuit) -> Self {
        let wire_count = circuit.wire_count;
        let counter_count = match &circuit.programs {
            SimdPrograms::Compact(program) => program.counters.len(),
            SimdPrograms::Wide(program) => program.counters.len(),
        };
        Self {
            circuit,
            wire_values: vec![u64x4::ZERO; wire_count],
            tick_count: 0,
            previous_clock: vec![u64x4::ZERO; counter_count],
        }
    }

    pub fn set_input_u64x4(&mut self, node: NodeId, pin: u8, vector: u64x4) {
        if let Some(wires) = self.circuit.node_output_pins.get(&(node, pin)) {
            for &wire in wires {
                self.wire_values[wire] = vector;
            }
        }
    }

    pub fn output_u64x4(&self, node: NodeId, pin: u8) -> u64x4 {
        self.circuit
            .node_input_pins
            .get(&(node, pin))
            .map_or(u64x4::ZERO, |&wire| self.wire_values[wire])
    }

    pub fn set_input_u64(&mut self, node: NodeId, pin: u8, val: u64) {
        self.set_input_u64x4(node, pin, u64x4::from([val, 0, 0, 0]));
    }

    pub fn output_u64(&self, node: NodeId, pin: u8) -> u64 {
        let lanes: [u64; 4] = self.output_u64x4(node, pin).into();
        lanes[0]
    }
}

#[inline(always)]
fn execute_program<I: SimdIndex>(
    program: &SimdProgram<I>,
    wires: &mut [u64x4],
    previous_clock: &mut [u64x4],
    tick_count: u64,
) {
    for clock in &program.clocks {
        let elapsed =
            tick_count.wrapping_add(clock.phase as u64) % clock.period_ticks.max(1) as u64;
        let value = elapsed < clock.duty_ticks.max(1) as u64;
        wires[clock.output.as_usize()] = u64x4::splat(if value { u64::MAX } else { 0 });
    }

    for (counter, previous) in program.counters.iter().zip(previous_clock) {
        let clock = wires[counter.increment.as_usize()];
        let reset = wires[counter.reset.as_usize()];
        let mut carry = *previous & !clock;
        for outputs in &counter.outputs {
            let old = outputs
                .first()
                .map(|output| wires[output.as_usize()])
                .unwrap_or(u64x4::ZERO);
            let next = (old ^ carry) & !reset;
            for output in outputs {
                wires[output.as_usize()] = next;
            }
            carry &= old;
        }
        *previous = clock;
    }

    for block in &program.blocks {
        let start = block.start as usize;
        let end = start + block.len as usize;
        match block.op {
            SimdOp::And => {
                for inst in &program.binary[start..end] {
                    wires[inst.output.as_usize()] =
                        wires[inst.in0.as_usize()] & wires[inst.in1.as_usize()];
                }
            }
            SimdOp::Or => {
                for inst in &program.binary[start..end] {
                    wires[inst.output.as_usize()] =
                        wires[inst.in0.as_usize()] | wires[inst.in1.as_usize()];
                }
            }
            SimdOp::Xor => {
                for inst in &program.binary[start..end] {
                    wires[inst.output.as_usize()] =
                        wires[inst.in0.as_usize()] ^ wires[inst.in1.as_usize()];
                }
            }
            SimdOp::Nand => {
                for inst in &program.binary[start..end] {
                    wires[inst.output.as_usize()] =
                        !(wires[inst.in0.as_usize()] & wires[inst.in1.as_usize()]);
                }
            }
            SimdOp::Nor => {
                for inst in &program.binary[start..end] {
                    wires[inst.output.as_usize()] =
                        !(wires[inst.in0.as_usize()] | wires[inst.in1.as_usize()]);
                }
            }
            SimdOp::Xnor => {
                for inst in &program.binary[start..end] {
                    wires[inst.output.as_usize()] =
                        !(wires[inst.in0.as_usize()] ^ wires[inst.in1.as_usize()]);
                }
            }
            SimdOp::TriStateBuffer => {
                for inst in &program.binary[start..end] {
                    wires[inst.output.as_usize()] =
                        wires[inst.in0.as_usize()] & wires[inst.in1.as_usize()];
                }
            }
            SimdOp::Not => {
                for inst in &program.unary[start..end] {
                    wires[inst.output.as_usize()] = !wires[inst.input.as_usize()];
                }
            }
            SimdOp::Buffer => {
                for inst in &program.unary[start..end] {
                    wires[inst.output.as_usize()] = wires[inst.input.as_usize()];
                }
            }
            SimdOp::Mux => {
                for inst in &program.ternary[start..end] {
                    let in0 = wires[inst.in0.as_usize()];
                    let in1 = wires[inst.in1.as_usize()];
                    let select = wires[inst.in2.as_usize()];
                    wires[inst.output.as_usize()] = (in1 & select) | (in0 & !select);
                }
            }
            SimdOp::Demux => {
                for inst in &program.demux[start..end] {
                    let input = wires[inst.input.as_usize()];
                    let select = wires[inst.select.as_usize()];
                    wires[inst.out_false.as_usize()] = input & !select;
                    wires[inst.out_true.as_usize()] = input & select;
                }
            }
        }
    }
}

impl Runner for SimdRunner {
    fn set_input(&mut self, node: NodeId, pin: u8, value: bool) {
        let mask = if value { u64::MAX } else { 0 };
        self.set_input_u64x4(node, pin, u64x4::splat(mask));
    }

    fn tick(&mut self) {
        match &self.circuit.programs {
            SimdPrograms::Compact(program) => execute_program(
                program,
                &mut self.wire_values,
                &mut self.previous_clock,
                self.tick_count,
            ),
            SimdPrograms::Wide(program) => execute_program(
                program,
                &mut self.wire_values,
                &mut self.previous_clock,
                self.tick_count,
            ),
        }
        self.tick_count += 1;
    }

    fn output(&self, node: NodeId, pin: u8) -> bool {
        let lanes: [u64; 4] = self.output_u64x4(node, pin).into();
        (lanes[0] & 1) != 0
    }

    fn current_tick(&self) -> u64 {
        self.tick_count
    }
}

impl InspectableRunner for SimdRunner {
    fn wire_value(&self, wire: WireId) -> bool {
        self.circuit.wire_to_idx.get(&wire).is_some_and(|&index| {
            let lanes: [u64; 4] = self.wire_values[index].into();
            (lanes[0] & 1) != 0
        })
    }

    fn for_each_wire(&self, f: &mut dyn FnMut(WireId, bool)) {
        for (index, &wire) in self.circuit.idx_to_wire.iter().enumerate() {
            let lanes: [u64; 4] = self.wire_values[index].into();
            f(wire, (lanes[0] & 1) != 0);
        }
    }

    fn execution_state(&self) -> Vec<bool> {
        self.wire_values
            .iter()
            .chain(&self.previous_clock)
            .map(|value| {
                let lanes: [u64; 4] = (*value).into();
                (lanes[0] & 1) != 0
            })
            .collect()
    }

    fn restore_execution_state(&mut self, state: &[bool]) -> bool {
        if state.len() != self.wire_values.len() + self.previous_clock.len() {
            return false;
        }
        let (wires, clocks) = state.split_at(self.wire_values.len());
        for (wire, &value) in self.wire_values.iter_mut().zip(wires) {
            *wire = u64x4::splat(if value { u64::MAX } else { 0 });
        }
        for (clock, &value) in self.previous_clock.iter_mut().zip(clocks) {
            *clock = u64x4::splat(if value { u64::MAX } else { 0 });
        }
        true
    }

    fn restore_wire_value(&mut self, wire: WireId, value: bool) {
        if let Some(&index) = self.circuit.wire_to_idx.get(&wire) {
            self.wire_values[index] = u64x4::splat(if value { u64::MAX } else { 0 });
        }
    }

    fn set_current_tick(&mut self, tick: u64) {
        self.tick_count = tick;
    }
}
