use crate::{Circuit, CircuitTemplate};

/// A simple 1-to-1 circuit template.
/// This circuit takes one data inputs D0
/// It outputs D0 if S is 0, and D1 if S is 1.
///
/// The circuit is structured as follows:
/// - Input gate for D0
/// - An output gate to observe the final output.
///
/// The following input pins are available:
/// 0: d0
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
