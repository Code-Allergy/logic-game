pub mod geometry_tests;

use std::{
    thread,
    time::{Duration, Instant},
};

use bevy::prelude::*;

use crate::sim::{
    GameCircuit, GameNode, GameNodeKind, GameWire, SimBridge, SimCache, SimulationConfig,
    systems::bridge_flush_system,
};

struct NestedFixture {
    circuit: GameCircuit,
    input: uuid::Uuid,
    output_a: uuid::Uuid,
    output_b: uuid::Uuid,
    output_wire_a: uuid::Uuid,
    output_wire_b: uuid::Uuid,
}

fn interface_node(kind: GameNodeKind, label: &str) -> GameNode {
    let mut node = GameNode::new(kind, Vec2::ZERO);
    node.label = Some(label.to_owned());
    node
}

fn nested_fanout_fixture() -> NestedFixture {
    let mut leaf = GameCircuit::new("leaf");
    let leaf_in = leaf.add_node(interface_node(GameNodeKind::Input, "A"));
    let buffer = leaf.add_node(GameNode::new(GameNodeKind::Buffer, Vec2::ZERO));
    let leaf_out = leaf.add_node(interface_node(GameNodeKind::Output, "Y"));
    leaf.add_wire(GameWire::new(leaf_in, 0, buffer, 0));
    leaf.add_wire(GameWire::new(buffer, 0, leaf_out, 0));

    let mut wrapper = GameCircuit::new("wrapper");
    let wrapper_in = wrapper.add_node(interface_node(GameNodeKind::Input, "A"));
    let leaf_node = wrapper.add_node(GameNode::new(
        GameNodeKind::SubCircuit {
            circuit: Box::new(leaf),
        },
        Vec2::ZERO,
    ));
    let wrapper_out = wrapper.add_node(interface_node(GameNodeKind::Output, "Y"));
    wrapper.add_wire(GameWire::new(wrapper_in, 0, leaf_node, 0));
    wrapper.add_wire(GameWire::new(leaf_node, 0, wrapper_out, 0));

    let mut circuit = GameCircuit::new("root");
    let input = circuit.add_node(interface_node(GameNodeKind::Input, "A"));
    let nested = circuit.add_node(GameNode::new(
        GameNodeKind::SubCircuit {
            circuit: Box::new(wrapper),
        },
        Vec2::ZERO,
    ));
    let output_a = circuit.add_node(interface_node(GameNodeKind::Output, "Y1"));
    let output_b = circuit.add_node(interface_node(GameNodeKind::Output, "Y2"));
    circuit.add_wire(GameWire::new(input, 0, nested, 0));
    let output_wire_a = circuit.add_wire(GameWire::new(nested, 0, output_a, 0));
    let output_wire_b = circuit.add_wire(GameWire::new(nested, 0, output_b, 0));

    NestedFixture {
        circuit,
        input,
        output_a,
        output_b,
        output_wire_a,
        output_wire_b,
    }
}

#[test]
fn bridge_api_translates_recursive_subcircuit_output_fanout() {
    let fixture = nested_fanout_fixture();
    let mut bridge = SimBridge::from_circuit(&fixture.circuit);
    bridge.set_tick_rate(None);
    bridge.set_input(fixture.input, 0, true);

    let wire_a = bridge.wire_id(fixture.output_wire_a).unwrap();
    let wire_b = bridge.wire_id(fixture.output_wire_b).unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut values = (false, false);
    while Instant::now() < deadline {
        bridge.snapshot_wire_states(&mut |wire, value| {
            if wire == wire_a {
                values.0 = value;
            }
            if wire == wire_b {
                values.1 = value;
            }
        });
        if values == (true, true) {
            break;
        }
        thread::sleep(Duration::from_millis(1));
    }
    bridge.stop();

    assert_eq!(values, (true, true));
}

#[test]
fn bevy_bridge_flush_exposes_all_nested_outputs_in_game_uuid_space() {
    let fixture = nested_fanout_fixture();
    let output_a = fixture.output_a;
    let output_b = fixture.output_b;
    let input = fixture.input;

    let bridge = SimBridge::from_circuit(&fixture.circuit);
    bridge.set_tick_rate(None);
    bridge.set_input(input, 0, true);

    let mut app = App::new();
    app.insert_resource(fixture.circuit)
        .insert_resource(bridge)
        .init_resource::<SimCache>()
        .init_resource::<SimulationConfig>()
        .add_systems(Update, bridge_flush_system);

    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        app.update();
        let cache = app.world().resource::<SimCache>();
        if cache.node_inputs.get(&(output_a, 0)) == Some(&true)
            && cache.node_inputs.get(&(output_b, 0)) == Some(&true)
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "nested outputs never reached the Bevy cache"
        );
        thread::sleep(Duration::from_millis(1));
    }
}
