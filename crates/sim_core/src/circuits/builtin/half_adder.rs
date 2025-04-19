use crate::{Circuit, CircuitTemplate};

/// A half-adder template.
///
/// This template creates a circuit with:
/// - Two input pins (input 0: A, input 1: B)
/// - Two output pins (output 0: Sum, output 1: Carry)
///
/// It builds the following logic:
///   not_A = NOT A
///   not_B = NOT B
///   and1 = A AND (NOT B)
///   and2 = (NOT A) AND B
///   sum  = (and1 OR and2)       // XOR-like function for sum
///   carry = A AND B
///
/// The connections are made so that:
///   A feeds both the NOT gate (to compute not_A) and directly into the carry gate and and1.
///   B feeds both the NOT gate (to compute not_B) and directly into the carry gate and and2.
///   The outputs of and1 and and2 feed into an OR gate, which then connects to the Sum output.
///   The carry gate connects directly to the Carry output.
pub struct HalfAdderTemplate;

impl CircuitTemplate for HalfAdderTemplate {
    fn template() -> Circuit {
        // Create a new circuit (which gets a UUID)
        let mut circuit = Circuit::new();

        // Create two input gates for A and B.
        // Use input pins with indices 0 and 1.
        let a_gate_id = circuit.add_input_pin(0);
        let b_gate_id = circuit.add_input_pin(1);

        // Create two NOT gates to compute NOT A and NOT B.
        let not_a_gate_id = circuit.add_not();
        let not_b_gate_id = circuit.add_not();

        // Create two AND gates for the XOR branches:
        // and1 = A AND (NOT B)
        let and1_gate_id = circuit.add_and();
        // and2 = (NOT A) AND B
        let and2_gate_id = circuit.add_and();

        // Create an OR gate for the Sum output:
        let or_gate_id = circuit.add_or();

        // Create an AND gate for the Carry output:
        let carry_gate_id = circuit.add_and();

        // Create two output gates: one for the Sum and one for the Carry.
        let sum_output_id = circuit.add_output_pin(0); // Sum output on pin 0
        let carry_output_id = circuit.add_output_pin(1); // Carry output on pin 1

        // === Build the connections ===
        // Compute NOT A and NOT B:
        circuit.add_connection(a_gate_id, not_a_gate_id, 0, 0); // A -> NOT A
        circuit.add_connection(b_gate_id, not_b_gate_id, 0, 0); // B -> NOT B

        // Build AND branch for and1 = A AND (NOT B)
        circuit.add_connection(a_gate_id, and1_gate_id, 0, 0); // A -> and1 input 0
        circuit.add_connection(not_b_gate_id, and1_gate_id, 1, 0); // NOT B -> and1 input 1

        // Build AND branch for and2 = (NOT A) AND B
        circuit.add_connection(not_a_gate_id, and2_gate_id, 0, 0); // NOT A -> and2 input 0
        circuit.add_connection(b_gate_id, and2_gate_id, 1, 0); // B -> and2 input 1

        // Sum: (and1 OR and2)
        circuit.add_connection(and1_gate_id, or_gate_id, 0, 0); // and1 -> OR input 0
        circuit.add_connection(and2_gate_id, or_gate_id, 1, 0); // and2 -> OR input 1
        circuit.add_connection(or_gate_id, sum_output_id, 0, 0); // OR gate -> Sum output

        // Carry: A AND B
        circuit.add_connection(a_gate_id, carry_gate_id, 0, 0); // A -> Carry AND input 0
        circuit.add_connection(b_gate_id, carry_gate_id, 1, 0); // B -> Carry AND input 1
        circuit.add_connection(carry_gate_id, carry_output_id, 0, 0); // Carry gate -> Carry output

        circuit
    }
}
