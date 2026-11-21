//! Integration tests for the ECS → `Design` → v2 engine path, run in a
//! headless `App` with [`SimulationRuntimePlugin`].

mod demos;
mod gallery;
pub mod geometry_tests;
mod instance_views;
mod rtl;
mod rtl_cells;
mod visuals;

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use bevy::ecs::system::RunSystemOnce;
use bevy::prelude::*;
use uuid::Uuid;

use crate::editor::workspace::{
    WorkspaceError, list_workspaces_in, read_workspace_in, save_workspace_in,
};
use crate::sim::demos::DemoCatalog;
use crate::sim::{
    CircuitDocument, CircuitReader, CreateGate, CreateWire, DeleteGate, DeleteWire, DocNode,
    GateIdentity, GateKind, GateLabel, GateType, GateValues, LoadWorkspaceEvent, SetGateKind,
    SetInputValue, SimBridge, SimDiagnostics, SimRuntime, SimulationConfig,
    SimulationRuntimePlugin, WireEndpoints, WireState, WorkspaceDocument,
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

/// Load `workspace` (editable).
fn load(app: &mut App, workspace: WorkspaceDocument) {
    app.world_mut().write_message(LoadWorkspaceEvent {
        name: workspace.root.name.clone(),
        workspace: Arc::new(workspace),
        read_only: false,
    });
    settle(app);
}

/// The built-in demos, from the embedded files.
fn demos() -> &'static DemoCatalog {
    static CATALOG: OnceLock<DemoCatalog> = OnceLock::new();
    CATALOG.get_or_init(|| DemoCatalog::embedded().expect("the demos load"))
}

/// The expanded workspace of demo `id`.
fn demo(id: &str) -> WorkspaceDocument {
    let demo = demos().get(id).unwrap_or_else(|| panic!("no demo {id}"));
    (*demo.workspace).clone()
}

/// The value on input pin 0 of the gate labelled `label` (an output port
/// or display).
fn value_at(app: &mut App, label: &str) -> u64 {
    let e = gate(app, label);
    input_value(app, e, 0)
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

/// Set inputs, run one tick, and read outputs.
fn eval(app: &mut App, inputs: &[(&str, u64)], outputs: &[&str]) -> Vec<u64> {
    for &(label, value) in inputs {
        set_input(app, label, value);
    }
    run(app, 1);
    outputs.iter().map(|label| value_at(app, label)).collect()
}

fn gate(app: &mut App, label: &str) -> Entity {
    let mut q = app.world_mut().query::<(Entity, &GateLabel)>();
    q.iter(app.world())
        .find(|(_, l)| l.value == label)
        .map(|(e, _)| e)
        .unwrap_or_else(|| panic!("no gate labelled {label}"))
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
    let mut q = app
        .world_mut()
        .query::<(Entity, &WireEndpoints, &WireState)>();
    q.iter(app.world()).map(|(e, ep, s)| (e, *ep, *s)).collect()
}

fn set_input(app: &mut App, label: &str, value: u64) {
    let entity = gate(app, label);
    send(app, SetInputValue { entity, value });
}

fn create_gate(app: &mut App, kind: GateKind, label: &str) -> Entity {
    send(
        app,
        CreateGate {
            kind,
            position: Vec2::ZERO,
            label: Some(label.into()),
        },
    );
    gate(app, label)
}

fn create_wire(app: &mut App, from: Entity, from_pin: u8, to: Entity, to_pin: u8) {
    let (source_gate, sink_gate) = (uuid(app, from), uuid(app, to));
    send(
        app,
        CreateWire {
            source_gate,
            source_pin: from_pin,
            sink_gate,
            sink_pin: to_pin,
        },
    );
}

fn root_node_count(app: &App) -> usize {
    app.world()
        .resource::<SimBridge>()
        .design()
        .root_def()
        .nodes
        .len()
}

fn temp_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("gatequest-test-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
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
    assert!(
        wires(&mut app)
            .iter()
            .all(|(_, _, s)| s.connected && s.width == 1)
    );

    run(&mut app, 1);
    assert_eq!(input_value(&app, y, 0), 0);

    set_input(&mut app, "A", 1);
    run(&mut app, 1);
    assert_eq!(output_value(&app, a, 0), 1);
    assert_eq!(input_value(&app, y, 0), 1);
    assert!(
        wires(&mut app)
            .iter()
            .all(|(_, _, s)| s.value == 1 || s.value == 0)
    );
    let out_wire = wires(&mut app)
        .into_iter()
        .find(|(_, ep, _)| ep.sink_gate == uuid(&app, y))
        .unwrap();
    assert_eq!(
        out_wire.2.value, 1,
        "wire value comes from the engine probe"
    );

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
    assert!(
        app.world()
            .resource::<crate::sim::AuthoringFeedback>()
            .message
            .is_some()
    );
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
    let (wire, ..) = wires(&mut app)
        .into_iter()
        .find(|(_, ep, _)| ep.sink_gate == uuid(&app, y))
        .unwrap();
    send(&mut app, DeleteWire { entity: wire });
    run(&mut app, 1);
    assert_eq!(input_value(&app, y, 0), 0);
    let y_node = app.world().resource::<SimBridge>().node_of(y).unwrap();
    assert_eq!(
        app.world()
            .resource::<SimBridge>()
            .design()
            .root_def()
            .input_net(y_node, 0),
        None
    );

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
    let hi = create_gate(
        &mut app,
        GateKind::Split {
            w: 8,
            lo: 4,
            len: 4,
        },
        "hi",
    );
    let lo = create_gate(
        &mut app,
        GateKind::Split {
            w: 8,
            lo: 0,
            len: 4,
        },
        "lo",
    );
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
    assert!(
        wires(&mut app)
            .iter()
            .any(|(_, _, s)| s.width == 8 && s.value == 0xA5)
    );
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

    send(
        &mut app,
        SetGateKind {
            entity: y,
            kind: GateKind::Output { w: 8 },
        },
    );
    assert!(!wires(&mut app)[0].2.connected);
    send(
        &mut app,
        SetGateKind {
            entity: a,
            kind: GateKind::Input { w: 8 },
        },
    );
    assert!(wires(&mut app)[0].2.connected, "widths match again");
    run(&mut app, 1);
    assert_eq!(
        input_value(&app, y, 0),
        9,
        "the input keeps its driven value"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// State across edits
// ─────────────────────────────────────────────────────────────────────────────

/// The counter demo, loaded editable: edits elsewhere keep the state of
/// both its gate-built counter (flip-flops with Delays inside nested
/// definitions) and its word-level `Register` counter.
#[test]
fn register_state_survives_edits() {
    let mut app = app();
    load(&mut app, demo("counter"));
    set_input(&mut app, "Enable", 1);
    run(&mut app, 40);
    let before = value_at(&mut app, "Count");
    assert!(before >= 9, "counter ran: {before}");
    assert_eq!(value_at(&mut app, "Count (Register)"), before);

    // Edits: add a gate, wire it, delete it again.
    let extra = create_gate(&mut app, GateKind::Not { w: 1 }, "extra");
    let enable = gate(&mut app, "Enable");
    create_wire(&mut app, enable, 0, extra, 0);
    assert_eq!(
        value_at(&mut app, "Count"),
        before,
        "edit did not reset the flip-flops"
    );
    assert_eq!(
        value_at(&mut app, "Count (Register)"),
        before,
        "edit did not reset the register"
    );
    send(&mut app, DeleteGate { entity: extra });
    assert_eq!(value_at(&mut app, "Count"), before);

    // One clock period is one count.
    run(&mut app, 8);
    assert_eq!(value_at(&mut app, "Count"), before + 2);
    assert_eq!(value_at(&mut app, "Count (Register)"), before + 2);
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

/// A dual-port RAM with initial contents: written at `WA`, read at `RA`
/// and `RB`.
fn ram2_circuit(app: &mut App) -> (Entity, Entity, Entity) {
    let wa = create_gate(app, GateKind::Input { w: 3 }, "WA");
    let we = create_gate(app, GateKind::Input { w: 1 }, "WE");
    let data = create_gate(app, GateKind::Input { w: 8 }, "Data");
    let ra = create_gate(app, GateKind::Input { w: 3 }, "RA");
    let rb = create_gate(app, GateKind::Input { w: 3 }, "RB");
    let kind = GateKind::Ram2 {
        addr_bits: 3,
        w: 8,
        reads: 2,
        mask: false,
        init: vec![0x11, 0x22, 0x33],
    };
    let ram = create_gate(app, kind, "ram");
    let qa = create_gate(app, GateKind::Output { w: 8 }, "QA");
    let qb = create_gate(app, GateKind::Output { w: 8 }, "QB");
    for (k, src) in [wa, we, data, ra, rb].into_iter().enumerate() {
        create_wire(app, src, 0, ram, k as u8);
    }
    create_wire(app, ram, 0, qa, 0);
    create_wire(app, ram, 1, qb, 0);
    (ram, qa, qb)
}

#[test]
fn ram2_reads_two_ports_and_keeps_its_contents_across_edits() {
    let mut app = app();
    let (_, qa, qb) = ram2_circuit(&mut app);
    set_input(&mut app, "RA", 1);
    set_input(&mut app, "RB", 2);
    run(&mut app, 1);
    assert_eq!(
        (input_value(&app, qa, 0), input_value(&app, qb, 0)),
        (0x22, 0x33)
    );

    // Write 5 while reading 1 and 5 elsewhere.
    set_input(&mut app, "WA", 5);
    set_input(&mut app, "Data", 0xA5);
    set_input(&mut app, "WE", 1);
    set_input(&mut app, "RB", 5);
    run(&mut app, 2);
    set_input(&mut app, "WE", 0);
    run(&mut app, 1);
    assert_eq!(
        (input_value(&app, qa, 0), input_value(&app, qb, 0)),
        (0x22, 0xA5)
    );

    // An unrelated edit keeps the contents.
    let extra = create_gate(&mut app, GateKind::Not { w: 8 }, "extra");
    let data = gate(&mut app, "Data");
    create_wire(&mut app, data, 0, extra, 0);
    run(&mut app, 1);
    assert_eq!(input_value(&app, qb, 0), 0xA5);
    set_input(&mut app, "RA", 0);
    run(&mut app, 1);
    assert_eq!(input_value(&app, qa, 0), 0x11);
}

#[test]
fn ram2_round_trips_through_version_5_and_version_6() {
    let mut app = app();
    ram2_circuit(&mut app);
    let saved = read_back(&mut app);
    let ram = saved
        .root
        .nodes
        .iter()
        .find(|n| matches!(n.kind, GateKind::Ram2 { .. }))
        .unwrap();
    assert_eq!(ram.kind.display_name(), "DUAL-PORT RAM");

    // Version 5.
    let dir = temp_dir();
    save_workspace_in(&dir, "ram2", &saved).unwrap();
    let text = std::fs::read_to_string(dir.join("ram2/manifest.json")).unwrap();
    assert!(text.contains("\"type\": \"Ram2\""), "{text}");
    assert_eq!(read_workspace_in(&dir, "ram2").unwrap(), saved);
    std::fs::remove_dir_all(dir).ok();

    // Version 6: named pins, hex initial contents, optional parameters.
    let text = r#"{"format": "gatequest-workspace", "version": 6, "root": "c", "circuits": [
        {"name": "c", "nodes": [
            {"name": "a", "type": "Input", "w": 4, "at": [0, 0]},
            {"name": "d", "type": "Input", "w": 16, "at": [0, 60]},
            {"name": "m", "type": "Ram2", "addr_bits": 4, "w": 16, "reads": 3,
             "mask": true, "init": ["0xBEEF", 7], "at": [200, 0]},
            {"name": "k", "type": "DualPortRam", "addr_bits": 4, "w": 16, "reads": 1,
             "at": [200, 200]},
            {"name": "q", "type": "Output", "w": 16, "at": [400, 0]}],
          "wires": [["a", "m.waddr", "m.raddr2", "k.raddr0"], ["d", "m.we", "m.wdata"],
                    ["m.rdata2", "q"]]}]}"#;
    let file = crate::sim::source::parse(text).unwrap();
    let expanded =
        crate::sim::source::expand(&file, "test", &crate::sim::source::Externals::default())
            .unwrap();
    let kinds: Vec<&GateKind> = expanded
        .workspace
        .root
        .nodes
        .iter()
        .map(|n| &n.kind)
        .collect();
    assert!(kinds.contains(&&GateKind::Ram2 {
        addr_bits: 4,
        w: 16,
        reads: 3,
        mask: true,
        init: vec![0xBEEF, 7],
    }));
    assert!(kinds.contains(&&GateKind::Ram2 {
        addr_bits: 4,
        w: 16,
        reads: 1,
        mask: false,
        init: vec![],
    }));
    assert_eq!(expanded.workspace.root.wires.len(), 6);
    let canonical = crate::sim::source::to_text(&file);
    assert_eq!(crate::sim::source::parse(&canonical).unwrap(), file);

    // Simulated: the masked write reaches only the bits set in `we`.
    let mut app = self::app();
    load(&mut app, expanded.workspace);
    assert!(wires(&mut app).iter().all(|(_, _, s)| s.connected));
    run(&mut app, 1);
    assert_eq!(value_at(&mut app, "q"), 0xBEEF);
    set_input(&mut app, "d", 0x00F0);
    run(&mut app, 2);
    assert_eq!(value_at(&mut app, "q"), 0xBEFF);

    // Bad parameters are refused with the kind's own message.
    let bad = text.replace("\"reads\": 3", "\"reads\": 9");
    let err = crate::sim::source::expand(
        &crate::sim::source::parse(&bad).unwrap(),
        "test",
        &crate::sim::source::Externals::default(),
    )
    .unwrap_err();
    assert!(
        err.to_string().contains("read ports must be 1..=8"),
        "{err}"
    );
}

#[test]
fn combinational_loop_is_reported_and_previous_program_keeps_running() {
    let mut app = app();
    let a = create_gate(&mut app, GateKind::Input { w: 1 }, "A");
    let y = create_gate(&mut app, GateKind::Output { w: 1 }, "Y");
    create_wire(&mut app, a, 0, y, 0);
    run(&mut app, 1);
    assert!(
        app.world()
            .resource::<SimDiagnostics>()
            .compile_error
            .is_none()
    );

    // A NOT gate feeding itself is a combinational loop.
    let not = create_gate(&mut app, GateKind::Not { w: 1 }, "loop");
    create_wire(&mut app, not, 0, not, 0);
    let error = app
        .world()
        .resource::<SimDiagnostics>()
        .compile_error
        .clone();
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
    assert!(
        app.world()
            .resource::<SimDiagnostics>()
            .compile_error
            .is_none()
    );
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
    let wl = wrapper.add(DocNode::new(
        GateKind::SubCircuit {
            definition: leaf_id,
        },
        Vec2::ZERO,
    ));
    let wo = wrapper.add(DocNode::labeled(GateKind::Output { w: 1 }, Vec2::ZERO, "Y"));
    wrapper.connect(wi, 0, wl, 0);
    wrapper.connect(wl, 0, wo, 0);
    let wrapper_id = ws.define(wrapper);

    let root = &mut ws.root;
    let input = root.add(DocNode::labeled(GateKind::Input { w: 1 }, Vec2::ZERO, "A"));
    let nested = root.add(DocNode::labeled(
        GateKind::SubCircuit {
            definition: wrapper_id,
        },
        Vec2::ZERO,
        "nested",
    ));
    let y1 = root.add(DocNode::labeled(
        GateKind::Output { w: 1 },
        Vec2::ZERO,
        "Y1",
    ));
    let y2 = root.add(DocNode::labeled(
        GateKind::Output { w: 1 },
        Vec2::ZERO,
        "Y2",
    ));
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
        app.world_mut()
            .resource_mut::<SimBridge>()
            .wait_until_current(Duration::from_secs(5));
        app.update();
        if input_value(&app, y1, 0) == 1 && input_value(&app, y2, 0) == 1 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "nested outputs never reached the ECS"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn counter_definition_has_word_interface() {
    let counter = demos().get("counter").unwrap();
    let def = &counter
        .circuits
        .iter()
        .find(|(name, _)| name == "counter8")
        .unwrap()
        .1;
    let (inputs, outputs) = def.interface();
    let names = |v: &[&DocNode]| {
        v.iter()
            .map(|n| n.label.clone().unwrap())
            .collect::<Vec<_>>()
    };
    assert_eq!(names(&inputs), ["Clk", "Enable", "Reset"]);
    assert_eq!(names(&outputs), ["Q"]);
    assert_eq!(outputs[0].kind, GateKind::Output { w: 8 });
}

/// The gate-built counter (flip-flops with a Delay in each latch loop)
/// compiles, starts at 0 without a reset pulse, counts once per clock
/// period in step with the word-level counter beside it, and resets.
#[test]
fn counter_demo_compiles_starts_at_zero_and_counts() {
    let mut app = app();
    load(&mut app, demo("counter"));
    assert!(
        wires(&mut app).iter().all(|(_, _, s)| s.connected),
        "every wire simulated"
    );
    assert!(
        app.world()
            .resource::<SimDiagnostics>()
            .compile_error
            .is_none(),
        "compiles"
    );
    assert_eq!(value_at(&mut app, "Count"), 0, "powers up at 0");
    set_input(&mut app, "Enable", 1);
    let mut previous = 0;
    for _ in 0..40 {
        run(&mut app, 1);
        let v = value_at(&mut app, "Count");
        assert!(v == previous || v == previous + 1, "{previous} -> {v}");
        assert_eq!(value_at(&mut app, "Count (Register)"), v, "same count");
        previous = v;
    }
    assert!((9..=10).contains(&previous), "counted to {previous}");

    // Holding Reset clears it.
    set_input(&mut app, "Reset", 1);
    run(&mut app, 8);
    assert_eq!(value_at(&mut app, "Count"), 0, "resets");
    assert_eq!(value_at(&mut app, "Count (Register)"), 0);
    set_input(&mut app, "Reset", 0);
    run(&mut app, 8);
    let again = value_at(&mut app, "Count");
    assert!((1..=2).contains(&again), "counts again: {again}");
    assert_eq!(value_at(&mut app, "Count (Register)"), again);
}

#[test]
fn every_demo_loads_and_compiles() {
    for demo in &demos().demos {
        let name = &demo.id;
        let mut app = app();
        load(&mut app, (*demo.workspace).clone());
        assert!(
            app.world()
                .resource::<SimDiagnostics>()
                .compile_error
                .is_none(),
            "{name}"
        );
        assert!(
            app.world()
                .resource::<SimDiagnostics>()
                .running_revision
                .is_some(),
            "{name}"
        );
        assert!(
            wires(&mut app).iter().all(|(_, _, s)| s.connected),
            "{name}: wires"
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Save / load
// ─────────────────────────────────────────────────────────────────────────────

fn read_back(app: &mut App) -> WorkspaceDocument {
    app.world_mut()
        .run_system_once(|reader: CircuitReader| reader.workspace())
        .unwrap()
}

#[test]
fn save_and_load_round_trip() {
    let dir = temp_dir();
    let mut app = app();
    load(&mut app, demo("register"));
    let saved = read_back(&mut app);
    save_workspace_in(&dir, "register", &saved).unwrap();
    assert_eq!(list_workspaces_in(&dir).unwrap(), ["register"]);

    let loaded = read_workspace_in(&dir, "register").unwrap();
    assert_eq!(loaded, saved);
    // register8, mux2, dff, d_latch, sr_latch, and the not/and/or/nor
    // gates: each stored once, however many instances there are.
    assert_eq!(
        loaded.definitions.len(),
        9,
        "each definition is stored once"
    );

    // Load into a fresh app; it simulates the same.
    let mut app = self::app();
    load(&mut app, loaded);
    assert!(wires(&mut app).iter().all(|(_, _, s)| s.connected));
    set_input(&mut app, "D", 0x5A);
    set_input(&mut app, "Load", 1);
    run(&mut app, 3);
    set_input(&mut app, "Clk", 1);
    run(&mut app, 3);
    assert_eq!(value_at(&mut app, "Q (gates)"), 0x5A);

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
    // A manifest with no version, or a version from the future.
    for (name, text) in [
        (
            "none",
            r#"{"format":"gatequest-workspace","root":"x","circuits":[]}"#,
        ),
        (
            "seven",
            r#"{"format":"gatequest-workspace","version":7,"root":"x","circuits":[]}"#,
        ),
    ] {
        std::fs::create_dir_all(dir.join(name)).unwrap();
        std::fs::write(dir.join(name).join("manifest.json"), text).unwrap();
        let err = read_workspace_in(&dir, name).unwrap_err();
        assert!(
            matches!(err, WorkspaceError::UnsupportedVersion { .. }),
            "{err:?}"
        );
        assert!(err.to_string().contains("versions 5 and 6"), "{err}");
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
    app.init_resource::<ConfigChanges>()
        .add_systems(Last, count_changes);
    load(&mut app, nested_fanout_fixture());
    for _ in 0..20 {
        app.update();
    }
    assert_eq!(
        app.world().resource::<ConfigChanges>().0,
        1,
        "only the initial insert"
    );
}

#[test]
fn tick_rate_uses_si_units() {
    assert_eq!(super::systems::format_si_hz(999.0), "999.0 Hz");
    assert_eq!(super::systems::format_si_hz(12_345.0), "12.3 kHz");
    assert_eq!(super::systems::format_si_hz(12_345_678.0), "12.3 MHz");
    assert_eq!(super::systems::format_si_hz(1_500_000_000.0), "1.5 GHz");
}
