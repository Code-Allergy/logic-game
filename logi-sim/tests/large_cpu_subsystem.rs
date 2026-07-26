use logi_sim::prelude::*;
use std::collections::HashMap;

fn run_cpu_test<R: Runner>(
    runner_name: &str,
    mut runner: R,
    fixture: &logi_sim::fixtures::Fixture,
    node_map: Option<&HashMap<logi_sim::circuit::NodeId, logi_sim::circuit::NodeId>>,
) {
    let get_node = |name: &'static str| -> logi_sim::circuit::NodeId {
        let raw_id = fixture.nodes[name];
        node_map.and_then(|m| m.get(&raw_id).copied()).unwrap_or(raw_id)
    };

    let bus_enable = get_node("Bus_Enable");
    let alu_op = get_node("ALU_OpSel");
    let zero_flag = get_node("ZeroFlag");

    runner.set_input(bus_enable, 0, true);
    runner.set_input(alu_op, 0, false);

    // A = 0b11001100 (bits 0..7: 0,0,1,1,0,0,1,1)
    // B = 0b10101010 (bits 0..7: 0,1,0,1,0,1,0,1)
    let a_bits = [false, false, true, true, false, false, true, true];
    let b_bits = [false, true, false, true, false, true, false, true];

    for bit in 0..8 {
        let a_node = get_node(Box::leak(format!("A_{}", bit).into_boxed_str()));
        let b_node = get_node(Box::leak(format!("B_{}", bit).into_boxed_str()));
        runner.set_input(a_node, 0, a_bits[bit]);
        runner.set_input(b_node, 0, b_bits[bit]);
    }

    for _ in 0..20 {
        runner.tick();
    }

    // Expected AND result: 0b10001000 (bits 0..7: 0,0,0,1,0,0,0,1)
    let expected_and = [false, false, false, true, false, false, false, true];
    for bit in 0..8 {
        let bus_node = get_node(Box::leak(format!("Bus_{}", bit).into_boxed_str()));
        assert_eq!(
            runner.output(bus_node, 0),
            expected_and[bit],
            "[{}] Bus_{} mismatch for AND operation",
            runner_name,
            bit
        );
    }

    // Assert ZeroFlag (Equal) = false because A != B
    assert_eq!(runner.output(zero_flag, 0), false, "[{}] ZeroFlag mismatch for A != B", runner_name);

    // Now set A = B = 0b10101010 -> ZeroFlag should be true!
    for bit in 0..8 {
        let a_node = get_node(Box::leak(format!("A_{}", bit).into_boxed_str()));
        let b_node = get_node(Box::leak(format!("B_{}", bit).into_boxed_str()));
        runner.set_input(a_node, 0, b_bits[bit]);
        runner.set_input(b_node, 0, b_bits[bit]);
    }

    for _ in 0..20 {
        runner.tick();
    }

    assert_eq!(runner.output(zero_flag, 0), true, "[{}] ZeroFlag mismatch for A == B", runner_name);
}

#[test]
fn test_large_8bit_cpu_subsystem_flat_runner() {
    let fixture = logi_sim::fixtures::cpu_subsystem_8bit();
    let flat_runner = FlatRunner::new(FlattenedCircuit::compile(&fixture.circuit));
    run_cpu_test("FlatRunner-unoptimized", flat_runner, &fixture, None);

    let (opt_circuit, node_map) = Compiler::optimize_with_map(&fixture.circuit);
    let opt_flat_runner = FlatRunner::new(FlattenedCircuit::compile(&opt_circuit));
    run_cpu_test("FlatRunner-optimized", opt_flat_runner, &fixture, Some(&node_map));
}

#[test]
fn test_large_8bit_cpu_subsystem_event_driven_runner() {
    let fixture = logi_sim::fixtures::cpu_subsystem_8bit();
    let event_runner = EventDrivenRunner::new(EventDrivenCircuit::compile(&fixture.circuit));
    run_cpu_test("EventDrivenRunner-unoptimized", event_runner, &fixture, None);

    let (opt_circuit, node_map) = Compiler::optimize_with_map(&fixture.circuit);
    let opt_event_runner = EventDrivenRunner::new(EventDrivenCircuit::compile(&opt_circuit));
    run_cpu_test("EventDrivenRunner-optimized", opt_event_runner, &fixture, Some(&node_map));
}

#[test]
fn test_large_8bit_cpu_subsystem_simd_runner() {
    let fixture = logi_sim::fixtures::cpu_subsystem_8bit();
    let simd_runner = SimdRunner::new(BitPackedCircuit::compile(&fixture.circuit));
    run_cpu_test("SimdRunner-unoptimized", simd_runner, &fixture, None);

    let (opt_circuit, node_map) = Compiler::optimize_with_map(&fixture.circuit);
    let opt_simd_runner = SimdRunner::new(BitPackedCircuit::compile(&opt_circuit));
    run_cpu_test("SimdRunner-optimized", opt_simd_runner, &fixture, Some(&node_map));
}

#[test]
fn test_large_8bit_cpu_subsystem_naive_runner() {
    let fixture = logi_sim::fixtures::cpu_subsystem_8bit();
    let naive_runner = NaiveRunner::new(&fixture.circuit);
    run_cpu_test("NaiveRunner-unoptimized", naive_runner, &fixture, None);

    let (opt_circuit, node_map) = Compiler::optimize_with_map(&fixture.circuit);
    let opt_naive_runner = NaiveRunner::new(&opt_circuit);
    run_cpu_test("NaiveRunner-optimized", opt_naive_runner, &fixture, Some(&node_map));
}
