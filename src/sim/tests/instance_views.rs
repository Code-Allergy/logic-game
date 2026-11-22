//! Subcircuit definitions and live instance views (issue #29): a definition
//! placed twice, simulated, opened and inspected live through its instance
//! path, edited (every instance changes, state carries over), saved and
//! reloaded; recursion refused; nested views.

use std::collections::HashMap;

use bevy::ecs::entity_disabling::Disabled;
use logi_sim::netlist::NodeKind;

use super::*;
use crate::sim::{
    AuthoringFeedback, CircuitLibrary, CircuitView, CloseView, InstanceViewItem,
    OpenDefinitionView, OpenInstanceView, RegisterDefinitions,
};

/// A cell with state: `Sum = A ^ B`, `Carry = A & B`, and an 8-bit counter
/// `Count` that adds the constant `one` every tick while `En` is high.
///
/// Pins: inputs `A`, `B`, `En` (0, 1, 2); outputs `Carry`, `Count`, `Sum`
/// (0, 1, 2).
fn cell_definition() -> CircuitDocument {
    let mut d = CircuitDocument::new("Cell");
    let at = |x: f32, y: f32| Vec2::new(x, y);
    let a = d.add(DocNode::labeled(
        GateKind::Input { w: 1 },
        at(-200.0, 50.0),
        "A",
    ));
    let b = d.add(DocNode::labeled(
        GateKind::Input { w: 1 },
        at(-200.0, -50.0),
        "B",
    ));
    let en = d.add(DocNode::labeled(
        GateKind::Input { w: 1 },
        at(-200.0, -150.0),
        "En",
    ));
    let xor = d.add(DocNode::labeled(
        GateKind::Xor { w: 1 },
        at(0.0, 50.0),
        "xor",
    ));
    let and = d.add(DocNode::labeled(
        GateKind::And { w: 1 },
        at(0.0, -50.0),
        "and",
    ));
    let reg = d.add(DocNode::labeled(
        GateKind::Register { w: 8 },
        at(0.0, -150.0),
        "reg",
    ));
    let add = d.add(DocNode::labeled(
        GateKind::Add { w: 8 },
        at(-100.0, -200.0),
        "add",
    ));
    let one = d.add(DocNode::labeled(
        GateKind::Const { w: 8, value: 1 },
        at(-200.0, -250.0),
        "one",
    ));
    let sum = d.add(DocNode::labeled(
        GateKind::Output { w: 1 },
        at(200.0, 50.0),
        "Sum",
    ));
    let carry = d.add(DocNode::labeled(
        GateKind::Output { w: 1 },
        at(200.0, -50.0),
        "Carry",
    ));
    let count = d.add(DocNode::labeled(
        GateKind::Output { w: 8 },
        at(200.0, -150.0),
        "Count",
    ));
    d.connect(a, 0, xor, 0);
    d.connect(b, 0, xor, 1);
    d.connect(a, 0, and, 0);
    d.connect(b, 0, and, 1);
    d.connect(xor, 0, sum, 0);
    d.connect(and, 0, carry, 0);
    d.connect(en, 0, reg, 0);
    d.connect(add, 0, reg, 1);
    d.connect(reg, 0, add, 0);
    d.connect(one, 0, add, 1);
    d.connect(reg, 0, count, 0);
    d
}

const CARRY: u8 = 0;
const COUNT: u8 = 1;
const SUM: u8 = 2;

/// Register `defs` and place two instances of `cell`, each with its own
/// inputs `A{i}`, `B{i}`, `En{i}` and outputs `S{i}`, `C{i}`, `N{i}`, the
/// way the editor does (register, then `CreateGate`/`CreateWire`).
fn place_two_cells(
    app: &mut App,
    cell: Uuid,
    defs: HashMap<Uuid, Arc<CircuitDocument>>,
) -> [Entity; 2] {
    send(app, RegisterDefinitions { definitions: defs });
    let mut cells = [Entity::PLACEHOLDER; 2];
    for i in 1..=2 {
        let c = create_gate(
            app,
            GateKind::SubCircuit { definition: cell },
            &format!("cell{i}"),
        );
        let a = create_gate(app, GateKind::Input { w: 1 }, &format!("A{i}"));
        let b = create_gate(app, GateKind::Input { w: 1 }, &format!("B{i}"));
        let en = create_gate(app, GateKind::Input { w: 1 }, &format!("En{i}"));
        let s = create_gate(app, GateKind::Output { w: 1 }, &format!("S{i}"));
        let k = create_gate(app, GateKind::Output { w: 1 }, &format!("C{i}"));
        let n = create_gate(app, GateKind::Output { w: 8 }, &format!("N{i}"));
        create_wire(app, a, 0, c, 0);
        create_wire(app, b, 0, c, 1);
        create_wire(app, en, 0, c, 2);
        create_wire(app, c, SUM, s, 0);
        create_wire(app, c, CARRY, k, 0);
        create_wire(app, c, COUNT, n, 0);
        cells[i - 1] = c;
    }
    cells
}

fn one_def(def: CircuitDocument) -> HashMap<Uuid, Arc<CircuitDocument>> {
    HashMap::from([(def.id, Arc::new(def))])
}

fn out(app: &mut App, label: &str) -> u64 {
    let e = gate(app, label);
    input_value(app, e, 0)
}

/// Output pin 0 of a gate of the open view.
fn view_out(app: &mut App, label: &str) -> u64 {
    let e = gate(app, label);
    assert!(
        app.world().get::<InstanceViewItem>(e).is_some(),
        "{label} is a view item"
    );
    output_value(app, e, 0)
}

fn view(app: &App) -> CircuitView {
    app.world().resource::<CircuitView>().clone()
}

fn feedback(app: &App) -> String {
    app.world()
        .resource::<AuthoringFeedback>()
        .message
        .clone()
        .unwrap_or_default()
}

fn count_entities<F: bevy::ecs::query::QueryFilter>(app: &mut App) -> usize {
    let mut q = app.world_mut().query_filtered::<Entity, F>();
    q.iter(app.world()).count()
}

fn open(app: &mut App, entity: Entity) {
    send(app, OpenInstanceView { entity });
}

fn close(app: &mut App) {
    send(app, CloseView { to_root: false });
}

#[test]
fn definition_placed_twice_is_simulated_inspected_live_edited_saved_and_reloaded() {
    let cell = cell_definition();
    let cell_id = cell.id;
    let mut app = app();
    let [c1, c2] = place_two_cells(&mut app, cell_id, one_def(cell));
    assert!(
        wires(&mut app).iter().all(|(_, _, s)| s.connected),
        "every wire simulated"
    );
    assert!(
        app.world()
            .resource::<SimDiagnostics>()
            .compile_error
            .is_none()
    );

    // One definition in the design, instanced twice.
    {
        let bridge = app.world().resource::<SimBridge>();
        let design = bridge.design();
        let (n1, n2) = (bridge.node_of(c1).unwrap(), bridge.node_of(c2).unwrap());
        let (NodeKind::Instance(d1), NodeKind::Instance(d2)) = (
            &design.root_def().nodes[n1].kind,
            &design.root_def().nodes[n2].kind,
        ) else {
            panic!("cells are instances");
        };
        assert_eq!(d1, d2, "both instances share one definition");
    }

    set_input(&mut app, "A1", 1);
    set_input(&mut app, "En1", 1);
    set_input(&mut app, "A2", 1);
    set_input(&mut app, "B2", 1);
    run(&mut app, 10);
    assert_eq!((out(&mut app, "S1"), out(&mut app, "C1")), (1, 0));
    assert_eq!((out(&mut app, "S2"), out(&mut app, "C2")), (0, 1));
    let n1 = out(&mut app, "N1");
    assert!(n1 >= 8, "instance 1 counts: {n1}");
    assert_eq!(out(&mut app, "N2"), 0, "instance 2 is not enabled");
    let revision = app.world().resource::<SimBridge>().revision();

    // Open instance 1: its gates show the running root simulation's values.
    open(&mut app, c1);
    assert_eq!(view(&app).instance_path(), Some(vec![uuid(&app, c1)]));
    {
        let bridge = app.world().resource::<SimBridge>();
        assert_eq!(
            bridge.view_node_path(),
            Some(&[bridge.node_of(c1).unwrap()][..])
        );
        assert_eq!(
            bridge.revision(),
            revision,
            "opening a view does not recompile"
        );
    }
    assert_eq!(
        count_entities::<(With<GateIdentity>, Without<InstanceViewItem>)>(&mut app),
        0,
        "root parked"
    );
    assert_eq!(view_out(&mut app, "A"), 1);
    assert_eq!(view_out(&mut app, "B"), 0);
    assert_eq!(view_out(&mut app, "xor"), 1);
    assert_eq!(view_out(&mut app, "and"), 0);
    assert_eq!(view_out(&mut app, "reg"), n1, "the register of instance 1");
    let sum = gate(&mut app, "Sum");
    assert_eq!(
        input_value(&app, sum, 0),
        1,
        "an output port shows its input"
    );
    let sum_wire = wires(&mut app)
        .into_iter()
        .find(|(_, ep, _)| ep.sink_gate == uuid(&app, sum))
        .unwrap();
    assert!(sum_wire.2.connected);
    assert_eq!(sum_wire.2.value, 1, "wires show live values too");

    // Live: the view follows the running simulation.
    run(&mut app, 3);
    assert_eq!(view_out(&mut app, "reg"), n1 + 3);

    // Inputs inside an instance are driven by the parent.
    let a = gate(&mut app, "A");
    send(
        &mut app,
        SetInputValue {
            entity: a,
            value: 0,
        },
    );
    assert!(
        feedback(&app).contains("driven by the parent"),
        "{}",
        feedback(&app)
    );
    run(&mut app, 1);
    assert_eq!(view_out(&mut app, "A"), 1);

    // Instance 2 has different inputs, so different live values.
    close(&mut app);
    assert!(view(&app).is_root());
    assert_eq!(
        app.world().resource::<SimBridge>().revision(),
        revision,
        "no recompile"
    );
    let n1 = out(&mut app, "N1");
    open(&mut app, c2);
    assert_eq!(view(&app).instance_path(), Some(vec![uuid(&app, c2)]));
    assert_eq!(view_out(&mut app, "A"), 1);
    assert_eq!(view_out(&mut app, "B"), 1);
    assert_eq!(view_out(&mut app, "xor"), 0);
    assert_eq!(view_out(&mut app, "and"), 1);
    assert_eq!(view_out(&mut app, "reg"), 0);

    // Edit the definition in instance 2's view: count by 2, and add an
    // output port "Diff" that sorts between "Count" and "Sum", moving the
    // Sum pin from 2 to 3.
    let one = gate(&mut app, "one");
    send(
        &mut app,
        SetGateKind {
            entity: one,
            kind: GateKind::Const { w: 8, value: 2 },
        },
    );
    let xor = gate(&mut app, "xor");
    let diff = create_gate(&mut app, GateKind::Output { w: 1 }, "Diff");
    create_wire(&mut app, xor, 0, diff, 0);
    settle(&mut app);
    let library = app.world().resource::<CircuitLibrary>();
    let def = library.get(cell_id).expect("same id after the edit");
    assert_eq!(
        def.interface()
            .1
            .iter()
            .map(|n| n.label.clone().unwrap())
            .collect::<Vec<_>>(),
        ["Carry", "Count", "Diff", "Sum"]
    );
    assert!(
        app.world()
            .resource::<SimDiagnostics>()
            .compile_error
            .is_none()
    );
    assert_eq!(view_out(&mut app, "reg"), 0, "still instance 2's state");

    // Back in the root: both instances changed, state survived, wires to
    // the moved port followed it.
    close(&mut app);
    assert_eq!(
        out(&mut app, "N1"),
        n1,
        "instance 1's register kept its value across the edit"
    );
    run(&mut app, 3);
    assert_eq!(
        out(&mut app, "N1"),
        (n1 + 6) & 0xFF,
        "instance 1 now counts by 2"
    );
    assert_eq!(
        (out(&mut app, "S1"), out(&mut app, "C1")),
        (1, 0),
        "Sum still reaches S1"
    );
    assert_eq!((out(&mut app, "S2"), out(&mut app, "C2")), (0, 1));
    let s1 = gate(&mut app, "S1");
    let s1_wire = wires(&mut app)
        .into_iter()
        .find(|(_, ep, _)| ep.sink_gate == uuid(&app, s1))
        .unwrap();
    assert_eq!(s1_wire.1.source_pin, 3, "renumbered to the Sum pin");
    assert!(wires(&mut app).iter().all(|(_, _, s)| s.connected));
    set_input(&mut app, "En2", 1);
    run(&mut app, 1);
    let n2 = out(&mut app, "N2");
    run(&mut app, 2);
    assert_eq!(
        out(&mut app, "N2"),
        n2 + 4,
        "instance 2 counts by 2 as well"
    );

    // Save and reload: the definition is stored once, by the same id.
    let dir = temp_dir();
    let saved = read_back(&mut app);
    assert_eq!(
        saved.definitions.keys().copied().collect::<Vec<_>>(),
        [cell_id]
    );
    save_workspace_in(&dir, "two cells", &saved).unwrap();
    let manifest = std::fs::read_to_string(dir.join("two cells/manifest.json")).unwrap();
    assert_eq!(
        manifest.matches("\"name\": \"Cell\"").count(),
        1,
        "definition table stored once"
    );
    let loaded = read_workspace_in(&dir, "two cells").unwrap();
    assert_eq!(loaded, saved);
    std::fs::remove_dir_all(dir).ok();

    let mut app = self::app();
    load(&mut app, loaded);
    assert!(wires(&mut app).iter().all(|(_, _, s)| s.connected));
    set_input(&mut app, "A1", 1);
    set_input(&mut app, "En1", 1);
    set_input(&mut app, "B2", 1);
    run(&mut app, 4);
    assert_eq!((out(&mut app, "S1"), out(&mut app, "C1")), (1, 0));
    assert_eq!((out(&mut app, "S2"), out(&mut app, "C2")), (1, 0));
    let n1 = out(&mut app, "N1");
    assert!(n1 >= 6 && n1 % 2 == 0, "reloaded with the edit: {n1}");
    let (c1, c2) = (gate(&mut app, "cell1"), gate(&mut app, "cell2"));
    open(&mut app, c1);
    assert_eq!(view_out(&mut app, "reg"), n1);
    assert_eq!((view_out(&mut app, "A"), view_out(&mut app, "B")), (1, 0));
    close(&mut app);
    open(&mut app, c2);
    assert_eq!(view_out(&mut app, "reg"), 0);
    assert_eq!((view_out(&mut app, "A"), view_out(&mut app, "B")), (0, 1));
}

/// A definition that wraps one cell: `outer.Sum = cell.Sum`.
fn outer_definition(cell: Uuid) -> CircuitDocument {
    let mut d = CircuitDocument::new("Outer");
    let a = d.add(DocNode::labeled(GateKind::Input { w: 1 }, Vec2::ZERO, "A"));
    let inner = d.add(DocNode::labeled(
        GateKind::SubCircuit { definition: cell },
        Vec2::ZERO,
        "inner",
    ));
    let y = d.add(DocNode::labeled(GateKind::Output { w: 1 }, Vec2::ZERO, "Y"));
    d.connect(a, 0, inner, 0);
    d.connect(inner, SUM, y, 0);
    d
}

#[test]
fn nested_instance_views_extend_the_path() {
    let cell = cell_definition();
    let cell_id = cell.id;
    let outer = outer_definition(cell_id);
    let outer_id = outer.id;
    let mut app = app();
    send(
        &mut app,
        RegisterDefinitions {
            definitions: HashMap::from([(cell_id, Arc::new(cell)), (outer_id, Arc::new(outer))]),
        },
    );
    let a = create_gate(&mut app, GateKind::Input { w: 1 }, "A");
    let o = create_gate(
        &mut app,
        GateKind::SubCircuit {
            definition: outer_id,
        },
        "outer",
    );
    let y = create_gate(&mut app, GateKind::Output { w: 1 }, "Y");
    create_wire(&mut app, a, 0, o, 0);
    create_wire(&mut app, o, 0, y, 0);
    set_input(&mut app, "A", 1);
    run(&mut app, 2);
    assert_eq!(out(&mut app, "Y"), 1);

    open(&mut app, o);
    assert_eq!(view_out(&mut app, "A"), 1);
    let inner = gate(&mut app, "inner");
    let inner_uuid = uuid(&app, inner);
    open(&mut app, inner);
    let v = view(&app);
    assert_eq!(v.instance_path(), Some(vec![uuid(&app, o), inner_uuid]));
    assert_eq!(v.breadcrumb("root"), "root / Outer / Cell");
    {
        let bridge = app.world().resource::<SimBridge>();
        let design = bridge.design();
        let path = bridge.view_node_path().expect("live").to_vec();
        assert_eq!(path.len(), 2);
        assert_eq!(path[0], bridge.node_of(o).unwrap());
        let NodeKind::Instance(outer_def) = design.root_def().nodes[path[0]].kind else {
            panic!()
        };
        let NodeKind::Instance(cell_def) = design.def(outer_def).unwrap().nodes[path[1]].kind
        else {
            panic!("second hop is the cell inside Outer")
        };
        assert_eq!(design.def(cell_def).unwrap().name, "Cell");
    }
    // Live values two levels down.
    assert_eq!(view_out(&mut app, "A"), 1);
    assert_eq!(view_out(&mut app, "xor"), 1);
    set_input_through_root(&mut app, 0);
    assert_eq!(view_out(&mut app, "xor"), 0, "follows the root input");

    // One level up, then back to the root.
    close(&mut app);
    assert_eq!(
        view(&app).instance_path(),
        Some(vec![uuid_of_parked(&mut app, "outer")])
    );
    assert_eq!(view_out(&mut app, "A"), 0);
    send(&mut app, CloseView { to_root: true });
    assert!(view(&app).is_root());
    assert_eq!(count_entities::<With<InstanceViewItem>>(&mut app), 0);
    assert_eq!(
        count_entities::<With<Disabled>>(&mut app),
        0,
        "root unparked"
    );
    set_input(&mut app, "A", 1);
    run(&mut app, 1);
    assert_eq!(out(&mut app, "Y"), 1);
}

/// Drive the root input `A` while a view is open (the root is parked, so
/// send the command to its entity directly).
fn set_input_through_root(app: &mut App, value: u64) {
    let a = {
        let mut q = app
            .world_mut()
            .query_filtered::<(Entity, &GateLabel), (Allow<Disabled>, Without<InstanceViewItem>)>();
        q.iter(app.world())
            .find(|(_, l)| l.value == "A")
            .map(|(e, _)| e)
            .unwrap()
    };
    // The editor cannot do this (the root is not shown), but the bridge
    // still applies input changes to the running simulation.
    app.world_mut()
        .entity_mut(a)
        .insert(crate::sim::InputDrive { value });
    settle(app);
    run(app, 1);
}

fn uuid_of_parked(app: &mut App, label: &str) -> Uuid {
    let mut q = app
        .world_mut()
        .query_filtered::<(&GateIdentity, &GateLabel), (Allow<Disabled>, Without<InstanceViewItem>)>();
    q.iter(app.world())
        .find(|(_, l)| l.value == label)
        .map(|(id, _)| id.uuid)
        .unwrap()
}

#[test]
fn recursive_instantiation_is_refused_with_a_message() {
    let cell = cell_definition();
    let cell_id = cell.id;
    let outer = outer_definition(cell_id);
    let outer_id = outer.id;
    let mut app = app();
    send(
        &mut app,
        RegisterDefinitions {
            definitions: HashMap::from([(cell_id, Arc::new(cell)), (outer_id, Arc::new(outer))]),
        },
    );
    let o = create_gate(
        &mut app,
        GateKind::SubCircuit {
            definition: outer_id,
        },
        "outer",
    );
    open(&mut app, o);
    let gates_before = count_entities::<With<GateIdentity>>(&mut app);

    // Outer inside Outer.
    send(
        &mut app,
        CreateGate {
            kind: GateKind::SubCircuit {
                definition: outer_id,
            },
            position: Vec2::ZERO,
            label: None,
        },
    );
    let message = feedback(&app);
    assert!(
        message.contains("Cannot place \"Outer\" inside itself"),
        "{message}"
    );
    assert_eq!(count_entities::<With<GateIdentity>>(&mut app), gates_before);

    // Outer inside Cell (which Outer contains).
    let inner = gate(&mut app, "inner");
    open(&mut app, inner);
    let gates_before = count_entities::<With<GateIdentity>>(&mut app);
    send(
        &mut app,
        CreateGate {
            kind: GateKind::SubCircuit {
                definition: outer_id,
            },
            position: Vec2::ZERO,
            label: None,
        },
    );
    let message = feedback(&app);
    assert!(
        message.contains("Cannot place \"Outer\" inside \"Cell\"")
            && message.contains("already contains \"Cell\""),
        "{message}"
    );
    assert_eq!(count_entities::<With<GateIdentity>>(&mut app), gates_before);
    // The library is unchanged and the simulation still compiles.
    let library = app.world().resource::<CircuitLibrary>();
    assert!(library.find_cycle().is_none());
    assert!(!library.contains(cell_id, outer_id));
    assert!(
        app.world()
            .resource::<SimDiagnostics>()
            .compile_error
            .is_none()
    );

    // A cell inside Cell is refused as well; another definition is fine.
    send(
        &mut app,
        CreateGate {
            kind: GateKind::SubCircuit {
                definition: cell_id,
            },
            position: Vec2::ZERO,
            label: None,
        },
    );
    assert!(
        feedback(&app).contains("inside itself"),
        "{}",
        feedback(&app)
    );
}

#[test]
fn saved_workspace_with_a_definition_cycle_is_refused() {
    let dir = temp_dir();
    let a = Uuid::new_v4();
    let b = Uuid::new_v4();
    let root = Uuid::new_v4();
    let circuit = |id: Uuid, name: &str, child: Uuid| {
        serde_json::json!({
            "id": id, "name": name, "wires": [],
            "nodes": [{ "id": Uuid::new_v4(), "kind": { "type": "SubCircuit", "definition": child }, "x": 0.0, "y": 0.0 }]
        })
    };
    let manifest = serde_json::json!({
        "format": "gatequest-workspace", "version": 5, "name": "loop", "root": root,
        "circuits": [circuit(root, "root", a), circuit(a, "A", b), circuit(b, "B", a)],
    });
    std::fs::create_dir_all(dir.join("loop")).unwrap();
    std::fs::write(dir.join("loop/manifest.json"), manifest.to_string()).unwrap();
    let err = read_workspace_in(&dir, "loop").unwrap_err();
    assert!(matches!(err, WorkspaceError::Invalid(_)), "{err:?}");
    assert!(err.to_string().contains("contains itself"), "{err}");
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn definition_table_opens_the_first_live_instance_or_the_bare_definition() {
    let cell = cell_definition();
    let cell_id = cell.id;
    let outer = outer_definition(cell_id);
    let outer_id = outer.id;
    let spare = CircuitDocument::new("Spare");
    let spare_id = spare.id;
    let mut app = app();
    send(
        &mut app,
        RegisterDefinitions {
            definitions: HashMap::from([
                (cell_id, Arc::new(cell)),
                (outer_id, Arc::new(outer)),
                (spare_id, Arc::new(spare)),
            ]),
        },
    );
    let a = create_gate(&mut app, GateKind::Input { w: 1 }, "A");
    let o = create_gate(
        &mut app,
        GateKind::SubCircuit {
            definition: outer_id,
        },
        "outer",
    );
    create_wire(&mut app, a, 0, o, 0);
    set_input(&mut app, "A", 1);
    run(&mut app, 1);

    // The cell is only instanced inside Outer: the table opens that one.
    send(
        &mut app,
        OpenDefinitionView {
            definition: cell_id,
        },
    );
    let v = view(&app);
    assert_eq!(v.frames().len(), 2);
    assert!(v.is_live());
    assert_eq!(
        view_out(&mut app, "xor"),
        1,
        "live values of the nested instance"
    );

    // Nothing instances "Spare": opened as a bare definition, not live.
    send(
        &mut app,
        OpenDefinitionView {
            definition: spare_id,
        },
    );
    let v = view(&app);
    assert_eq!(v.frames().len(), 1);
    assert!(!v.is_live());
    assert_eq!(app.world().resource::<SimBridge>().view_node_path(), None);
    send(&mut app, CloseView { to_root: true });
    assert!(view(&app).is_root());
}

#[test]
fn placing_a_known_definition_keeps_the_edited_version() {
    let cell = cell_definition();
    let cell_id = cell.id;
    let mut app = app();
    let [c1, _] = place_two_cells(&mut app, cell_id, one_def(cell.clone()));
    open(&mut app, c1);
    let one = gate(&mut app, "one");
    send(
        &mut app,
        SetGateKind {
            entity: one,
            kind: GateKind::Const { w: 8, value: 3 },
        },
    );
    close(&mut app);
    // Registering the original again (placing it from the catalog) does
    // not undo the edit: the definition is identified by its id.
    send(
        &mut app,
        RegisterDefinitions {
            definitions: one_def(cell),
        },
    );
    let library = app.world().resource::<CircuitLibrary>();
    let def = library.get(cell_id).unwrap();
    assert!(
        def.nodes
            .iter()
            .any(|n| n.kind == GateKind::Const { w: 8, value: 3 })
    );
}

#[test]
fn threaded_engine_serves_instance_views() {
    let cell = cell_definition();
    let cell_id = cell.id;
    let mut app = app_with(SimRuntime::Threaded);
    let [c1, c2] = place_two_cells(&mut app, cell_id, one_def(cell));
    set_input(&mut app, "A1", 1);
    set_input(&mut app, "B2", 1);
    set_input(&mut app, "A2", 1);
    app.world().resource::<SimBridge>().step(2);
    for (cell, xor, and) in [(c1, 1, 0), (c2, 0, 1)] {
        open(&mut app, cell);
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            app.world_mut()
                .resource_mut::<SimBridge>()
                .wait_until_current(Duration::from_secs(5));
            app.update();
            if view_out(&mut app, "xor") == xor && view_out(&mut app, "and") == and {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "live values never reached the view"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        close(&mut app);
    }
}

#[test]
fn saving_in_a_view_saves_the_workspace_and_loading_closes_the_view() {
    let cell = cell_definition();
    let cell_id = cell.id;
    let mut app = app();
    let [c1, _] = place_two_cells(&mut app, cell_id, one_def(cell));
    let at_root = read_back(&mut app);
    open(&mut app, c1);
    let in_view = read_back(&mut app);
    assert_eq!(
        in_view, at_root,
        "save reads the parked root and the definition table"
    );

    load(&mut app, demo("half_adder"));
    assert!(view(&app).is_root());
    assert_eq!(count_entities::<With<InstanceViewItem>>(&mut app), 0);
    assert_eq!(count_entities::<With<Disabled>>(&mut app), 0);
    assert_eq!(root_node_count(&app), 6, "only the half adder is simulated");
    assert_eq!(
        app.world().resource::<SimBridge>().design().defs.len(),
        3,
        "the cell definition is dropped: the root and its XOR and AND remain"
    );
    set_input(&mut app, "A", 1);
    run(&mut app, 1);
    assert_eq!(out(&mut app, "Sum"), 1);
}
