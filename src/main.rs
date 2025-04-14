use std::collections::HashMap;

fn main() {
    println!("Hello, world!");

    // let input_gate1 = sim_core::Gate::new_input(sim_core::GateId(0), false);
    // let input_gate2 = sim_core::Gate::new_input(sim_core::GateId(1), false);
    // let output_gate = sim_core::Gate::new_output(sim_core::GateId(2));

    // let or_gate = sim_core::Gate::new_or(sim_core::GateId(3), input_gate1.id, input_gate2.id);

    // let mut circuit = sim_core::Circuit::new();
    // // Store the IDs first
    // let input_id1 = input_gate1.id;
    // let input_id2 = input_gate2.id;
    // let output_id = output_gate.id;
    // let or_id = or_gate.id;

    // // Move the gates
    // circuit.add_gate(input_gate1);
    // circuit.add_gate(input_gate2);
    // circuit.add_gate(output_gate);
    // circuit.add_gate(or_gate);

    // circuit.add_connection(input_id1, or_id, 0);
    // circuit.add_connection(input_id2, or_id, 1);
    // circuit.add_connection(or_id, output_id, 0);

    // let topo = circuit.topological_sort();
    // dbg!(&topo);
    // dbg!(&circuit);
    // circuit.tick();
    // dbg!(&circuit);
}
