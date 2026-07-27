use bevy::prelude::Vec2;

use crate::sim::{GameCircuit, GameNode, GameNodeKind, GameWire};
use uuid::Uuid;

/// Helper to add a Master-Slave T Flip-Flop to the circuit.
/// Takes clock and reset node IDs, returns (Q, Qn) output pin node IDs.
/// Uses primitive gates to construct a falling-edge triggered T-FF with asynchronous reset.
fn add_t_ff(circuit: &mut GameCircuit, clk: Uuid, rst: Uuid, pos: Vec2) -> (Uuid, Uuid) {
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

/// Builds a T Flip-Flop as a standalone reusable `GameCircuit`.
/// Inputs: "Clock", "Reset"
/// Outputs: "Q", "Qn"
pub fn build_t_ff_circuit() -> GameCircuit {
    let mut circuit = GameCircuit::new("T Flip-Flop Subcircuit");

    let mut create_node = |kind, dx, dy, label: Option<&str>| {
        let mut node = GameNode::new(kind, Vec2::new(dx, dy));
        if let Some(l) = label {
            node.label = Some(l.to_string());
        }
        circuit.add_node(node.clone());
        node.id
    };

    // Inputs
    let clk = create_node(GameNodeKind::Input, -100.0, 50.0, Some("Clock"));
    let rst = create_node(GameNodeKind::Input, -100.0, -50.0, Some("Reset"));

    // Master
    let s_m = create_node(GameNodeKind::And, -40.0, 20.0, None);
    let r_m = create_node(GameNodeKind::And, -40.0, -20.0, None);
    let rm_rst = create_node(GameNodeKind::Or, -10.0, -30.0, None);
    let q_m = create_node(GameNodeKind::Nor, 20.0, 20.0, None);
    let qn_m = create_node(GameNodeKind::Nor, 20.0, -20.0, None);

    // Slave
    let not_clk = create_node(GameNodeKind::Not, 40.0, -50.0, None);
    let s_s = create_node(GameNodeKind::And, 60.0, 20.0, None);
    let r_s = create_node(GameNodeKind::And, 60.0, -20.0, None);
    let rs_rst = create_node(GameNodeKind::Or, 90.0, -30.0, None);
    let q_s = create_node(GameNodeKind::Nor, 120.0, 20.0, None);
    let qn_s = create_node(GameNodeKind::Nor, 120.0, -20.0, None);

    // Outputs
    let out_q = create_node(GameNodeKind::Output, 180.0, 20.0, Some("Q"));
    let out_qn = create_node(GameNodeKind::Output, 180.0, -20.0, Some("Qn"));

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

    circuit.add_wire(GameWire::new(q_s, 0, out_q, 0));
    circuit.add_wire(GameWire::new(qn_s, 0, out_qn, 0));

    circuit
}

/// Builds an 8-bit ripple counter fixture using T Flip-Flop subcircuits.
pub fn build_8bit_counter_subcircuit() -> GameCircuit {
    let mut circuit = GameCircuit::new("8-Bit Counter (Subcircuits)");

    let mut clock = GameNode::new(
        GameNodeKind::Clock { period_ticks: 4, duty_ticks: 1 },
        Vec2::new(-400.0, 100.0),
    );
    clock.label = Some("Clock".to_string());
    let clock_id = circuit.add_node(clock);

    let mut reset = GameNode::new(GameNodeKind::Input, Vec2::new(-400.0, -100.0));
    reset.label = Some("Reset".to_string());
    let reset_id = circuit.add_node(reset);

    let mut current_clk = clock_id;

    for i in 0..8 {
        let ff_pos = Vec2::new(-200.0 + (i as f32) * 200.0, 0.0);
        let mut ff_node = GameNode::new(
            GameNodeKind::SubCircuit {
                circuit: Box::new(build_t_ff_circuit()),
            },
            ff_pos,
        );
        ff_node.label = Some(format!("T-FF {}", i));
        let ff_id = circuit.add_node(ff_node);

        circuit.add_wire(GameWire::new(current_clk, 0, ff_id, 0)); // Clock to Clock
        circuit.add_wire(GameWire::new(reset_id, 0, ff_id, 1)); // Reset to Reset

        let mut out_node = GameNode::new(
            GameNodeKind::Output,
            Vec2::new(ff_pos.x + 150.0, ff_pos.y + 100.0),
        );
        out_node.label = Some(format!("Bit {}", i));
        let out_node_id = circuit.add_node(out_node);
        circuit.add_wire(GameWire::new(ff_id, 0, out_node_id, 0)); // Q to Output

        current_clk = ff_id;
    }
    circuit
}

/// Builds an 8-bit ripple counter fixture using T Flip-Flops.
pub fn build_8bit_counter() -> GameCircuit {
    let mut circuit = GameCircuit::new("8-Bit Counter");

    let clock = GameNode::new(
        GameNodeKind::Clock { period_ticks: 4, duty_ticks: 1 },
        Vec2::new(-400.0, 100.0),
    );
    let clock_id = circuit.add_node(clock);

    let mut reset = GameNode::new(GameNodeKind::Input, Vec2::new(-400.0, -100.0));
    reset.label = Some("Reset".to_string());
    let reset_id = circuit.add_node(reset);

    let mut current_clk = clock_id;

    // Add 8 T Flip-Flops in a ripple counter configuration
    for i in 0..8 {
        let ff_pos = Vec2::new(-200.0 + (i as f32) * 200.0, 0.0);

        let (q, _qn) = add_t_ff(&mut circuit, current_clk, reset_id, ff_pos);

        let mut out_node = GameNode::new(
            GameNodeKind::Output,
            Vec2::new(ff_pos.x + 150.0, ff_pos.y + 100.0),
        );
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

    // Gates
    let xor_id = create_node(GameNodeKind::Xor, 0.0, 50.0);
    let and_id = create_node(GameNodeKind::And, 0.0, -50.0);

    // Drop the closure before adding nodes manually to satisfy borrow checker
    drop(create_node);

    // Inputs
    let mut a_node = GameNode::new(GameNodeKind::Input, Vec2::new(-200.0, 50.0));
    a_node.label = Some("A".to_string());
    let a_id = circuit.add_node(a_node);

    let mut b_node = GameNode::new(GameNodeKind::Input, Vec2::new(-200.0, -50.0));
    b_node.label = Some("B".to_string());
    let b_id = circuit.add_node(b_node);

    // Outputs
    let mut sum_node = GameNode::new(GameNodeKind::Output, Vec2::new(200.0, 50.0));
    sum_node.label = Some("Sum".to_string());
    let sum_id = circuit.add_node(sum_node);

    let mut carry_node = GameNode::new(GameNodeKind::Output, Vec2::new(200.0, -50.0));
    carry_node.label = Some("Carry".to_string());
    let carry_id = circuit.add_node(carry_node);

    // Wiring
    circuit.add_wire(GameWire::new(a_id, 0, xor_id, 0));
    circuit.add_wire(GameWire::new(b_id, 0, xor_id, 1));

    circuit.add_wire(GameWire::new(a_id, 0, and_id, 0));
    circuit.add_wire(GameWire::new(b_id, 0, and_id, 1));

    circuit.add_wire(GameWire::new(xor_id, 0, sum_id, 0));
    circuit.add_wire(GameWire::new(and_id, 0, carry_id, 0));

    circuit
}
