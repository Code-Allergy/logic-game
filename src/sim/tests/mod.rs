pub mod geometry_tests;

use std::{
    thread,
    time::{Duration, Instant},
};

use bevy::prelude::*;
use logi_sim::engine::{RunnerType, SimEvent};

use crate::sim::{
    GameCircuit, GameNode, GameNodeKind, GameWire, SimBridge, SimCache, SimulationConfig,
    systems::bridge_flush_system,
};

#[test]
fn counter_block_has_stable_two_input_eight_output_interface() {
    let circuit = super::fixtures::build_8bit_counter_block();
    let mut inputs: Vec<_> = circuit
        .nodes
        .values()
        .filter(|node| matches!(node.kind, GameNodeKind::Input))
        .map(|node| node.label.as_deref().unwrap())
        .collect();
    let mut outputs: Vec<_> = circuit
        .nodes
        .values()
        .filter(|node| matches!(node.kind, GameNodeKind::Output))
        .map(|node| node.label.as_deref().unwrap())
        .collect();
    inputs.sort_unstable();
    outputs.sort_unstable();

    assert_eq!(inputs, ["Increment", "Reset"]);
    assert_eq!(outputs.len(), 8);
    assert_eq!(outputs[0], "Bit 00");
    assert_eq!(outputs[7], "Bit 07");
}

#[test]
fn counter_64_fixture_chains_bytes_and_connects_every_display_pin() {
    let circuit = super::fixtures::build_64bit_counter_subcircuits();
    let display = circuit
        .nodes
        .values()
        .find(|node| matches!(node.kind, GameNodeKind::Display { bits: 64 }))
        .expect("64-bit display");
    let mut counters: Vec<_> = circuit
        .nodes
        .values()
        .filter(|node| matches!(node.kind, GameNodeKind::SubCircuit { .. }))
        .collect();
    counters.sort_by_key(|node| node.label.as_deref());
    assert_eq!(counters.len(), 8);
    assert!(circuit.nodes.values().any(|node| {
        matches!(node.kind, GameNodeKind::Clock { .. })
            && node.label.as_deref() == Some("Increment Clock")
    }));

    let mut display_pins: Vec<_> = circuit
        .wires
        .values()
        .filter(|wire| wire.sink_node == display.id)
        .map(|wire| wire.sink_pin)
        .collect();
    display_pins.sort_unstable();
    assert_eq!(display_pins, (0_u8..64).collect::<Vec<_>>());

    for pair in counters.windows(2) {
        assert!(circuit.wires.values().any(|wire| {
            wire.source_node == pair[0].id
                && wire.source_pin == 7
                && wire.sink_node == pair[1].id
                && wire.sink_pin == 0
        }));
    }

    let bridge = SimBridge::from_circuit(&circuit);
    for wire in circuit.wires.values() {
        assert!(
            bridge.wire_id(wire.id).is_some(),
            "wire {} was not translated",
            wire.id
        );
    }
}

#[test]
fn counter_64_power_on_reset_starts_at_zero_then_counts() {
    let circuit = super::fixtures::build_64bit_counter_subcircuits();
    let reset = circuit
        .nodes
        .values()
        .find(|node| node.label.as_deref() == Some("Reset"))
        .unwrap()
        .id;
    let display = circuit
        .nodes
        .values()
        .find(|node| matches!(node.kind, GameNodeKind::Display { bits: 64 }))
        .unwrap()
        .id;
    let mut display_wires: Vec<_> = circuit
        .wires
        .values()
        .filter(|wire| wire.sink_node == display)
        .map(|wire| (wire.sink_pin, wire.id))
        .collect();
    display_wires.sort_unstable_by_key(|(pin, _)| *pin);

    let mut bridge = SimBridge::from_circuit(&circuit);
    bridge.pause();
    bridge.set_input(reset, 0, true);
    let reset_target = bridge.tick_count() + 64;
    for _ in 0..64 {
        bridge.step();
    }
    let deadline = Instant::now() + Duration::from_secs(2);
    while bridge.tick_count() < reset_target && Instant::now() < deadline {
        thread::yield_now();
    }

    let sim_wires: Vec<_> = display_wires
        .iter()
        .map(|(_, wire)| bridge.wire_id(*wire).unwrap())
        .collect();
    let mut any_set = false;
    bridge.snapshot_wire_states(&mut |wire, value| {
        any_set |= value && sim_wires.contains(&wire);
    });
    assert!(!any_set, "power-on reset did not clear the display");

    let display_value = |bridge: &SimBridge| {
        let mut value = 0_u64;
        bridge.snapshot_wire_states(&mut |wire, set| {
            if set {
                if let Some(bit) = sim_wires.iter().position(|candidate| *candidate == wire) {
                    value |= 1_u64 << bit;
                }
            }
        });
        value
    };
    bridge.set_input(reset, 0, false);
    let mut previous = 0_u64;
    for step in 0..32 {
        let target = bridge.tick_count() + 1;
        bridge.step();
        let deadline = Instant::now() + Duration::from_secs(2);
        while bridge.tick_count() < target && Instant::now() < deadline {
            thread::yield_now();
        }
        let current = display_value(&bridge);
        assert!(
            current == previous || current == previous + 1,
            "counter fluctuated from {previous:#x} to {current:#x} at step {step}"
        );
        previous = current;
    }
    assert_ne!(
        previous, 0,
        "counter remained at zero after reset was released"
    );

    let before_switch = display_value(&bridge);

    bridge.set_runner_type(RunnerType::EventDrivenOptimized);
    let switch_deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if bridge
            .drain_events()
            .iter()
            .any(|event| matches!(event, SimEvent::RunnerSet(RunnerType::EventDrivenOptimized)))
        {
            break;
        }
        assert!(Instant::now() < switch_deadline, "runner switch timed out");
        thread::yield_now();
    }
    assert_eq!(display_value(&bridge), before_switch);

    let continued_target = bridge.tick_count() + 8;
    for _ in 0..8 {
        bridge.step();
    }
    while bridge.tick_count() < continued_target && Instant::now() < switch_deadline {
        thread::yield_now();
    }
    assert_ne!(
        display_value(&bridge),
        0,
        "counter state was lost after ticking the replacement runner"
    );

    for runner in [
        RunnerType::SimdOptimized,
        RunnerType::Flat,
        RunnerType::ThreadedFlat,
        RunnerType::EventDriven,
        RunnerType::Simd,
        RunnerType::FlatOptimized,
        RunnerType::ThreadedFlatOptimized,
        RunnerType::NaiveOptimized,
        RunnerType::Naive,
    ] {
        let before = display_value(&bridge);
        bridge.set_runner_type(runner);
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if bridge
                .drain_events()
                .iter()
                .any(|event| matches!(event, SimEvent::RunnerSet(active) if *active == runner))
            {
                break;
            }
            assert!(Instant::now() < deadline, "runner switch timed out");
            thread::yield_now();
        }
        assert_eq!(
            display_value(&bridge),
            before,
            "state changed switching to {runner}"
        );

        let mut previous = before;
        for _ in 0..8 {
            let target = bridge.tick_count() + 1;
            bridge.step();
            while bridge.tick_count() < target && Instant::now() < deadline {
                thread::yield_now();
            }
            let current = display_value(&bridge);
            assert!(
                current == previous || current == previous + 1,
                "{runner} counter fluctuated from {previous:#x} to {current:#x}"
            );
            previous = current;
        }
        assert_ne!(previous, 0, "counter reset after switching to {runner}");
    }
    bridge.stop();
}

#[test]
fn threadless_bridge_steps_without_a_worker() {
    let fixture = super::fixtures::build_8bit_counter_block();
    let mut bridge = SimBridge::from_circuit_threadless(&fixture);

    let start = bridge.tick_count();
    bridge.step_many(10);
    assert_eq!(bridge.tick_count(), start + 10);

    bridge.set_runner_type(RunnerType::SimdOptimized);
    bridge.step_many(5);
    assert_eq!(bridge.tick_count(), start + 15);

    let mut observed = Vec::new();
    bridge.snapshot_wire_states(&mut |wire, value| observed.push((wire, value)));
    assert!(
        !observed.is_empty(),
        "snapshot should include wire state in threadless mode"
    );
}

#[test]
fn tick_rate_uses_si_units() {
    assert_eq!(super::systems::format_si_hz(999.0), "999.0 Hz");
    assert_eq!(super::systems::format_si_hz(12_345.0), "12.3 kHz");
    assert_eq!(super::systems::format_si_hz(12_345_678.0), "12.3 MHz");
    assert_eq!(super::systems::format_si_hz(1_500_000_000.0), "1.5 GHz");
}

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
