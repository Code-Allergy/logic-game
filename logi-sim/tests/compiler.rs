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
