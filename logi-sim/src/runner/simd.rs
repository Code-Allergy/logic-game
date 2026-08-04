use crate::circuit::{
    BitPackedCircuit, NodeId, SimdIndex, SimdOp, SimdProgram, SimdPrograms, WireId,
};
use crate::runner::{InspectableRunner, Runner};
use slotmap::SecondaryMap;
use smallvec::SmallVec;
use wide::u64x4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SimdLaneMode {
    Scalar,
    Vector,
}

pub struct SimdRunner {
    circuit: BitPackedCircuit,
    is_wide: bool,
    has_clocks: bool,
    has_counters: bool,
    wire_values: Vec<u64x4>,
    wire_values_scalar: Vec<u8>,
    tick_count: u64,
    previous_clock: Vec<u64x4>,
    previous_clock_scalar: Vec<u8>,
    clock_phase: Vec<u64>,
    pin0_input_cache_single: SecondaryMap<NodeId, usize>,
    pin0_input_cache_multi: SecondaryMap<NodeId, SmallVec<[usize; 2]>>,
    lane_mode: SimdLaneMode,
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
        let (is_wide, has_clocks, has_counters) = match &circuit.programs {
            SimdPrograms::Compact(program) => (
                false,
                !program.clocks.is_empty(),
                !program.counters.is_empty(),
            ),
            SimdPrograms::Wide(program) => (
                true,
                !program.clocks.is_empty(),
                !program.counters.is_empty(),
            ),
        };
        let mut pin0_input_cache_single = SecondaryMap::new();
        let mut pin0_input_cache_multi = SecondaryMap::new();
        for ((node, pin), wires) in &circuit.node_output_pins {
            if *pin == 0 {
                if wires.len() == 1 {
                    if let Some(&wire) = wires.first() {
                        pin0_input_cache_single.insert(*node, wire);
                    }
                } else {
                    pin0_input_cache_multi.insert(*node, wires.clone());
                }
            }
        }
        Self {
            circuit,
            is_wide,
            has_clocks,
            has_counters,
            wire_values: vec![u64x4::ZERO; wire_count],
            wire_values_scalar: vec![0; wire_count],
            tick_count: 0,
            previous_clock: vec![u64x4::ZERO; counter_count],
            previous_clock_scalar: vec![0; counter_count],
            clock_phase,
            pin0_input_cache_single,
            pin0_input_cache_multi,
            lane_mode: SimdLaneMode::Scalar,
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
        if let Some(value) = maybe_uniform_vector_first_lane(vector) {
            if self.lane_mode == SimdLaneMode::Scalar {
                if pin == 0 && self.set_input_u64_scalar_cached(node, pin, value) {
                    return;
                }
                if pin != 0 {
                    if let Some(wires) = self.circuit.node_output_pins.get(&(node, pin)) {
                        for &wire in wires {
                            self.wire_values_scalar[wire] = if value != 0 { 1 } else { 0 };
                        }
                    }
                }
            } else {
                let vector = u64x4::splat(value);
                if pin == 0 && self.set_input_u64x4_cached(node, pin, vector) {
                    return;
                }
                if let Some(wires) = self.circuit.node_output_pins.get(&(node, pin)) {
                    for &wire in wires {
                        self.wire_values[wire] = vector;
                    }
                }
            }
            return;
        }

        self.promote_to_vector_mode();
        if self.set_input_u64x4_cached(node, pin, vector) {
            return;
        }
        if let Some(wires) = self.circuit.node_output_pins.get(&(node, pin)) {
            for &wire in wires {
                self.wire_values[wire] = vector;
            }
        }
    }

    pub fn output_u64x4(&self, node: NodeId, pin: u8) -> u64x4 {
        if self.lane_mode == SimdLaneMode::Scalar {
            self.circuit
                .node_input_pins
                .get(&(node, pin))
                .map_or(u64x4::ZERO, |&wire| {
                    let value = self.wire_values_scalar[wire];
                    u64x4::splat(value as u64)
                })
        } else {
            self.circuit
                .node_input_pins
                .get(&(node, pin))
                .map_or(u64x4::ZERO, |&wire| self.wire_values[wire])
        }
    }

    pub fn set_input_u64(&mut self, node: NodeId, pin: u8, val: u64) {
        self.set_input_u64x4(node, pin, u64x4::from([val, 0, 0, 0]));
    }

    pub fn output_u64(&self, node: NodeId, pin: u8) -> u64 {
        if self.lane_mode == SimdLaneMode::Scalar {
            self.circuit
                .node_input_pins
                .get(&(node, pin))
                .map_or(0, |&wire| self.wire_values_scalar[wire] as u64)
        } else {
            let lanes: [u64; 4] = self.output_u64x4(node, pin).into();
            lanes[0]
        }
    }

    /// Set a small batch of pin0 scalar inputs using pre-resolved wire indices.
    ///
    /// This bypasses `NodeId` lookups and is intended for internal high-throughput
    /// paths that can precompute wire slots once.
    #[inline(always)]
    pub fn set_input_scalar_by_wires(&mut self, updates: &[(usize, bool)]) {
        if self.lane_mode == SimdLaneMode::Scalar {
            let scalar_ptr = self.wire_values_scalar.as_mut_ptr();
            for &(wire, on) in updates {
                unsafe {
                    *scalar_ptr.add(wire) = on as u8;
                }
            }
            return;
        }

        let vector_ptr = self.wire_values.as_mut_ptr();
        for &(wire, on) in updates {
            unsafe {
                *vector_ptr.add(wire) = if on {
                    u64x4::splat(u64::MAX)
                } else {
                    u64x4::ZERO
                };
            }
        }
    }

    /// Set a batch of scalar wire values using pre-resolved wire indices.
    ///
    /// The provided value is interpreted as raw packed wire value so this can be
    /// used by high-throughput paths where callers already normalize booleans to
    /// this representation.
    #[inline(always)]
    pub fn set_input_scalar_wire_values(&mut self, updates: &[(usize, u64)]) {
        if self.lane_mode == SimdLaneMode::Scalar {
            let scalar_ptr = self.wire_values_scalar.as_mut_ptr();
            for &(wire, value) in updates {
                unsafe {
                    *scalar_ptr.add(wire) = (value != 0) as u8;
                }
            }
            return;
        }

        let vector_ptr = self.wire_values.as_mut_ptr();
        for &(wire, value) in updates {
            unsafe {
                *vector_ptr.add(wire) = u64x4::splat(value);
            }
        }
    }

    #[inline(always)]
    fn set_input_u64_scalar_cached(&mut self, node: NodeId, pin: u8, value: u64) -> bool {
        if pin != 0 {
            return false;
        }
        if let Some(&wire) = self.pin0_input_cache_single.get(node) {
            self.wire_values_scalar[wire] = (value != 0) as u8;
            return true;
        }
        if let Some(wires) = self.pin0_input_cache_multi.get(node) {
            for &wire in wires {
                self.wire_values_scalar[wire] = (value != 0) as u8;
            }
            return true;
        }
        false
    }

    #[inline(always)]
    fn set_input_u64x4_cached(&mut self, node: NodeId, pin: u8, value: u64x4) -> bool {
        if pin != 0 {
            return false;
        }
        if self.lane_mode != SimdLaneMode::Vector {
            return false;
        }
        if let Some(&wire) = self.pin0_input_cache_single.get(node) {
            self.wire_values[wire] = value;
            return true;
        }
        if let Some(wires) = self.pin0_input_cache_multi.get(node) {
            for &wire in wires {
                self.wire_values[wire] = value;
            }
            return true;
        }
        false
    }

    #[inline(always)]
    fn promote_to_vector_mode(&mut self) {
        if self.lane_mode == SimdLaneMode::Vector {
            return;
        }

        for (target, &source) in self
            .wire_values
            .iter_mut()
            .zip(self.wire_values_scalar.iter())
        {
            *target = u64x4::splat(source as u64);
        }
        for (target, &source) in self
            .previous_clock
            .iter_mut()
            .zip(self.previous_clock_scalar.iter())
        {
            *target = u64x4::splat(source as u64);
        }

        self.lane_mode = SimdLaneMode::Vector;
    }

    pub fn set_lane_mode_vector(&mut self) {
        self.promote_to_vector_mode();
    }

    #[inline(always)]
    fn tick_scalar_compact<const HAS_CLOCKS: bool, const HAS_COUNTERS: bool>(&mut self) {
        let SimdPrograms::Compact(program) = &self.circuit.programs else {
            unreachable!()
        };
        execute_program_scalar::<HAS_CLOCKS, HAS_COUNTERS, u16>(
            program,
            &mut self.wire_values_scalar,
            &mut self.previous_clock_scalar,
            &mut self.clock_phase,
        );
    }

    #[inline(always)]
    fn tick_scalar_wide<const HAS_CLOCKS: bool, const HAS_COUNTERS: bool>(&mut self) {
        let SimdPrograms::Wide(program) = &self.circuit.programs else {
            unreachable!()
        };
        execute_program_scalar::<HAS_CLOCKS, HAS_COUNTERS, u32>(
            program,
            &mut self.wire_values_scalar,
            &mut self.previous_clock_scalar,
            &mut self.clock_phase,
        );
    }

    #[inline(always)]
    fn tick_vector_compact<const HAS_CLOCKS: bool, const HAS_COUNTERS: bool>(&mut self) {
        let SimdPrograms::Compact(program) = &self.circuit.programs else {
            unreachable!()
        };
        execute_program::<HAS_CLOCKS, HAS_COUNTERS, u16>(
            program,
            &mut self.wire_values,
            &mut self.previous_clock,
            &mut self.clock_phase,
        );
    }

    #[inline(always)]
    fn tick_vector_wide<const HAS_CLOCKS: bool, const HAS_COUNTERS: bool>(&mut self) {
        let SimdPrograms::Wide(program) = &self.circuit.programs else {
            unreachable!()
        };
        execute_program::<HAS_CLOCKS, HAS_COUNTERS, u32>(
            program,
            &mut self.wire_values,
            &mut self.previous_clock,
            &mut self.clock_phase,
        );
    }
}

#[cfg_attr(not(feature = "simd-profile"), inline(always))]
#[cfg_attr(feature = "simd-profile", inline(never))]
fn execute_program<const HAS_CLOCKS: bool, const HAS_COUNTERS: bool, I: SimdIndex>(
    program: &SimdProgram<I>,
    wires: &mut [u64x4],
    previous_clock: &mut [u64x4],
    clock_phase: &mut [u64],
) {
    let wires_ptr = wires.as_mut_ptr();
    if HAS_CLOCKS {
        unsafe {
            execute_clock_block_vector(program, wires_ptr, clock_phase);
        }
    }

    if HAS_COUNTERS {
        unsafe {
            execute_counter_block_vector(program, wires_ptr, previous_clock);
        }
    }

    let unary = &program.unary;
    let ternary = &program.ternary;
    let demux = &program.demux;
    let adders = &program.adders;
    let binary_in0 = program.binary.in0.as_ptr();
    let binary_in1 = program.binary.in1.as_ptr();
    let binary_out = program.binary.output.as_ptr();
    let blocks = program.blocks.as_ptr();
    let mut block_idx = 0;
    while block_idx < program.blocks.len() {
        let block = unsafe { &*blocks.add(block_idx) };
        let start = block.start;
        let mut remaining = block.len;
        match block.op {
            SimdOp::And => unsafe {
                let mut binary_in0_cursor = binary_in0.add(start);
                let mut binary_in1_cursor = binary_in1.add(start);
                let mut binary_out_cursor = binary_out.add(start);
                execute_and_block_vector(
                    wires_ptr,
                    binary_in0_cursor,
                    binary_in1_cursor,
                    binary_out_cursor,
                    remaining,
                );
            },
            SimdOp::Or => unsafe {
                let mut binary_in0_cursor = binary_in0.add(start);
                let mut binary_in1_cursor = binary_in1.add(start);
                let mut binary_out_cursor = binary_out.add(start);
                execute_or_block_vector(
                    wires_ptr,
                    binary_in0_cursor,
                    binary_in1_cursor,
                    binary_out_cursor,
                    remaining,
                );
            },
            SimdOp::Xor => unsafe {
                let mut binary_in0_cursor = binary_in0.add(start);
                let mut binary_in1_cursor = binary_in1.add(start);
                let mut binary_out_cursor = binary_out.add(start);
                execute_xor_block_vector(
                    wires_ptr,
                    binary_in0_cursor,
                    binary_in1_cursor,
                    binary_out_cursor,
                    remaining,
                );
            },
            SimdOp::Nand => unsafe {
                let mut binary_in0_cursor = binary_in0.add(start);
                let mut binary_in1_cursor = binary_in1.add(start);
                let mut binary_out_cursor = binary_out.add(start);
                execute_nand_block_vector(
                    wires_ptr,
                    binary_in0_cursor,
                    binary_in1_cursor,
                    binary_out_cursor,
                    remaining,
                );
            },
            SimdOp::Nor => unsafe {
                let mut binary_in0_cursor = binary_in0.add(start);
                let mut binary_in1_cursor = binary_in1.add(start);
                let mut binary_out_cursor = binary_out.add(start);
                execute_nor_block_vector(
                    wires_ptr,
                    binary_in0_cursor,
                    binary_in1_cursor,
                    binary_out_cursor,
                    remaining,
                );
            },
            SimdOp::Xnor => unsafe {
                let mut binary_in0_cursor = binary_in0.add(start);
                let mut binary_in1_cursor = binary_in1.add(start);
                let mut binary_out_cursor = binary_out.add(start);
                execute_xnor_block_vector(
                    wires_ptr,
                    binary_in0_cursor,
                    binary_in1_cursor,
                    binary_out_cursor,
                    remaining,
                );
            },
            SimdOp::TriStateBuffer => unsafe {
                let mut binary_in0_cursor = binary_in0.add(start);
                let mut binary_in1_cursor = binary_in1.add(start);
                let mut binary_out_cursor = binary_out.add(start);
                execute_tri_state_buffer_block_vector(
                    wires_ptr,
                    binary_in0_cursor,
                    binary_in1_cursor,
                    binary_out_cursor,
                    remaining,
                );
            },
            SimdOp::Not => unsafe {
                let mut unary_cursor = unary.as_ptr().add(start);
                execute_not_block_vector(wires_ptr, unary_cursor, remaining);
            },
            SimdOp::Buffer => unsafe {
                let mut unary_cursor = unary.as_ptr().add(start);
                execute_buffer_block_vector(wires_ptr, unary_cursor, remaining);
            },
            SimdOp::Mux => unsafe {
                let mut ternary_cursor = ternary.as_ptr().add(start);
                execute_mux_block_vector(wires_ptr, ternary_cursor, remaining);
            },
            SimdOp::Demux => unsafe {
                let mut demux_cursor = demux.as_ptr().add(start);
                execute_demux_block_vector(wires_ptr, demux_cursor, remaining);
            },
            SimdOp::Adder => unsafe {
                let mut adders_cursor = adders.as_ptr().add(start);
                execute_adder_block_vector(wires_ptr, adders_cursor, remaining);
            },
        }
        block_idx += 1;
    }
}

#[cfg_attr(not(feature = "simd-profile"), inline(always))]
#[cfg_attr(feature = "simd-profile", inline(never))]
fn execute_program_scalar<const HAS_CLOCKS: bool, const HAS_COUNTERS: bool, I: SimdIndex>(
    program: &SimdProgram<I>,
    wires: &mut [u8],
    previous_clock: &mut [u8],
    clock_phase: &mut [u64],
) {
    let wires_ptr = wires.as_mut_ptr();
    if HAS_CLOCKS {
        unsafe {
            execute_clock_block_scalar(program, wires_ptr, clock_phase);
        }
    }

    if HAS_COUNTERS {
        unsafe {
            execute_counter_block_scalar(program, wires_ptr, previous_clock);
        }
    }

    let unary = &program.unary;
    let ternary = &program.ternary;
    let demux = &program.demux;
    let adders = &program.adders;
    let binary_in0 = program.binary.in0.as_ptr();
    let binary_in1 = program.binary.in1.as_ptr();
    let binary_out = program.binary.output.as_ptr();
    let blocks = program.blocks.as_ptr();
    let mut block_idx = 0;
    while block_idx < program.blocks.len() {
        let block = unsafe { &*blocks.add(block_idx) };
        let start = block.start;
        let mut remaining = block.len;
        match block.op {
            SimdOp::And => unsafe {
                let mut binary_in0_cursor = binary_in0.add(start);
                let mut binary_in1_cursor = binary_in1.add(start);
                let mut binary_out_cursor = binary_out.add(start);
                execute_and_block_scalar(
                    wires_ptr,
                    binary_in0_cursor,
                    binary_in1_cursor,
                    binary_out_cursor,
                    remaining,
                );
            },
            SimdOp::Or => unsafe {
                let mut binary_in0_cursor = binary_in0.add(start);
                let mut binary_in1_cursor = binary_in1.add(start);
                let mut binary_out_cursor = binary_out.add(start);
                execute_or_block_scalar(
                    wires_ptr,
                    binary_in0_cursor,
                    binary_in1_cursor,
                    binary_out_cursor,
                    remaining,
                );
            },
            SimdOp::Xor => unsafe {
                let mut binary_in0_cursor = binary_in0.add(start);
                let mut binary_in1_cursor = binary_in1.add(start);
                let mut binary_out_cursor = binary_out.add(start);
                execute_xor_block_scalar(
                    wires_ptr,
                    binary_in0_cursor,
                    binary_in1_cursor,
                    binary_out_cursor,
                    remaining,
                );
            },
            SimdOp::Nand => unsafe {
                let mut binary_in0_cursor = binary_in0.add(start);
                let mut binary_in1_cursor = binary_in1.add(start);
                let mut binary_out_cursor = binary_out.add(start);
                execute_nand_block_scalar(
                    wires_ptr,
                    binary_in0_cursor,
                    binary_in1_cursor,
                    binary_out_cursor,
                    remaining,
                );
            },
            SimdOp::Nor => unsafe {
                let mut binary_in0_cursor = binary_in0.add(start);
                let mut binary_in1_cursor = binary_in1.add(start);
                let mut binary_out_cursor = binary_out.add(start);
                execute_nor_block_scalar(
                    wires_ptr,
                    binary_in0_cursor,
                    binary_in1_cursor,
                    binary_out_cursor,
                    remaining,
                );
            },
            SimdOp::Xnor => unsafe {
                let mut binary_in0_cursor = binary_in0.add(start);
                let mut binary_in1_cursor = binary_in1.add(start);
                let mut binary_out_cursor = binary_out.add(start);
                execute_xnor_block_scalar(
                    wires_ptr,
                    binary_in0_cursor,
                    binary_in1_cursor,
                    binary_out_cursor,
                    remaining,
                );
            },
            SimdOp::TriStateBuffer => unsafe {
                let mut binary_in0_cursor = binary_in0.add(start);
                let mut binary_in1_cursor = binary_in1.add(start);
                let mut binary_out_cursor = binary_out.add(start);
                execute_tri_state_buffer_block_scalar(
                    wires_ptr,
                    binary_in0_cursor,
                    binary_in1_cursor,
                    binary_out_cursor,
                    remaining,
                );
            },
            SimdOp::Not => unsafe {
                let mut unary_cursor = unary.as_ptr().add(start);
                execute_not_block_scalar(wires_ptr, unary_cursor, remaining);
            },
            SimdOp::Buffer => unsafe {
                let mut unary_cursor = unary.as_ptr().add(start);
                execute_buffer_block_scalar(wires_ptr, unary_cursor, remaining);
            },
            SimdOp::Mux => unsafe {
                let mut ternary_cursor = ternary.as_ptr().add(start);
                execute_mux_block_scalar(wires_ptr, ternary_cursor, remaining);
            },
            SimdOp::Demux => unsafe {
                let mut demux_cursor = demux.as_ptr().add(start);
                execute_demux_block_scalar(wires_ptr, demux_cursor, remaining);
            },
            SimdOp::Adder => unsafe {
                let mut adders_cursor = adders.as_ptr().add(start);
                execute_adder_block_scalar(wires_ptr, adders_cursor, remaining);
            },
        }
        block_idx += 1;
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
        unsafe { *wires.add(outputs[0].as_usize()) }
    }
}

#[cfg_attr(not(feature = "simd-profile"), inline(always))]
#[cfg_attr(feature = "simd-profile", inline(never))]
unsafe fn execute_clock_block_vector<I: SimdIndex>(
    program: &SimdProgram<I>,
    wires_ptr: *mut u64x4,
    clock_phase: &mut [u64],
) {
    let ones = u64x4::splat(u64::MAX);
    let clocks = program.clocks.as_ptr();
    let mut clock_idx = 0;
    while clock_idx < program.clocks.len() {
        let clock = &*clocks.add(clock_idx);
        let phase = clock_phase.get_unchecked_mut(clock_idx);
        let output = *phase < clock.duty_ticks;
        *wires_ptr.add(clock.output.as_usize()) = if output { ones } else { u64x4::ZERO };
        *phase += 1;
        if *phase >= clock.period_ticks {
            *phase = 0;
        }
        clock_idx += 1;
    }
}

#[cfg_attr(not(feature = "simd-profile"), inline(always))]
#[cfg_attr(feature = "simd-profile", inline(never))]
unsafe fn execute_counter_block_vector<I: SimdIndex>(
    program: &SimdProgram<I>,
    wires_ptr: *mut u64x4,
    previous_clock: &mut [u64x4],
) {
    let counters = program.counters.as_ptr();
    let mut counter_idx = 0;
    while counter_idx < program.counters.len() {
        let counter = &*counters.add(counter_idx);
        let previous = previous_clock.get_unchecked_mut(counter_idx);
        let counter_clock = *wires_ptr.add(counter.increment.as_usize());
        let counter_reset = *wires_ptr.add(counter.reset.as_usize());
        let mut carry = *previous & !counter_clock;
        let mut outputs_idx = 0;
        while outputs_idx < counter.outputs.len() {
            let outputs = &counter.outputs[outputs_idx];
            let old = first_wire_or_zero(wires_ptr, outputs);
            let next = (old ^ carry) & !counter_reset;
            write_outputs(wires_ptr, outputs, next);
            carry &= old;
            outputs_idx += 1;
        }
        *previous = counter_clock;
        counter_idx += 1;
    }
}

#[cfg_attr(not(feature = "simd-profile"), inline(always))]
#[cfg_attr(feature = "simd-profile", inline(never))]
unsafe fn execute_and_block_vector<I: SimdIndex>(
    wires_ptr: *mut u64x4,
    mut binary_in0_cursor: *const I,
    mut binary_in1_cursor: *const I,
    mut binary_out_cursor: *const I,
    mut remaining: usize,
) {
    while remaining > 0 {
        let out = (*binary_out_cursor).as_usize();
        let in0 = (*binary_in0_cursor).as_usize();
        let in1 = (*binary_in1_cursor).as_usize();
        *wires_ptr.add(out) = *wires_ptr.add(in0) & *wires_ptr.add(in1);
        binary_in0_cursor = binary_in0_cursor.add(1);
        binary_in1_cursor = binary_in1_cursor.add(1);
        binary_out_cursor = binary_out_cursor.add(1);
        remaining -= 1;
    }
}

#[cfg_attr(not(feature = "simd-profile"), inline(always))]
#[cfg_attr(feature = "simd-profile", inline(never))]
unsafe fn execute_or_block_vector<I: SimdIndex>(
    wires_ptr: *mut u64x4,
    mut binary_in0_cursor: *const I,
    mut binary_in1_cursor: *const I,
    mut binary_out_cursor: *const I,
    mut remaining: usize,
) {
    while remaining > 0 {
        let out = (*binary_out_cursor).as_usize();
        let in0 = (*binary_in0_cursor).as_usize();
        let in1 = (*binary_in1_cursor).as_usize();
        *wires_ptr.add(out) = *wires_ptr.add(in0) | *wires_ptr.add(in1);
        binary_in0_cursor = binary_in0_cursor.add(1);
        binary_in1_cursor = binary_in1_cursor.add(1);
        binary_out_cursor = binary_out_cursor.add(1);
        remaining -= 1;
    }
}

#[cfg_attr(not(feature = "simd-profile"), inline(always))]
#[cfg_attr(feature = "simd-profile", inline(never))]
unsafe fn execute_xor_block_vector<I: SimdIndex>(
    wires_ptr: *mut u64x4,
    mut binary_in0_cursor: *const I,
    mut binary_in1_cursor: *const I,
    mut binary_out_cursor: *const I,
    mut remaining: usize,
) {
    while remaining > 0 {
        let out = (*binary_out_cursor).as_usize();
        let in0 = (*binary_in0_cursor).as_usize();
        let in1 = (*binary_in1_cursor).as_usize();
        *wires_ptr.add(out) = *wires_ptr.add(in0) ^ *wires_ptr.add(in1);
        binary_in0_cursor = binary_in0_cursor.add(1);
        binary_in1_cursor = binary_in1_cursor.add(1);
        binary_out_cursor = binary_out_cursor.add(1);
        remaining -= 1;
    }
}

#[cfg_attr(not(feature = "simd-profile"), inline(always))]
#[cfg_attr(feature = "simd-profile", inline(never))]
unsafe fn execute_nand_block_vector<I: SimdIndex>(
    wires_ptr: *mut u64x4,
    mut binary_in0_cursor: *const I,
    mut binary_in1_cursor: *const I,
    mut binary_out_cursor: *const I,
    mut remaining: usize,
) {
    while remaining > 0 {
        let out = (*binary_out_cursor).as_usize();
        let in0 = (*binary_in0_cursor).as_usize();
        let in1 = (*binary_in1_cursor).as_usize();
        *wires_ptr.add(out) = !(*wires_ptr.add(in0) & *wires_ptr.add(in1));
        binary_in0_cursor = binary_in0_cursor.add(1);
        binary_in1_cursor = binary_in1_cursor.add(1);
        binary_out_cursor = binary_out_cursor.add(1);
        remaining -= 1;
    }
}

#[cfg_attr(not(feature = "simd-profile"), inline(always))]
#[cfg_attr(feature = "simd-profile", inline(never))]
unsafe fn execute_nor_block_vector<I: SimdIndex>(
    wires_ptr: *mut u64x4,
    mut binary_in0_cursor: *const I,
    mut binary_in1_cursor: *const I,
    mut binary_out_cursor: *const I,
    mut remaining: usize,
) {
    while remaining > 0 {
        let out = (*binary_out_cursor).as_usize();
        let in0 = (*binary_in0_cursor).as_usize();
        let in1 = (*binary_in1_cursor).as_usize();
        *wires_ptr.add(out) = !(*wires_ptr.add(in0) | *wires_ptr.add(in1));
        binary_in0_cursor = binary_in0_cursor.add(1);
        binary_in1_cursor = binary_in1_cursor.add(1);
        binary_out_cursor = binary_out_cursor.add(1);
        remaining -= 1;
    }
}

#[cfg_attr(not(feature = "simd-profile"), inline(always))]
#[cfg_attr(feature = "simd-profile", inline(never))]
unsafe fn execute_xnor_block_vector<I: SimdIndex>(
    wires_ptr: *mut u64x4,
    mut binary_in0_cursor: *const I,
    mut binary_in1_cursor: *const I,
    mut binary_out_cursor: *const I,
    mut remaining: usize,
) {
    while remaining > 0 {
        let out = (*binary_out_cursor).as_usize();
        let in0 = (*binary_in0_cursor).as_usize();
        let in1 = (*binary_in1_cursor).as_usize();
        *wires_ptr.add(out) = !(*wires_ptr.add(in0) ^ *wires_ptr.add(in1));
        binary_in0_cursor = binary_in0_cursor.add(1);
        binary_in1_cursor = binary_in1_cursor.add(1);
        binary_out_cursor = binary_out_cursor.add(1);
        remaining -= 1;
    }
}

#[cfg_attr(not(feature = "simd-profile"), inline(always))]
#[cfg_attr(feature = "simd-profile", inline(never))]
unsafe fn execute_tri_state_buffer_block_vector<I: SimdIndex>(
    wires_ptr: *mut u64x4,
    mut binary_in0_cursor: *const I,
    mut binary_in1_cursor: *const I,
    mut binary_out_cursor: *const I,
    mut remaining: usize,
) {
    while remaining > 0 {
        let out = (*binary_out_cursor).as_usize();
        let in0 = (*binary_in0_cursor).as_usize();
        let in1 = (*binary_in1_cursor).as_usize();
        *wires_ptr.add(out) = *wires_ptr.add(in0) & *wires_ptr.add(in1);
        binary_in0_cursor = binary_in0_cursor.add(1);
        binary_in1_cursor = binary_in1_cursor.add(1);
        binary_out_cursor = binary_out_cursor.add(1);
        remaining -= 1;
    }
}

#[cfg_attr(not(feature = "simd-profile"), inline(always))]
#[cfg_attr(feature = "simd-profile", inline(never))]
unsafe fn execute_not_block_vector<I: SimdIndex>(
    wires_ptr: *mut u64x4,
    mut unary_cursor: *const crate::circuit::SimdUnaryInstruction<I>,
    mut remaining: usize,
) {
    while remaining > 0 {
        let inst = &*unary_cursor;
        let out = inst.output.as_usize();
        let input = inst.input.as_usize();
        *wires_ptr.add(out) = !*wires_ptr.add(input);
        unary_cursor = unary_cursor.add(1);
        remaining -= 1;
    }
}

#[cfg_attr(not(feature = "simd-profile"), inline(always))]
#[cfg_attr(feature = "simd-profile", inline(never))]
unsafe fn execute_buffer_block_vector<I: SimdIndex>(
    wires_ptr: *mut u64x4,
    mut unary_cursor: *const crate::circuit::SimdUnaryInstruction<I>,
    mut remaining: usize,
) {
    while remaining > 0 {
        let inst = &*unary_cursor;
        let out = inst.output.as_usize();
        let input = inst.input.as_usize();
        *wires_ptr.add(out) = *wires_ptr.add(input);
        unary_cursor = unary_cursor.add(1);
        remaining -= 1;
    }
}

#[cfg_attr(not(feature = "simd-profile"), inline(always))]
#[cfg_attr(feature = "simd-profile", inline(never))]
unsafe fn execute_mux_block_vector<I: SimdIndex>(
    wires_ptr: *mut u64x4,
    mut ternary_cursor: *const crate::circuit::SimdTernaryInstruction<I>,
    mut remaining: usize,
) {
    while remaining > 0 {
        let inst = &*ternary_cursor;
        let out = inst.output.as_usize();
        let in0 = inst.in0.as_usize();
        let in1 = inst.in1.as_usize();
        let select = inst.in2.as_usize();
        let a = *wires_ptr.add(in0);
        let b = *wires_ptr.add(in1);
        let sel = *wires_ptr.add(select);
        *wires_ptr.add(out) = (b & sel) | (a & !sel);
        ternary_cursor = ternary_cursor.add(1);
        remaining -= 1;
    }
}

#[cfg_attr(not(feature = "simd-profile"), inline(always))]
#[cfg_attr(feature = "simd-profile", inline(never))]
unsafe fn execute_demux_block_vector<I: SimdIndex>(
    wires_ptr: *mut u64x4,
    mut demux_cursor: *const crate::circuit::SimdDemuxInstruction<I>,
    mut remaining: usize,
) {
    while remaining > 0 {
        let inst = &*demux_cursor;
        let out_false = inst.out_false.as_usize();
        let out_true = inst.out_true.as_usize();
        let input = inst.input.as_usize();
        let select = inst.select.as_usize();
        let input_value = *wires_ptr.add(input);
        let select_value = *wires_ptr.add(select);
        *wires_ptr.add(out_false) = input_value & !select_value;
        *wires_ptr.add(out_true) = input_value & select_value;
        demux_cursor = demux_cursor.add(1);
        remaining -= 1;
    }
}

#[cfg_attr(not(feature = "simd-profile"), inline(always))]
#[cfg_attr(feature = "simd-profile", inline(never))]
unsafe fn execute_adder_block_vector<I: SimdIndex>(
    wires_ptr: *mut u64x4,
    mut adders_cursor: *const crate::circuit::SimdAdder<I>,
    mut remaining: usize,
) {
    while remaining > 0 {
        let inst = &*adders_cursor;
        let mut carry = inst
            .carry_in
            .map(|wire| *wires_ptr.add(wire.as_usize()))
            .unwrap_or(u64x4::ZERO);
        let bits = inst.a.len();
        let mut bit = 0;
        while bit < bits {
            let a = *wires_ptr.add(inst.a[bit].as_usize());
            let b = *wires_ptr.add(inst.b[bit].as_usize());
            let sum = a ^ b ^ carry;
            write_outputs(wires_ptr, &inst.outputs[bit], sum);
            carry = (a & b) | (carry & (a ^ b));
            bit += 1;
        }
        write_outputs(wires_ptr, &inst.outputs[inst.a.len()], carry);
        adders_cursor = adders_cursor.add(1);
        remaining -= 1;
    }
}

#[cfg_attr(not(feature = "simd-profile"), inline(always))]
#[cfg_attr(feature = "simd-profile", inline(never))]
unsafe fn execute_clock_block_scalar<I: SimdIndex>(
    program: &SimdProgram<I>,
    wires_ptr: *mut u8,
    clock_phase: &mut [u64],
) {
    let clocks = program.clocks.as_ptr();
    let mut clock_idx = 0;
    while clock_idx < program.clocks.len() {
        let clock = &*clocks.add(clock_idx);
        let phase = clock_phase.get_unchecked_mut(clock_idx);
        let output = *phase < clock.duty_ticks;
        *wires_ptr.add(clock.output.as_usize()) = if output { 1 } else { 0 };
        *phase += 1;
        if *phase >= clock.period_ticks {
            *phase = 0;
        }
        clock_idx += 1;
    }
}

#[cfg_attr(not(feature = "simd-profile"), inline(always))]
#[cfg_attr(feature = "simd-profile", inline(never))]
unsafe fn execute_counter_block_scalar<I: SimdIndex>(
    program: &SimdProgram<I>,
    wires_ptr: *mut u8,
    previous_clock: &mut [u8],
) {
    let counters = program.counters.as_ptr();
    let mut counter_idx = 0;
    while counter_idx < program.counters.len() {
        let counter = &*counters.add(counter_idx);
        let previous = previous_clock.get_unchecked_mut(counter_idx);
        let counter_clock = *wires_ptr.add(counter.increment.as_usize());
        let counter_reset = *wires_ptr.add(counter.reset.as_usize());
        let mut carry = *previous & !counter_clock;
        let mut output_idx = 0;
        while output_idx < counter.outputs.len() {
            let outputs = &counter.outputs[output_idx];
            let old = first_wire_or_zero_scalar(wires_ptr, outputs);
            let next = (old ^ carry) & !counter_reset;
            write_outputs_scalar(wires_ptr, outputs, next);
            carry &= old;
            output_idx += 1;
        }
        *previous = counter_clock;
        counter_idx += 1;
    }
}

#[cfg_attr(not(feature = "simd-profile"), inline(always))]
#[cfg_attr(feature = "simd-profile", inline(never))]
unsafe fn execute_and_block_scalar<I: SimdIndex>(
    wires_ptr: *mut u8,
    mut binary_in0_cursor: *const I,
    mut binary_in1_cursor: *const I,
    mut binary_out_cursor: *const I,
    mut remaining: usize,
) {
    while remaining > 0 {
        let out = (*binary_out_cursor).as_usize();
        let in0 = (*binary_in0_cursor).as_usize();
        let in1 = (*binary_in1_cursor).as_usize();
        *wires_ptr.add(out) = *wires_ptr.add(in0) & *wires_ptr.add(in1);
        binary_in0_cursor = binary_in0_cursor.add(1);
        binary_in1_cursor = binary_in1_cursor.add(1);
        binary_out_cursor = binary_out_cursor.add(1);
        remaining -= 1;
    }
}

#[cfg_attr(not(feature = "simd-profile"), inline(always))]
#[cfg_attr(feature = "simd-profile", inline(never))]
unsafe fn execute_or_block_scalar<I: SimdIndex>(
    wires_ptr: *mut u8,
    mut binary_in0_cursor: *const I,
    mut binary_in1_cursor: *const I,
    mut binary_out_cursor: *const I,
    mut remaining: usize,
) {
    while remaining > 0 {
        let out = (*binary_out_cursor).as_usize();
        let in0 = (*binary_in0_cursor).as_usize();
        let in1 = (*binary_in1_cursor).as_usize();
        *wires_ptr.add(out) = *wires_ptr.add(in0) | *wires_ptr.add(in1);
        binary_in0_cursor = binary_in0_cursor.add(1);
        binary_in1_cursor = binary_in1_cursor.add(1);
        binary_out_cursor = binary_out_cursor.add(1);
        remaining -= 1;
    }
}

#[cfg_attr(not(feature = "simd-profile"), inline(always))]
#[cfg_attr(feature = "simd-profile", inline(never))]
unsafe fn execute_xor_block_scalar<I: SimdIndex>(
    wires_ptr: *mut u8,
    mut binary_in0_cursor: *const I,
    mut binary_in1_cursor: *const I,
    mut binary_out_cursor: *const I,
    mut remaining: usize,
) {
    while remaining > 0 {
        let out = (*binary_out_cursor).as_usize();
        let in0 = (*binary_in0_cursor).as_usize();
        let in1 = (*binary_in1_cursor).as_usize();
        *wires_ptr.add(out) = *wires_ptr.add(in0) ^ *wires_ptr.add(in1);
        binary_in0_cursor = binary_in0_cursor.add(1);
        binary_in1_cursor = binary_in1_cursor.add(1);
        binary_out_cursor = binary_out_cursor.add(1);
        remaining -= 1;
    }
}

#[cfg_attr(not(feature = "simd-profile"), inline(always))]
#[cfg_attr(feature = "simd-profile", inline(never))]
unsafe fn execute_nand_block_scalar<I: SimdIndex>(
    wires_ptr: *mut u8,
    mut binary_in0_cursor: *const I,
    mut binary_in1_cursor: *const I,
    mut binary_out_cursor: *const I,
    mut remaining: usize,
) {
    while remaining > 0 {
        let out = (*binary_out_cursor).as_usize();
        let in0 = (*binary_in0_cursor).as_usize();
        let in1 = (*binary_in1_cursor).as_usize();
        *wires_ptr.add(out) = 1 ^ (*wires_ptr.add(in0) & *wires_ptr.add(in1));
        binary_in0_cursor = binary_in0_cursor.add(1);
        binary_in1_cursor = binary_in1_cursor.add(1);
        binary_out_cursor = binary_out_cursor.add(1);
        remaining -= 1;
    }
}

#[cfg_attr(not(feature = "simd-profile"), inline(always))]
#[cfg_attr(feature = "simd-profile", inline(never))]
unsafe fn execute_nor_block_scalar<I: SimdIndex>(
    wires_ptr: *mut u8,
    mut binary_in0_cursor: *const I,
    mut binary_in1_cursor: *const I,
    mut binary_out_cursor: *const I,
    mut remaining: usize,
) {
    while remaining > 0 {
        let out = (*binary_out_cursor).as_usize();
        let in0 = (*binary_in0_cursor).as_usize();
        let in1 = (*binary_in1_cursor).as_usize();
        *wires_ptr.add(out) = 1 ^ (*wires_ptr.add(in0) | *wires_ptr.add(in1));
        binary_in0_cursor = binary_in0_cursor.add(1);
        binary_in1_cursor = binary_in1_cursor.add(1);
        binary_out_cursor = binary_out_cursor.add(1);
        remaining -= 1;
    }
}

#[cfg_attr(not(feature = "simd-profile"), inline(always))]
#[cfg_attr(feature = "simd-profile", inline(never))]
unsafe fn execute_xnor_block_scalar<I: SimdIndex>(
    wires_ptr: *mut u8,
    mut binary_in0_cursor: *const I,
    mut binary_in1_cursor: *const I,
    mut binary_out_cursor: *const I,
    mut remaining: usize,
) {
    while remaining > 0 {
        let out = (*binary_out_cursor).as_usize();
        let in0 = (*binary_in0_cursor).as_usize();
        let in1 = (*binary_in1_cursor).as_usize();
        *wires_ptr.add(out) = 1 ^ (*wires_ptr.add(in0) ^ *wires_ptr.add(in1));
        binary_in0_cursor = binary_in0_cursor.add(1);
        binary_in1_cursor = binary_in1_cursor.add(1);
        binary_out_cursor = binary_out_cursor.add(1);
        remaining -= 1;
    }
}

#[cfg_attr(not(feature = "simd-profile"), inline(always))]
#[cfg_attr(feature = "simd-profile", inline(never))]
unsafe fn execute_tri_state_buffer_block_scalar<I: SimdIndex>(
    wires_ptr: *mut u8,
    mut binary_in0_cursor: *const I,
    mut binary_in1_cursor: *const I,
    mut binary_out_cursor: *const I,
    mut remaining: usize,
) {
    while remaining > 0 {
        let out = (*binary_out_cursor).as_usize();
        let in0 = (*binary_in0_cursor).as_usize();
        let in1 = (*binary_in1_cursor).as_usize();
        *wires_ptr.add(out) = *wires_ptr.add(in0) & *wires_ptr.add(in1);
        binary_in0_cursor = binary_in0_cursor.add(1);
        binary_in1_cursor = binary_in1_cursor.add(1);
        binary_out_cursor = binary_out_cursor.add(1);
        remaining -= 1;
    }
}

#[cfg_attr(not(feature = "simd-profile"), inline(always))]
#[cfg_attr(feature = "simd-profile", inline(never))]
unsafe fn execute_not_block_scalar<I: SimdIndex>(
    wires_ptr: *mut u8,
    mut unary_cursor: *const crate::circuit::SimdUnaryInstruction<I>,
    mut remaining: usize,
) {
    while remaining > 0 {
        let inst = &*unary_cursor;
        let out = inst.output.as_usize();
        let input = inst.input.as_usize();
        *wires_ptr.add(out) = 1 ^ *wires_ptr.add(input);
        unary_cursor = unary_cursor.add(1);
        remaining -= 1;
    }
}

#[cfg_attr(not(feature = "simd-profile"), inline(always))]
#[cfg_attr(feature = "simd-profile", inline(never))]
unsafe fn execute_buffer_block_scalar<I: SimdIndex>(
    wires_ptr: *mut u8,
    mut unary_cursor: *const crate::circuit::SimdUnaryInstruction<I>,
    mut remaining: usize,
) {
    while remaining > 0 {
        let inst = &*unary_cursor;
        let out = inst.output.as_usize();
        let input = inst.input.as_usize();
        *wires_ptr.add(out) = *wires_ptr.add(input);
        unary_cursor = unary_cursor.add(1);
        remaining -= 1;
    }
}

#[cfg_attr(not(feature = "simd-profile"), inline(always))]
#[cfg_attr(feature = "simd-profile", inline(never))]
unsafe fn execute_mux_block_scalar<I: SimdIndex>(
    wires_ptr: *mut u8,
    mut ternary_cursor: *const crate::circuit::SimdTernaryInstruction<I>,
    mut remaining: usize,
) {
    while remaining > 0 {
        let inst = &*ternary_cursor;
        let out = inst.output.as_usize();
        let in0 = inst.in0.as_usize();
        let in1 = inst.in1.as_usize();
        let select = inst.in2.as_usize();
        let a = *wires_ptr.add(in0);
        let b = *wires_ptr.add(in1);
        let sel = *wires_ptr.add(select);
        *wires_ptr.add(out) = (b & sel) | (a & (1 ^ sel));
        ternary_cursor = ternary_cursor.add(1);
        remaining -= 1;
    }
}

#[cfg_attr(not(feature = "simd-profile"), inline(always))]
#[cfg_attr(feature = "simd-profile", inline(never))]
unsafe fn execute_demux_block_scalar<I: SimdIndex>(
    wires_ptr: *mut u8,
    mut demux_cursor: *const crate::circuit::SimdDemuxInstruction<I>,
    mut remaining: usize,
) {
    while remaining > 0 {
        let inst = &*demux_cursor;
        let out_false = inst.out_false.as_usize();
        let out_true = inst.out_true.as_usize();
        let input = inst.input.as_usize();
        let select = inst.select.as_usize();
        let input_value = *wires_ptr.add(input);
        let select_value = *wires_ptr.add(select);
        *wires_ptr.add(out_false) = input_value & (1 ^ select_value);
        *wires_ptr.add(out_true) = input_value & select_value;
        demux_cursor = demux_cursor.add(1);
        remaining -= 1;
    }
}

#[cfg_attr(not(feature = "simd-profile"), inline(always))]
#[cfg_attr(feature = "simd-profile", inline(never))]
unsafe fn execute_adder_block_scalar<I: SimdIndex>(
    wires_ptr: *mut u8,
    mut adders_cursor: *const crate::circuit::SimdAdder<I>,
    mut remaining: usize,
) {
    while remaining > 0 {
        let inst = &*adders_cursor;
        let mut carry = inst
            .carry_in
            .map(|wire| *wires_ptr.add(wire.as_usize()))
            .unwrap_or(0);
        let bits = inst.a.len();
        let mut bit = 0;
        while bit < bits {
            let a = *wires_ptr.add(inst.a[bit].as_usize());
            let b = *wires_ptr.add(inst.b[bit].as_usize());
            let sum = a ^ b ^ carry;
            write_outputs_scalar(wires_ptr, &inst.outputs[bit], sum);
            carry = (a & b) | (carry & (a ^ b));
            bit += 1;
        }
        write_outputs_scalar(wires_ptr, &inst.outputs[inst.a.len()], carry);
        adders_cursor = adders_cursor.add(1);
        remaining -= 1;
    }
}

#[inline(always)]
unsafe fn first_wire_or_zero_scalar<I: SimdIndex>(
    wires: *const u8,
    outputs: &SmallVec<[I; 2]>,
) -> u8 {
    if outputs.is_empty() {
        0
    } else {
        unsafe { *wires.add(outputs[0].as_usize()) }
    }
}

#[inline(always)]
unsafe fn write_outputs<I: SimdIndex>(wires: *mut u64x4, outputs: &SmallVec<[I; 2]>, value: u64x4) {
    let len = outputs.len();
    if len == 0 {
        return;
    }
    let first = outputs[0];
    unsafe {
        *wires.add(first.as_usize()) = value;
    }
    if len == 1 {
        return;
    }
    let second = outputs[1];
    unsafe {
        *wires.add(second.as_usize()) = value;
    }
    if len == 2 {
        return;
    }
    for &output in &outputs[2..] {
        unsafe {
            *wires.add(output.as_usize()) = value;
        }
    }
}

#[inline(always)]
unsafe fn write_outputs_scalar<I: SimdIndex>(
    wires: *mut u8,
    outputs: &SmallVec<[I; 2]>,
    value: u8,
) {
    let len = outputs.len();
    if len == 0 {
        return;
    }
    let first = outputs[0];
    unsafe {
        *wires.add(first.as_usize()) = value;
    }
    if len == 1 {
        return;
    }
    let second = outputs[1];
    unsafe {
        *wires.add(second.as_usize()) = value;
    }
    if len == 2 {
        return;
    }
    for &output in &outputs[2..] {
        unsafe {
            *wires.add(output.as_usize()) = value;
        }
    }
}

#[inline(always)]
fn maybe_uniform_vector_first_lane(vector: u64x4) -> Option<u64> {
    let lanes: [u64; 4] = vector.to_array();
    if lanes[0] == lanes[1] && lanes[0] == lanes[2] && lanes[0] == lanes[3] {
        Some(lanes[0])
    } else {
        None
    }
}

impl Runner for SimdRunner {
    fn set_input(&mut self, node: NodeId, pin: u8, value: bool) {
        if self.lane_mode == SimdLaneMode::Scalar {
            if pin == 0 {
                if self.set_input_u64_scalar_cached(node, pin, value as u64) {
                    return;
                }
            } else if let Some(wires) = self.circuit.node_output_pins.get(&(node, pin)) {
                for &wire in wires {
                    self.wire_values_scalar[wire] = value as u8;
                }
                return;
            }
            return;
        }
        if self.set_input_u64x4_cached(node, pin, u64x4::splat(value as u8 as u64)) {
            return;
        }
        if let Some(wires) = self.circuit.node_output_pins.get(&(node, pin)) {
            let value = u64x4::splat(value as u8 as u64);
            for &wire in wires {
                self.wire_values[wire] = value;
            }
        }
    }

    #[inline(always)]
    fn tick(&mut self) {
        if self.lane_mode == SimdLaneMode::Scalar {
            if self.is_wide {
                if self.has_clocks {
                    if self.has_counters {
                        self.tick_scalar_wide::<true, true>();
                    } else {
                        self.tick_scalar_wide::<true, false>();
                    }
                } else if self.has_counters {
                    self.tick_scalar_wide::<false, true>();
                } else {
                    self.tick_scalar_wide::<false, false>();
                }
            } else if self.has_clocks {
                if self.has_counters {
                    self.tick_scalar_compact::<true, true>();
                } else {
                    self.tick_scalar_compact::<true, false>();
                }
            } else if self.has_counters {
                self.tick_scalar_compact::<false, true>();
            } else {
                self.tick_scalar_compact::<false, false>();
            }
        } else if self.is_wide {
            if self.has_clocks {
                if self.has_counters {
                    self.tick_vector_wide::<true, true>();
                } else {
                    self.tick_vector_wide::<true, false>();
                }
            } else if self.has_counters {
                self.tick_vector_wide::<false, true>();
            } else {
                self.tick_vector_wide::<false, false>();
            }
        } else if self.has_clocks {
            if self.has_counters {
                self.tick_vector_compact::<true, true>();
            } else {
                self.tick_vector_compact::<true, false>();
            }
        } else if self.has_counters {
            self.tick_vector_compact::<false, true>();
        } else {
            self.tick_vector_compact::<false, false>();
        }
        self.tick_count += 1;
    }

    fn output(&self, node: NodeId, pin: u8) -> bool {
        if self.lane_mode == SimdLaneMode::Scalar {
            self.circuit
                .node_input_pins
                .get(&(node, pin))
                .is_some_and(|&wire| self.wire_values_scalar[wire] != 0)
        } else {
            (self.output_u64(node, pin) & 1) != 0
        }
    }

    fn current_tick(&self) -> u64 {
        self.tick_count
    }
}

impl InspectableRunner for SimdRunner {
    fn wire_value(&self, wire: WireId) -> bool {
        self.circuit.wire_to_idx.get(&wire).is_some_and(|&index| {
            if self.lane_mode == SimdLaneMode::Scalar {
                self.wire_values_scalar[index] != 0
            } else {
                let lanes: [u64; 4] = self.wire_values[index].into();
                (lanes[0] & 1) != 0
            }
        })
    }

    fn for_each_wire(&self, f: &mut dyn FnMut(WireId, bool)) {
        for (index, &wire) in self.circuit.idx_to_wire.iter().enumerate() {
            if self.lane_mode == SimdLaneMode::Scalar {
                f(wire, self.wire_values_scalar[index] != 0);
            } else {
                let lanes: [u64; 4] = self.wire_values[index].into();
                f(wire, (lanes[0] & 1) != 0);
            }
        }
    }

    fn execution_state(&self) -> Vec<bool> {
        if self.lane_mode == SimdLaneMode::Scalar {
            self.wire_values_scalar
                .iter()
                .chain(&self.previous_clock_scalar)
                .map(|value| *value != 0)
                .collect()
        } else {
            self.wire_values
                .iter()
                .chain(&self.previous_clock)
                .map(|value| {
                    let lanes: [u64; 4] = (*value).into();
                    (lanes[0] & 1) != 0
                })
                .collect()
        }
    }

    fn restore_execution_state(&mut self, state: &[bool]) -> bool {
        if state.len() != self.wire_values.len() + self.previous_clock.len() {
            return false;
        }
        let (wires, clocks) = state.split_at(self.wire_values.len());
        if self.lane_mode == SimdLaneMode::Scalar {
            for (wire, &value) in self.wire_values_scalar.iter_mut().zip(wires) {
                *wire = if value { 1 } else { 0 };
            }
            for (clock, &value) in self.previous_clock_scalar.iter_mut().zip(clocks) {
                *clock = if value { 1 } else { 0 };
            }
        } else {
            for (wire, &value) in self.wire_values.iter_mut().zip(wires) {
                *wire = u64x4::splat(if value { u64::MAX } else { 0 });
            }
            for (clock, &value) in self.previous_clock.iter_mut().zip(clocks) {
                *clock = u64x4::splat(if value { u64::MAX } else { 0 });
            }
        }
        true
    }

    fn restore_wire_value(&mut self, wire: WireId, value: bool) {
        if let Some(&index) = self.circuit.wire_to_idx.get(&wire) {
            if self.lane_mode == SimdLaneMode::Scalar {
                self.wire_values_scalar[index] = if value { 1 } else { 0 };
            } else {
                self.wire_values[index] = u64x4::splat(if value { u64::MAX } else { 0 });
            }
        }
    }

    fn set_current_tick(&mut self, tick: u64) {
        self.tick_count = tick;
        self.sync_clock_phase_with_tick();
    }
}
