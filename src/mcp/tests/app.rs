//! Tests of the app host: a session working on the workspace of a
//! running (headless) app through the channel link.

use std::sync::Arc;
use std::time::Duration;

use bevy::ecs::system::RunSystemOnce;
use bevy::prelude::*;
use serde_json::{Map, Value, json};

use super::scratch;
use crate::editor::workspace::WorkspaceStore;
use crate::mcp::app::{ChannelLink, McpAppPlugin, link};
use crate::mcp::session::{AppLink, Session};
use crate::sim::demos::catalog;
use crate::sim::{
    ActiveCircuit, CircuitLibrary, CircuitReader, DeleteGate, GateIdentity, GateLabel,
    HistoryCommand, LoadWorkspaceEvent, SimBridge, SimRuntime, SimulationRuntimePlugin,
    UndoHistory, WorkspaceDocument, WorkspaceRevision, WorkspaceTests,
};

fn object(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        other => panic!("not an object: {other}"),
    }
}

/// A headless app serving a link, and the link's other end.
fn app() -> (App, ChannelLink) {
    let (link, inbox) = link();
    let mut app = App::new();
    app.add_plugins(SimulationRuntimePlugin {
        runtime: SimRuntime::Threadless,
    })
    .add_plugins(McpAppPlugin::new(inbox))
    .init_resource::<ButtonInput<MouseButton>>();
    app.update();
    (app, link)
}

/// Run `work` (calls of a session) on another thread while the app's
/// frames go by, as the server's thread and the game do.
fn drive<T: Send>(app: &mut App, work: impl FnOnce() -> T + Send) -> T {
    std::thread::scope(|scope| {
        let worker = scope.spawn(work);
        while !worker.is_finished() {
            app.update();
            std::thread::sleep(Duration::from_millis(1));
        }
        worker.join().expect("the session's thread")
    })
}

fn session(link: ChannelLink, test: &str) -> (Session, std::path::PathBuf) {
    let dir = scratch(test);
    (
        Session::with_app(WorkspaceStore::in_dir(&dir), Box::new(link)),
        dir,
    )
}

fn labels(app: &mut App) -> Vec<String> {
    let mut labels: Vec<String> = app
        .world_mut()
        .run_system_once(|gates: Query<&GateLabel, With<GateIdentity>>| {
            gates
                .iter()
                .map(|l| l.value.clone())
                .collect::<Vec<String>>()
        })
        .unwrap();
    labels.sort();
    labels
}

fn entity_of(app: &mut App, label: &'static str) -> Entity {
    app.world_mut()
        .run_system_once(move |gates: Query<(Entity, &GateLabel)>| {
            gates
                .iter()
                .find(|(_, l)| l.value == label)
                .map(|(e, _)| e)
                .unwrap_or_else(|| panic!("no part {label}"))
        })
        .unwrap()
}

fn counter() -> Value {
    json!({"ops": [
        {"add": [
            {"name": "En", "type": "Input"},
            {"name": "one", "type": "Const", "w": 8, "value": 1},
            {"name": "zero", "type": "Const", "value": 0},
            {"name": "inc", "type": "Add", "w": 8},
            {"name": "r", "type": "Register", "w": 8},
            {"name": "Q", "type": "Output", "w": 8}
        ]},
        {"wire": [["r", "inc.a", "Q"], ["one", "inc.b"], ["zero", "inc.cin"],
                  ["inc.sum", "r.d"], ["En", "r.en"]]}
    ]})
}

#[test]
fn an_edit_reaches_the_open_circuit_as_one_undo_step() {
    let (mut app, link) = app();
    let (mut s, dir) = session(link, "app-edit");
    let report = drive(&mut app, || s.edit_tool(object(counter()), None, false)).unwrap();
    assert!(report.contains("main: +6 parts +6 wires"), "{report}");
    app.update();
    assert_eq!(labels(&mut app), ["En", "Q", "inc", "one", "r", "zero"]);
    assert_eq!(app.world().resource::<UndoHistory>().undo_len(), 1);
    assert!(app.world().resource::<WorkspaceRevision>().is_dirty());

    // A second edit: what it does not touch keeps its entity.
    let register = entity_of(&mut app, "r");
    let more = json!({"ops": [{"add": [{"name": "half", "type": "Split", "w": 8, "lo": 0, "len": 4},
                                       {"name": "Low", "type": "Output", "w": 4}]},
                              {"wire": [["r", "half"], ["half", "Low"]]}]});
    drive(&mut app, || s.edit_tool(object(more), None, false)).unwrap();
    app.update();
    assert_eq!(entity_of(&mut app, "r"), register);
    assert_eq!(app.world().resource::<UndoHistory>().undo_len(), 2);

    // Undo takes the second edit back whole, and the session sees it.
    app.world_mut().write_message(HistoryCommand::Undo);
    app.update();
    app.update();
    assert_eq!(labels(&mut app), ["En", "Q", "inc", "one", "r", "zero"]);
    let view = drive(&mut app, || s.view_tool(None, None, None, None)).unwrap();
    assert!(
        view.contains("6 parts, 6 wires") && !view.contains("half"),
        "{view}"
    );
    app.world_mut().write_message(HistoryCommand::Redo);
    app.update();
    app.update();
    assert!(labels(&mut app).iter().any(|l| l == "half"));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn the_visible_simulation_is_driven_and_keeps_its_state_over_an_edit() {
    let (mut app, link) = app();
    let (mut s, dir) = session(link, "app-transport");
    drive(&mut app, || s.edit_tool(object(counter()), None, false)).unwrap();
    app.update();
    app.update();

    let transport = |s: &mut Session, command: Value| s.transport_tool(object(command));
    let out = drive(&mut app, || {
        transport(
            &mut s,
            json!({"action": "step", "ticks": 9, "inputs": {"En": 1}}),
        )
    })
    .unwrap();
    // The answer comes when the step has run.
    assert!(out.starts_with("paused, tick 9, rate "), "{out}");
    let tick = app.world().resource::<SimBridge>().tick_count();
    assert_eq!(tick, 9);
    let state = drive(&mut app, || s.app_tool("state")).unwrap();
    assert!(
        state.contains("workspace: \"workspace\" (unsaved changes)"),
        "{state}"
    );
    assert!(
        state.contains("transport: paused, tick 9, rate "),
        "{state}"
    );
    assert!(state.contains("signals: in En=1, out Q="), "{state}");
    assert!(state.contains("build: ok"), "{state}");
    let q = |state: &str| -> u64 {
        let at = state.find("out Q=").expect("Q is a signal") + 6;
        state[at..]
            .split(|c: char| !c.is_ascii_digit())
            .next()
            .unwrap()
            .parse()
            .unwrap()
    };
    let counted = q(&state);
    assert!(counted >= 7, "{state}");

    // An edit elsewhere in the circuit: the register keeps counting from
    // where it was.
    let more = json!({"ops": [{"add": [{"name": "spare", "type": "Not"}]}]});
    drive(&mut app, || s.edit_tool(object(more), None, false)).unwrap();
    for _ in 0..4 {
        app.update();
    }
    assert_eq!(app.world().resource::<SimBridge>().tick_count(), 9);
    let state = drive(&mut app, || s.app_tool("state")).unwrap();
    assert_eq!(q(&state), counted, "{state}");

    // A private run does not disturb what the editor shows.
    let out = drive(&mut app, || {
        s.run_tool(
            None,
            None,
            object(json!({"steps": [{"set": {"En": 1}}, {"ticks": 100}, {"read": ["Q"]}]})),
            None,
        )
    })
    .unwrap();
    assert!(out.contains("ran 100 ticks"), "{out}");
    app.update();
    assert_eq!(app.world().resource::<SimBridge>().tick_count(), 9);

    let out = drive(&mut app, || transport(&mut s, json!({"action": "reset"}))).unwrap();
    assert!(out.contains("tick 0") || out.contains("paused"), "{out}");
    let out = drive(&mut app, || {
        transport(&mut s, json!({"action": "rate", "hz": "max"}))
    })
    .unwrap();
    assert!(out.ends_with("rate max\n"), "{out}");
    let out = drive(&mut app, || {
        transport(&mut s, json!({"action": "run", "hz": 60}))
    })
    .unwrap();
    assert!(
        out.starts_with("running") && out.ends_with("rate 60 Hz\n"),
        "{out}"
    );
    for (bad, said) in [
        (json!({"action": "fly"}), "unknown action \"fly\""),
        (
            json!({"inputs": {"Nope": 1}}),
            "0 inputs of the root circuit are labelled \"Nope\"",
        ),
        (
            json!({"inputs": {"En": 2}}),
            "does not fit input \"En\" (1 bits)",
        ),
        (json!({"hz": 0}), "\"hz\" is a rate above 0, or \"max\""),
        (json!({"speed": 1}), "unknown field \"speed\""),
    ] {
        let e = drive(&mut app, || transport(&mut s, bad.clone())).unwrap_err();
        assert!(e.contains(said), "{bad}: {e}");
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn requests_wait_for_a_gesture_to_end() {
    let (mut app, link) = app();
    let (mut s, dir) = session(link, "app-gesture");
    app.world_mut()
        .resource_mut::<ButtonInput<MouseButton>>()
        .press(MouseButton::Left);
    std::thread::scope(|scope| {
        let worker = scope.spawn(|| s.edit_tool(object(counter()), None, false));
        for _ in 0..30 {
            app.update();
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(
            !worker.is_finished(),
            "the call waits while the button is down"
        );
        assert!(labels(&mut app).is_empty());
        app.world_mut()
            .resource_mut::<ButtonInput<MouseButton>>()
            .release(MouseButton::Left);
        while !worker.is_finished() {
            app.update();
            std::thread::sleep(Duration::from_millis(1));
        }
        worker.join().unwrap().unwrap();
    });
    app.update();
    assert_eq!(labels(&mut app).len(), 6);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn an_edit_of_a_circuit_that_moved_on_is_refused() {
    let (mut app, mut link) = app();
    let read = drive(&mut app, || link.read()).unwrap();
    let (built, _) = super::edit(&read.workspace, counter());
    drive(&mut app, || link.commit(built, read.stamp)).unwrap();
    app.update();

    // The player deletes a part between a read and its commit.
    let read = drive(&mut app, || link.read()).unwrap();
    let zero = entity_of(&mut app, "zero");
    app.world_mut().write_message(DeleteGate { entity: zero });
    app.update();
    app.update();
    let e = drive(&mut app, || link.commit(read.workspace.clone(), read.stamp)).unwrap_err();
    assert!(
        e.contains("was changed in the editor while this edit was being made"),
        "{e}"
    );
    assert_eq!(labels(&mut app).len(), 5, "the stale edit changed nothing");
    // Read again and it goes through.
    let read = drive(&mut app, || link.read()).unwrap();
    drive(&mut app, || link.commit(read.workspace.clone(), read.stamp)).unwrap();
}

#[test]
fn workspaces_are_replaced_through_the_editor_and_demos_stay_read_only() {
    let (mut app, link) = app();
    let (mut s, dir) = session(link, "app-open");
    let out = drive(&mut app, || {
        s.workspace_tool("open_demo", Some("half_adder"), false)
    })
    .unwrap();
    assert!(out.contains("opened a copy of \"half_adder\""), "{out}");
    app.update();
    assert_eq!(app.world().resource::<ActiveCircuit>().name, "half_adder");
    assert!(!app.world().resource::<ActiveCircuit>().read_only);
    assert!(!labels(&mut app).is_empty());
    // Unsaved work is not replaced by accident; with discard it is.
    let e = drive(&mut app, || s.workspace_tool("new", Some("fresh"), false)).unwrap_err();
    assert!(e.contains("unsaved changes"), "{e}");
    drive(&mut app, || s.workspace_tool("new", Some("fresh"), true)).unwrap();
    app.update();
    assert_eq!(app.world().resource::<ActiveCircuit>().name, "fresh");
    assert!(labels(&mut app).is_empty());

    // A demo the player opened read-only cannot be edited through us.
    let demo = catalog().unwrap().get("half_adder").unwrap();
    app.world_mut().write_message(LoadWorkspaceEvent {
        name: "half_adder".into(),
        workspace: Arc::clone(&demo.workspace),
        read_only: true,
        unsaved: false,
    });
    app.update();
    app.update();
    let e = drive(&mut app, || {
        s.edit_tool(
            object(json!({"ops": [{"add": [{"name": "n", "type": "Not"}]}]})),
            None,
            false,
        )
    })
    .unwrap_err();
    assert!(
        e.contains("read-only demo") && e.contains("open_demo"),
        "{e}"
    );
    let state = drive(&mut app, || s.app_tool("state")).unwrap();
    assert!(
        state.contains("workspace: \"half_adder\" (read-only demo)"),
        "{state}"
    );
    // A refused save is an error, though the workspace is clean (which a
    // refused save leaves it) and keeps its name.
    assert!(!app.world().resource::<WorkspaceRevision>().is_dirty());
    let e = drive(&mut app, || s.workspace_tool("save", None, false)).unwrap_err();
    assert!(e.contains("read-only"), "{e}");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn chips_and_tests_made_through_the_session_live_in_the_app() {
    let (mut app, link) = app();
    let (mut s, dir) = session(link, "app-chips");
    let chip = json!({"circuit": "inv2", "ops": [
        {"add": [{"name": "A", "type": "Input"}, {"name": "n1", "type": "Not"},
                 {"name": "n2", "type": "Not"}, {"name": "Y", "type": "Output"}]},
        {"wire": [["A", "n1"], ["n1", "n2"], ["n2", "Y"]]},
        {"tests": [{"name": "buffer", "table": {"inputs": ["A"], "outputs": ["Y"], "rows": [[0, 0], [1, 1]]}}]}
    ]});
    drive(&mut app, || s.edit_tool(object(chip), None, false)).unwrap();
    app.update();
    // Not placed yet, and still there for the next call.
    assert_eq!(
        app.world().resource::<CircuitLibrary>().definitions.len(),
        1
    );
    assert_eq!(app.world().resource::<WorkspaceTests>().0.len(), 1);
    let place = json!({"ops": [{"add": [{"name": "In", "type": "Input"}, {"name": "b", "use": "inv2"},
                                        {"name": "Out", "type": "Output"}]},
                               {"wire": [["In", "b.A"], ["b.Y", "Out"]]}]});
    let report = drive(&mut app, || s.edit_tool(object(place), None, false)).unwrap();
    assert!(report.contains("build: ok (6 parts flattened)"), "{report}");
    let out = drive(&mut app, || s.test_tool(None, None)).unwrap();
    assert!(out.starts_with("1 of 1 tests passed"), "{out}");
    app.update();
    // What the editor would save has the chip and its test.
    let saved: WorkspaceDocument = app
        .world_mut()
        .run_system_once(|reader: CircuitReader| reader.workspace())
        .unwrap();
    assert_eq!((saved.definitions.len(), saved.tests.len()), (1, 1));

    // The player deletes a part; the session's next view shows it.
    let b = entity_of(&mut app, "b");
    app.world_mut().write_message(DeleteGate { entity: b });
    app.update();
    app.update();
    let view = drive(&mut app, || s.view_tool(None, None, None, None)).unwrap();
    assert!(view.contains("2 parts, 0 wires"), "{view}");

    // Without an app the app tools say so.
    let mut alone = Session::new(WorkspaceStore::in_dir(&dir));
    let e = alone.transport_tool(Map::new()).unwrap_err();
    assert!(e.contains("without the game"), "{e}");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn an_edit_of_only_the_tests_is_unsaved_and_can_be_undone() {
    let (mut app, link) = app();
    let (mut s, dir) = session(link, "app-tests-only");
    let circuit = json!({"ops": [
        {"add": [{"name": "A", "type": "Input"}, {"name": "Y", "type": "Output"}]},
        {"wire": [["A", "Y"]]}
    ]});
    drive(&mut app, || s.edit_tool(object(circuit), None, false)).unwrap();
    app.update();
    app.world_mut()
        .resource_mut::<WorkspaceRevision>()
        .mark_saved();
    let steps = app.world().resource::<UndoHistory>().undo_len();

    let tests = json!({"ops": [{"tests": [
        {"name": "wire", "table": {"inputs": ["A"], "outputs": ["Y"], "rows": [[0, 0], [1, 1]]}}
    ]}]});
    drive(&mut app, || s.edit_tool(object(tests), None, false)).unwrap();
    app.update();
    app.update();
    assert_eq!(app.world().resource::<WorkspaceTests>().0.len(), 1);
    assert!(
        app.world().resource::<WorkspaceRevision>().is_dirty(),
        "the new test is not saved yet"
    );
    assert_eq!(app.world().resource::<UndoHistory>().undo_len(), steps + 1);

    app.world_mut().write_message(HistoryCommand::Undo);
    app.update();
    app.update();
    assert!(app.world().resource::<WorkspaceTests>().0.is_empty());
    assert!(!app.world().resource::<WorkspaceRevision>().is_dirty());
    app.world_mut().write_message(HistoryCommand::Redo);
    app.update();
    app.update();
    assert_eq!(app.world().resource::<WorkspaceTests>().0.len(), 1);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_request_whose_caller_gave_up_is_not_served_later() {
    let (mut app, mut link) = app();
    link.timeout = Duration::from_millis(20);
    // A gesture holds requests back for longer than the caller waits.
    app.world_mut()
        .resource_mut::<ButtonInput<MouseButton>>()
        .press(MouseButton::Left);
    let demo = (*catalog().unwrap().get("half_adder").unwrap().workspace).clone();
    let e = std::thread::scope(|scope| {
        let worker = scope.spawn(|| link.open("late".into(), demo, false));
        while !worker.is_finished() {
            app.update();
            std::thread::sleep(Duration::from_millis(1));
        }
        worker.join().unwrap().unwrap_err()
    });
    assert!(
        e.contains("did not answer") && e.contains("nothing was done"),
        "{e}"
    );
    // The gesture ends: the abandoned request must not open the workspace.
    app.world_mut()
        .resource_mut::<ButtonInput<MouseButton>>()
        .release(MouseButton::Left);
    for _ in 0..5 {
        app.update();
    }
    assert_ne!(app.world().resource::<ActiveCircuit>().name, "late");
    assert!(labels(&mut app).is_empty());
}

#[test]
fn a_save_names_the_session_chips_it_leaves_out() {
    use crate::sim::{CircuitDocument, DocNode, GateKind};
    let chip = |name: &str| CircuitDocument::new(name);
    let (placed, inner, loose) = (chip("placed"), chip("inner"), chip("loose"));
    let other = chip("of another workspace");
    let mut placed = placed;
    placed.add(DocNode::new(
        GateKind::SubCircuit {
            definition: inner.id,
        },
        Vec2::ZERO,
    ));
    let mut root = CircuitDocument::new("root");
    root.add(DocNode::new(
        GateKind::SubCircuit {
            definition: placed.id,
        },
        Vec2::ZERO,
    ));
    // The session made three chips; the library also has one it did not.
    let session: std::collections::HashSet<_> = [placed.id, inner.id, loose.id].into();
    let mut library = CircuitLibrary::default();
    for def in [placed, inner, loose, other] {
        library.definitions.insert(def.id, Arc::new(def));
    }
    assert_eq!(
        crate::mcp::app::unplaced(&library, &root, &session),
        ["loose"]
    );
}

#[test]
fn a_refused_transport_request_changes_nothing() {
    let (mut app, link) = app();
    let (mut s, dir) = session(link, "app-transport-refused");
    drive(&mut app, || s.edit_tool(object(counter()), None, false)).unwrap();
    app.update();
    app.update();
    let transport = |s: &mut Session, command: Value| s.transport_tool(object(command));
    // A good input before a bad one, and a good input with a bad action.
    for command in [
        json!({"action": "set", "inputs": {"En": 1, "Missing": 0}}),
        json!({"action": "set", "inputs": {"En": 2}}),
        json!({"action": "nonsense", "hz": 5, "inputs": {"En": 1}}),
        // More ticks than a step may run (its end must not overflow).
        json!({"action": "step", "ticks": u64::MAX, "inputs": {"En": 1}}),
    ] {
        drive(&mut app, || transport(&mut s, command.clone())).unwrap_err();
        app.update();
        app.update();
        let state = drive(&mut app, || s.app_tool("state")).unwrap();
        assert!(state.contains("in En=0"), "{command}: {state}");
    }
    let _ = std::fs::remove_dir_all(dir);
}
