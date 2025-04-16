use crate::{
    Circuit, CircuitTemplate, GateData, GateId, GateType, LogicComponent, TickError,
    circuits::builtin::{FullAdderTemplate, HalfAdderTemplate, MultiplexerTemplate},
    gates::compute_gate_logic,
};

#[test]
fn test_compute_gate_logic_and() {
    assert_eq!(
        compute_gate_logic(GateType::And, &mut GateData::None, &vec![true, true]),
        true,
        "AND gate should return true when both inputs are true"
    );
    assert_eq!(
        compute_gate_logic(GateType::And, &mut GateData::None, &vec![true, false]),
        false,
        "AND gate should return false when one input is false"
    );
    assert_eq!(
        compute_gate_logic(GateType::And, &mut GateData::None, &vec![false, false]),
        false,
        "AND gate should return false when both inputs are false"
    );
    assert_eq!(
        compute_gate_logic(GateType::And, &mut GateData::None, &vec![false, false]),
        false,
        "AND gate should return false when both inputs are false"
    );
}

#[test]
#[should_panic]
fn test_compute_gate_logic_and_empty() {
    compute_gate_logic(GateType::And, &mut GateData::None, &vec![]);
}

#[test]
#[should_panic]
fn test_compute_gate_logic_and_low() {
    assert_eq!(
        compute_gate_logic(GateType::And, &mut GateData::None, &vec![true]),
        false,
        "AND gate should return false when only one input is provided"
    );
}

#[test]
#[should_panic]
fn test_compute_gate_logic_and_high() {
    assert_eq!(
        compute_gate_logic(GateType::And, &mut GateData::None, &vec![true, true, true]),
        true,
        "AND gate should return true when all inputs are true"
    );
}

#[test]
fn test_compute_gate_logic_or() {
    assert_eq!(
        compute_gate_logic(GateType::Or, &mut GateData::None, &vec![true, true]),
        true,
        "OR gate should return true when both inputs are true"
    );
    assert_eq!(
        compute_gate_logic(GateType::Or, &mut GateData::None, &vec![true, false]),
        true,
        "OR gate should return true when one input is true"
    );
    assert_eq!(
        compute_gate_logic(GateType::Or, &mut GateData::None, &vec![false, true]),
        true,
        "OR gate should return true when one input is true"
    );
    assert_eq!(
        compute_gate_logic(GateType::Or, &mut GateData::None, &vec![false, false]),
        false,
        "OR gate should return false when both inputs are false"
    );
}

#[test]
#[should_panic]
fn test_compute_gate_logic_or_empty() {
    compute_gate_logic(GateType::Or, &mut GateData::None, &vec![]);
}

#[test]
#[should_panic]
fn test_compute_gate_logic_or_low() {
    assert_eq!(
        compute_gate_logic(GateType::Or, &mut GateData::None, &vec![true]),
        true,
        "OR gate should return true when only one input is provided"
    );
}

#[test]
#[should_panic]
fn test_compute_gate_logic_or_high() {
    assert_eq!(
        compute_gate_logic(GateType::Or, &mut GateData::None, &vec![true, true, true]),
        true,
        "OR gate should return true when all inputs are true"
    );
}

#[test]
fn test_compute_gate_logic_not() {
    assert_eq!(
        compute_gate_logic(GateType::Not, &mut GateData::None, &vec![true]),
        false,
        "NOT gate should return false when input is true"
    );
    assert_eq!(
        compute_gate_logic(GateType::Not, &mut GateData::None, &vec![false]),
        true,
        "NOT gate should return true when input is false"
    );
}

#[test]
#[should_panic]
fn test_compute_gate_logic_not_empty() {
    compute_gate_logic(GateType::Not, &mut GateData::None, &vec![]);
}

#[test]
#[should_panic]
fn test_compute_gate_logic_not_high() {
    assert_eq!(
        compute_gate_logic(GateType::Not, &mut GateData::None, &vec![true, false]),
        false,
        "NOT gate should return the not of the first value provided"
    );
}

#[test]
fn test_compute_gate_logic_input() {
    assert_eq!(
        compute_gate_logic(GateType::Input, &mut GateData::None, &vec![true]),
        true,
        "Input gate should return true when input is true"
    );
    assert_eq!(
        compute_gate_logic(GateType::Input, &mut GateData::None, &vec![false]),
        false,
        "Input gate should return false when input is false"
    );
}

#[test]
#[should_panic]
fn test_compute_gate_logic_input_empty() {
    compute_gate_logic(GateType::Input, &mut GateData::None, &vec![]);
}

#[test]
#[should_panic]
fn test_compute_gate_logic_input_high() {
    compute_gate_logic(GateType::Input, &mut GateData::None, &vec![true, false]); // should always panic
}

#[test]
fn test_compute_gate_logic_output() {
    assert_eq!(
        compute_gate_logic(GateType::Output, &mut GateData::None, &vec![true]),
        true,
        "Output gate should return true when input is true"
    );
    assert_eq!(
        compute_gate_logic(GateType::Output, &mut GateData::None, &vec![false]),
        false,
        "Output gate should return false when input is false"
    );
}

#[test]
fn test_compute_gate_logic_oscillate_2t() {
    let mut gate_data = GateData::Oscillator {
        frequency: 2,
        tick: 0,
    };

    assert_eq!(
        compute_gate_logic(GateType::Oscillator, &mut gate_data, &vec![false]),
        false,
        "Oscillator gate should return false for the first tick"
    );
    assert_eq!(
        compute_gate_logic(GateType::Oscillator, &mut gate_data, &vec![false]),
        true,
        "Oscillator gate should return true on the 2nd tick"
    );
    assert_eq!(
        compute_gate_logic(GateType::Oscillator, &mut gate_data, &vec![false]),
        false,
        "Oscillator gate should return false on the 3rd tick"
    );
    assert_eq!(
        compute_gate_logic(GateType::Oscillator, &mut gate_data, &vec![false]),
        true,
        "Oscillator gate should return true on the 4th tick"
    );
}

#[test]
fn test_compute_gate_logic_oscillate_5t() {
    let mut gate_data = GateData::Oscillator {
        frequency: 5,
        tick: 0,
    };

    assert_eq!(
        compute_gate_logic(GateType::Oscillator, &mut gate_data, &vec![false]),
        false,
        "Oscillator gate should return false for the first tick"
    );
    assert_eq!(
        compute_gate_logic(GateType::Oscillator, &mut gate_data, &vec![false]),
        false,
        "Oscillator gate should return true on the 2nd tick"
    );
    assert_eq!(
        compute_gate_logic(GateType::Oscillator, &mut gate_data, &vec![false]),
        false,
        "Oscillator gate should return false on the 3rd tick"
    );
    assert_eq!(
        compute_gate_logic(GateType::Oscillator, &mut gate_data, &vec![false]),
        false,
        "Oscillator gate should return true on the 4th tick"
    );
    assert_eq!(
        compute_gate_logic(GateType::Oscillator, &mut gate_data, &vec![false]),
        true,
        "Oscillator gate should return true on the 5th tick"
    );
}

#[test]
#[should_panic]
fn test_compute_gate_logic_output_empty() {
    compute_gate_logic(GateType::Output, &mut GateData::None, &vec![]);
}

#[test]
#[should_panic]
fn test_compute_gate_logic_output_high() {
    compute_gate_logic(GateType::Output, &mut GateData::None, &vec![true, false]); // should always panic
}

#[test]
fn test_simple_circuit_input_output() {
    let mut circuit = Circuit::new();
    let input_id = circuit.add_input_pin(None, 0);
    let output_id = circuit.add_output_pin(0);

    circuit.add_connection(input_id, output_id, 0, 0);
    circuit.set_input(0, Some(true));
    dbg!(&circuit);
    circuit.tick().expect("Tick should succeed");

    assert_eq!(
        circuit.get_output(0),
        Some(true),
        "Circuit should return true when input is true"
    );

    circuit.set_input(0, Some(false));
    circuit.tick().expect("Tick should succeed");

    assert_eq!(
        circuit.get_output(0),
        Some(false),
        "Circuit should return false when input is false"
    );
}

#[test]
fn test_simple_circuit_identity() {
    let mut circuit = Circuit::new();

    let input_id = circuit.add_input_pin(None, 0);
    let output_id = circuit.add_output_pin(0);
    let identity_id = circuit.add_identity();

    circuit.add_connection(input_id, identity_id, 0, 0);
    circuit.add_connection(identity_id, output_id, 0, 0);
    circuit.set_input(0, Some(true));
    circuit.tick().expect("Tick should succeed");

    assert_eq!(
        circuit.get_output(0),
        Some(true),
        "Circuit should return true when input is true"
    );

    circuit.set_input(0, Some(false));
    circuit.tick().expect("Tick should succeed");

    assert_eq!(
        circuit.get_output(0),
        Some(false),
        "Circuit should return false when input is false"
    );
}

#[test]
fn test_simple_circuit_not() {
    let mut circuit = Circuit::new();

    let input_id = circuit.add_input_pin(None, 0);
    let output_id: GateId = circuit.add_output_pin(0);
    let not_id = circuit.add_not();

    circuit.add_connection(input_id, not_id, 0, 0);
    circuit.add_connection(not_id, output_id, 0, 0);
    circuit.set_input(0, Some(true));
    circuit.tick().expect("Tick should succeed");

    assert_eq!(
        circuit.get_output(0),
        Some(false),
        "Circuit should return false when input is true"
    );

    circuit.set_input(0, Some(false));
    circuit.tick().expect("Tick should succeed");

    assert_eq!(
        circuit.get_output(0),
        Some(true),
        "Circuit should return true when input is false"
    );
}

#[test]
fn test_simple_circuit_or() {
    let mut circuit = Circuit::new();

    let input_id = circuit.add_input_pin(None, 0);
    let input_id2 = circuit.add_input_pin(None, 1);
    let output_id = circuit.add_output_pin(0);
    let or_id = circuit.add_or();

    circuit.add_connection(input_id, or_id, 0, 0);
    circuit.add_connection(input_id2, or_id, 1, 0);
    circuit.add_connection(or_id, output_id, 0, 0);
    circuit.set_input(0, Some(true));
    circuit.set_input(1, Some(true));
    circuit.tick().expect("Tick should succeed");

    assert_eq!(
        circuit.get_output(0),
        Some(true),
        "Circuit should return true when input is true, true: {:?}",
        circuit
    );

    circuit.set_input(0, Some(false));
    circuit.set_input(1, Some(true));
    circuit.tick().expect("Tick should succeed");

    assert_eq!(
        circuit.get_output(0),
        Some(true),
        "Circuit should return true when input is false, true: {:?}",
        circuit
    );

    circuit.set_input(0, Some(true));
    circuit.set_input(1, Some(false));
    circuit.tick().expect("Tick should succeed");

    assert_eq!(
        circuit.get_output(0),
        Some(true),
        "Circuit should return true when input is true, false: {:?}",
        circuit
    );

    circuit.set_input(0, Some(false));
    circuit.set_input(1, Some(false));
    circuit.tick().expect("Tick should succeed");

    assert_eq!(
        circuit.get_output(0),
        Some(false),
        "Circuit should return false when input is false, false: {:?}",
        circuit
    );
}

#[test]
fn test_simple_circuit_and() {
    let mut circuit = Circuit::new();

    let input_id = circuit.add_input_pin(None, 0);
    let input_id2 = circuit.add_input_pin(None, 1);
    let output_id = circuit.add_output_pin(0);
    let and_id = circuit.add_and();

    circuit.add_connection(input_id, and_id, 0, 0);
    circuit.add_connection(input_id2, and_id, 1, 0);
    circuit.add_connection(and_id, output_id, 0, 0);
    circuit.set_input(0, Some(true));
    circuit.set_input(1, Some(true));
    circuit.tick().expect("Tick should succeed");

    assert_eq!(
        circuit.get_output(0),
        Some(true),
        "Circuit should return true when input is true, true: {:?}",
        circuit
    );

    circuit.set_input(0, Some(false));
    circuit.set_input(1, Some(true));
    circuit.tick().expect("Tick should succeed");

    assert_eq!(
        circuit.get_output(0),
        Some(false),
        "Circuit should return false when input is false, true: {:?}",
        circuit
    );

    circuit.set_input(0, Some(true));
    circuit.set_input(1, Some(false));
    circuit.tick().expect("Tick should succeed");

    assert_eq!(
        circuit.get_output(0),
        Some(false),
        "Circuit should return false when input is true, false: {:?}",
        circuit
    );

    circuit.set_input(0, Some(false));
    circuit.set_input(1, Some(false));
    circuit.tick().expect("Tick should succeed");

    assert_eq!(
        circuit.get_output(0),
        Some(false),
        "Circuit should return false when input is false, false: {:?}",
        circuit
    );
}

#[test]
fn test_topological_sort() {
    let mut circuit = Circuit::new();

    let input_id = circuit.add_input_pin(None, 0);
    let output_id = circuit.add_output_pin(0);
    let and_id = circuit.add_and();

    circuit.add_connection(input_id, and_id, 0, 0);
    circuit.add_connection(and_id, output_id, 0, 0);

    assert!(circuit.topological_sort().is_some());

    // Create a cycle
    circuit.add_connection(output_id, input_id, 0, 0);
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

    let input_id = circuit.add_input_pin(None, 0);
    let output_id = circuit.add_output_pin(0);
    let output_id2 = circuit.add_output_pin(1);
    let identity_id = circuit.add_identity();
    let not_id = circuit.add_not();

    circuit.add_connection(input_id, identity_id, 0, 0);
    circuit.add_connection(input_id, not_id, 0, 0);
    circuit.add_connection(identity_id, output_id, 0, 0);
    circuit.add_connection(not_id, output_id2, 0, 0);

    circuit.set_input(0, Some(true));
    circuit.tick().expect("Tick should succeed");

    let output1 = circuit.get_output(0);
    let output2 = circuit.get_output(1);

    assert_eq!(
        (output1, output2),
        (Some(true), Some(false)),
        "Circuit should return true,false when input is true"
    );

    circuit.set_input(0, Some(false));
    circuit.tick().expect("Tick should succeed");

    let output1 = circuit.get_output(0);
    let output2 = circuit.get_output(1);

    assert_eq!(
        (output1, output2),
        (Some(false), Some(true)),
        "Circuit should return false,true when input is true"
    );
}

#[test]
fn test_simple_circuit_deep_chain() {
    let mut circuit = Circuit::new();

    let input_id = circuit.add_input_pin(None, 0);
    let input_id2 = circuit.add_input_pin(None, 1);
    let output_id = circuit.add_output_pin(0);
    let and_id = circuit.add_and();
    let or_id = circuit.add_or();
    let not_id = circuit.add_not();

    circuit.add_connection(input_id, and_id, 0, 0);
    circuit.add_connection(input_id2, and_id, 1, 0);
    circuit.add_connection(and_id, or_id, 1, 0);
    circuit.add_connection(input_id2, or_id, 0, 0);
    circuit.add_connection(or_id, not_id, 0, 0);
    circuit.add_connection(not_id, output_id, 0, 0);

    circuit.set_input(0, Some(true));
    circuit.set_input(1, Some(true));

    circuit.tick().expect("Tick should succeed");
    assert_eq!(
        circuit.get_output(0),
        Some(false),
        "Circuit should return false when input is true, true: {:?}",
        circuit
    );

    circuit.set_input(0, Some(false));
    circuit.set_input(1, Some(true));
    circuit.tick().expect("Tick should succeed");
    assert_eq!(
        circuit.get_output(0),
        Some(false),
        "Circuit should return false when input is false, true: {:?}",
        circuit
    );

    circuit.set_input(0, Some(true));
    circuit.set_input(1, Some(false));
    circuit.tick().expect("Tick should succeed");
    assert_eq!(
        circuit.get_output(0),
        Some(true),
        "Circuit should return false when input is true, false: {:?}",
        circuit
    );

    circuit.set_input(0, Some(false));
    circuit.set_input(1, Some(false));
    circuit.tick().expect("Tick should succeed");
    assert_eq!(
        circuit.get_output(0),
        Some(true),
        "Circuit should return false when input is false, false: {:?}",
        circuit
    );
}

#[test]
fn test_multiplexer_template() {
    // pins:
    // d0 = 0
    // d1 = 1
    // s = 2
    let mut circuit = MultiplexerTemplate::template();

    // Test the multiplexer:
    //
    // Case 1: S false, output should equal D0.
    // Set D0 = true, D1 = false, S = false.
    circuit.set_input(0, Some(true));
    circuit.set_input(1, Some(false));
    circuit.set_input(2, Some(false));
    circuit.tick().expect("Tick should succeed");
    assert_eq!(
        circuit.get_output(0),
        Some(true),
        "With S false and D0 true, output should be true"
    );

    // Case 2: S true, output should equal D1.
    // Set D0 = true, D1 = false, S = true.
    circuit.set_input(0, Some(true));
    circuit.set_input(1, Some(false));
    circuit.set_input(2, Some(true));
    circuit.tick().expect("Tick should succeed");
    assert_eq!(
        circuit.get_output(0),
        Some(false),
        "With S true and D1 false, output should be false"
    );

    // Case 3: Switch data inputs.
    // Set D0 = false, D1 = true, S false → output should equal D0 (false).
    circuit.set_input(0, Some(false));
    circuit.set_input(1, Some(true));
    circuit.set_input(2, Some(false));
    circuit.tick().expect("Tick should succeed");
    assert_eq!(
        circuit.get_output(0),
        Some(false),
        "With S false and D0 false, output should be false"
    );

    // Case 4: Set S true → output should equal D1 (true).
    circuit.set_input(0, Some(false));
    circuit.set_input(1, Some(true));
    circuit.set_input(2, Some(true));
    circuit.tick().expect("Tick should succeed");
    assert_eq!(
        circuit.get_output(0),
        Some(true),
        "With S true and D1 true, output should be true"
    );
}

#[test]
fn test_simple_cycle() {
    let mut circuit = Circuit::new();

    let input_id = circuit.add_input(None);
    let output_id = circuit.add_output();
    let and_id = circuit.add_and();

    circuit.add_connection(input_id, and_id, 0, 0);
    circuit.add_connection(and_id, output_id, 0, 0);
    circuit.add_connection(output_id, input_id, 0, 0); // This creates a cycle

    assert_eq!(circuit.tick(), Err(TickError::CycleDetected));
}

#[test]
fn test_nested_multiplexer() {
    // Outer circuit
    let mut outer = Circuit::new();

    // Inner multiplexer circuit
    let inner_mux = MultiplexerTemplate::template();
    let inner_mux_id = inner_mux.id();

    // Outer circuit IO
    let in0 = outer.add_input_pin(Some(false), 0);
    let in1 = outer.add_input_pin(Some(false), 1);
    let in2 = outer.add_input_pin(Some(false), 2);
    let out = outer.add_output_pin(0);

    // Add inner multiplexer as a component
    outer.add_component(LogicComponent::Circuit(inner_mux));

    // Connect outer inputs to inner multiplexer
    outer.add_connection(in0, inner_mux_id, 0, 0);
    outer.add_connection(in1, inner_mux_id, 1, 0);
    outer.add_connection(in2, inner_mux_id, 2, 0);

    // Connect inner multiplexer output to outer output
    outer.add_connection(inner_mux_id, out, 0, 0);

    // Test: S = false, output should equal D0
    outer.set_input(0, Some(true));
    outer.set_input(1, Some(false));
    outer.set_input(2, Some(false));
    outer.tick().expect("Tick should succeed");
    assert_eq!(outer.get_output(0), Some(true));

    // Test: S = true, output should equal D1
    outer.set_input(0, Some(true));
    outer.set_input(1, Some(false));
    outer.set_input(2, Some(true));
    outer.tick().expect("Tick should succeed");
    assert_eq!(outer.get_output(0), Some(false));
}

#[test]
fn test_nested_half_adder() {
    let mut outer = Circuit::new();
    let inner_ha = HalfAdderTemplate::template();
    let inner_ha_id = inner_ha.id();

    let in0 = outer.add_input_pin(Some(false), 0);
    let in1 = outer.add_input_pin(Some(false), 1);
    let out = outer.add_output_pin(0);

    outer.add_component(LogicComponent::Circuit(inner_ha));
    outer.add_connection(in0, inner_ha_id, 0, 0);
    outer.add_connection(in1, inner_ha_id, 1, 0);
    outer.add_connection(inner_ha_id, out, 0, 0);

    // 0 + 0
    outer.set_input(0, Some(false));
    outer.set_input(1, Some(false));
    outer.tick().expect("Tick should succeed");
    assert_eq!(outer.get_output(0), Some(false));

    // 1 + 0
    outer.set_input(0, Some(true));
    outer.set_input(1, Some(false));
    outer.tick().expect("Tick should succeed");
    assert_eq!(outer.get_output(0), Some(true));

    // 1 + 1
    outer.set_input(0, Some(true));
    outer.set_input(1, Some(true));
    outer.tick().expect("Tick should succeed");
    assert_eq!(outer.get_output(0), Some(false));
}

#[test]
fn test_nested_full_adder() {
    let mut outer = Circuit::new();
    let inner_fa = FullAdderTemplate::template();
    let inner_fa_id = inner_fa.id();

    let in0 = outer.add_input_pin(Some(false), 0);
    let in1 = outer.add_input_pin(Some(false), 1);
    let in2 = outer.add_input_pin(Some(false), 2);
    let out = outer.add_output_pin(0);

    outer.add_component(LogicComponent::Circuit(inner_fa));
    outer.add_connection(in0, inner_fa_id, 0, 0);
    outer.add_connection(in1, inner_fa_id, 1, 0);
    outer.add_connection(in2, inner_fa_id, 2, 0);
    outer.add_connection(inner_fa_id, out, 0, 0);

    // 0 + 0 + 0
    outer.set_input(0, Some(false));
    outer.set_input(1, Some(false));
    outer.set_input(2, Some(false));
    outer.tick().expect("Tick should succeed");
    assert_eq!(outer.get_output(0), Some(false));

    // 1 + 0 + 0
    outer.set_input(0, Some(true));
    outer.set_input(1, Some(false));
    outer.set_input(2, Some(false));
    outer.tick().expect("Tick should succeed");
    assert_eq!(outer.get_output(0), Some(true));

    // 1 + 1 + 0
    outer.set_input(0, Some(true));
    outer.set_input(1, Some(true));
    outer.set_input(2, Some(false));
    outer.tick().expect("Tick should succeed");
    assert_eq!(outer.get_output(0), Some(false));

    // 1 + 1 + 1
    outer.set_input(0, Some(true));
    outer.set_input(1, Some(true));
    outer.set_input(2, Some(true));
    outer.tick().expect("Tick should succeed");
    assert_eq!(outer.get_output(0), Some(true));
}

#[test]
fn test_deeply_nested_circuits() {
    // Inner-most: a half adder
    let inner = HalfAdderTemplate::template();
    let inner_id = inner.id();

    // Middle: wraps the half adder
    let mut middle = Circuit::new();
    let in0 = middle.add_input_pin(Some(false), 0);
    let in1 = middle.add_input_pin(Some(false), 1);
    let out = middle.add_output_pin(0);
    middle.add_component(LogicComponent::Circuit(inner));
    middle.add_connection(in0, inner_id, 0, 0);
    middle.add_connection(in1, inner_id, 1, 0);
    middle.add_connection(inner_id, out, 0, 0);

    let middle_id = middle.id();

    // Outer: wraps the middle circuit
    let mut outer = Circuit::new();
    let oin0 = outer.add_input_pin(Some(false), 0);
    let oin1 = outer.add_input_pin(Some(false), 1);
    let oout = outer.add_output_pin(0);
    outer.add_component(LogicComponent::Circuit(middle));
    outer.add_connection(oin0, middle_id, 0, 0);
    outer.add_connection(oin1, middle_id, 1, 0);
    outer.add_connection(middle_id, oout, 0, 0);

    // 1 + 1
    outer.set_input(0, Some(true));
    outer.set_input(1, Some(true));
    outer.tick().expect("Tick should succeed");
    assert_eq!(outer.get_output(0), Some(false));

    // 1 + 0
    outer.set_input(0, Some(true));
    outer.set_input(1, Some(false));
    outer.tick().expect("Tick should succeed");
    assert_eq!(outer.get_output(0), Some(true));
}
