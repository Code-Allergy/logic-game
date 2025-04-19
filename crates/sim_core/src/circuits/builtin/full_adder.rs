use crate::{Circuit, CircuitTemplate};

// A full adder template that produces a circuit with three inputs (A, B, Cin)
// and two outputs (Sum and Carry).
pub struct FullAdderTemplate;

impl CircuitTemplate for FullAdderTemplate {
    fn template() -> Circuit {
        let mut circuit = Circuit::new();

        // --- Input Gates ---
        // Create three input gates for A, B, and Cin.
        // (We assume the 'add_input_pin' method assigns the port number for external mapping.)
        let a_id = circuit.add_input_pin(0); // Input A on port 0
        let b_id = circuit.add_input_pin(1); // Input B on port 1
        let cin_id = circuit.add_input_pin(2); // Carry-in on port 2

        // --- Half-Adder 1: Computes A XOR B and Carry1 = A AND B ---
        // To construct XOR, we use two NOTs, two ANDs, and one OR.
        // NOT gates for inputs (to compute inverse as needed)
        let not_a = circuit.add_not();
        let not_b = circuit.add_not();
        circuit.add_connection(a_id, not_a, 0, 0); // A -> NOT A
        circuit.add_connection(b_id, not_b, 0, 0); // B -> NOT B

        // AND for A AND (NOT B)
        let and1 = circuit.add_and();
        circuit.add_connection(a_id, and1, 0, 0);
        circuit.add_connection(not_b, and1, 1, 0);

        // AND for (NOT A) AND B
        let and2 = circuit.add_and();
        circuit.add_connection(not_a, and2, 0, 0);
        circuit.add_connection(b_id, and2, 1, 0);

        // OR to combine the two ANDs (this is A XOR B, call it sum1)
        let or1 = circuit.add_or();
        circuit.add_connection(and1, or1, 0, 0);
        circuit.add_connection(and2, or1, 1, 0);

        // Also compute carry1 = A AND B
        let and3 = circuit.add_and();
        circuit.add_connection(a_id, and3, 0, 0);
        circuit.add_connection(b_id, and3, 1, 0);

        // --- Half-Adder 2: Computes final Sum = sum1 XOR Cin and carry2 = sum1 AND Cin ---
        // For the second half-adder, inputs are sum1 (from or1) and Cin.
        let not_sum1 = circuit.add_not();
        let not_cin = circuit.add_not();
        circuit.add_connection(or1, not_sum1, 0, 0); // Compute NOT sum1.
        circuit.add_connection(cin_id, not_cin, 0, 0); // Compute NOT Cin.

        // AND for sum1 AND (NOT Cin)
        let and4 = circuit.add_and();
        circuit.add_connection(or1, and4, 0, 0);
        circuit.add_connection(not_cin, and4, 1, 0);

        // AND for (NOT sum1) AND Cin
        let and5 = circuit.add_and();
        circuit.add_connection(not_sum1, and5, 0, 0);
        circuit.add_connection(cin_id, and5, 1, 0);

        // OR to combine and4 and and5 to get the final Sum output.
        let or2 = circuit.add_or();
        circuit.add_connection(and4, or2, 0, 0);
        circuit.add_connection(and5, or2, 1, 0);

        // Also compute carry2 = sum1 AND Cin
        let and6 = circuit.add_and();
        circuit.add_connection(or1, and6, 0, 0);
        circuit.add_connection(cin_id, and6, 1, 0);

        // --- Final Carry: Combine carry1 and carry2 ---
        let or3 = circuit.add_or();
        circuit.add_connection(and3, or3, 0, 0);
        circuit.add_connection(and6, or3, 1, 0);

        // --- Output Gates ---
        // Create two output gates for Sum (port 0) and Carry (port 1).
        let sum_out = circuit.add_output_pin(0);
        let carry_out = circuit.add_output_pin(1);

        // Connect the final Sum and Carry outputs.
        circuit.add_connection(or2, sum_out, 0, 0);
        circuit.add_connection(or3, carry_out, 0, 0);

        circuit
    }
}
