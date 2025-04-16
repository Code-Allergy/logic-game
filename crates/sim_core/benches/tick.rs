use criterion::{Criterion, criterion_group, criterion_main};
use sim_core::{
    Circuit, CircuitTemplate, LogicComponent,
    circuits::builtin::{FullAdderTemplate, HalfAdderTemplate, MultiplexerTemplate},
};

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
    // Outer circuit with a nested multiplexer
    let mut outer = Circuit::new();
    let inner_mux = MultiplexerTemplate::template();
    let inner_mux_id = inner_mux.id();

    let in0 = outer.add_input_pin(Some(true), 0);
    let in1 = outer.add_input_pin(Some(false), 1);
    let in2 = outer.add_input_pin(Some(true), 2);
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
);
criterion_main!(benches);
