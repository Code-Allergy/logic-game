use crate::fixtures::builder::{Fixture, FixtureBuilder};

mod builder;
pub mod harness;

pub fn and_gate() -> Fixture {
    let mut b = FixtureBuilder::new();
    let a = b.input("A");
    let b_in = b.input("B");
    let and = b.and();
    let out = b.output("Y");

    b.circuit.connect((a, 0), (and, 0));
    b.circuit.connect((b_in, 0), (and, 1));
    b.circuit.connect((and, 0), (out, 0));

    b.build()
}
pub fn or_gate() -> Fixture {
    let mut b = FixtureBuilder::new();
    let a = b.input("A");
    let b_in = b.input("B");
    let or = b.or();
    let out = b.output("Y");

    b.circuit.connect((a, 0), (or, 0));
    b.circuit.connect((b_in, 0), (or, 1));
    b.circuit.connect((or, 0), (out, 0));

    b.build()
}
pub fn not_gate() -> Fixture {
    let mut b = FixtureBuilder::new();
    let a = b.input("A");
    let not = b.not();
    let out = b.output("Y");

    b.circuit.connect((a, 0), (not, 0));
    b.circuit.connect((not, 0), (out, 0));

    b.build()
}

pub fn nand_gate() -> Fixture {
    let mut b = FixtureBuilder::new();
    let a = b.input("A");
    let b_in = b.input("B");
    let and = b.and();
    let not = b.not();
    let out = b.output("Y");

    b.circuit.connect((a, 0), (and, 0));
    b.circuit.connect((b_in, 0), (and, 1));
    b.circuit.connect((and, 0), (not, 0));
    b.circuit.connect((not, 0), (out, 0));

    b.build()
}

pub fn nor_gate() -> Fixture {
    let mut b = FixtureBuilder::new();
    let a = b.input("A");
    let b_in = b.input("B");
    let or = b.or();
    let not = b.not();
    let out = b.output("Y");

    b.circuit.connect((a, 0), (or, 0));
    b.circuit.connect((b_in, 0), (or, 1));
    b.circuit.connect((or, 0), (not, 0));
    b.circuit.connect((not, 0), (out, 0));

    b.build()
}

pub fn xor_gate() -> Fixture {
    // A XOR B = (A OR B) AND NOT (A AND B)
    let mut b = FixtureBuilder::new();
    let a = b.input("A");
    let b_in = b.input("B");
    let or = b.or();
    let and = b.and();
    let not = b.not();
    let final_and = b.and();
    let out = b.output("Y");

    b.circuit.connect((a, 0), (or, 0));
    b.circuit.connect((b_in, 0), (or, 1));

    b.circuit.connect((a, 0), (and, 0));
    b.circuit.connect((b_in, 0), (and, 1));

    b.circuit.connect((and, 0), (not, 0));

    b.circuit.connect((or, 0), (final_and, 0));
    b.circuit.connect((not, 0), (final_and, 1));

    b.circuit.connect((final_and, 0), (out, 0));

    b.build()
}

pub fn xnor_gate() -> Fixture {
    // XNOR is NOT XOR
    let mut b = FixtureBuilder::new();
    let a = b.input("A");
    let b_in = b.input("B");
    let or = b.or();
    let and = b.and();
    let not1 = b.not();
    let final_and = b.and();
    let not2 = b.not();
    let out = b.output("Y");

    b.circuit.connect((a, 0), (or, 0));
    b.circuit.connect((b_in, 0), (or, 1));

    b.circuit.connect((a, 0), (and, 0));
    b.circuit.connect((b_in, 0), (and, 1));

    b.circuit.connect((and, 0), (not1, 0));

    b.circuit.connect((or, 0), (final_and, 0));
    b.circuit.connect((not1, 0), (final_and, 1));

    b.circuit.connect((final_and, 0), (not2, 0));
    b.circuit.connect((not2, 0), (out, 0));

    b.build()
}

pub fn half_adder() -> Fixture {
    // Inputs: A, B
    // Outputs: Sum (A XOR B), Carry (A AND B)
    let mut b = FixtureBuilder::new();
    let a = b.input("A");
    let b_in = b.input("B");

    // XOR part for Sum
    let or = b.or();
    let and1 = b.and();
    let not1 = b.not();
    let and2 = b.and(); // This is the final AND of XOR

    // AND part for Carry
    let and3 = b.and();

    let sum = b.output("Sum");
    let carry = b.output("Carry");

    // Connect XOR
    b.circuit.connect((a, 0), (or, 0));
    b.circuit.connect((b_in, 0), (or, 1));

    b.circuit.connect((a, 0), (and1, 0));
    b.circuit.connect((b_in, 0), (and1, 1));

    b.circuit.connect((and1, 0), (not1, 0));

    b.circuit.connect((or, 0), (and2, 0));
    b.circuit.connect((not1, 0), (and2, 1));

    b.circuit.connect((and2, 0), (sum, 0));

    // Connect Carry
    b.circuit.connect((a, 0), (and3, 0));
    b.circuit.connect((b_in, 0), (and3, 1));

    b.circuit.connect((and3, 0), (carry, 0));

    b.build()
}

pub fn full_adder() -> Fixture {
    // Inputs: A, B, Cin
    // Outputs: Sum, Cout
    // Sum = A XOR B XOR Cin
    // Cout = (A AND B) OR (Cin AND (A XOR B))
    let mut b = FixtureBuilder::new();
    let a = b.input("A");
    let b_in = b.input("B");
    let cin = b.input("Cin");

    // First Half Adder (A, B) -> Sum1, Carry1
    let or1 = b.or();
    let and1 = b.and();
    let not1 = b.not();
    let xor_and1 = b.and(); // Sum1
    let carry1 = b.and();

    // Connect First HA
    b.circuit.connect((a, 0), (or1, 0));
    b.circuit.connect((b_in, 0), (or1, 1));
    b.circuit.connect((a, 0), (and1, 0));
    b.circuit.connect((b_in, 0), (and1, 1));
    b.circuit.connect((and1, 0), (not1, 0));
    b.circuit.connect((or1, 0), (xor_and1, 0));
    b.circuit.connect((not1, 0), (xor_and1, 1));
    b.circuit.connect((a, 0), (carry1, 0));
    b.circuit.connect((b_in, 0), (carry1, 1));

    // Second Half Adder (Sum1, Cin) -> Sum, Carry2
    let or2 = b.or();
    let and2 = b.and();
    let not2 = b.not();
    let xor_and2 = b.and(); // Final Sum
    let carry2 = b.and();

    // Connect Second HA
    b.circuit.connect((xor_and1, 0), (or2, 0));
    b.circuit.connect((cin, 0), (or2, 1));
    b.circuit.connect((xor_and1, 0), (and2, 0));
    b.circuit.connect((cin, 0), (and2, 1));
    b.circuit.connect((and2, 0), (not2, 0));
    b.circuit.connect((or2, 0), (xor_and2, 0));
    b.circuit.connect((not2, 0), (xor_and2, 1));
    b.circuit.connect((xor_and1, 0), (carry2, 0));
    b.circuit.connect((cin, 0), (carry2, 1));

    // Final Cout = Carry1 OR Carry2
    let cout_or = b.or();
    b.circuit.connect((carry1, 0), (cout_or, 0));
    b.circuit.connect((carry2, 0), (cout_or, 1));

    let sum = b.output("Sum");
    let cout = b.output("Cout");

    b.circuit.connect((xor_and2, 0), (sum, 0));
    b.circuit.connect((cout_or, 0), (cout, 0));

    b.build()
}

pub fn multiplexer() -> Fixture {
    // 2-to-1 MUX
    // Inputs: D0, D1, Sel
    // Output: Y = (D0 AND NOT Sel) OR (D1 AND Sel)
    let mut b = FixtureBuilder::new();
    let d0 = b.input("D0");
    let d1 = b.input("D1");
    let sel = b.input("Sel");

    let not_sel = b.not();
    let and0 = b.and();
    let and1 = b.and();
    let or = b.or();
    let out = b.output("Y");

    b.circuit.connect((sel, 0), (not_sel, 0));

    b.circuit.connect((d0, 0), (and0, 0));
    b.circuit.connect((not_sel, 0), (and0, 1));

    b.circuit.connect((d1, 0), (and1, 0));
    b.circuit.connect((sel, 0), (and1, 1));

    b.circuit.connect((and0, 0), (or, 0));
    b.circuit.connect((and1, 0), (or, 1));

    b.circuit.connect((or, 0), (out, 0));

    b.build()
}

pub fn sr_latch() -> Fixture {
    // NOR-based SR Latch
    // Inputs: R, S
    // Outputs: Q, Qn
    // NOR1: R, Qn -> Q
    // NOR2: S, Q -> Qn
    let mut b = FixtureBuilder::new();
    let r = b.input("R");
    let s = b.input("S");

    // NOR1
    let or1 = b.or();
    let not1 = b.not(); // Output of this is Q

    // NOR2
    let or2 = b.or();
    let not2 = b.not(); // Output of this is Qn

    b.circuit.connect((r, 0), (or1, 0));
    b.circuit.connect((not2, 0), (or1, 1)); // Feedback Qn to NOR1
    b.circuit.connect((or1, 0), (not1, 0));

    b.circuit.connect((s, 0), (or2, 0));
    b.circuit.connect((not1, 0), (or2, 1)); // Feedback Q to NOR2
    b.circuit.connect((or2, 0), (not2, 0));

    let q = b.output("Q");
    let qn = b.output("Qn");

    b.circuit.connect((not1, 0), (q, 0));
    b.circuit.connect((not2, 0), (qn, 0));

    b.build()
}

pub fn d_flip_flop() -> Fixture {
    // D Latch (Level-sensitive)
    // Inputs: D, E (Enable/Clock)
    // Outputs: Q, Qn
    // R = NOT D AND E
    // S = D AND E
    // Then into SR Latch
    let mut b = FixtureBuilder::new();
    let d = b.input("D");
    let e = b.input("E");

    let not_d = b.not();
    let and_r = b.and();
    let and_s = b.and();

    b.circuit.connect((d, 0), (not_d, 0));

    b.circuit.connect((not_d, 0), (and_r, 0));
    b.circuit.connect((e, 0), (and_r, 1));

    b.circuit.connect((d, 0), (and_s, 0));
    b.circuit.connect((e, 0), (and_s, 1));

    // SR Latch (NOR based)
    // NOR1 (R)
    let or1 = b.or();
    let not1 = b.not();
    // NOR2 (S)
    let or2 = b.or();
    let not2 = b.not();

    b.circuit.connect((and_r, 0), (or1, 0));
    b.circuit.connect((not2, 0), (or1, 1));
    b.circuit.connect((or1, 0), (not1, 0));

    b.circuit.connect((and_s, 0), (or2, 0));
    b.circuit.connect((not1, 0), (or2, 1));
    b.circuit.connect((or2, 0), (not2, 0));

    let q = b.output("Q");
    let qn = b.output("Qn");

    b.circuit.connect((not1, 0), (q, 0));
    b.circuit.connect((not2, 0), (qn, 0));

    b.build()
}

pub fn ripple_carry_adder_8bit() -> Fixture {
    let mut b = FixtureBuilder::new();

    let cin = b.input("Cin");
    let mut prev_carry = cin;

    for bit in 0..8 {
        let a = b.input(Box::leak(format!("A_{}", bit).into_boxed_str()));
        let b_in = b.input(Box::leak(format!("B_{}", bit).into_boxed_str()));

        // Half adder 1: A, B -> Sum1, Carry1
        let xor1 = b.circuit.xor();
        let carry1 = b.and();
        b.circuit.connect((a, 0), (xor1, 0));
        b.circuit.connect((b_in, 0), (xor1, 1));
        b.circuit.connect((a, 0), (carry1, 0));
        b.circuit.connect((b_in, 0), (carry1, 1));

        // Half adder 2: Sum1, prev_carry -> Sum, Carry2
        let xor2 = b.circuit.xor();
        let carry2 = b.and();
        b.circuit.connect((xor1, 0), (xor2, 0));
        b.circuit.connect((prev_carry, 0), (xor2, 1));
        b.circuit.connect((xor1, 0), (carry2, 0));
        b.circuit.connect((prev_carry, 0), (carry2, 1));

        // Cout = Carry1 OR Carry2
        let cout_or = b.or();
        b.circuit.connect((carry1, 0), (cout_or, 0));
        b.circuit.connect((carry2, 0), (cout_or, 1));

        let sum_out = b.output(Box::leak(format!("Sum_{}", bit).into_boxed_str()));
        b.circuit.connect((xor2, 0), (sum_out, 0));

        prev_carry = cout_or;
    }

    let final_cout = b.output("Cout");
    b.circuit.connect((prev_carry, 0), (final_cout, 0));

    b.build()
}

pub fn large_quiescent_grid_500() -> Fixture {
    let mut b = FixtureBuilder::new();

    let mut inputs = Vec::new();
    for i in 0..16 {
        inputs.push(b.input(Box::leak(format!("In_{}", i).into_boxed_str())));
    }

    let mut current_layer = inputs;
    let mut gate_count = 0;

    while gate_count < 500 {
        let mut next_layer = Vec::new();
        let len = current_layer.len();
        for i in (0..len).step_by(2) {
            let n1 = current_layer[i];
            let n2 = if i + 1 < len { current_layer[i + 1] } else { current_layer[0] };

            let gate = match gate_count % 3 {
                0 => b.and(),
                1 => b.or(),
                _ => b.circuit.xor(),
            };

            b.circuit.connect((n1, 0), (gate, 0));
            b.circuit.connect((n2, 0), (gate, 1));
            next_layer.push(gate);
            gate_count += 1;

            if gate_count >= 500 {
                break;
            }
        }
        if next_layer.is_empty() {
            break;
        }
        current_layer = next_layer;
    }

    let out = b.output("Out");
    if let Some(&last_node) = current_layer.last() {
        b.circuit.connect((last_node, 0), (out, 0));
    }

    b.build()
}

