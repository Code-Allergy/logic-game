use bevy::prelude::Vec2;

use uuid::Uuid;
use crate::sim::{GameCircuit, GameNode, GameNodeKind, GameWire};

/// Helper to add a Master-Slave T Flip-Flop to the circuit.
/// Takes clock and reset node IDs, returns (Q, Qn) output pin node IDs.
/// Uses primitive gates to construct a falling-edge triggered T-FF with asynchronous reset.
fn add_t_ff(
    circuit: &mut GameCircuit,
    clk: Uuid,
    rst: Uuid,
    pos: Vec2,
) -> (Uuid, Uuid) {
    let mut create_node = |kind, dx, dy| {
        let node = GameNode::new(kind, pos + Vec2::new(dx, dy));
        circuit.add_node(node.clone());
        node.id
    };

    // Master
    let s_m = create_node(GameNodeKind::And, -40.0, 20.0);
    let r_m = create_node(GameNodeKind::And, -40.0, -20.0);
    let rm_rst = create_node(GameNodeKind::Or, -10.0, -30.0);
    let q_m = create_node(GameNodeKind::Nor, 20.0, 20.0);
    let qn_m = create_node(GameNodeKind::Nor, 20.0, -20.0);

    // Slave
    let not_clk = create_node(GameNodeKind::Not, 40.0, -50.0);
    let s_s = create_node(GameNodeKind::And, 60.0, 20.0);
    let r_s = create_node(GameNodeKind::And, 60.0, -20.0);
    let rs_rst = create_node(GameNodeKind::Or, 90.0, -30.0);
    let q_s = create_node(GameNodeKind::Nor, 120.0, 20.0);
    let qn_s = create_node(GameNodeKind::Nor, 120.0, -20.0);

    // Wire Not Clock
    circuit.add_wire(GameWire::new(clk, 0, not_clk, 0));

    // Wire Master
    circuit.add_wire(GameWire::new(qn_s, 0, s_m, 0));
    circuit.add_wire(GameWire::new(clk, 0, s_m, 1));

    circuit.add_wire(GameWire::new(q_s, 0, r_m, 0));
    circuit.add_wire(GameWire::new(clk, 0, r_m, 1));

    circuit.add_wire(GameWire::new(r_m, 0, rm_rst, 0));
    circuit.add_wire(GameWire::new(rst, 0, rm_rst, 1));

    circuit.add_wire(GameWire::new(rm_rst, 0, q_m, 0));
    circuit.add_wire(GameWire::new(qn_m, 0, q_m, 1));

    circuit.add_wire(GameWire::new(s_m, 0, qn_m, 0));
    circuit.add_wire(GameWire::new(q_m, 0, qn_m, 1));

    // Wire Slave
    circuit.add_wire(GameWire::new(q_m, 0, s_s, 0));
    circuit.add_wire(GameWire::new(not_clk, 0, s_s, 1));

    circuit.add_wire(GameWire::new(qn_m, 0, r_s, 0));
    circuit.add_wire(GameWire::new(not_clk, 0, r_s, 1));

    circuit.add_wire(GameWire::new(r_s, 0, rs_rst, 0));
    circuit.add_wire(GameWire::new(rst, 0, rs_rst, 1));

    circuit.add_wire(GameWire::new(rs_rst, 0, q_s, 0));
    circuit.add_wire(GameWire::new(qn_s, 0, q_s, 1));

    circuit.add_wire(GameWire::new(s_s, 0, qn_s, 0));
    circuit.add_wire(GameWire::new(q_s, 0, qn_s, 1));

    (q_s, qn_s)
}

/// Builds an 8-bit ripple counter fixture using T Flip-Flops.
pub fn build_8bit_counter() -> GameCircuit {
    let mut circuit = GameCircuit::new("8-Bit Counter");

    let clock = GameNode::new(GameNodeKind::Clock { period_ticks: 4 }, Vec2::new(-400.0, 100.0));
    let clock_id = circuit.add_node(clock);

    let reset = GameNode::new(GameNodeKind::Input, Vec2::new(-400.0, -100.0));
    let reset_id = circuit.add_node(reset);

    let mut current_clk = clock_id;

    // Add 8 T Flip-Flops in a ripple counter configuration
    for i in 0..8 {
        let ff_pos = Vec2::new(-200.0 + (i as f32) * 200.0, 0.0);

        let (q, _qn) = add_t_ff(&mut circuit, current_clk, reset_id, ff_pos);

        let mut out_node = GameNode::new(GameNodeKind::Output, Vec2::new(ff_pos.x + 150.0, ff_pos.y + 100.0));
        out_node.label = Some(format!("Bit {}", i));
        let out_node_id = circuit.add_node(out_node);

        circuit.add_wire(GameWire::new(q, 0, out_node_id, 0));

        // The Q output drives the clock of the next stage (falling edge triggered FF acting as a ripple counter when counting up)
        current_clk = q;
    }

    circuit
}

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
