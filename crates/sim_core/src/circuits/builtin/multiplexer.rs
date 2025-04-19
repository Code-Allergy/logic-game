use crate::{Circuit, CircuitTemplate};

/// A simple 2-to-1 multiplexer circuit template.
/// This circuit takes two data inputs (D0 and D1) and a select input (S).
/// It outputs D0 if S is 0, and D1 if S is 1.
///
/// The circuit is structured as follows:
/// - Input gates for D0, D1, and S.
/// - A NOT gate to invert the select signal S.
/// - Two AND gates:
///  - One AND gate for the condition (NOT S AND D0).
/// - Another AND gate for the condition (S AND D1).
/// - An OR gate to combine the outputs of the two AND gates.
/// - An output gate to observe the final output.
///
/// The following input pins are available:
/// 0: d0
/// 1: d1
/// 2: s
///
/// The following output pin is available:
/// 0: output
pub struct MultiplexerTemplate;

impl CircuitTemplate for MultiplexerTemplate {
    fn template() -> Circuit {
        let mut circuit = Circuit::new();

        // Create input gates for data and select
        let d0_gate_id = circuit.add_input_pin(0); // Data input D0
        let d1_gate_id = circuit.add_input_pin(1); // Data input D1
        let s_gate_id = circuit.add_input_pin(2); // Select input S

        // Create a NOT gate to invert S
        let not_gate_id = circuit.add_not();

        // Create two AND gates:
        // one for (NOT S AND D0) and one for (S AND D1)
        let and_gate_left_id = circuit.add_and(); // (NOT S) AND D0
        let and_gate_right_id = circuit.add_and(); // S AND D1

        // Create an OR gate to combine the AND outputs
        let or_gate_id = circuit.add_or();

        // Create an output gate to observe the final output.
        let output_gate_id = circuit.add_output_pin(0);

        // Build the multiplexer connections:
        //
        // Invert the select signal:
        circuit.add_connection(s_gate_id, not_gate_id, 0, 0); // S -> NOT

        // Left AND gate for (NOT S AND D0)
        circuit.add_connection(d0_gate_id, and_gate_left_id, 0, 0); // D0 -> left AND input 0
        circuit.add_connection(not_gate_id, and_gate_left_id, 1, 0); // NOT S -> left AND input 1

        // Right AND gate for (S AND D1)
        circuit.add_connection(s_gate_id, and_gate_right_id, 0, 0); // S -> right AND input 0
        circuit.add_connection(d1_gate_id, and_gate_right_id, 1, 0); // D1 -> right AND input 1

        // OR gate combines both AND outputs:
        circuit.add_connection(and_gate_left_id, or_gate_id, 0, 0); // left AND -> OR input 0
        circuit.add_connection(and_gate_right_id, or_gate_id, 1, 0); // right AND -> OR input 1

        // Connect OR output to the final output gate
        circuit.add_connection(or_gate_id, output_gate_id, 0, 0);

        circuit
    }
}
