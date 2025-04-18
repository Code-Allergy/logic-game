fn add_n_bit_adder(circuit: &mut Circuit, n: usize, input_offset: usize, output_offset: usize) {
    // Inputs: A0..An-1, B0..Bn-1, Cin
    let mut a = Vec::with_capacity(n);
    let mut b = Vec::with_capacity(n);
    for i in 0..n {
        a.push(circuit.add_input_pin(Some(false), input_offset + i));
        b.push(circuit.add_input_pin(Some(false), input_offset + n + i));
    }
    let cin = circuit.add_input_pin(Some(false), input_offset + 2 * n);

    // Outputs: S0..Sn-1, Cout
    let mut s = Vec::with_capacity(n);
    for i in 0..n {
        s.push(circuit.add_output_pin(output_offset + i));
    }
    let cout = circuit.add_output_pin(output_offset + n);

    // Add full adders
    let mut fa_ids = Vec::with_capacity(n);
    for _ in 0..n {
        let fa = FullAdderTemplate::template();
        let fa_id = fa.id();
        circuit.add_component(LogicComponent::Circuit(fa));
        fa_ids.push(fa_id);
    }

    // Wire up the first adder
    circuit.add_connection(a[0], fa_ids[0], 0, 0); // A0 -> FA0.A
    circuit.add_connection(b[0], fa_ids[0], 1, 0); // B0 -> FA0.B
    circuit.add_connection(cin, fa_ids[0], 2, 0); // Cin -> FA0.Cin
    circuit.add_connection(fa_ids[0], s[0], 0, 0); // FA0.Sum -> S0

    // Wire up the rest
    for i in 1..n {
        circuit.add_connection(a[i], fa_ids[i], 0, 0); // Ai -> FAi.A
        circuit.add_connection(b[i], fa_ids[i], 1, 0); // Bi -> FAi.B
        circuit.add_connection(fa_ids[i - 1], fa_ids[i], 2, 1); // FA(i-1).Cout -> FAi.Cin
        circuit.add_connection(fa_ids[i], s[i], 0, 0); // FAi.Sum -> Si
    }

    // Final carry out
    circuit.add_connection(fa_ids[n - 1], cout, 0, 1); // FA(n-1).Cout -> Cout
}
