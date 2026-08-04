use crate::circuit::{FlattenedCircuit, NodeId, NodeKind, WireId};
use crate::runner::{InspectableRunner, Runner};
use std::sync::mpsc;
use std::thread;

/// Multi-threaded execution engine for flattened circuits.
///
/// The runner partitions the flattened node graph into topological levels and
/// evaluates one level at a time. Nodes within a level are independent by
/// construction, so they can be executed in parallel.
pub struct ThreadedFlatRunner {
    circuit: FlattenedCircuit,
    wire_values: Vec<bool>,
    previous_clock: Vec<bool>,
    tick_count: u64,
    node_levels: Vec<Vec<usize>>,
    thread_count: usize,
}

#[derive(Default)]
struct NodeUpdates {
    wire_updates: Vec<(usize, bool)>,
    clock_updates: Vec<(usize, bool)>,
}

impl ThreadedFlatRunner {
    pub fn new(circuit: FlattenedCircuit) -> Self {
        let thread_count = thread::available_parallelism()
            .map(|threads| threads.get().max(1))
            .unwrap_or(1)
            .min(8);
        Self::new_with_threads(circuit, thread_count)
    }

    pub fn new_with_threads(circuit: FlattenedCircuit, thread_count: usize) -> Self {
        let wire_count = circuit.wire_count;
        let node_count = circuit.nodes.len();
        let previous_clock = vec![false; node_count];

        let node_levels = build_node_levels(&circuit, &circuit.executable_nodes);

        Self {
            circuit,
            wire_values: vec![false; wire_count],
            previous_clock,
            tick_count: 0,
            node_levels,
            thread_count: thread_count.max(1),
        }
    }

    fn input_value(&self, node_idx: usize, pin: usize) -> bool {
        let node = &self.circuit.nodes[node_idx];
        node.inputs
            .get(pin)
            .is_some_and(|&wire| self.wire_values[wire])
    }

    fn set_output_wires(&self, updates: &mut Vec<(usize, bool)>, node_idx: usize, value: bool) {
        let node = &self.circuit.nodes[node_idx];
        for &wire in &node.outputs {
            updates.push((wire, value));
        }
    }

    fn set_output_pin(
        &self,
        updates: &mut Vec<(usize, bool)>,
        node_idx: usize,
        pin: usize,
        value: bool,
    ) {
        let node = &self.circuit.nodes[node_idx];
        if let Some(wires) = node.output_pins.get(pin) {
            for &wire in wires {
                updates.push((wire, value));
            }
        }
    }

    fn evaluate_counter(
        &self,
        node_idx: usize,
        bits: usize,
        updates: &mut Vec<(usize, bool)>,
        clock_updates: &mut Vec<(usize, bool)>,
    ) {
        let clock = self.input_value(node_idx, 0);
        let reset = self.input_value(node_idx, 1);
        let falling = self.previous_clock[node_idx] && !clock;
        let pins = &self.circuit.nodes[node_idx].output_pins;
        let observable = self.circuit.idx_to_wire.len();

        let mut carry = falling;
        for (bit, pin_wires) in pins.iter().take(bits).enumerate() {
            let _ = bit;
            let old = pin_wires
                .iter()
                .find(|&&wire| wire < observable)
                .or_else(|| pin_wires.first())
                .map(|&wire| self.wire_values[wire])
                .unwrap_or(false);

            let next = if reset { false } else { old ^ carry };
            carry &= old;

            for &wire in pin_wires {
                updates.push((wire, next));
            }
        }

        clock_updates.push((node_idx, clock));
    }

    fn evaluate_adder(
        &self,
        node_idx: usize,
        bits: usize,
        has_carry: bool,
        updates: &mut Vec<(usize, bool)>,
    ) {
        let mut carry = has_carry && self.input_value(node_idx, bits * 2);

        for bit in 0..bits {
            let a = self.input_value(node_idx, bit);
            let b = self.input_value(node_idx, bits + bit);
            self.set_output_pin(updates, node_idx, bit, a ^ b ^ carry);
            carry = (a && b) || (carry && (a ^ b));
        }

        self.set_output_pin(updates, node_idx, bits, carry);
    }

    fn evaluate_node(
        &self,
        node_idx: usize,
        updates: &mut Vec<(usize, bool)>,
        clock_updates: &mut Vec<(usize, bool)>,
    ) {
        match &self.circuit.nodes[node_idx].kind {
            NodeKind::Input | NodeKind::Output | NodeKind::SubCircuit(_) => {}
            NodeKind::And => {
                let value = self.input_value(node_idx, 0) && self.input_value(node_idx, 1);
                self.set_output_wires(updates, node_idx, value);
            }
            NodeKind::Or => {
                let value = self.input_value(node_idx, 0) || self.input_value(node_idx, 1);
                self.set_output_wires(updates, node_idx, value);
            }
            NodeKind::Not => {
                let value = !self.input_value(node_idx, 0);
                self.set_output_wires(updates, node_idx, value);
            }
            NodeKind::Xor => {
                let value = self.input_value(node_idx, 0) ^ self.input_value(node_idx, 1);
                self.set_output_wires(updates, node_idx, value);
            }
            NodeKind::Nand => {
                let value = !(self.input_value(node_idx, 0) && self.input_value(node_idx, 1));
                self.set_output_wires(updates, node_idx, value);
            }
            NodeKind::Nor => {
                let value = !(self.input_value(node_idx, 0) || self.input_value(node_idx, 1));
                self.set_output_wires(updates, node_idx, value);
            }
            NodeKind::Xnor => {
                let value = !(self.input_value(node_idx, 0) ^ self.input_value(node_idx, 1));
                self.set_output_wires(updates, node_idx, value);
            }
            NodeKind::Buffer => {
                let value = self.input_value(node_idx, 0);
                self.set_output_wires(updates, node_idx, value);
            }
            NodeKind::Mux => {
                let d0 = self.input_value(node_idx, 0);
                let d1 = self.input_value(node_idx, 1);
                let sel = self.input_value(node_idx, 2);
                self.set_output_wires(updates, node_idx, if sel { d1 } else { d0 });
            }
            NodeKind::Demux => {
                let sig = self.input_value(node_idx, 0);
                let sel = self.input_value(node_idx, 1);
                self.set_output_pin(updates, node_idx, 0, if !sel { sig } else { false });
                self.set_output_pin(updates, node_idx, 1, if sel { sig } else { false });
            }
            NodeKind::TriStateBuffer => {
                let sig = self.input_value(node_idx, 0);
                let enable = self.input_value(node_idx, 1);
                self.set_output_wires(updates, node_idx, if enable { sig } else { false });
            }
            NodeKind::Counter8 => {
                self.evaluate_counter(node_idx, 8, updates, clock_updates);
            }
            NodeKind::Counter64 => {
                self.evaluate_counter(node_idx, 64, updates, clock_updates);
            }
            NodeKind::Adder { bits, carry_in } => {
                self.evaluate_adder(node_idx, usize::from(*bits), *carry_in, updates);
            }
            NodeKind::Clock {
                period_ticks,
                duty_ticks,
                phase,
            } => {
                let period = (*period_ticks).max(1) as u64;
                let elapsed = self.tick_count.wrapping_add(*phase as u64) % period;
                self.set_output_wires(updates, node_idx, elapsed < (*duty_ticks).max(1) as u64);
            }
        }
    }

    fn process_level(
        &self,
        level_nodes: &[usize],
        level_updates: &mut Vec<(usize, bool)>,
        level_clock_updates: &mut Vec<(usize, bool)>,
    ) {
        const PARALLEL_MIN_NODES: usize = 256;

        if level_nodes.len() <= 1
            || self.thread_count <= 1
            || level_nodes.len() < PARALLEL_MIN_NODES
        {
            for &node_idx in level_nodes {
                self.evaluate_node(node_idx, level_updates, level_clock_updates);
            }
            return;
        }

        let thread_count = self.thread_count.min(level_nodes.len());
        let chunk_size = level_nodes.len().div_ceil(thread_count).max(1);
        let (tx, rx) = mpsc::channel::<(Vec<(usize, bool)>, Vec<(usize, bool)>)>();

        thread::scope(|scope| {
            for chunk in level_nodes.chunks(chunk_size) {
                let tx = tx.clone();
                let circuit = &self.circuit;
                let wire_values = &self.wire_values;
                let previous_clock = &self.previous_clock;
                let tick_count = self.tick_count;
                scope.spawn(move || {
                    let mut updates = NodeUpdates::default();

                    for &node_idx in chunk {
                        match &circuit.nodes[node_idx].kind {
                            NodeKind::Input | NodeKind::Output | NodeKind::SubCircuit(_) => {}
                            NodeKind::And => {
                                let value = wire_values[circuit.nodes[node_idx].inputs[0]]
                                    && wire_values[circuit.nodes[node_idx].inputs[1]];
                                for &wire in &circuit.nodes[node_idx].outputs {
                                    updates.wire_updates.push((wire, value));
                                }
                            }
                            NodeKind::Or => {
                                let value = wire_values[circuit.nodes[node_idx].inputs[0]]
                                    || wire_values[circuit.nodes[node_idx].inputs[1]];
                                for &wire in &circuit.nodes[node_idx].outputs {
                                    updates.wire_updates.push((wire, value));
                                }
                            }
                            NodeKind::Not => {
                                let value = !wire_values[circuit.nodes[node_idx].inputs[0]];
                                for &wire in &circuit.nodes[node_idx].outputs {
                                    updates.wire_updates.push((wire, value));
                                }
                            }
                            NodeKind::Xor => {
                                let value = wire_values[circuit.nodes[node_idx].inputs[0]]
                                    ^ wire_values[circuit.nodes[node_idx].inputs[1]];
                                for &wire in &circuit.nodes[node_idx].outputs {
                                    updates.wire_updates.push((wire, value));
                                }
                            }
                            NodeKind::Nand => {
                                let value = !(wire_values[circuit.nodes[node_idx].inputs[0]]
                                    && wire_values[circuit.nodes[node_idx].inputs[1]]);
                                for &wire in &circuit.nodes[node_idx].outputs {
                                    updates.wire_updates.push((wire, value));
                                }
                            }
                            NodeKind::Nor => {
                                let value = !(wire_values[circuit.nodes[node_idx].inputs[0]]
                                    || wire_values[circuit.nodes[node_idx].inputs[1]]);
                                for &wire in &circuit.nodes[node_idx].outputs {
                                    updates.wire_updates.push((wire, value));
                                }
                            }
                            NodeKind::Xnor => {
                                let value = !(wire_values[circuit.nodes[node_idx].inputs[0]]
                                    ^ wire_values[circuit.nodes[node_idx].inputs[1]]);
                                for &wire in &circuit.nodes[node_idx].outputs {
                                    updates.wire_updates.push((wire, value));
                                }
                            }
                            NodeKind::Buffer => {
                                let value = wire_values[circuit.nodes[node_idx].inputs[0]];
                                for &wire in &circuit.nodes[node_idx].outputs {
                                    updates.wire_updates.push((wire, value));
                                }
                            }
                            NodeKind::Mux => {
                                let d0 = wire_values[circuit.nodes[node_idx].inputs[0]];
                                let d1 = wire_values[circuit.nodes[node_idx].inputs[1]];
                                let sel = wire_values[circuit.nodes[node_idx].inputs[2]];
                                for &wire in &circuit.nodes[node_idx].outputs {
                                    updates.wire_updates.push((wire, if sel { d1 } else { d0 }));
                                }
                            }
                            NodeKind::Demux => {
                                let sig = wire_values[circuit.nodes[node_idx].inputs[0]];
                                let sel = wire_values[circuit.nodes[node_idx].inputs[1]];
                                if let Some(wires) = circuit.nodes[node_idx].output_pins.first() {
                                    for &wire in wires {
                                        updates
                                            .wire_updates
                                            .push((wire, if !sel { sig } else { false }));
                                    }
                                }
                                if let Some(wires) = circuit.nodes[node_idx].output_pins.get(1) {
                                    for &wire in wires {
                                        updates
                                            .wire_updates
                                            .push((wire, if sel { sig } else { false }));
                                    }
                                }
                            }
                            NodeKind::TriStateBuffer => {
                                let sig = wire_values[circuit.nodes[node_idx].inputs[0]];
                                let enable = wire_values[circuit.nodes[node_idx].inputs[1]];
                                let value = if enable { sig } else { false };
                                for &wire in &circuit.nodes[node_idx].outputs {
                                    updates.wire_updates.push((wire, value));
                                }
                            }
                            NodeKind::Counter8 => {
                                evaluate_counter_threaded(
                                    circuit,
                                    node_idx,
                                    wire_values,
                                    previous_clock,
                                    8,
                                    &mut updates,
                                );
                            }
                            NodeKind::Counter64 => {
                                evaluate_counter_threaded(
                                    circuit,
                                    node_idx,
                                    wire_values,
                                    previous_clock,
                                    64,
                                    &mut updates,
                                );
                            }
                            NodeKind::Adder { bits, carry_in } => {
                                let bit_count = usize::from(*bits);
                                let carry_in_idx = circuit.nodes[node_idx].inputs[bit_count * 2];
                                let mut carry = *carry_in && wire_values[carry_in_idx];

                                for bit in 0..bit_count {
                                    let a = wire_values[circuit.nodes[node_idx].inputs[bit]];
                                    let b = wire_values
                                        [circuit.nodes[node_idx].inputs[bit_count + bit]];
                                    if let Some(wires) =
                                        circuit.nodes[node_idx].output_pins.get(bit)
                                    {
                                        let sum = a ^ b ^ carry;
                                        for &wire in wires {
                                            updates.wire_updates.push((wire, sum));
                                        }
                                    }
                                    carry = (a && b) || (carry && (a ^ b));
                                }

                                if let Some(wires) =
                                    circuit.nodes[node_idx].output_pins.get(bit_count)
                                {
                                    for &wire in wires {
                                        updates.wire_updates.push((wire, carry));
                                    }
                                }
                            }
                            NodeKind::Clock {
                                period_ticks,
                                duty_ticks,
                                phase,
                            } => {
                                let period = (*period_ticks).max(1) as u64;
                                let elapsed = tick_count.wrapping_add(*phase as u64) % period;
                                let value = elapsed < (*duty_ticks).max(1) as u64;
                                for &wire in &circuit.nodes[node_idx].outputs {
                                    updates.wire_updates.push((wire, value));
                                }
                            }
                        }
                    }

                    let _ = tx.send((updates.wire_updates, updates.clock_updates));
                });
            }
        });

        drop(tx);
        for (wire_updates, clock_updates) in rx {
            level_updates.extend(wire_updates);
            level_clock_updates.extend(clock_updates);
        }
    }
}

impl Runner for ThreadedFlatRunner {
    fn set_input(&mut self, node: NodeId, pin: u8, value: bool) {
        if let Some(w_indices) = self.circuit.node_output_pins.get(&(node, pin)) {
            for &wire in w_indices {
                self.wire_values[wire] = value;
            }
        }
    }

    fn tick(&mut self) {
        let mut level_wire_updates = Vec::new();
        let mut level_clock_updates = Vec::new();

        for level_nodes in &self.node_levels {
            self.process_level(
                level_nodes,
                &mut level_wire_updates,
                &mut level_clock_updates,
            );

            for (wire_idx, value) in level_wire_updates.drain(..) {
                self.wire_values[wire_idx] = value;
            }

            for (node_idx, value) in level_clock_updates.drain(..) {
                self.previous_clock[node_idx] = value;
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

impl InspectableRunner for ThreadedFlatRunner {
    fn wire_value(&self, wire: WireId) -> bool {
        self.circuit
            .wire_to_idx
            .get(&wire)
            .is_some_and(|&index| self.wire_values[index])
    }

    fn for_each_wire(&self, f: &mut dyn FnMut(WireId, bool)) {
        for (idx, &wire) in self.circuit.idx_to_wire.iter().enumerate() {
            f(wire, self.wire_values[idx]);
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

fn evaluate_counter_threaded(
    circuit: &FlattenedCircuit,
    node_idx: usize,
    wire_values: &[bool],
    previous_clock: &[bool],
    bits: usize,
    updates: &mut NodeUpdates,
) {
    let node = &circuit.nodes[node_idx];
    let inputs = &node.inputs;
    let clock = inputs.first().is_some_and(|&wire| wire_values[wire]);
    let reset = inputs.get(1).is_some_and(|&wire| wire_values[wire]);
    let mut carry = previous_clock[node_idx] && !clock;
    let pins = &node.output_pins;
    let observable = circuit.idx_to_wire.len();

    for pin in pins.iter().take(bits) {
        let old = pin
            .iter()
            .find(|&&wire| wire < observable)
            .or_else(|| pin.first())
            .map(|&wire| wire_values[wire])
            .unwrap_or(false);
        let next = if reset { false } else { old ^ carry };
        carry &= old;
        for &wire in pin {
            updates.wire_updates.push((wire, next));
        }
    }

    updates.clock_updates.push((node_idx, clock));
}

fn build_node_levels(circuit: &FlattenedCircuit, executable_nodes: &[usize]) -> Vec<Vec<usize>> {
    let mut source_of_wire = vec![usize::MAX; circuit.wire_count];
    for (node_idx, node) in circuit.nodes.iter().enumerate() {
        for &wire in &node.outputs {
            source_of_wire[wire] = node_idx;
        }
    }

    let mut node_level = vec![0usize; circuit.nodes.len()];
    let mut levels: Vec<Vec<usize>> = Vec::new();

    for &node_idx in executable_nodes {
        let mut level = 0;
        for &in_wire in &circuit.nodes[node_idx].inputs {
            if let Some(&src_idx) = source_of_wire.get(in_wire)
                && src_idx < node_level.len()
            {
                level = level.max(node_level[src_idx] + 1);
            }
        }
        node_level[node_idx] = level;
        if levels.len() <= level {
            levels.resize_with(level + 1, Vec::new);
        }
        levels[level].push(node_idx);
    }

    levels
}
