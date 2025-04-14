use sim_core::gates::GateId;

fn main() {
    let mut circuit = sim_core::circuits::Circuit::new();
    let gate1 = sim_core::gates::Gate::new_input(sim_core::gates::GateId(0), Some(true));
    let gate2 = sim_core::gates::Gate::new_input(sim_core::gates::GateId(1), Some(false));
    let gate3 = sim_core::gates::Gate::new_or(sim_core::gates::GateId(2));
    let gate4 = sim_core::gates::Gate::new_output(sim_core::gates::GateId(3));

    circuit.add_gate(gate1);
    circuit.add_gate(gate2);
    circuit.add_gate(gate3);
    circuit.add_gate(gate4);

    circuit.add_connection(sim_core::gates::GateId(0), sim_core::gates::GateId(2), 0);
    circuit.add_connection(sim_core::gates::GateId(1), sim_core::gates::GateId(2), 1);
    circuit.add_connection(sim_core::gates::GateId(2), sim_core::gates::GateId(3), 0);

    circuit.tick().expect("Tick failed");
    assert!(circuit.gates[3].output == Some(true));
    assert!(circuit.read_output(GateId(3)) == Some(true));

    println!("Hello, world! Ticked once!");
}
