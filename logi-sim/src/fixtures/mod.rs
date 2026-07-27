use crate::circuit::Circuit;
use crate::fixtures::builder::FixtureBuilder;
pub use builder::Fixture;

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
            let n2 = if i + 1 < len {
                current_layer[i + 1]
            } else {
                current_layer[0]
            };

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

pub fn recursive_16bit_adder() -> Fixture {
    let mut b = FixtureBuilder::new();

    // Level 1: Half Adder sub-circuit + 10 dead floating gates
    let mut half_adder = Circuit::new();
    let ha_a = half_adder.input("In");
    let ha_b = half_adder.input("In");
    let ha_sum = half_adder.xor();
    let ha_carry = half_adder.and();

    half_adder.connect((ha_a, 0), (ha_sum, 0));
    half_adder.connect((ha_b, 0), (ha_sum, 1));
    half_adder.connect((ha_a, 0), (ha_carry, 0));
    half_adder.connect((ha_b, 0), (ha_carry, 1));

    let ha_out_sum = half_adder.output("Out");
    let ha_out_carry = half_adder.output("Out");
    half_adder.connect((ha_sum, 0), (ha_out_sum, 0));
    half_adder.connect((ha_carry, 0), (ha_out_carry, 0));

    // Floating dead code inside Level 1
    for _ in 0..10 {
        let d_and = half_adder.and();
        let d_or = half_adder.or();
        half_adder.connect((d_and, 0), (d_or, 0));
    }

    // Level 2: Full Adder sub-circuit (2x Half Adder + OR)
    let mut full_adder = Circuit::new();
    let fa_a = full_adder.input("In");
    let fa_b = full_adder.input("In");
    let fa_cin = full_adder.input("In");

    let ha1 = full_adder.add_subcircuit(half_adder.clone());
    full_adder.connect((fa_a, 0), (ha1, 0));
    full_adder.connect((fa_b, 0), (ha1, 1));

    let ha2 = full_adder.add_subcircuit(half_adder.clone());
    full_adder.connect((ha1, 0), (ha2, 0)); // HA1 Sum -> HA2 Input 0
    full_adder.connect((fa_cin, 0), (ha2, 1)); // Cin -> HA2 Input 1

    let fa_or = full_adder.or();
    full_adder.connect((ha1, 1), (fa_or, 0)); // HA1 Carry -> OR 0
    full_adder.connect((ha2, 1), (fa_or, 1)); // HA2 Carry -> OR 1

    let fa_out_sum = full_adder.output("Out");
    let fa_out_cout = full_adder.output("Out");
    full_adder.connect((ha2, 0), (fa_out_sum, 0));
    full_adder.connect((fa_or, 0), (fa_out_cout, 0));

    // Level 3: 4-Bit Adder sub-circuit (4x Full Adder)
    let mut adder_4bit = Circuit::new();
    let mut a_4bit_in = Vec::new();
    let mut b_4bit_in = Vec::new();
    for _ in 0..4 {
        a_4bit_in.push(adder_4bit.input("In"));
    }
    for _ in 0..4 {
        b_4bit_in.push(adder_4bit.input("In"));
    }
    let cin_4bit = adder_4bit.input("In");

    let mut fas = Vec::new();
    for i in 0..4 {
        let fa = adder_4bit.add_subcircuit(full_adder.clone());
        adder_4bit.connect((a_4bit_in[i], 0), (fa, 0));
        adder_4bit.connect((b_4bit_in[i], 0), (fa, 1));

        let sum_out = adder_4bit.output("Out");
        adder_4bit.connect((fa, 0), (sum_out, 0));
        fas.push(fa);
    }

    // Connect carry chain across 4-bit adder
    adder_4bit.connect((cin_4bit, 0), (fas[0], 2));
    adder_4bit.connect((fas[0], 1), (fas[1], 2));
    adder_4bit.connect((fas[1], 1), (fas[2], 2));
    adder_4bit.connect((fas[2], 1), (fas[3], 2));

    let cout_4bit = adder_4bit.output("Out");
    adder_4bit.connect((fas[3], 1), (cout_4bit, 0));

    // Level 4: 16-Bit Adder Root Circuit (4x 4-Bit Adder)
    let cin = b.input("Cin");
    let mut a_inputs = Vec::new();
    let mut b_inputs = Vec::new();
    for bit in 0..16 {
        a_inputs.push(b.input(Box::leak(format!("A_{}", bit).into_boxed_str())));
    }
    for bit in 0..16 {
        b_inputs.push(b.input(Box::leak(format!("B_{}", bit).into_boxed_str())));
    }

    let mut prev_carry_pin = (cin, 0);

    for block in 0..4 {
        let b4 = b.circuit.add_subcircuit(adder_4bit.clone());

        for bit in 0..4 {
            let global_bit = block * 4 + bit;
            b.circuit
                .connect((a_inputs[global_bit], 0), (b4, bit as u8));
            b.circuit
                .connect((b_inputs[global_bit], 0), (b4, (4 + bit) as u8));

            let s_out = b.output(Box::leak(format!("Sum_{}", global_bit).into_boxed_str()));
            b.circuit.connect((b4, bit as u8), (s_out, 0));
        }

        b.circuit.connect(prev_carry_pin, (b4, 8));
        prev_carry_pin = (b4, 4);
    }

    let final_cout = b.output("Cout");
    b.circuit.connect(prev_carry_pin, (final_cout, 0));

    b.build()
}

pub fn cpu_subsystem_8bit() -> Fixture {
    let mut b = FixtureBuilder::new();

    // Level 1: 1-Bit ALU Slice (AND, OR, XOR, MUX)
    let mut alu_slice = Circuit::new();
    let slice_a = alu_slice.input("In");
    let slice_b = alu_slice.input("In");
    let slice_op = alu_slice.input("In"); // OpSel (0 = AND, 1 = OR)

    let and_gate = alu_slice.and();
    alu_slice.connect((slice_a, 0), (and_gate, 0));
    alu_slice.connect((slice_b, 0), (and_gate, 1));

    let or_gate = alu_slice.or();
    alu_slice.connect((slice_a, 0), (or_gate, 0));
    alu_slice.connect((slice_b, 0), (or_gate, 1));

    let mux_op = alu_slice.mux();
    alu_slice.connect((and_gate, 0), (mux_op, 0));
    alu_slice.connect((or_gate, 0), (mux_op, 1));
    alu_slice.connect((slice_op, 0), (mux_op, 2));

    let slice_out = alu_slice.output("Out");
    alu_slice.connect((mux_op, 0), (slice_out, 0));

    // Level 2: 8-Bit ALU Sub-Circuit (8x 1-Bit ALU Slices + XNOR Equal Flag)
    let mut alu_8bit = Circuit::new();
    let mut alu_a_inputs = Vec::new();
    let mut alu_b_inputs = Vec::new();
    for _ in 0..8 {
        alu_a_inputs.push(alu_8bit.input("In"));
    }
    for _ in 0..8 {
        alu_b_inputs.push(alu_8bit.input("In"));
    }
    let alu_op_in = alu_8bit.input("In");

    let mut alu_slice_nodes = Vec::new();
    for bit in 0..8 {
        let slice_node = alu_8bit.add_subcircuit(alu_slice.clone());
        alu_8bit.connect((alu_a_inputs[bit], 0), (slice_node, 0));
        alu_8bit.connect((alu_b_inputs[bit], 0), (slice_node, 1));
        alu_8bit.connect((alu_op_in, 0), (slice_node, 2));

        let s_out = alu_8bit.output("Out");
        alu_8bit.connect((slice_node, 0), (s_out, 0));
        alu_slice_nodes.push(slice_node);
    }

    // XNOR Equal Flag: check if A_0..7 == B_0..7
    let mut xnor_nodes = Vec::new();
    for bit in 0..8 {
        let xn = alu_8bit.xnor();
        alu_8bit.connect((alu_a_inputs[bit], 0), (xn, 0));
        alu_8bit.connect((alu_b_inputs[bit], 0), (xn, 1));
        xnor_nodes.push(xn);
    }

    let mut current_flag_node = xnor_nodes[0];
    for bit in 1..8 {
        let and_flag = alu_8bit.and();
        alu_8bit.connect((current_flag_node, 0), (and_flag, 0));
        alu_8bit.connect((xnor_nodes[bit], 0), (and_flag, 1));
        current_flag_node = and_flag;
    }
    let equal_flag_out = alu_8bit.output("Out");
    alu_8bit.connect((current_flag_node, 0), (equal_flag_out, 0));

    // Level 3: 8-Bit Register Sub-Circuit
    let mut reg_8bit = Circuit::new();
    let mut reg_d_in = Vec::new();
    for _ in 0..8 {
        reg_d_in.push(reg_8bit.input("In"));
    }
    let reg_enable = reg_8bit.input("In");

    let mut reg_q_out = Vec::new();
    for bit in 0..8 {
        let tri = reg_8bit.tristate();
        reg_8bit.connect((reg_d_in[bit], 0), (tri, 0));
        reg_8bit.connect((reg_enable, 0), (tri, 1));

        let q_out = reg_8bit.output("Out");
        reg_8bit.connect((tri, 0), (q_out, 0));
        reg_q_out.push(q_out);
    }

    // Level 4: 4x 8-Bit Register File Sub-Circuit (4 Registers + Demux Decoder)
    let mut reg_file = Circuit::new();
    let mut rf_data_in = Vec::new();
    for _ in 0..8 {
        rf_data_in.push(reg_file.input("In"));
    }
    let rf_write_sel = reg_file.input("In");

    let demux_decoder = reg_file.demux();
    let rf_global_write_en = reg_file.input("In");
    reg_file.connect((rf_global_write_en, 0), (demux_decoder, 0));
    reg_file.connect((rf_write_sel, 0), (demux_decoder, 1));

    let r0_sub = reg_file.add_subcircuit(reg_8bit.clone());
    for bit in 0..8 {
        reg_file.connect((rf_data_in[bit], 0), (r0_sub, bit as u8));
    }
    reg_file.connect((demux_decoder, 0), (r0_sub, 8));

    let r1_sub = reg_file.add_subcircuit(reg_8bit.clone());
    for bit in 0..8 {
        reg_file.connect((rf_data_in[bit], 0), (r1_sub, bit as u8));
    }
    reg_file.connect((demux_decoder, 1), (r1_sub, 8));

    let mut rf_out_nodes = Vec::new();
    for bit in 0..8 {
        let mux_read = reg_file.mux();
        reg_file.connect((r0_sub, bit as u8), (mux_read, 0));
        reg_file.connect((r1_sub, bit as u8), (mux_read, 1));
        reg_file.connect((rf_write_sel, 0), (mux_read, 2));

        let rf_out = reg_file.output("Out");
        reg_file.connect((mux_read, 0), (rf_out, 0));
        rf_out_nodes.push(rf_out);
    }

    // Level 5 (Root): 8-Bit CPU Core Subsystem
    let _clk_node = b.circuit.clock(2);
    let alu_op = b.input("ALU_OpSel");

    let mut cpu_a_in = Vec::new();
    let mut cpu_b_in = Vec::new();
    for bit in 0..8 {
        cpu_a_in.push(b.input(Box::leak(format!("A_{}", bit).into_boxed_str())));
    }
    for bit in 0..8 {
        cpu_b_in.push(b.input(Box::leak(format!("B_{}", bit).into_boxed_str())));
    }

    let alu_sub_node = b.circuit.add_subcircuit(alu_8bit.clone());
    for bit in 0..8 {
        b.circuit
            .connect((cpu_a_in[bit], 0), (alu_sub_node, bit as u8));
        b.circuit
            .connect((cpu_b_in[bit], 0), (alu_sub_node, (8 + bit) as u8));
    }
    b.circuit.connect((alu_op, 0), (alu_sub_node, 16));

    let bus_enable = b.input("Bus_Enable");
    let mut data_bus_outputs = Vec::new();

    for bit in 0..8 {
        let tri_driver = b.circuit.tristate();
        b.circuit
            .connect((alu_sub_node, bit as u8), (tri_driver, 0));
        b.circuit.connect((bus_enable, 0), (tri_driver, 1));

        let bus_out = b.output(Box::leak(format!("Bus_{}", bit).into_boxed_str()));
        b.circuit.connect((tri_driver, 0), (bus_out, 0));
        data_bus_outputs.push(bus_out);
    }

    let zero_flag_out = b.output("ZeroFlag");
    b.circuit.connect((alu_sub_node, 8), (zero_flag_out, 0));

    b.build()
}
