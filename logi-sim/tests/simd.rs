use logi_sim::circuit::{BitPackedCircuit, SimdPrograms};
use logi_sim::fixtures;
use logi_sim::prelude::*;
use std::collections::HashMap;
use std::mem::size_of;

#[test]
fn floating_binary_gates_compile_without_panicking() {
    let mut circuit = Circuit::new();
    let floating_and = circuit.and();
    let floating_or = circuit.or();
    circuit.connect((floating_and, 0), (floating_or, 0));
    let mut runner = SimdRunner::new(BitPackedCircuit::compile(&circuit));
    runner.tick();
}

#[test]
fn test_simd_64_parallel_runs() {
    let fixture = fixtures::and_gate();
    let bit_packed = BitPackedCircuit::compile(&fixture.circuit);
    let mut runner = SimdRunner::new(bit_packed);

    let input_a = fixture.nodes["A"];
    let input_b = fixture.nodes["B"];
    let output_y = fixture.nodes["Y"];

    // Input A bit vector: alternating bits 0xAAAA_AAAA_AAAA_AAAA
    // Input B bit vector: contiguous blocks 0xFFFF_0000_FFFF_0000
    let vec_a: u64 = 0xAAAA_AAAA_AAAA_AAAA;
    let vec_b: u64 = 0xFFFF_0000_FFFF_0000;

    runner.set_input_u64(input_a, 0, vec_a);
    runner.set_input_u64(input_b, 0, vec_b);

    runner.tick();

    // Expected Output Y = A AND B
    let expected_y = vec_a & vec_b;
    assert_eq!(runner.output_u64(output_y, 0), expected_y);
}

#[test]
fn compact_simd_records_have_fixed_specialized_sizes() {
    assert_eq!(size_of::<SimdUnaryInstruction<u16>>(), 4);
    assert_eq!(size_of::<SimdBinaryInstruction<u16>>(), 6);
    assert_eq!(size_of::<SimdTernaryInstruction<u16>>(), 8);
    assert_eq!(size_of::<SimdDemuxInstruction<u16>>(), 8);
    assert_eq!(size_of::<SimdClock<u16>>(), 32);
}

#[test]
fn circuits_above_u16_wire_space_use_wide_records() {
    let flat = FlattenedCircuit {
        nodes: Vec::new(),
        executable_nodes: Vec::new(),
        wire_count: u16::MAX as usize + 1,
        node_to_idx: HashMap::new(),
        wire_to_idx: HashMap::new(),
        idx_to_wire: Vec::new(),
        node_input_pins: HashMap::new(),
        node_output_pins: HashMap::new(),
    };
    assert!(matches!(
        BitPackedCircuit::compile_from_flat(&flat).programs,
        SimdPrograms::Wide(_)
    ));
}

#[test]
fn hidden_and_not_pattern_compiles_to_one_nand_instruction() {
    let mut child = Circuit::new();
    let a = child.input();
    let b = child.input();
    let and = child.and();
    let not = child.not();
    let output = child.output();
    child.connect((a, 0), (and, 0));
    child.connect((b, 0), (and, 1));
    child.connect((and, 0), (not, 0));
    child.connect((not, 0), (output, 0));

    let mut root = Circuit::new();
    let root_a = root.input();
    let root_b = root.input();
    let nested = root.add_subcircuit(child);
    let root_output = root.output();
    root.connect((root_a, 0), (nested, 0));
    root.connect((root_b, 0), (nested, 1));
    root.connect((nested, 0), (root_output, 0));

    let compiled = BitPackedCircuit::compile(&root);
    let SimdPrograms::Compact(program) = &compiled.programs else {
        panic!("small test circuit should use compact SIMD records");
    };
    assert_eq!(program.binary.len(), 1, "program: {program:#?}");
    assert_eq!(program.unary.len(), 0);
    assert_eq!(program.blocks.len(), 1);
    assert_eq!(program.blocks[0].op, SimdOp::Nand);

    let mut runner = SimdRunner::new(compiled);
    runner.set_input(root_a, 0, true);
    runner.set_input(root_b, 0, true);
    runner.tick();
    assert!(!runner.output(root_output, 0));
}
