use logi_sim::circuit::Circuit;
use logi_sim::prelude::*;

fn test_primitive_equivalence<F: FnOnce(&mut Circuit) -> (Vec<(logi_sim::circuit::NodeId, bool)>, Vec<(logi_sim::circuit::NodeId, bool)>)>(build: F) {
    let mut circuit = Circuit::new();
    let (inputs, expected_outputs) = build(&mut circuit);

    // 1. NaiveRunner
    let mut naive = NaiveRunner::new(&circuit);
    for (node, val) in &inputs {
        naive.set_input(*node, 0, *val);
    }
    naive.tick();
    for (node, expected) in &expected_outputs {
        assert_eq!(naive.output(*node, 0), *expected, "NaiveRunner output mismatch");
    }

    // 2. FlatRunner
    let flat_circuit = FlattenedCircuit::compile(&circuit);
    let mut flat = FlatRunner::new(flat_circuit);
    for (node, val) in &inputs {
        flat.set_input(*node, 0, *val);
    }
    flat.tick();
    for (node, expected) in &expected_outputs {
        assert_eq!(flat.output(*node, 0), *expected, "FlatRunner output mismatch");
    }

    // 3. EventDrivenRunner
    let event_circuit = EventDrivenCircuit::compile(&circuit);
    let mut event = EventDrivenRunner::new(event_circuit);
    for (node, val) in &inputs {
        event.set_input(*node, 0, *val);
    }
    event.tick();
    for (node, expected) in &expected_outputs {
        assert_eq!(event.output(*node, 0), *expected, "EventDrivenRunner output mismatch");
    }

    // 4. SimdRunner (256-bit SIMD)
    let bit_packed = BitPackedCircuit::compile(&circuit);
    let mut simd = SimdRunner::new(bit_packed);
    for (node, val) in &inputs {
        simd.set_input(*node, 0, *val);
    }
    simd.tick();
    for (node, expected) in &expected_outputs {
        assert_eq!(simd.output(*node, 0), *expected, "SimdRunner output mismatch");
    }
}

#[test]
fn test_nand_gate_primitives() {
    test_primitive_equivalence(|c| {
        let a = c.input();
        let b = c.input();
        let nand = c.nand();
        let out = c.output();

        c.connect((a, 0), (nand, 0));
        c.connect((b, 0), (nand, 1));
        c.connect((nand, 0), (out, 0));

        (vec![(a, true), (b, true)], vec![(out, false)])
    });
}

#[test]
fn test_nor_gate_primitives() {
    test_primitive_equivalence(|c| {
        let a = c.input();
        let b = c.input();
        let nor = c.nor();
        let out = c.output();

        c.connect((a, 0), (nor, 0));
        c.connect((b, 0), (nor, 1));
        c.connect((nor, 0), (out, 0));

        (vec![(a, false), (b, false)], vec![(out, true)])
    });
}

#[test]
fn test_xnor_gate_primitives() {
    test_primitive_equivalence(|c| {
        let a = c.input();
        let b = c.input();
        let xnor = c.xnor();
        let out = c.output();

        c.connect((a, 0), (xnor, 0));
        c.connect((b, 0), (xnor, 1));
        c.connect((xnor, 0), (out, 0));

        (vec![(a, true), (b, true)], vec![(out, true)])
    });
}

#[test]
fn test_buffer_gate_primitives() {
    test_primitive_equivalence(|c| {
        let a = c.input();
        let buf = c.buffer();
        let out = c.output();

        c.connect((a, 0), (buf, 0));
        c.connect((buf, 0), (out, 0));

        (vec![(a, true)], vec![(out, true)])
    });
}

#[test]
fn test_mux_gate_primitives() {
    test_primitive_equivalence(|c| {
        let d0 = c.input();
        let d1 = c.input();
        let sel = c.input();
        let mux = c.mux();
        let out = c.output();

        c.connect((d0, 0), (mux, 0));
        c.connect((d1, 0), (mux, 1));
        c.connect((sel, 0), (mux, 2));
        c.connect((mux, 0), (out, 0));

        // When Sel = 1, Output = D1 (true)
        (vec![(d0, false), (d1, true), (sel, true)], vec![(out, true)])
    });
}

#[test]
fn test_tristate_buffer_primitives() {
    test_primitive_equivalence(|c| {
        let data = c.input();
        let enable = c.input();
        let tri = c.tristate();
        let out = c.output();

        c.connect((data, 0), (tri, 0));
        c.connect((enable, 0), (tri, 1));
        c.connect((tri, 0), (out, 0));

        // When Enable = 1, Output = Data (true)
        (vec![(data, true), (enable, true)], vec![(out, true)])
    });
}
