use crate::circuit::{
    BitPackedCircuit, NodeId, SimdIndex, SimdOp, SimdProgram, SimdPrograms, WireId,
};
use crate::runner::{InspectableRunner, Runner};
use smallvec::SmallVec;
use wide::u64x4;

pub struct SimdRunner {
    circuit: BitPackedCircuit,
    wire_values: Vec<u64x4>,
    tick_count: u64,
    previous_clock: Vec<u64x4>,
    clock_phase: Vec<u64>,
}

impl SimdRunner {
    pub fn new(circuit: BitPackedCircuit) -> Self {
        let wire_count = circuit.wire_count;
        let (counter_count, clock_phase) = match &circuit.programs {
            SimdPrograms::Compact(program) => (
                program.counters.len(),
                program.clocks.iter().map(|clock| clock.phase).collect(),
            ),
            SimdPrograms::Wide(program) => (
                program.counters.len(),
                program.clocks.iter().map(|clock| clock.phase).collect(),
            ),
        };
        Self {
            circuit,
            wire_values: vec![u64x4::ZERO; wire_count],
            tick_count: 0,
            previous_clock: vec![u64x4::ZERO; counter_count],
            clock_phase,
        }
    }

    #[inline(always)]
    fn sync_clock_phase_with_tick(&mut self) {
        match &self.circuit.programs {
            SimdPrograms::Compact(program) => {
                for (clock, phase) in program.clocks.iter().zip(self.clock_phase.iter_mut()) {
                    *phase = self
                        .tick_count
                        .wrapping_add(clock.phase)
                        .wrapping_rem(clock.period_ticks);
                }
            }
            SimdPrograms::Wide(program) => {
                for (clock, phase) in program.clocks.iter().zip(self.clock_phase.iter_mut()) {
                    *phase = self
                        .tick_count
                        .wrapping_add(clock.phase)
                        .wrapping_rem(clock.period_ticks);
                }
            }
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
    clock_phase: &mut [u64],
) {
    let wires_ptr = wires.as_mut_ptr();
    if !program.clocks.is_empty() {
        let ones = u64x4::splat(u64::MAX);
        for (clock, phase) in program.clocks.iter().zip(clock_phase.iter_mut()) {
            let output = *phase < clock.duty_ticks;
            unsafe {
                *wires_ptr.add(clock.output.as_usize()) = if output { ones } else { u64x4::ZERO };
            }
            *phase += 1;
            if *phase >= clock.period_ticks {
                *phase = 0;
            }
        }
    }

    if !program.counters.is_empty() {
        for (counter, previous) in program.counters.iter().zip(previous_clock) {
            let counter_clock = unsafe { *wires_ptr.add(counter.increment.as_usize()) };
            let counter_reset = unsafe { *wires_ptr.add(counter.reset.as_usize()) };
            let mut carry = *previous & !counter_clock;
            for outputs in &counter.outputs {
                let old = unsafe { first_wire_or_zero(wires_ptr, outputs) };
                let next = (old ^ carry) & !counter_reset;
                unsafe { write_outputs(wires_ptr, outputs, next) };
                carry &= old;
            }
            *previous = counter_clock;
        }
    }

    let binary = &program.binary;
    let unary = &program.unary;
    let ternary = &program.ternary;
    let demux = &program.demux;
    let adders = &program.adders;
    for block in &program.blocks {
        let start = block.start;
        let end = start + block.len;
        match block.op {
            SimdOp::And => {
                for inst in &binary[start..end] {
                    let out = inst.output.as_usize();
                    let in0 = inst.in0.as_usize();
                    let in1 = inst.in1.as_usize();
                    unsafe {
                        *wires_ptr.add(out) = *wires_ptr.add(in0) & *wires_ptr.add(in1);
                    }
                }
            }
            SimdOp::Or => {
                for inst in &binary[start..end] {
                    let out = inst.output.as_usize();
                    let in0 = inst.in0.as_usize();
                    let in1 = inst.in1.as_usize();
                    unsafe {
                        *wires_ptr.add(out) = *wires_ptr.add(in0) | *wires_ptr.add(in1);
                    }
                }
            }
            SimdOp::Xor => {
                for inst in &binary[start..end] {
                    let out = inst.output.as_usize();
                    let in0 = inst.in0.as_usize();
                    let in1 = inst.in1.as_usize();
                    unsafe {
                        *wires_ptr.add(out) = *wires_ptr.add(in0) ^ *wires_ptr.add(in1);
                    }
                }
            }
            SimdOp::Nand => {
                for inst in &binary[start..end] {
                    let out = inst.output.as_usize();
                    let in0 = inst.in0.as_usize();
                    let in1 = inst.in1.as_usize();
                    unsafe {
                        *wires_ptr.add(out) = !(*wires_ptr.add(in0) & *wires_ptr.add(in1));
                    }
                }
            }
            SimdOp::Nor => {
                for inst in &binary[start..end] {
                    let out = inst.output.as_usize();
                    let in0 = inst.in0.as_usize();
                    let in1 = inst.in1.as_usize();
                    unsafe {
                        *wires_ptr.add(out) = !(*wires_ptr.add(in0) | *wires_ptr.add(in1));
                    }
                }
            }
            SimdOp::Xnor => {
                for inst in &binary[start..end] {
                    let out = inst.output.as_usize();
                    let in0 = inst.in0.as_usize();
                    let in1 = inst.in1.as_usize();
                    unsafe {
                        *wires_ptr.add(out) = !(*wires_ptr.add(in0) ^ *wires_ptr.add(in1));
                    }
                }
            }
            SimdOp::TriStateBuffer => {
                for inst in &binary[start..end] {
                    let out = inst.output.as_usize();
                    let in0 = inst.in0.as_usize();
                    let in1 = inst.in1.as_usize();
                    unsafe {
                        *wires_ptr.add(out) = *wires_ptr.add(in0) & *wires_ptr.add(in1);
                    }
                }
            }
            SimdOp::Not => {
                for inst in &unary[start..end] {
                    let out = inst.output.as_usize();
                    let input = inst.input.as_usize();
                    unsafe {
                        *wires_ptr.add(out) = !*wires_ptr.add(input);
                    }
                }
            }
            SimdOp::Buffer => {
                for inst in &unary[start..end] {
                    let out = inst.output.as_usize();
                    let input = inst.input.as_usize();
                    unsafe {
                        *wires_ptr.add(out) = *wires_ptr.add(input);
                    }
                }
            }
            SimdOp::Mux => {
                for inst in &ternary[start..end] {
                    let out = inst.output.as_usize();
                    let in0 = inst.in0.as_usize();
                    let in1 = inst.in1.as_usize();
                    let select = inst.in2.as_usize();
                    unsafe {
                        let a = *wires_ptr.add(in0);
                        let b = *wires_ptr.add(in1);
                        let sel = *wires_ptr.add(select);
                        *wires_ptr.add(out) = (b & sel) | (a & !sel);
                    }
                }
            }
            SimdOp::Demux => {
                for inst in &demux[start..end] {
                    let out_false = inst.out_false.as_usize();
                    let out_true = inst.out_true.as_usize();
                    let input = inst.input.as_usize();
                    let select = inst.select.as_usize();
                    unsafe {
                        let input = *wires_ptr.add(input);
                        let select = *wires_ptr.add(select);
                        *wires_ptr.add(out_false) = input & !select;
                        *wires_ptr.add(out_true) = input & select;
                    }
                }
            }
            SimdOp::Adder => {
                for inst in &adders[start..end] {
                    let mut carry = inst
                        .carry_in
                        .map(|wire| unsafe { *wires_ptr.add(wire.as_usize()) })
                        .unwrap_or(u64x4::ZERO);
                    for bit in 0..inst.a.len() {
                        let a = unsafe { *wires_ptr.add(inst.a[bit].as_usize()) };
                        let b = unsafe { *wires_ptr.add(inst.b[bit].as_usize()) };
                        let sum = a ^ b ^ carry;
                        unsafe { write_outputs(wires_ptr, &inst.outputs[bit], sum) };
                        carry = (a & b) | (carry & (a ^ b));
                    }
                    unsafe { write_outputs(wires_ptr, &inst.outputs[inst.a.len()], carry) };
                }
            }
        }
    }
}

#[inline(always)]
unsafe fn first_wire_or_zero<I: SimdIndex>(
    wires: *const u64x4,
    outputs: &SmallVec<[I; 2]>,
) -> u64x4 {
    if outputs.is_empty() {
        u64x4::ZERO
    } else {
        *wires.add(outputs[0].as_usize())
    }
}

#[inline(always)]
unsafe fn write_outputs<I: SimdIndex>(wires: *mut u64x4, outputs: &SmallVec<[I; 2]>, value: u64x4) {
    if outputs.is_empty() {
        return;
    }
    *wires.add(outputs[0].as_usize()) = value;
    if outputs.len() > 1 {
        *wires.add(outputs[1].as_usize()) = value;
        if outputs.len() > 2 {
            for &output in &outputs[2..] {
                *wires.add(output.as_usize()) = value;
            }
        }
    }
}

impl Runner for SimdRunner {
    fn set_input(&mut self, node: NodeId, pin: u8, value: bool) {
        let value = if value {
            u64x4::splat(u64::MAX)
        } else {
            u64x4::ZERO
        };
        if let Some(wires) = self.circuit.node_output_pins.get(&(node, pin)) {
            for &wire in wires {
                self.wire_values[wire] = value;
            }
        }
    }

    fn tick(&mut self) {
        match &self.circuit.programs {
            SimdPrograms::Compact(program) => execute_program(
                program,
                &mut self.wire_values,
                &mut self.previous_clock,
                &mut self.clock_phase,
            ),
            SimdPrograms::Wide(program) => execute_program(
                program,
                &mut self.wire_values,
                &mut self.previous_clock,
                &mut self.clock_phase,
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
        self.sync_clock_phase_with_tick();
    }
}
