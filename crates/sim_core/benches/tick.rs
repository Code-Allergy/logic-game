use criterion::{Criterion, criterion_group, criterion_main};
use sim_core::{
    Circuit, CircuitTemplate, LogicComponent, PinId, ComponentId,
    circuits::builtin::{FullAdderTemplate, HalfAdderTemplate, MultiplexerTemplate},
};

// Helper struct for Full Adder pin definitions
struct FullAdderPins {
    a: PinId,
    b: PinId,
    carry_in: PinId,
    sum: PinId,
    carry_out: PinId,
}

impl Default for FullAdderPins {
    fn default() -> Self {
        Self { a: 0, b: 1, carry_in: 2, sum: 0, carry_out: 1 }
    }
}

fn create_n_bit_adder(n: usize) -> Circuit {
    let mut n_bit_adder = Circuit::new();
    let fa_pins = FullAdderPins::default();
    let mut a_pins = Vec::with_capacity(n);
    let mut b_pins = Vec::with_capacity(n);
    let mut sum_pins = Vec::with_capacity(n);
    for i in 0..n {
        a_pins.push(n_bit_adder.add_input_pin(i));
        b_pins.push(n_bit_adder.add_input_pin(n + i));
        sum_pins.push(n_bit_adder.add_output_pin(i));
    }
    let carry_in_0_gate_id = n_bit_adder.add_input_pin(2 * n);
    let carry_out_n_gate_id = n_bit_adder.add_output_pin(n);
    let mut current_carry_source_gate: ComponentId = carry_in_0_gate_id;
    let mut current_carry_source_pin: PinId = 0;
    for i in 0..n {
        let mut full_adder_i = FullAdderTemplate::template();
        let full_adder_i_id = full_adder_i.id();
        n_bit_adder.add_component(LogicComponent::Circuit(full_adder_i));
        n_bit_adder.add_connection(a_pins[i], full_adder_i_id, fa_pins.a, 0);
        n_bit_adder.add_connection(b_pins[i], full_adder_i_id, fa_pins.b, 0);
        n_bit_adder.add_connection(current_carry_source_gate, full_adder_i_id, fa_pins.carry_in, current_carry_source_pin);
        n_bit_adder.add_connection(full_adder_i_id, sum_pins[i], 0, fa_pins.sum);
        current_carry_source_gate = full_adder_i_id;
        current_carry_source_pin = fa_pins.carry_out;
    }
    n_bit_adder.add_connection(current_carry_source_gate, carry_out_n_gate_id, 0, current_carry_source_pin);
    n_bit_adder
}

fn bench_n_bit_adder_tick(c: &mut Criterion) {
    const N_BIT_SIZES: [usize; 8] = [8, 16, 32, 64, 128, 256, 512, 1024];

    for &n_bits in N_BIT_SIZES.iter() {
        let mut circuit = create_n_bit_adder(n_bits);
        for i in 0..n_bits {
            circuit.set_input(i, Some(false)); // A_i
            circuit.set_input(n_bits + i, Some(false)); // B_i
        }
        circuit.set_input(2 * n_bits, Some(false)); // Carry_In_0

        c.bench_function(&format!("{}-bit adder tick", n_bits), |b| {
            b.iter(|| {
                circuit.tick().unwrap();
            });
        });
    }
}

// Helper struct for Multiplexer pin definitions
struct MultiplexerPins {
    data0: PinId,
    data1: PinId,
    select: PinId,
    out: PinId,
}

impl Default for MultiplexerPins {
    fn default() -> Self {
        Self { data0: 0, data1: 1, select: 2, out: 0 }
    }
}

fn create_recursive_mux(levels: usize) -> Circuit {
    let mux_pins = MultiplexerPins::default();
    if levels == 0 { panic!("create_recursive_mux called with levels = 0, expected >= 1"); }
    if levels == 1 { return MultiplexerTemplate::template(); }
    let mut top_mux_circuit = Circuit::new();
    let num_data_inputs = 1 << levels;
    let num_select_inputs = levels;
    let mut top_level_data_input_gates = Vec::with_capacity(num_data_inputs);
    for i in 0..num_data_inputs {
        top_level_data_input_gates.push(top_mux_circuit.add_input_pin(i));
    }
    let mut top_level_select_input_gates = Vec::with_capacity(num_select_inputs);
    for i in 0..num_select_inputs {
        top_level_select_input_gates.push(top_mux_circuit.add_input_pin(num_data_inputs + i));
    }
    let top_level_output_gate = top_mux_circuit.add_output_pin(0);
    let mut sub_mux_0 = create_recursive_mux(levels - 1);
    let sub_mux_0_id = sub_mux_0.id();
    top_mux_circuit.add_component(LogicComponent::Circuit(sub_mux_0));
    let mut sub_mux_1 = create_recursive_mux(levels - 1);
    let sub_mux_1_id = sub_mux_1.id();
    top_mux_circuit.add_component(LogicComponent::Circuit(sub_mux_1));
    let num_sub_mux_select_lines = levels - 1;
    let sub_mux_data_inputs_count = 1 << (levels - 1);
    for i in 0..num_sub_mux_select_lines {
        let top_select_gate_id = top_level_select_input_gates[i];
        top_mux_circuit.add_connection(top_select_gate_id, sub_mux_0_id, sub_mux_data_inputs_count + i, 0);
        top_mux_circuit.add_connection(top_select_gate_id, sub_mux_1_id, sub_mux_data_inputs_count + i, 0);
    }
    for i in 0..sub_mux_data_inputs_count {
        let top_data_gate_id_0 = top_level_data_input_gates[i];
        top_mux_circuit.add_connection(top_data_gate_id_0, sub_mux_0_id, i, 0);
        let top_data_gate_id_1 = top_level_data_input_gates[i + sub_mux_data_inputs_count];
        top_mux_circuit.add_connection(top_data_gate_id_1, sub_mux_1_id, i, 0);
    }
    let mut selector_mux = MultiplexerTemplate::template();
    let selector_mux_id = selector_mux.id();
    top_mux_circuit.add_component(LogicComponent::Circuit(selector_mux));
    top_mux_circuit.add_connection(sub_mux_0_id, selector_mux_id, mux_pins.data0, 0);
    top_mux_circuit.add_connection(sub_mux_1_id, selector_mux_id, mux_pins.data1, 0);
    let most_significant_select_gate_id = top_level_select_input_gates[levels - 1];
    top_mux_circuit.add_connection(most_significant_select_gate_id, selector_mux_id, mux_pins.select, 0);
    top_mux_circuit.add_connection(selector_mux_id, top_level_output_gate, 0, mux_pins.out);
    top_mux_circuit
}

fn bench_recursive_mux_tick(c: &mut Criterion) {
    const LEVELS: usize = 4;
    let mut circuit = create_recursive_mux(LEVELS);
    for i in 0..(1 << LEVELS) {
        circuit.set_input(i, Some(i % 2 == 0));
    }
    for i in 0..LEVELS {
        circuit.set_input((1 << LEVELS) + i, Some(i % 2 != 0));
    }
    c.bench_function(&format!("{}-level recursive mux tick", LEVELS), |b| {
        b.iter(|| { circuit.tick().unwrap(); })
    });
}

// Original benchmarks
fn bench_multiplexer_tick(c: &mut Criterion) {
    let mut circuit = MultiplexerTemplate::template();
    circuit.set_input(0, Some(true));
    circuit.set_input(1, Some(false));
    circuit.set_input(2, Some(true));
    c.bench_function("multiplexer tick", |b| {
        b.iter(|| {
            circuit.tick().unwrap();
        })
    });
}

fn bench_half_adder_tick(c: &mut Criterion) {
    let mut circuit = HalfAdderTemplate::template();
    circuit.set_input(0, Some(true));
    circuit.set_input(1, Some(false));
    c.bench_function("half adder tick", |b| {
        b.iter(|| {
            circuit.tick().unwrap();
        })
    });
}

fn bench_full_adder_tick(c: &mut Criterion) {
    let mut circuit = FullAdderTemplate::template();
    circuit.set_input(0, Some(true));
    circuit.set_input(1, Some(false));
    circuit.set_input(2, Some(true));
    c.bench_function("full adder tick", |b| {
        b.iter(|| {
            circuit.tick().unwrap();
        })
    });
}

fn bench_nested_multiplexer_tick(c: &mut Criterion) {
    let mut outer = Circuit::new();
    let inner_mux = MultiplexerTemplate::template();
    let inner_mux_id = inner_mux.id();
    let in0 = outer.add_input_pin(0);
    let in1 = outer.add_input_pin(1);
    let in2 = outer.add_input_pin(2);
    let out = outer.add_output_pin(0);
    outer.add_component(LogicComponent::Circuit(inner_mux));
    outer.add_connection(in0, inner_mux_id, 0, 0);
    outer.add_connection(in1, inner_mux_id, 1, 0);
    outer.add_connection(in2, inner_mux_id, 2, 0);
    outer.add_connection(inner_mux_id, out, 0, 0);
    c.bench_function("nested multiplexer tick", |b| {
        b.iter(|| {
            outer.tick().unwrap();
        })
    });
}

criterion_group!(
    benches,
    bench_multiplexer_tick,
    bench_half_adder_tick,
    bench_full_adder_tick,
    bench_nested_multiplexer_tick,
    bench_n_bit_adder_tick,
    bench_recursive_mux_tick
);
criterion_main!(benches);
