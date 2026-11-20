//! Integration tests for the ECS → `Design` → v2 engine path, run in a
//! headless `App` with [`SimulationRuntimePlugin`].

pub mod geometry_tests;
mod instance_views;
mod visuals;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bevy::ecs::system::RunSystemOnce;
use bevy::prelude::*;
use uuid::Uuid;

use crate::editor::workspace::{
    WorkspaceError, list_workspaces_in, read_workspace_in, save_workspace_in,
};
use crate::sim::{
    CircuitDocument, CircuitReader, CreateGate, CreateWire, DeleteGate, DeleteWire, DocNode,
    GateIdentity, GateKind, GateLabel, GateType, GateValues, LoadWorkspaceEvent, SetGateKind,
    SetInputValue, SimBridge, SimDiagnostics, SimRuntime, SimulationConfig,
    SimulationRuntimePlugin, WireEndpoints, WireState, WorkspaceDocument, fixtures,
};

// ─────────────────────────────────────────────────────────────────────────────
// Harness
// ─────────────────────────────────────────────────────────────────────────────

fn app_with(runtime: SimRuntime) -> App {
    let mut app = App::new();
    app.add_plugins(SimulationRuntimePlugin { runtime });
    app
}

fn app() -> App {
    app_with(SimRuntime::Threadless)
}

fn load(app: &mut App, workspace: WorkspaceDocument) {
    app.world_mut().write_message(LoadWorkspaceEvent {
        name: workspace.root.name.clone(),
        workspace: Arc::new(workspace),
    });
    settle(app);
}

/// Apply pending commands: authoring in `Update`, design sync and load in
/// `PostUpdate`, probe projection in the next `PreUpdate`.
fn settle(app: &mut App) {
    app.update();
    app.update();
}

fn send<M: Message>(app: &mut App, message: M) {
    app.world_mut().write_message(message);
    settle(app);
}

/// Run `ticks` ticks and project the result.
fn run(app: &mut App, ticks: u64) {
    let target = app.world().resource::<SimBridge>().tick_count() + ticks;
    app.world().resource::<SimBridge>().step(ticks);
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        app.update();
        if app.world().resource::<SimBridge>().tick_count() >= target {
            break;
        }
        assert!(Instant::now() < deadline, "ticks did not run");
    }
    app.update();
}

fn gate(app: &mut App, label: &str) -> Entity {
    let mut q = app.world_mut().query::<(Entity, &GateLabel)>();
    q.iter(app.world())
        .find(|(_, l)| l.value == label)
        .map(|(e, _)| e)
        .unwrap_or_else(|| panic!("no gate labelled {label}"))
}

fn gate_of_kind(app: &mut App, pred: impl Fn(&GateKind) -> bool) -> Entity {
    let mut q = app.world_mut().query::<(Entity, &GateType)>();
    let found: Vec<Entity> = q.iter(app.world()).filter(|(_, t)| pred(&t.kind)).map(|(e, _)| e).collect();
    assert_eq!(found.len(), 1, "expected exactly one matching gate");
    found[0]
}

fn uuid(app: &App, entity: Entity) -> Uuid {
    app.world().get::<GateIdentity>(entity).unwrap().uuid
}

fn input_value(app: &App, entity: Entity, pin: usize) -> u64 {
    app.world().get::<GateValues>(entity).unwrap().inputs[pin]
}

fn output_value(app: &App, entity: Entity, pin: usize) -> u64 {
    app.world().get::<GateValues>(entity).unwrap().outputs[pin]
}

fn wires(app: &mut App) -> Vec<(Entity, WireEndpoints, WireState)> {
    let mut q = app.world_mut().query::<(Entity, &WireEndpoints, &WireState)>();
    q.iter(app.world()).map(|(e, ep, s)| (e, *ep, *s)).collect()
}

fn set_input(app: &mut App, label: &str, value: u64) {
    let entity = gate(app, label);
    send(app, SetInputValue { entity, value });
}

fn create_gate(app: &mut App, kind: GateKind, label: &str) -> Entity {
    send(app, CreateGate { kind, position: Vec2::ZERO, label: Some(label.into()) });
    gate(app, label)
}

fn create_wire(app: &mut App, from: Entity, from_pin: u8, to: Entity, to_pin: u8) {
    let (source_gate, sink_gate) = (uuid(app, from), uuid(app, to));
    send(app, CreateWire { source_gate, source_pin: from_pin, sink_gate, sink_pin: to_pin });
}

fn root_node_count(app: &App) -> usize {
    app.world().resource::<SimBridge>().design().root_def().nodes.len()
}

fn temp_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("gatequest-test-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn display_value(app: &mut App) -> u64 {
    let display = gate_of_kind(app, |k| matches!(k, GateKind::Display { .. }));
    input_value(app, display, 0)
}

// ─────────────────────────────────────────────────────────────────────────────
// Add, connect, toggle, delete
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn add_connect_toggle_through_authoring_messages() {
    let mut app = app();
    let a = create_gate(&mut app, GateKind::Input { w: 1 }, "A");
    let b = create_gate(&mut app, GateKind::Input { w: 1 }, "B");
    let xor = create_gate(&mut app, GateKind::Xor { w: 1 }, "x");
    let y = create_gate(&mut app, GateKind::Output { w: 1 }, "Y");
    create_wire(&mut app, a, 0, xor, 0);
    create_wire(&mut app, b, 0, xor, 1);
    create_wire(&mut app, xor, 0, y, 0);
    assert_eq!(root_node_count(&app), 4, "one design node per gate");
    assert!(wires(&mut app).iter().all(|(_, _, s)| s.connected && s.width == 1));

    run(&mut app, 1);
    assert_eq!(input_value(&app, y, 0), 0);

    set_input(&mut app, "A", 1);
    run(&mut app, 1);
    assert_eq!(output_value(&app, a, 0), 1);
    assert_eq!(input_value(&app, y, 0), 1);
    assert!(wires(&mut app).iter().all(|(_, _, s)| s.value == 1 || s.value == 0));
    let out_wire = wires(&mut app).into_iter().find(|(_, ep, _)| ep.sink_gate == uuid(&app, y)).unwrap();
    assert_eq!(out_wire.2.value, 1, "wire value comes from the engine probe");

    set_input(&mut app, "B", 1);
    run(&mut app, 1);
    assert_eq!(input_value(&app, y, 0), 0);
}

#[test]
fn new_wire_replaces_the_driver_of_an_input_pin() {
    let mut app = app();
    let a = create_gate(&mut app, GateKind::Input { w: 1 }, "A");
    let b = create_gate(&mut app, GateKind::Input { w: 1 }, "B");
    let y = create_gate(&mut app, GateKind::Output { w: 1 }, "Y");
    create_wire(&mut app, a, 0, y, 0);
    create_wire(&mut app, b, 0, y, 0);
    assert_eq!(wires(&mut app).len(), 1, "one driver per input pin");
    set_input(&mut app, "B", 1);
    run(&mut app, 1);
    assert_eq!(input_value(&app, y, 0), 1);
}

#[test]
fn width_mismatch_is_refused() {
    let mut app = app();
    let a = create_gate(&mut app, GateKind::Input { w: 8 }, "A");
    let y = create_gate(&mut app, GateKind::Output { w: 4 }, "Y");
    create_wire(&mut app, a, 0, y, 0);
    assert!(wires(&mut app).is_empty());
    assert!(app.world().resource::<crate::sim::AuthoringFeedback>().message.is_some());
}

#[test]
fn delete_wire_and_gate_end_to_end() {
    let mut app = app();
    let a = create_gate(&mut app, GateKind::Input { w: 1 }, "A");
    let not = create_gate(&mut app, GateKind::Not { w: 1 }, "n");
    let y = create_gate(&mut app, GateKind::Output { w: 1 }, "Y");
    create_wire(&mut app, a, 0, not, 0);
    create_wire(&mut app, not, 0, y, 0);
    run(&mut app, 1);
    assert_eq!(input_value(&app, y, 0), 1);

    // Delete the wire into the output: the pin is disconnected and reads 0.
    let (wire, ..) = wires(&mut app).into_iter().find(|(_, ep, _)| ep.sink_gate == uuid(&app, y)).unwrap();
    send(&mut app, DeleteWire { entity: wire });
    run(&mut app, 1);
    assert_eq!(input_value(&app, y, 0), 0);
    let y_node = app.world().resource::<SimBridge>().node_of(y).unwrap();
    assert_eq!(app.world().resource::<SimBridge>().design().root_def().input_net(y_node, 0), None);

    // Delete the NOT gate: its node and its remaining wire go too.
    send(&mut app, DeleteGate { entity: not });
    assert!(app.world().get_entity(not).is_err());
    assert!(wires(&mut app).is_empty());
    assert_eq!(root_node_count(&app), 2);
    run(&mut app, 1);
    assert_eq!(input_value(&app, y, 0), 0);
}

#[test]
fn word_wires_with_split_and_merge() {
    let mut app = app();
    let a = create_gate(&mut app, GateKind::Input { w: 8 }, "A");
    let hi = create_gate(&mut app, GateKind::Split { w: 8, lo: 4, len: 4 }, "hi");
    let lo = create_gate(&mut app, GateKind::Split { w: 8, lo: 0, len: 4 }, "lo");
    let swap = create_gate(&mut app, GateKind::Merge { widths: vec![4, 4] }, "swap");
    let y = create_gate(&mut app, GateKind::Output { w: 8 }, "Y");
    create_wire(&mut app, a, 0, hi, 0);
    create_wire(&mut app, a, 0, lo, 0);
    create_wire(&mut app, hi, 0, swap, 0);
    create_wire(&mut app, lo, 0, swap, 1);
    create_wire(&mut app, swap, 0, y, 0);
    set_input(&mut app, "A", 0xA5);
    run(&mut app, 1);
    assert_eq!(input_value(&app, y, 0), 0x5A);
    assert!(wires(&mut app).iter().any(|(_, _, s)| s.width == 8 && s.value == 0xA5));
}

#[test]
fn changing_a_width_disconnects_mismatched_wires() {
    let mut app = app();
    let a = create_gate(&mut app, GateKind::Input { w: 4 }, "A");
    let y = create_gate(&mut app, GateKind::Output { w: 4 }, "Y");
    create_wire(&mut app, a, 0, y, 0);
    set_input(&mut app, "A", 9);
    run(&mut app, 1);
    assert_eq!(input_value(&app, y, 0), 9);

    send(&mut app, SetGateKind { entity: y, kind: GateKind::Output { w: 8 } });
    assert!(!wires(&mut app)[0].2.connected);
    send(&mut app, SetGateKind { entity: a, kind: GateKind::Input { w: 8 } });
    assert!(wires(&mut app)[0].2.connected, "widths match again");
    run(&mut app, 1);
    assert_eq!(input_value(&app, y, 0), 9, "the input keeps its driven value");
}

// ─────────────────────────────────────────────────────────────────────────────
// State across edits
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn register_state_survives_edits() {
    let mut app = app();
    load(&mut app, fixtures::build_register_counter());
    set_input(&mut app, "Enable", 1);
    run(&mut app, 20);
    let before = display_value(&mut app);
    assert!(before >= 18, "counter ran: {before}");

    // Edits: add a gate, wire it, delete it again.
    let extra = create_gate(&mut app, GateKind::Not { w: 1 }, "extra");
    let enable = gate(&mut app, "Enable");
    create_wire(&mut app, enable, 0, extra, 0);
    assert_eq!(display_value(&mut app), before, "edit did not reset the register");
    send(&mut app, DeleteGate { entity: extra });
    assert_eq!(display_value(&mut app), before);

    run(&mut app, 5);
    assert_eq!(display_value(&mut app), before + 5);
}

#[test]
fn ram_contents_survive_edits() {
    let mut app = app();
    let addr = create_gate(&mut app, GateKind::Input { w: 2 }, "Addr");
    let we = create_gate(&mut app, GateKind::Input { w: 1 }, "WE");
    let data = create_gate(&mut app, GateKind::Input { w: 8 }, "Data");
    let ram = create_gate(&mut app, GateKind::Ram { addr_bits: 2, w: 8 }, "ram");
    let q = create_gate(&mut app, GateKind::Output { w: 8 }, "Q");
    create_wire(&mut app, addr, 0, ram, 0);
    create_wire(&mut app, we, 0, ram, 1);
    create_wire(&mut app, data, 0, ram, 2);
    create_wire(&mut app, ram, 0, q, 0);

    set_input(&mut app, "Addr", 1);
    set_input(&mut app, "Data", 0x5A);
    set_input(&mut app, "WE", 1);
    run(&mut app, 2);
    set_input(&mut app, "WE", 0);
    set_input(&mut app, "Data", 0);
    run(&mut app, 2);
    assert_eq!(input_value(&app, q, 0), 0x5A);

    // Edit the circuit: the RAM keeps its contents.
    let extra = create_gate(&mut app, GateKind::And { w: 8 }, "extra");
    create_wire(&mut app, data, 0, extra, 0);
    run(&mut app, 1);
    assert_eq!(input_value(&app, q, 0), 0x5A);
    set_input(&mut app, "Addr", 0);
    run(&mut app, 1);
    assert_eq!(input_value(&app, q, 0), 0);
    set_input(&mut app, "Addr", 1);
    run(&mut app, 1);
    assert_eq!(input_value(&app, q, 0), 0x5A);
}

#[test]
fn combinational_loop_is_reported_and_previous_program_keeps_running() {
    let mut app = app();
    let a = create_gate(&mut app, GateKind::Input { w: 1 }, "A");
    let y = create_gate(&mut app, GateKind::Output { w: 1 }, "Y");
    create_wire(&mut app, a, 0, y, 0);
    run(&mut app, 1);
    assert!(app.world().resource::<SimDiagnostics>().compile_error.is_none());

    // A NOT gate feeding itself is a combinational loop.
    let not = create_gate(&mut app, GateKind::Not { w: 1 }, "loop");
    create_wire(&mut app, not, 0, not, 0);
    let error = app.world().resource::<SimDiagnostics>().compile_error.clone();
    let error = error.expect("loop reported");
    assert_eq!(error.loop_gates, vec![not]);
    assert!(error.messages[0].contains("loop"), "{:?}", error.messages);

    // The previous program still runs and still takes input changes.
    set_input(&mut app, "A", 1);
    run(&mut app, 1);
    assert_eq!(input_value(&app, y, 0), 1);

    // Breaking the loop with a Delay compiles again and clears the error.
    let delay = create_gate(&mut app, GateKind::Delay { w: 1 }, "delay");
    create_wire(&mut app, not, 0, delay, 0);
    create_wire(&mut app, delay, 0, not, 0);
    assert!(app.world().resource::<SimDiagnostics>().compile_error.is_none());
    run(&mut app, 4);
    assert_eq!(input_value(&app, y, 0), 1, "state and inputs carried over");
}

// ─────────────────────────────────────────────────────────────────────────────
// Subcircuits and examples
// ─────────────────────────────────────────────────────────────────────────────

/// A passthrough leaf (input wired straight to output) inside a wrapper,
/// instanced in the root with fan-out to two outputs.
fn nested_fanout_fixture() -> WorkspaceDocument {
    let mut leaf = CircuitDocument::new("leaf");
    let li = leaf.add(DocNode::labeled(GateKind::Input { w: 1 }, Vec2::ZERO, "A"));
    let lo = leaf.add(DocNode::labeled(GateKind::Output { w: 1 }, Vec2::ZERO, "Y"));
    leaf.connect(li, 0, lo, 0);

    let mut ws = WorkspaceDocument::new(CircuitDocument::new("root"));
    let leaf_id = ws.define(leaf);
    let mut wrapper = CircuitDocument::new("wrapper");
    let wi = wrapper.add(DocNode::labeled(GateKind::Input { w: 1 }, Vec2::ZERO, "A"));
    let wl = wrapper.add(DocNode::new(GateKind::SubCircuit { definition: leaf_id }, Vec2::ZERO));
    let wo = wrapper.add(DocNode::labeled(GateKind::Output { w: 1 }, Vec2::ZERO, "Y"));
    wrapper.connect(wi, 0, wl, 0);
    wrapper.connect(wl, 0, wo, 0);
    let wrapper_id = ws.define(wrapper);

    let root = &mut ws.root;
    let input = root.add(DocNode::labeled(GateKind::Input { w: 1 }, Vec2::ZERO, "A"));
    let nested = root.add(DocNode::labeled(GateKind::SubCircuit { definition: wrapper_id }, Vec2::ZERO, "nested"));
    let y1 = root.add(DocNode::labeled(GateKind::Output { w: 1 }, Vec2::ZERO, "Y1"));
    let y2 = root.add(DocNode::labeled(GateKind::Output { w: 1 }, Vec2::ZERO, "Y2"));
    root.connect(input, 0, nested, 0);
    root.connect(nested, 0, y1, 0);
    root.connect(nested, 0, y2, 0);
    ws
}

#[test]
fn nested_subcircuit_outputs_fan_out() {
    let mut app = app();
    load(&mut app, nested_fanout_fixture());
    assert!(wires(&mut app).iter().all(|(_, _, s)| s.connected));
    set_input(&mut app, "A", 1);
    run(&mut app, 1);
    let (y1, y2) = (gate(&mut app, "Y1"), gate(&mut app, "Y2"));
    assert_eq!((input_value(&app, y1, 0), input_value(&app, y2, 0)), (1, 1));
}

#[test]
fn threaded_engine_runs_the_same_path() {
    let mut app = app_with(SimRuntime::Threaded);
    load(&mut app, nested_fanout_fixture());
    set_input(&mut app, "A", 1);
    app.world().resource::<SimBridge>().step(1);
    let (y1, y2) = (gate(&mut app, "Y1"), gate(&mut app, "Y2"));
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        app.world_mut().resource_mut::<SimBridge>().wait_until_current(Duration::from_secs(5));
        app.update();
        if input_value(&app, y1, 0) == 1 && input_value(&app, y2, 0) == 1 {
            break;
        }
        assert!(Instant::now() < deadline, "nested outputs never reached the ECS");
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn counter_block_has_word_interface() {
    let ws = fixtures::build_8bit_counter_block();
    let (inputs, outputs) = ws.root.interface();
    let names = |v: &[&DocNode]| v.iter().map(|n| n.label.clone().unwrap()).collect::<Vec<_>>();
    assert_eq!(names(&inputs), ["Increment", "Reset"]);
    assert_eq!(names(&outputs), ["Bits", "Ripple"]);
    assert_eq!(outputs[0].kind, GateKind::Output { w: 8 });
}

/// The counters use gate flip-flops with a Delay in each latch loop; they
/// compile, start at 0 without a reset pulse, and count.
#[test]
fn counter_examples_compile_start_at_zero_and_count() {
    for (ws, ticks) in [
        (fixtures::build_8bit_counter(), 40),
        (fixtures::build_64bit_counter_subcircuits(), 40),
        (fixtures::build_nested_64bit_counter(), 40),
    ] {
        let name = ws.root.name.clone();
        let mut app = app();
        load(&mut app, ws);
        assert!(wires(&mut app).iter().all(|(_, _, s)| s.connected), "{name}: every wire simulated");
        assert!(app.world().resource::<SimDiagnostics>().compile_error.is_none(), "{name} compiles");
        assert_eq!(display_value(&mut app), 0, "{name} powers up at 0");
        let mut previous = 0;
        for _ in 0..ticks {
            run(&mut app, 1);
            let v = display_value(&mut app);
            assert!(v == previous || v == previous + 1, "{name}: {previous} -> {v}");
            previous = v;
        }
        assert!(previous >= 8, "{name} counted to {previous}");

        // Holding Reset clears it.
        set_input(&mut app, "Reset", 1);
        run(&mut app, 8);
        assert_eq!(display_value(&mut app), 0, "{name} resets");
    }
}

#[test]
fn every_example_loads_and_compiles() {
    for (name, ws) in fixtures::fixture_workspace_catalog() {
        let mut app = app();
        load(&mut app, (*ws).clone());
        assert!(app.world().resource::<SimDiagnostics>().compile_error.is_none(), "{name}");
        assert!(app.world().resource::<SimDiagnostics>().running_revision.is_some(), "{name}");
        assert!(wires(&mut app).iter().all(|(_, _, s)| s.connected), "{name}: wires");
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Save / load
// ─────────────────────────────────────────────────────────────────────────────

fn read_back(app: &mut App) -> WorkspaceDocument {
    app.world_mut().run_system_once(|reader: CircuitReader| reader.workspace()).unwrap()
}

#[test]
fn save_and_load_round_trip() {
    let dir = temp_dir();
    let mut app = app();
    load(&mut app, fixtures::build_8bit_counter_subcircuit());
    let saved = read_back(&mut app);
    save_workspace_in(&dir, "counter", &saved).unwrap();
    assert_eq!(list_workspaces_in(&dir).unwrap(), ["counter"]);

    let loaded = read_workspace_in(&dir, "counter").unwrap();
    assert_eq!(loaded, saved);
    assert_eq!(loaded.definitions.len(), 1, "the T flip-flop is stored once");

    // Load into a fresh app; it simulates the same.
    let mut app = self::app();
    load(&mut app, loaded);
    assert!(wires(&mut app).iter().all(|(_, _, s)| s.connected));
    set_input(&mut app, "Increment", 1);
    run(&mut app, 4);
    set_input(&mut app, "Increment", 0);
    run(&mut app, 4);
    let count = gate(&mut app, "Count");
    assert_eq!(input_value(&app, count, 0), 1);

    // An editor-made circuit also round-trips, port labels included.
    let mut app = self::app();
    let a = create_gate(&mut app, GateKind::Input { w: 3 }, "A");
    let y = create_gate(&mut app, GateKind::Output { w: 3 }, "Y");
    create_wire(&mut app, a, 0, y, 0);
    let saved = read_back(&mut app);
    save_workspace_in(&dir, "small/one", &saved).unwrap();
    assert_eq!(read_workspace_in(&dir, "small/one").unwrap(), saved);
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn old_format_workspaces_are_refused_with_a_message() {
    let dir = temp_dir();
    // A version 4 bundle (the pre-v2 format).
    std::fs::create_dir_all(dir.join("old")).unwrap();
    std::fs::write(
        dir.join("old/manifest.json"),
        r#"{"version":4,"root_circuit_id":"00000000-0000-0000-0000-000000000001","circuit_ids":[]}"#,
    )
    .unwrap();
    // A legacy single-file save.
    std::fs::write(dir.join("legacy.json"), r#"{"version":1,"circuit":{}}"#).unwrap();

    for (name, version) in [("old", 4), ("legacy", 1)] {
        let err = read_workspace_in(&dir, name).unwrap_err();
        assert!(
            matches!(err, WorkspaceError::UnsupportedVersion { found: Some(v), .. } if v == version),
            "{err:?}"
        );
        let message = err.to_string();
        assert!(message.contains(&format!("version {version}")), "{message}");
        assert!(message.contains("cannot open"), "{message}");
        assert!(message.contains("not converted"), "{message}");
    }
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn unlabeled_ports_are_not_saved() {
    let dir = temp_dir();
    let mut doc = CircuitDocument::new("bad");
    doc.add(DocNode::new(GateKind::Input { w: 1 }, Vec2::ZERO));
    let err = save_workspace_in(&dir, "bad", &WorkspaceDocument::new(doc)).unwrap_err();
    assert!(matches!(err, WorkspaceError::Invalid(_)));
    std::fs::remove_dir_all(dir).ok();
}

// ─────────────────────────────────────────────────────────────────────────────
// Misc
// ─────────────────────────────────────────────────────────────────────────────

/// Regression for issue #19: running the bridge systems every frame must not
/// mark `SimulationConfig` changed (which would resend the tick rate).
#[test]
fn bridge_systems_do_not_mark_simulation_config_changed_every_frame() {
    #[derive(Resource, Default)]
    struct ConfigChanges(u32);

    fn count_changes(config: Res<SimulationConfig>, mut changes: ResMut<ConfigChanges>) {
        if config.is_changed() {
            changes.0 += 1;
        }
    }

    let mut app = app();
    app.init_resource::<ConfigChanges>().add_systems(Last, count_changes);
    load(&mut app, nested_fanout_fixture());
    for _ in 0..20 {
        app.update();
    }
    assert_eq!(app.world().resource::<ConfigChanges>().0, 1, "only the initial insert");
}

#[test]
fn tick_rate_uses_si_units() {
    assert_eq!(super::systems::format_si_hz(999.0), "999.0 Hz");
    assert_eq!(super::systems::format_si_hz(12_345.0), "12.3 kHz");
    assert_eq!(super::systems::format_si_hz(12_345_678.0), "12.3 MHz");
    assert_eq!(super::systems::format_si_hz(1_500_000_000.0), "1.5 GHz");
}
