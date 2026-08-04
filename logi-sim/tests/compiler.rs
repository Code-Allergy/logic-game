use logi_sim::prelude::*;

#[test]
fn test_dead_code_culling() {
    let mut c = Circuit::new();

    // Live branch: A -> AND -> Output
    let in_a = c.input();
    let and1 = c.and();
    let out = c.output();
    c.connect((in_a, 0), (and1, 0));
    c.connect((and1, 0), (out, 0));

    // Dead branch: Unconnected AND gate + OR gate floating in space
    let dead_and = c.and();
    let dead_or = c.or();
    c.connect((dead_and, 0), (dead_or, 0));

    assert_eq!(c.nodes.len(), 5); // 5 nodes total initially

    // Run optimization pass
    let optimized = Compiler::optimize(&c);

    // Optimized circuit should only contain 3 live nodes (in_a, and1, out)
    assert_eq!(optimized.nodes.len(), 3);
}

#[test]
fn common_cells_are_merged_without_losing_wire_observability() {
    let mut circuit = Circuit::new();
    let a = circuit.input();
    let b = circuit.input();
    let and_a = circuit.and();
    let and_b = circuit.and();
    let out_a = circuit.output();
    let out_b = circuit.output();

    circuit.connect((a, 0), (and_a, 0));
    circuit.connect((b, 0), (and_a, 1));
    // Reversed inputs verify commutative-cell canonicalization.
    circuit.connect((b, 0), (and_b, 0));
    circuit.connect((a, 0), (and_b, 1));
    let raw_out_a = circuit.connect((and_a, 0), (out_a, 0));
    let raw_out_b = circuit.connect((and_b, 0), (out_b, 0));

    let (optimized, node_map, wire_map) = Compiler::optimize_with_maps(&circuit);
    assert_eq!(optimized.nodes.len(), circuit.nodes.len() - 1);
    assert_eq!(node_map[&and_a], node_map[&and_b]);
    assert_eq!(wire_map[&raw_out_a], wire_map[&raw_out_b]);

    let mut runner = FlatRunner::new(FlattenedCircuit::compile(&optimized));
    runner.set_input(node_map[&a], 0, true);
    runner.set_input(node_map[&b], 0, true);
    runner.tick();

    assert!(runner.wire_value(wire_map[&raw_out_a]));
    assert!(runner.wire_value(wire_map[&raw_out_b]));
    assert!(runner.output(node_map[&out_a], 0));
    assert!(runner.output(node_map[&out_b], 0));
}

#[test]
fn common_cell_pass_recurses_into_subcircuits() {
    let mut child = Circuit::new();
    let input = child.input();
    let not_a = child.not();
    let not_b = child.not();
    let out_a = child.output();
    let out_b = child.output();
    child.connect((input, 0), (not_a, 0));
    child.connect((input, 0), (not_b, 0));
    child.connect((not_a, 0), (out_a, 0));
    child.connect((not_b, 0), (out_b, 0));

    let mut root = Circuit::new();
    let root_input = root.input();
    let nested = root.add_subcircuit(child);
    let root_out_a = root.output();
    let root_out_b = root.output();
    root.connect((root_input, 0), (nested, 0));
    root.connect((nested, 0), (root_out_a, 0));
    root.connect((nested, 1), (root_out_b, 0));

    let optimized = Compiler::optimize(&root);
    let nested_child = optimized
        .nodes
        .values()
        .find_map(|node| match &node.kind {
            NodeKind::SubCircuit(child) => Some(child),
            _ => None,
        })
        .unwrap();

    assert_eq!(nested_child.nodes.len(), 4);
}
