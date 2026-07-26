use logi_sim::fixtures;
use logi_sim::prelude::*;

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
