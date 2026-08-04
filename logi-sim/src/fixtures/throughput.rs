use crate::circuit::{Circuit, NodeId};

pub struct ThroughputFixture {
    pub circuit: Circuit,
    pub input_nodes: Vec<NodeId>,
    pub output: NodeId,
}

pub fn large_dense_fixture() -> ThroughputFixture {
    const INPUT_BITS: usize = 1_024;
    const BUNDLE_COUNT: usize = 8;
    const REFERENCE_STEP: usize = 37;

    let mut circuit = Circuit::new();
    let mut input_nodes = Vec::with_capacity(INPUT_BITS);

    for _ in 0..INPUT_BITS {
        input_nodes.push(circuit.input());
    }

    let mut bundle_taps = Vec::with_capacity(BUNDLE_COUNT);
    for bundle in 0..BUNDLE_COUNT {
        let first = bundle % INPUT_BITS;
        let mut stage = circuit.xor();
        circuit.connect((input_nodes[first], 0), (stage, 0));
        circuit.connect((input_nodes[(first + 1) % INPUT_BITS], 0), (stage, 1));

        let mut source_idx = first + 2;
        for _ in 2..INPUT_BITS {
            let source = input_nodes[source_idx % INPUT_BITS];

            let and_gate = circuit.and();
            circuit.connect((stage, 0), (and_gate, 0));
            circuit.connect((source, 0), (and_gate, 1));

            let or_gate = circuit.or();
            circuit.connect((stage, 0), (or_gate, 0));
            circuit.connect((and_gate, 0), (or_gate, 1));

            let next_stage = circuit.xor();
            circuit.connect((or_gate, 0), (next_stage, 0));
            circuit.connect((source, 0), (next_stage, 1));
            stage = next_stage;
            source_idx = source_idx.wrapping_add(REFERENCE_STEP);
        }

        bundle_taps.push(stage);
    }

    let mut reduced = bundle_taps[0];
    for bundle_out in &bundle_taps[1..] {
        let reduce_gate = circuit.xor();
        circuit.connect((reduced, 0), (reduce_gate, 0));
        circuit.connect((*bundle_out, 0), (reduce_gate, 1));
        reduced = reduce_gate;
    }

    let output = circuit.output();
    circuit.connect((reduced, 0), (output, 0));

    ThroughputFixture {
        circuit,
        input_nodes,
        output,
    }
}
