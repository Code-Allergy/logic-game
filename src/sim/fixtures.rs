use bevy::prelude::Vec2;

use crate::sim::{GameCircuit, GameNode, GameNodeKind, GameWire};
use uuid::Uuid;

pub fn fixture_workspace_catalog() -> Vec<(String, GameCircuit)> {
    vec![
        ("Half Adder".to_string(), build_half_adder()),
        ("8-Bit Counter".to_string(), build_8bit_counter()),
        ("8-Bit Counter Block".to_string(), build_8bit_counter_block()),
        ("8-Bit Counter Subcircuit".to_string(), build_8bit_counter_subcircuit()),
        ("64-Bit Counter (8 x 8-Bit Subcircuits)".to_string(), build_64bit_counter_subcircuits()),
        (
            "Nested 64-Bit Counter (Subcircuit)".to_string(),
            build_nested_64bit_counter(),
        ),
        (
            "8-Bit Counter Subcircuit Wrapper".to_string(),
            build_8bit_counter_subcircuit_wrapper(),
        ),
        (
            "8-Bit Bus-Linked Counter".to_string(),
            build_8bit_bus_counter_wrapper(),
        ),
        (
            "64-Bit Bus-Linked Counter".to_string(),
            build_64bit_bus_counter_wrapper(),
        ),
        (
            "64-Bit Vector Counter (Vector Wire)".to_string(),
            build_64bit_vector_counter_wrapper(),
        ),
        ("T Flip-Flop".to_string(), build_t_ff_circuit()),
    ]
}

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

    let mut increment = GameNode::new(GameNodeKind::Input, Vec2::new(-400.0, 100.0));
    increment.label = Some("Increment".to_string());
    let increment_id = circuit.add_node(increment);

    let mut reset = GameNode::new(GameNodeKind::Input, Vec2::new(-400.0, -100.0));
    reset.label = Some("Reset".to_string());
    let reset_id = circuit.add_node(reset);

    let mut current_clk = increment_id;
    let output_bus_id = circuit.add_node(GameNode::new(
        GameNodeKind::Output8,
        Vec2::new(150.0, 120.0),
    ));

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

        circuit.add_wire(GameWire::new(ff_id, 0, output_bus_id, i)); // Q to output bit

        current_clk = ff_id;
    }
    circuit
}

/// Builds the reusable byte-sized counter used by the 64-bit GUI fixture.
/// Its alphabetically ordered interface is Increment, Reset, then Bit 00..07.
pub fn build_8bit_counter_block() -> GameCircuit {
    let mut circuit = GameCircuit::new("8-Bit Counter Block");

    let mut increment = GameNode::new(GameNodeKind::Input, Vec2::new(-300.0, 80.0));
    increment.label = Some("Increment".into());
    let increment_id = circuit.add_node(increment);
    let mut reset = GameNode::new(GameNodeKind::Input, Vec2::new(-300.0, -80.0));
    reset.label = Some("Reset".into());
    let reset_id = circuit.add_node(reset);

    let mut current_clk = increment_id;
    for bit in 0..8 {
        let ff_pos = Vec2::new(-200.0 + (bit as f32) * 200.0, 0.0);
        let (q, _qn) = add_t_ff(&mut circuit, current_clk, reset_id, ff_pos);

        let mut output = GameNode::new(
            GameNodeKind::Output,
            Vec2::new(ff_pos.x + 150.0, ff_pos.y + 100.0),
        );
        output.label = Some(format!("Bit {bit:02}"));
        let out_node_id = circuit.add_node(output);
        circuit.add_wire(GameWire::new(q, 0, out_node_id, 0));

        current_clk = q;
    }

    circuit
}

/// Builds eight byte counter subcircuits chained into a 64-bit ripple counter.
/// The 64 scalar simulation wires terminate at one hexadecimal display node.
pub fn build_64bit_counter_subcircuits() -> GameCircuit {
    let mut circuit = GameCircuit::new("64-Bit Counter (8 x 8-Bit Subcircuits)");

    let mut increment = GameNode::new(
        GameNodeKind::Clock {
            period_ticks: 4,
            duty_ticks: 2,
        },
        Vec2::new(-650.0, 120.0),
    );
    increment.label = Some("Increment Clock".into());
    let increment_id = circuit.add_node(increment);
    let mut reset = GameNode::new(GameNodeKind::Input, Vec2::new(-650.0, -120.0));
    reset.label = Some("Reset".into());
    let reset_id = circuit.add_node(reset);
    let display_id = circuit.add_node(GameNode::new(
        GameNodeKind::Display { bits: 64 },
        Vec2::new(650.0, 0.0),
    ));
    let mut output_bus = GameNode::new(
        GameNodeKind::Output64,
        Vec2::new(650.0, -180.0),
    );
    output_bus.label = Some("Output64".to_string());
    let output_bus_id = circuit.add_node(output_bus);

    let mut ripple = (increment_id, 0_u8);
    for byte in 0..8 {
        let x = -450.0 + byte as f32 * 125.0;
        let mut counter = GameNode::new(
            GameNodeKind::SubCircuit {
                circuit: Box::new(build_8bit_counter_subcircuit()),
            },
            Vec2::new(x, 0.0),
        );
        counter.label = Some(format!("Byte {byte:02}"));
        let counter_id = circuit.add_node(counter);
        circuit.add_wire(GameWire::new(ripple.0, ripple.1, counter_id, 0));
        circuit.add_wire(GameWire::new(reset_id, 0, counter_id, 1));
        for bit in 0..8_u8 {
            let output_pin = byte * 8 + bit;
            circuit.add_wire(GameWire::new(counter_id, bit, output_bus_id, output_pin));
            circuit.add_wire(GameWire::new(counter_id, bit, display_id, output_pin));
        }
        ripple = (counter_id, 7);
    }

    circuit
}

/// Builds a nested version of the 64-bit counter, where the entire 64-bit
/// counter lives inside one child subcircuit.
pub fn build_nested_64bit_counter() -> GameCircuit {
    let mut circuit = GameCircuit::new("Nested 64-Bit Counter (Subcircuit)");

    let mut increment = GameNode::new(
        GameNodeKind::Input,
        Vec2::new(-650.0, 120.0),
    );
    increment.label = Some("Increment Clock".into());
    let increment_id = circuit.add_node(increment);

    let mut reset = GameNode::new(GameNodeKind::Input, Vec2::new(-650.0, -120.0));
    reset.label = Some("Reset".into());
    let reset_id = circuit.add_node(reset);

    let mut counter = GameNode::new(
        GameNodeKind::SubCircuit {
            circuit: Box::new(build_64bit_counter_subcircuits()),
        },
        Vec2::new(-50.0, 0.0),
    );
    counter.label = Some("64-Bit Counter".to_string());
    let counter_id = circuit.add_node(counter);

    let display_id = circuit.add_node(GameNode::new(
        GameNodeKind::Display { bits: 64 },
        Vec2::new(650.0, 0.0),
    ));

    circuit.add_wire(GameWire::new(increment_id, 0, counter_id, 0));
    circuit.add_wire(GameWire::new(reset_id, 0, counter_id, 1));

    for bit in 0..64_u8 {
        circuit.add_wire(GameWire::new(counter_id, bit, display_id, bit));
    }

    circuit
}

/// Builds an 8-bit ripple counter fixture using T Flip-Flops.
pub fn build_8bit_counter() -> GameCircuit {
    let mut circuit = GameCircuit::new("8-Bit Counter");

    let clock = GameNode::new(
        GameNodeKind::Clock {
            period_ticks: 4,
            duty_ticks: 1,
        },
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

/// Builds an 8-bit counter where the existing 8-bit counter block is
/// wrapped in another subcircuit layer.
pub fn build_8bit_counter_subcircuit_wrapper() -> GameCircuit {
    let mut circuit = GameCircuit::new("8-Bit Counter Subcircuit Wrapper");

    let mut increment = GameNode::new(
        GameNodeKind::Clock {
            period_ticks: 4,
            duty_ticks: 2,
        },
        Vec2::new(-650.0, 120.0),
    );
    increment.label = Some("Increment Clock".to_string());
    let increment_id = circuit.add_node(increment);

    let mut reset = GameNode::new(GameNodeKind::Input, Vec2::new(-650.0, -120.0));
    reset.label = Some("Reset".to_string());
    let reset_id = circuit.add_node(reset);

    let mut counter = GameNode::new(
        GameNodeKind::SubCircuit {
            circuit: Box::new(build_8bit_counter_block()),
        },
        Vec2::new(-50.0, 0.0),
    );
    counter.label = Some("8-Bit Counter".to_string());
    let counter_id = circuit.add_node(counter);

    let display_id = circuit.add_node(GameNode::new(
        GameNodeKind::Display { bits: 8 },
        Vec2::new(650.0, 0.0),
    ));

    circuit.add_wire(GameWire::new(increment_id, 0, counter_id, 0));
    circuit.add_wire(GameWire::new(reset_id, 0, counter_id, 1));
    for bit in 0..8_u8 {
        circuit.add_wire(GameWire::new(counter_id, bit, display_id, bit));
    }

    circuit
}

/// Builds an 8-bit counter that exposes its bus on an `Output8` node and a hex
/// display.
pub fn build_8bit_bus_counter_wrapper() -> GameCircuit {
    let mut circuit = GameCircuit::new("8-Bit Bus-Linked Counter");

    let mut increment = GameNode::new(
        GameNodeKind::Clock {
            period_ticks: 4,
            duty_ticks: 2,
        },
        Vec2::new(-650.0, 120.0),
    );
    increment.label = Some("Increment Clock".into());
    let increment_id = circuit.add_node(increment);

    let mut reset = GameNode::new(GameNodeKind::Input, Vec2::new(-650.0, -120.0));
    reset.label = Some("Reset".into());
    let reset_id = circuit.add_node(reset);

    let mut counter = GameNode::new(
        GameNodeKind::SubCircuit {
            circuit: Box::new(build_8bit_counter_block()),
        },
        Vec2::new(-150.0, 0.0),
    );
    counter.label = Some("8-Bit Counter".into());
    let counter_id = circuit.add_node(counter);

    let mut output_bus = GameNode::new(
        GameNodeKind::Output8,
        Vec2::new(250.0, 0.0),
    );
    output_bus.label = Some("Output8".to_string());
    let output_bus_id = circuit.add_node(output_bus);

    let display_id = circuit.add_node(GameNode::new(
        GameNodeKind::Display { bits: 8 },
        Vec2::new(650.0, 0.0),
    ));

    circuit.add_wire(GameWire::new(increment_id, 0, counter_id, 0));
    circuit.add_wire(GameWire::new(reset_id, 0, counter_id, 1));
    for bit in 0..8_u8 {
        circuit.add_wire(GameWire::new(counter_id, bit, output_bus_id, bit));
        circuit.add_wire(GameWire::new(counter_id, bit, display_id, bit));
    }

    circuit
}

/// Builds a 64-bit counter that exposes its bus on an `Output64` node and a 64-bit display.
pub fn build_64bit_bus_counter_wrapper() -> GameCircuit {
    let mut circuit = GameCircuit::new("64-Bit Bus-Linked Counter");

    let mut increment = GameNode::new(
        GameNodeKind::Clock {
            period_ticks: 4,
            duty_ticks: 2,
        },
        Vec2::new(-650.0, 120.0),
    );
    increment.label = Some("Increment Clock".into());
    let increment_id = circuit.add_node(increment);

    let mut reset = GameNode::new(GameNodeKind::Input, Vec2::new(-650.0, -120.0));
    reset.label = Some("Reset".into());
    let reset_id = circuit.add_node(reset);

    let mut counter = GameNode::new(
        GameNodeKind::SubCircuit {
            circuit: Box::new(build_64bit_counter_subcircuits()),
        },
        Vec2::new(-150.0, 0.0),
    );
    counter.label = Some("64-Bit Counter".into());
    let counter_id = circuit.add_node(counter);

    let mut output_bus = GameNode::new(
        GameNodeKind::Output64,
        Vec2::new(250.0, 0.0),
    );
    output_bus.label = Some("Output64".to_string());
    let output_bus_id = circuit.add_node(output_bus);

    let display_id = circuit.add_node(GameNode::new(
        GameNodeKind::Display { bits: 64 },
        Vec2::new(750.0, 0.0),
    ));

    circuit.add_wire(GameWire::new(increment_id, 0, counter_id, 0));
    circuit.add_wire(GameWire::new(reset_id, 0, counter_id, 1));
    for bit in 0..64_u8 {
        circuit.add_wire(GameWire::new(counter_id, bit, output_bus_id, bit));
        circuit.add_wire(GameWire::new(counter_id, bit, display_id, bit));
    }

    circuit
}

/// Builds a 64-bit counter fixture where the entire bus is connected through
/// a single vector wire (not individual bit wires).
pub fn build_64bit_vector_counter_wrapper() -> GameCircuit {
    let mut circuit = GameCircuit::new("64-Bit Vector Counter");

    let mut increment = GameNode::new(
        GameNodeKind::Clock {
            period_ticks: 4,
            duty_ticks: 2,
        },
        Vec2::new(-650.0, 120.0),
    );
    increment.label = Some("Increment Clock".into());
    let increment_id = circuit.add_node(increment);

    let mut reset = GameNode::new(GameNodeKind::Input, Vec2::new(-650.0, -120.0));
    reset.label = Some("Reset".into());
    let reset_id = circuit.add_node(reset);

    let mut counter = GameNode::new(
        GameNodeKind::SubCircuit {
            circuit: Box::new(build_64bit_counter_subcircuits()),
        },
        Vec2::new(-150.0, 0.0),
    );
    counter.label = Some("64-Bit Counter".into());
    let counter_id = circuit.add_node(counter);

    let mut output_bus = GameNode::new(
        GameNodeKind::Output64,
        Vec2::new(250.0, 0.0),
    );
    output_bus.label = Some("Output64".to_string());
    let output_bus_id = circuit.add_node(output_bus);

    let display_id = circuit.add_node(GameNode::new(
        GameNodeKind::Display { bits: 64 },
        Vec2::new(750.0, 0.0),
    ));

    circuit.add_wire(GameWire::new(increment_id, 0, counter_id, 0));
    circuit.add_wire(GameWire::new(reset_id, 0, counter_id, 1));
    circuit.add_wire(GameWire::with_kind(
        counter_id,
        0,
        output_bus_id,
        0,
        crate::sim::model::WireKind::Vector { bits: 64 },
    ));
    circuit.add_wire(GameWire::with_kind(
        counter_id,
        0,
        display_id,
        0,
        crate::sim::model::WireKind::Vector { bits: 64 },
    ));

    circuit
}

/// Builds a `GameCircuit` populated with a Half Adder fixture.
pub fn build_half_adder() -> GameCircuit {
    let mut circuit = GameCircuit::new("Half Adder");

    // Gates
    let xor_id = {
        let node = GameNode::new(GameNodeKind::Xor, Vec2::new(0.0, 50.0));
        let id = node.id;
        circuit.add_node(node);
        id
    };

    let and_id = {
        let node = GameNode::new(GameNodeKind::And, Vec2::new(0.0, -50.0));
        let id = node.id;
        circuit.add_node(node);
        id
    };

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
