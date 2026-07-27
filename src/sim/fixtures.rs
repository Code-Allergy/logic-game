use bevy::prelude::Vec2;

use crate::sim::{GameCircuit, GameNode, GameNodeKind, GameWire};

/// Builds a `GameCircuit` populated with a Half Adder fixture.
pub fn build_half_adder() -> GameCircuit {
    let mut circuit = GameCircuit::new("Half Adder");

    let mut create_node = |kind, x, y| {
        let node = GameNode::new(kind, Vec2::new(x, y));
        circuit.add_node(node.clone());
        node.id
    };

    // Inputs
    let a_id = create_node(GameNodeKind::Input, -200.0, 50.0);
    let b_id = create_node(GameNodeKind::Input, -200.0, -50.0);

    // Gates
    let xor_id = create_node(GameNodeKind::Xor, 0.0, 50.0);
    let and_id = create_node(GameNodeKind::And, 0.0, -50.0);

    // Outputs
    let sum_id = create_node(GameNodeKind::Output, 200.0, 50.0);
    let carry_id = create_node(GameNodeKind::Output, 200.0, -50.0);

    // Wiring
    circuit.add_wire(GameWire::new(a_id, 0, xor_id, 0));
    circuit.add_wire(GameWire::new(b_id, 0, xor_id, 1));

    circuit.add_wire(GameWire::new(a_id, 0, and_id, 0));
    circuit.add_wire(GameWire::new(b_id, 0, and_id, 1));

    circuit.add_wire(GameWire::new(xor_id, 0, sum_id, 0));
    circuit.add_wire(GameWire::new(and_id, 0, carry_id, 0));

    circuit
}
