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
pub struct InputOutputCircuitTemplate;

impl CircuitTemplate for InputOutputCircuitTemplate {
    fn template() -> Circuit {
        let mut circuit = Circuit::new();

        let input_id = circuit.add_input_pin(0); // Data input D0
        let output_id = circuit.add_output_pin(0); // Output pin

        // connect input to output
        circuit.add_connection(input_id, output_id, 0, 0); // D0 -> output

        circuit
    }
}
