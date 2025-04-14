use super::*;

#[test]
fn test_compute_gate_logic_and() {
    assert_eq!(
        compute_gate_logic(GateType::And, vec![true, true]),
        true,
        "AND gate should return true when both inputs are true"
    );
    assert_eq!(
        compute_gate_logic(GateType::And, vec![true, false]),
        false,
        "AND gate should return false when one input is false"
    );
    assert_eq!(
        compute_gate_logic(GateType::And, vec![false, false]),
        false,
        "AND gate should return false when both inputs are false"
    );
    assert_eq!(
        compute_gate_logic(GateType::And, vec![false, false]),
        false,
        "AND gate should return false when both inputs are false"
    );
}

#[test]
#[should_panic]
fn test_compute_gate_logic_and_empty() {
    compute_gate_logic(GateType::And, vec![]);
}

#[test]
#[should_panic]
fn test_compute_gate_logic_and_low() {
    assert_eq!(
        compute_gate_logic(GateType::And, vec![true]),
        false,
        "AND gate should return false when only one input is provided"
    );
}

#[test]
#[should_panic]
fn test_compute_gate_logic_and_high() {
    assert_eq!(
        compute_gate_logic(GateType::And, vec![true, true, true]),
        true,
        "AND gate should return true when all inputs are true"
    );
}

#[test]
fn test_compute_gate_logic_or() {
    assert_eq!(
        compute_gate_logic(GateType::Or, vec![true, true]),
        true,
        "OR gate should return true when both inputs are true"
    );
    assert_eq!(
        compute_gate_logic(GateType::Or, vec![true, false]),
        true,
        "OR gate should return true when one input is true"
    );
    assert_eq!(
        compute_gate_logic(GateType::Or, vec![false, true]),
        true,
        "OR gate should return true when one input is true"
    );
    assert_eq!(
        compute_gate_logic(GateType::Or, vec![false, false]),
        false,
        "OR gate should return false when both inputs are false"
    );
}

#[test]
#[should_panic]
fn test_compute_gate_logic_or_empty() {
    compute_gate_logic(GateType::Or, vec![]);
}

#[test]
#[should_panic]
fn test_compute_gate_logic_or_low() {
    assert_eq!(
        compute_gate_logic(GateType::Or, vec![true]),
        true,
        "OR gate should return true when only one input is provided"
    );
}

#[test]
#[should_panic]
fn test_compute_gate_logic_or_high() {
    assert_eq!(
        compute_gate_logic(GateType::Or, vec![true, true, true]),
        true,
        "OR gate should return true when all inputs are true"
    );
}

#[test]
fn test_compute_gate_logic_not() {
    assert_eq!(
        compute_gate_logic(GateType::Not, vec![true]),
        false,
        "NOT gate should return false when input is true"
    );
    assert_eq!(
        compute_gate_logic(GateType::Not, vec![false]),
        true,
        "NOT gate should return true when input is false"
    );
}

#[test]
#[should_panic]
fn test_compute_gate_logic_not_empty() {
    compute_gate_logic(GateType::Not, vec![]);
}

#[test]
#[should_panic]
fn test_compute_gate_logic_not_high() {
    assert_eq!(
        compute_gate_logic(GateType::Not, vec![true, false]),
        false,
        "NOT gate should return the not of the first value provided"
    );
}

#[test]
fn test_compute_gate_logic_input() {
    assert_eq!(
        compute_gate_logic(GateType::Input, vec![true]),
        true,
        "Input gate should return true when input is true"
    );
    assert_eq!(
        compute_gate_logic(GateType::Input, vec![false]),
        false,
        "Input gate should return false when input is false"
    );
}

#[test]
#[should_panic]
fn test_compute_gate_logic_input_empty() {
    compute_gate_logic(GateType::Input, vec![]);
}

#[test]
#[should_panic]
fn test_compute_gate_logic_input_high() {
    compute_gate_logic(GateType::Input, vec![true, false]); // should always panic
}

#[test]
fn test_compute_gate_logic_output() {
    assert_eq!(
        compute_gate_logic(GateType::Output, vec![true]),
        true,
        "Output gate should return true when input is true"
    );
    assert_eq!(
        compute_gate_logic(GateType::Output, vec![false]),
        false,
        "Output gate should return false when input is false"
    );
}

#[test]
#[should_panic]
fn test_compute_gate_logic_output_empty() {
    compute_gate_logic(GateType::Output, vec![]);
}

#[test]
#[should_panic]
fn test_compute_gate_logic_output_high() {
    compute_gate_logic(GateType::Output, vec![true, false]); // should always panic
}

#[test]
fn test_simple_circuit_input_output() {
    let mut circuit = Circuit::new();
    let input_gate = Gate::new_input(GateId(0), None);
    let output_gate = Gate::new_output(GateId(1));
    circuit.add_gate(input_gate);
    circuit.add_gate(output_gate);

    circuit.add_connection(GateId(0), GateId(1), 0);
    circuit.set_input(GateId(0), true);
    circuit.tick().expect("Tick should succeed");

    assert_eq!(
        circuit.read_output(GateId(1)),
        Some(true),
        "Circuit should return true when input is true"
    );

    circuit.set_input(GateId(0), false);
    circuit.tick().expect("Tick should succeed");

    assert_eq!(
        circuit.read_output(GateId(1)),
        Some(false),
        "Circuit should return false when input is false"
    );
}

#[test]
fn test_simple_circuit_identity() {
    let mut circuit = Circuit::new();

    let input_gate = Gate::new_input(GateId(0), None);
    let output_gate = Gate::new_output(GateId(1));
    let identity_gate = Gate::new_identity(GateId(2));
    let input_id = input_gate.id;
    let output_id = output_gate.id;
    let identity_id = identity_gate.id;

    circuit.add_gate(input_gate);
    circuit.add_gate(output_gate);
    circuit.add_gate(identity_gate);

    circuit.add_connection(input_id, identity_id, 0);
    circuit.add_connection(identity_id, output_id, 0);
    circuit.set_input(input_id, true);
    circuit.tick().expect("Tick should succeed");

    assert_eq!(
        circuit.read_output(output_id),
        Some(true),
        "Circuit should return true when input is true"
    );

    circuit.set_input(input_id, false);
    circuit.tick().expect("Tick should succeed");

    assert_eq!(
        circuit.read_output(output_id),
        Some(false),
        "Circuit should return false when input is false"
    );
}

#[test]
fn test_simple_circuit_not() {
    let mut circuit = Circuit::new();

    let input_gate = Gate::new_input(GateId(0), None);
    let output_gate = Gate::new_output(GateId(1));
    let not_gate = Gate::new_not(GateId(2));

    let input_id = input_gate.id;
    let output_id = output_gate.id;
    let not_id = not_gate.id;

    circuit.add_gate(input_gate);
    circuit.add_gate(output_gate);
    circuit.add_gate(not_gate);

    circuit.add_connection(input_id, not_id, 0);
    circuit.add_connection(not_id, output_id, 0);
    circuit.set_input(input_id, true);
    circuit.tick().expect("Tick should succeed");

    assert_eq!(
        circuit.read_output(output_id),
        Some(false),
        "Circuit should return false when input is true"
    );

    circuit.set_input(input_id, false);
    circuit.tick().expect("Tick should succeed");

    assert_eq!(
        circuit.read_output(output_id),
        Some(true),
        "Circuit should return true when input is false"
    );
}

#[test]
fn test_simple_circuit_or() {
    let mut circuit = Circuit::new();
    let input_gate = Gate::new_input(GateId(0), None);
    let output_gate = Gate::new_output(GateId(1));
    let or_gate = Gate::new_or(GateId(2));
    let input_gate2 = Gate::new_input(GateId(3), None);
    let input_id = input_gate.id;
    let input_id2 = input_gate2.id;
    let output_id = output_gate.id;
    let or_id = or_gate.id;

    circuit.add_gate(input_gate);
    circuit.add_gate(output_gate);
    circuit.add_gate(or_gate);
    circuit.add_gate(input_gate2);

    circuit.add_connection(input_id, or_id, 0);
    circuit.add_connection(input_id2, or_id, 1);
    circuit.add_connection(or_id, output_id, 0);
    circuit.set_input(input_id, true);
    circuit.set_input(input_id2, true);
    circuit.tick().expect("Tick should succeed");

    assert_eq!(
        circuit.read_output(output_id),
        Some(true),
        "Circuit should return true when input is true, true: {:?}",
        circuit
    );

    circuit.set_input(input_id, false);
    circuit.set_input(input_id2, true);
    circuit.tick().expect("Tick should succeed");

    assert_eq!(
        circuit.read_output(output_id),
        Some(true),
        "Circuit should return true when input is false, true: {:?}",
        circuit
    );

    circuit.set_input(input_id, true);
    circuit.set_input(input_id2, false);
    circuit.tick().expect("Tick should succeed");

    assert_eq!(
        circuit.read_output(output_id),
        Some(true),
        "Circuit should return true when input is true, false: {:?}",
        circuit
    );

    circuit.set_input(input_id, false);
    circuit.set_input(input_id2, false);
    circuit.tick().expect("Tick should succeed");

    assert_eq!(
        circuit.read_output(output_id),
        Some(false),
        "Circuit should return false when input is false, false: {:?}",
        circuit
    );
}

#[test]
fn test_simple_circuit_and() {
    let mut circuit = Circuit::new();
    let input_gate = Gate::new_input(GateId(0), None);
    let output_gate = Gate::new_output(GateId(1));
    let and_gate = Gate::new_and(GateId(2));
    let input_gate2 = Gate::new_input(GateId(3), None);
    let input_id = input_gate.id;
    let input_id2 = input_gate2.id;
    let output_id = output_gate.id;
    let and_id = and_gate.id;

    circuit.add_gate(input_gate);
    circuit.add_gate(output_gate);
    circuit.add_gate(and_gate);
    circuit.add_gate(input_gate2);

    circuit.add_connection(input_id, and_id, 0);
    circuit.add_connection(input_id2, and_id, 1);
    circuit.add_connection(and_id, output_id, 0);
    circuit.set_input(input_id, true);
    circuit.set_input(input_id2, true);
    circuit.tick().expect("Tick should succeed");

    assert_eq!(
        circuit.read_output(output_id),
        Some(true),
        "Circuit should return true when input is true, true: {:?}",
        circuit
    );

    circuit.set_input(input_id, false);
    circuit.set_input(input_id2, true);
    circuit.tick().expect("Tick should succeed");

    assert_eq!(
        circuit.read_output(output_id),
        Some(false),
        "Circuit should return false when input is false, true: {:?}",
        circuit
    );

    circuit.set_input(input_id, true);
    circuit.set_input(input_id2, false);
    circuit.tick().expect("Tick should succeed");

    assert_eq!(
        circuit.read_output(output_id),
        Some(false),
        "Circuit should return false when input is true, false: {:?}",
        circuit
    );

    circuit.set_input(input_id, false);
    circuit.set_input(input_id2, false);
    circuit.tick().expect("Tick should succeed");

    assert_eq!(
        circuit.read_output(output_id),
        Some(false),
        "Circuit should return false when input is false, false: {:?}",
        circuit
    );
}

#[test]
fn test_topological_sort() {
    let mut circuit = Circuit::new();

    let input_gate = Gate::new_input(GateId(0), None);
    let output_gate = Gate::new_output(GateId(1));
    let and_gate = Gate::new_and(GateId(2));
    let input_id = input_gate.id;
    let output_id = output_gate.id;
    let and_id = and_gate.id;

    circuit.add_gate(input_gate);
    circuit.add_gate(output_gate);
    circuit.add_gate(and_gate);

    circuit.add_connection(input_id, and_id, 0);
    circuit.add_connection(and_id, output_id, 0);

    assert!(circuit.topological_sort().is_some());

    // Create a cycle
    circuit.add_connection(output_id, input_id, 0);
    assert!(
        circuit.topological_sort().is_none(),
        "Topological sort should fail on cyclic graph"
    );

    // Remove the cycle
    circuit.connections.pop();
    assert!(
        circuit.topological_sort().is_some(),
        "Topological sort should succeed"
    );
}

#[test]
fn test_simple_circuit_branching() {
    let mut circuit = Circuit::new();

    let input_gate = Gate::new_input(GateId(0), None);
    let identity_gate = Gate::new_identity(GateId(1));
    let not_gate = Gate::new_not(GateId(2));
    let output_gate1 = Gate::new_output(GateId(3));
    let output_gate2 = Gate::new_output(GateId(4));
    let input_id = input_gate.id;
    let output_id = output_gate1.id;
    let output_id2 = output_gate2.id;
    let identity_id = identity_gate.id;
    let not_id = not_gate.id;

    circuit.add_gate(input_gate);
    circuit.add_gate(identity_gate);
    circuit.add_gate(not_gate);
    circuit.add_gate(output_gate1);
    circuit.add_gate(output_gate2);

    circuit.add_connection(input_id, identity_id, 0);
    circuit.add_connection(input_id, not_id, 0);
    circuit.add_connection(identity_id, output_id, 0);
    circuit.add_connection(not_id, output_id2, 0);

    circuit.set_input(input_id, true);
    circuit.tick().expect("Tick should succeed");

    let output1 = circuit.read_output(output_id);
    let output2 = circuit.read_output(output_id2);

    assert_eq!(
        (output1, output2),
        (Some(true), Some(false)),
        "Circuit should return true,false when input is true"
    );

    circuit.set_input(input_id, false);
    circuit.tick().expect("Tick should succeed");

    let output1 = circuit.read_output(output_id);
    let output2 = circuit.read_output(output_id2);

    assert_eq!(
        (output1, output2),
        (Some(false), Some(true)),
        "Circuit should return false,true when input is true"
    );
}

#[test]
fn test_simple_circuit_deep_chain() {
    let mut circuit = Circuit::new();

    let input_gate = Gate::new_input(GateId(0), None);
    let input_gate2 = Gate::new_input(GateId(1), None);
    let output_gate = Gate::new_output(GateId(2));
    let and_gate = Gate::new_and(GateId(3));
    let or_gate = Gate::new_or(GateId(4));
    let not_gate = Gate::new_not(GateId(5));

    let input_id = input_gate.id;
    let input_id2 = input_gate2.id;
    let output_id = output_gate.id;
    let and_id = and_gate.id;
    let or_id = or_gate.id;
    let not_id = not_gate.id;

    circuit.add_gate(input_gate);
    circuit.add_gate(input_gate2);
    circuit.add_gate(output_gate);
    circuit.add_gate(and_gate);
    circuit.add_gate(or_gate);
    circuit.add_gate(not_gate);

    circuit.add_connection(input_id, and_id, 0);
    circuit.add_connection(input_id2, and_id, 1);
    circuit.add_connection(and_id, or_id, 1);
    circuit.add_connection(input_id2, or_id, 0);
    circuit.add_connection(or_id, not_id, 0);
    circuit.add_connection(not_id, output_id, 0);

    circuit.set_input(input_id, true); // changing this should not affect the output
    circuit.set_input(input_id2, true);

    circuit.tick().expect("Tick should succeed");
    assert_eq!(
        circuit.read_output(output_id),
        Some(false),
        "Circuit should return false when input is true, true: {:?}",
        circuit
    );

    circuit.set_input(input_id, false);
    circuit.set_input(input_id2, true);
    circuit.tick().expect("Tick should succeed");
    assert_eq!(
        circuit.read_output(output_id),
        Some(false),
        "Circuit should return false when input is false, true: {:?}",
        circuit
    );

    circuit.set_input(input_id, true);
    circuit.set_input(input_id2, false);
    circuit.tick().expect("Tick should succeed");
    assert_eq!(
        circuit.read_output(output_id),
        Some(true),
        "Circuit should return false when input is true, false: {:?}",
        circuit
    );

    circuit.set_input(input_id, false);
    circuit.set_input(input_id2, false);
    circuit.tick().expect("Tick should succeed");
    assert_eq!(
        circuit.read_output(output_id),
        Some(true),
        "Circuit should return false when input is false, false: {:?}",
        circuit
    );
}

#[test]
fn test_simple_multiplexer() {
    let mut circuit = Circuit::new();

    // Create input gates for data and select
    let d0_gate = Gate::new_input(GateId(0), None); // Data input D0
    let d1_gate = Gate::new_input(GateId(1), None); // Data input D1
    let s_gate = Gate::new_input(GateId(2), None); // Select input S

    // Create a NOT gate to invert S
    let not_gate = Gate::new_not(GateId(3));

    // Create two AND gates:
    // one for (NOT S AND D0) and one for (S AND D1)
    let and_gate_left = Gate::new_and(GateId(4)); // (NOT S) AND D0
    let and_gate_right = Gate::new_and(GateId(5)); // S AND D1

    // Create an OR gate to combine the AND outputs
    let or_gate = Gate::new_or(GateId(6));

    // Create an output gate to observe the final output.
    let output_gate = Gate::new_output(GateId(7));

    // Add all the gates to the circuit
    circuit.add_gate(d0_gate);
    circuit.add_gate(d1_gate);
    circuit.add_gate(s_gate);
    circuit.add_gate(not_gate);
    circuit.add_gate(and_gate_left);
    circuit.add_gate(and_gate_right);
    circuit.add_gate(or_gate);
    circuit.add_gate(output_gate);

    // Build the multiplexer connections:
    //
    // Invert the select signal:
    circuit.add_connection(GateId(2), GateId(3), 0); // S -> NOT

    // Left AND gate for (NOT S AND D0)
    circuit.add_connection(GateId(0), GateId(4), 0); // D0 -> left AND input 0
    circuit.add_connection(GateId(3), GateId(4), 1); // NOT S -> left AND input 1

    // Right AND gate for (S AND D1)
    circuit.add_connection(GateId(2), GateId(5), 0); // S -> right AND input 0
    circuit.add_connection(GateId(1), GateId(5), 1); // D1 -> right AND input 1

    // OR gate combines both AND outputs:
    circuit.add_connection(GateId(4), GateId(6), 0); // left AND -> OR input 0
    circuit.add_connection(GateId(5), GateId(6), 1); // right AND -> OR input 1

    // Connect OR output to the final output gate
    circuit.add_connection(GateId(6), GateId(7), 0);

    // Test the multiplexer:
    //
    // Case 1: S false, output should equal D0.
    // Set D0 = true, D1 = false, S = false.
    circuit.set_input(GateId(0), true);
    circuit.set_input(GateId(1), false);
    circuit.set_input(GateId(2), false);
    circuit.tick().expect("Tick should succeed");
    assert_eq!(
        circuit.read_output(GateId(7)),
        Some(true),
        "With S false and D0 true, output should be true"
    );

    // Case 2: S true, output should equal D1.
    // Set D0 = true, D1 = false, S = true.
    circuit.set_input(GateId(0), true);
    circuit.set_input(GateId(1), false);
    circuit.set_input(GateId(2), true);
    circuit.tick().expect("Tick should succeed");
    assert_eq!(
        circuit.read_output(GateId(7)),
        Some(false),
        "With S true and D1 false, output should be false"
    );

    // Case 3: Switch data inputs.
    // Set D0 = false, D1 = true, S false → output should equal D0 (false).
    circuit.set_input(GateId(0), false);
    circuit.set_input(GateId(1), true);
    circuit.set_input(GateId(2), false);
    circuit.tick().expect("Tick should succeed");
    assert_eq!(
        circuit.read_output(GateId(7)),
        Some(false),
        "With S false and D0 false, output should be false"
    );

    // Case 4: Set S true → output should equal D1 (true).
    circuit.set_input(GateId(0), false);
    circuit.set_input(GateId(1), true);
    circuit.set_input(GateId(2), true);
    circuit.tick().expect("Tick should succeed");
    assert_eq!(
        circuit.read_output(GateId(7)),
        Some(true),
        "With S true and D1 true, output should be true"
    );
}

#[test]
fn test_simple_cycle() {
    let mut circuit = Circuit::new();

    let input_gate = Gate::new_input(GateId(0), None);
    let output_gate = Gate::new_output(GateId(1));
    let and_gate = Gate::new_and(GateId(2));
    let input_id = input_gate.id;
    let output_id = output_gate.id;
    let and_id = and_gate.id;

    circuit.add_gate(input_gate);
    circuit.add_gate(output_gate);
    circuit.add_gate(and_gate);

    circuit.add_connection(input_id, and_id, 0);
    circuit.add_connection(and_id, output_id, 0);
    circuit.add_connection(output_id, input_id, 0); // This creates a cycle

    assert_eq!(circuit.tick(), Err(TickError::CycleDetected));
}
