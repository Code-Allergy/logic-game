//! The built-in demos (issue #40): the files are embedded and canonical,
//! load → save round-trips, the pyramid only builds on lower levels, every
//! demo compiles, and each one behaves as described (checked from the
//! files, through the app's bridge and engine).

use std::collections::BTreeSet;

use super::*;
use crate::editor::workspace::workspace_key;
use crate::sim::demos::{DEMO_DIR, EMBEDDED, MANIFEST_FILE, demo_key, editable_copy};
use crate::sim::source::{self, Externals, circuit_id};
use crate::sim::{ActiveCircuit, AuthoringFeedback, GateMoved, READ_ONLY_MESSAGE};

// ─────────────────────────────────────────────────────────────────────────────
// Files, format and round trips
// ─────────────────────────────────────────────────────────────────────────────

/// Every file in `assets/demos/` is embedded, unchanged, and nothing else.
#[test]
fn embedded_files_match_the_demo_folder() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(DEMO_DIR);
    let mut on_disk: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    on_disk.sort();
    let mut embedded: Vec<String> = EMBEDDED.iter().map(|(n, _)| n.to_string()).collect();
    embedded.sort();
    assert_eq!(embedded, on_disk);
    for (name, text) in EMBEDDED {
        assert_eq!(
            std::fs::read_to_string(dir.join(name)).unwrap(),
            *text,
            "{name}"
        );
    }
    // Every demo file is listed in the manifest.
    let listed: BTreeSet<String> = demos()
        .demos
        .iter()
        .map(|d| format!("{}.json", d.id))
        .chain([MANIFEST_FILE.to_string()])
        .collect();
    assert_eq!(listed, on_disk.into_iter().collect());
}

/// The checked-in files are canonical (load → save is byte-stable), the
/// syntax tree round-trips, and expansion is deterministic.
#[test]
fn demo_files_are_canonical_and_expand_deterministically() {
    for demo in &demos().demos {
        let (_, text) = EMBEDDED
            .iter()
            .find(|(n, _)| *n == format!("{}.json", demo.id))
            .unwrap();
        let parsed = source::parse(text).unwrap();
        assert_eq!(source::to_text(&parsed), *text, "{} is canonical", demo.id);
        assert_eq!(
            source::parse(&source::to_text(&parsed)).unwrap(),
            parsed,
            "{}",
            demo.id
        );
        // Lines stay readable.
        assert!(
            text.lines().all(|l| l.len() <= 100),
            "{}: a line is longer than 100 columns",
            demo.id
        );

        // Expanding again (with the same externals) gives identical documents.
        let again = source::expand(&parsed, &demo_key(&demo.id), &demos().externals()).unwrap();
        assert_eq!(again.workspace, *demo.workspace, "{}", demo.id);

        // Ids are derived from the demo and circuit names.
        for (name, doc) in &demo.circuits {
            assert_eq!(doc.id, circuit_id(&demo_key(&demo.id), name));
            assert_eq!(doc.id.get_version_num(), 5);
            for node in &doc.nodes {
                assert_eq!(node.id.get_version_num(), 5);
            }
        }
    }
}

/// Load a demo, save it as a version 5 workspace, load that: the same
/// documents. And a version 6 file in the workspace folder loads too.
#[test]
fn demos_round_trip_through_workspace_files() {
    let dir = temp_dir();
    for demo in &demos().demos {
        save_workspace_in(&dir, &demo.id, &demo.workspace).unwrap();
        let loaded = read_workspace_in(&dir, &demo.id).unwrap();
        assert_eq!(loaded, *demo.workspace, "{}", demo.id);

        // Through the app as well: the editor reads back what it loaded.
        if demo.level <= 4 {
            let mut app = app();
            load(&mut app, loaded);
            assert_eq!(read_back(&mut app), *demo.workspace, "{}", demo.id);
        }
    }

    // The demo text itself, saved as a workspace (version 6).
    let (_, text) = EMBEDDED
        .iter()
        .find(|(n, _)| *n == "full_adder.json")
        .unwrap();
    std::fs::create_dir_all(dir.join("mine")).unwrap();
    std::fs::write(dir.join("mine/manifest.json"), text).unwrap();
    let loaded = read_workspace_in(&dir, "mine").unwrap();
    let expected = source::expand(
        &source::parse(text).unwrap(),
        &workspace_key("mine"),
        &demos().externals(),
    )
    .unwrap()
    .workspace;
    assert_eq!(loaded, expected);
    assert_eq!(loaded.root.id, circuit_id("workspaces/mine", "full_adder"));
    // Its uses of other demos resolve to the shared demo definitions.
    let half_adder = demos().get("half_adder").unwrap().workspace.root.id;
    assert!(loaded.definitions.contains_key(&half_adder));
    std::fs::remove_dir_all(dir).ok();
}

/// Each demo above level 1 instances definitions of other demos, only of
/// demos earlier in the pyramid (at the same level, the "then" step of a
/// level such as decoder/mux then mux tree, or a lower one); the
/// manifest's `uses` lists exactly those.
#[test]
fn demos_form_a_pyramid() {
    let catalog = demos();
    assert_eq!(catalog.demos.len(), 16);
    assert_eq!(catalog.demos.first().unwrap().level, 1);
    assert_eq!(catalog.demos.last().unwrap().id, "cpu");
    assert_eq!(catalog.demos.last().unwrap().level, 10);
    for (i, demo) in catalog.demos.iter().enumerate() {
        assert_eq!(
            demo.source.external_uses(),
            demo.uses.iter().cloned().collect::<BTreeSet<_>>(),
            "{}",
            demo.id
        );
        if demo.level == 1 {
            continue;
        }
        assert!(!demo.uses.is_empty(), "{} uses lower-level demos", demo.id);
        for r in &demo.uses {
            let (other, circuit) = r.split_once('/').unwrap();
            let j = catalog.demos.iter().position(|d| d.id == other).unwrap();
            assert!(j < i, "{}: {r} comes later", demo.id);
            let used = &catalog.demos[j];
            assert!(used.level <= demo.level);
            // The demo's circuit really is built from it.
            let id = circuit_id(&demo_key(other), circuit);
            assert!(
                demo.workspace.definitions.contains_key(&id),
                "{}: {r} is not instanced",
                demo.id
            );
        }
    }
}

#[test]
fn bad_files_are_refused_with_a_message() {
    let file = |circuit: &str| {
        format!(
            r#"{{"format": "gatequest-workspace", "version": 6, "root": "c", "circuits": [{circuit}]}}"#
        )
    };
    let expand = |text: &str| -> String {
        match source::parse(text) {
            Err(e) => e.to_string(),
            Ok(f) => source::expand(&f, "test", &demos().externals())
                .err()
                .map(|e| e.to_string())
                .unwrap_or_default(),
        }
    };
    let cases = [
        (
            r#"{"name": "c", "nodes": [{"name": "a", "type": "And", "at": [0, 0], "widht": 2}]}"#,
            "no parameter \"widht\"",
        ),
        (
            r#"{"name": "c", "nodes": [{"name": "a", "type": "And"}]}"#,
            "missing \"at\"",
        ),
        (
            r#"{"name": "c", "nodes": [{"name": "a", "type": "Input", "at": [0, 0]},
                {"name": "g", "type": "Not", "at": [0, 0]}], "wires": [["a", "g.b"]]}"#,
            "no input pin \"b\"",
        ),
        (
            r#"{"name": "c", "nodes": [{"name": "a", "type": "Input", "w": 3, "at": [0, 0]},
                {"name": "g", "type": "Not", "w": 2, "at": [0, 0]}], "wires": [["a", "g"]]}"#,
            "widths must match",
        ),
        (
            r#"{"name": "c", "nodes": [{"name": "a", "type": "Input", "at": [0, 0]},
                {"name": "g", "type": "Not", "w": 2, "at": [0, 0]}], "wires": [["a", "g:0"]]}"#,
            "only some of its bits are driven",
        ),
        (
            r#"{"name": "c", "nodes": [{"name": "x", "use": "c", "at": [0, 0]}]}"#,
            "contains itself",
        ),
        (
            r#"{"name": "c", "nodes": [{"name": "x", "use": "nowhere/thing", "at": [0, 0]}]}"#,
            "unknown definition \"nowhere/thing\"",
        ),
        (
            r#"{"name": "c", "nodes": [{"name": "a", "type": "Input", "at": [0, 0]},
                {"name": "g", "type": "Not", "at": [0, 0]}], "wires": [["a", "g"], ["a", "g"]]}"#,
            "already driven",
        ),
    ];
    for (circuit, expected) in cases {
        let message = expand(&file(circuit));
        assert!(message.contains(expected), "{expected:?} in {message:?}");
    }
    let message = expand(r#"{"format": "gatequest-workspace", "version": 5}"#);
    assert!(message.contains("\"version\" must be 6"), "{message}");
}

/// Bus wiring: a word into an array of bits, bits back into a word, and a
/// bit field, with the Split and Merge adapters this needs.
#[test]
fn bus_wiring_expands_to_splits_and_merges() {
    let text = r#"{"format": "gatequest-workspace", "version": 6, "root": "c", "circuits": [
        {"name": "c", "nodes": [
            {"name": "A", "type": "Input", "w": 4, "at": [0, 0]},
            {"name": "inv", "type": "Not", "at": [100, 0], "count": 4, "step": [0, 50]},
            {"name": "Y", "type": "Output", "w": 4, "at": [200, 0]},
            {"name": "Hi", "type": "Output", "w": 2, "at": [200, -100]},
            {"name": "Same", "type": "Output", "w": 4, "at": [200, -200]}],
         "wires": [["A", "inv"], ["inv", "Y"], ["A:2..4", "Hi"], ["A:0..2", "Same:0..2"],
                   ["A:2..4", "Same:2..4"]]}]}"#;
    let ws = source::expand(&source::parse(text).unwrap(), "test", &Externals::default())
        .unwrap()
        .workspace;
    let count =
        |pred: fn(&GateKind) -> bool| ws.root.nodes.iter().filter(|n| pred(&n.kind)).count();
    // One split per bit of A (bits 2..4 are shared with Hi's field split),
    // one merge for Y; Same gets A back whole (neighbouring bits coalesce).
    assert_eq!(count(|k| matches!(k, GateKind::Split { len: 1, .. })), 4);
    assert_eq!(count(|k| matches!(k, GateKind::Split { len: 2, .. })), 1);
    assert_eq!(count(|k| matches!(k, GateKind::Merge { .. })), 1);
    let a = ws
        .root
        .nodes
        .iter()
        .find(|n| n.label.as_deref() == Some("A"))
        .unwrap()
        .id;
    let same = ws
        .root
        .nodes
        .iter()
        .find(|n| n.label.as_deref() == Some("Same"))
        .unwrap()
        .id;
    assert!(
        ws.root
            .wires
            .iter()
            .any(|w| w.from.node == a && w.to.node == same)
    );
    // Array elements are placed at at + i * step.
    let mut ys: Vec<f32> = ws
        .root
        .nodes
        .iter()
        .filter(|n| matches!(n.kind, GateKind::Not { .. }))
        .map(|n| n.y)
        .collect();
    ys.sort_by(f32::total_cmp);
    assert_eq!(ys, [0.0, 50.0, 100.0, 150.0]);

    let mut app = app();
    load(&mut app, ws);
    set_input(&mut app, "A", 0b1001);
    run(&mut app, 1);
    assert_eq!(value_at(&mut app, "Y"), 0b0110);
    assert_eq!(value_at(&mut app, "Hi"), 0b10);
    assert_eq!(value_at(&mut app, "Same"), 0b1001);
}

// ─────────────────────────────────────────────────────────────────────────────
// Read-only demos and copies
// ─────────────────────────────────────────────────────────────────────────────

fn load_read_only(app: &mut App, id: &str) {
    let demo = demos().get(id).unwrap();
    app.world_mut().write_message(LoadWorkspaceEvent {
        name: demo.title.clone(),
        workspace: Arc::clone(&demo.workspace),
        read_only: true,
    });
    settle(app);
}

fn feedback(app: &App) -> Option<String> {
    app.world().resource::<AuthoringFeedback>().message.clone()
}

#[test]
fn demos_open_read_only_and_copies_are_editable() {
    let mut app = app();
    load_read_only(&mut app, "half_adder");
    assert!(app.world().resource::<ActiveCircuit>().read_only);
    let nodes = root_node_count(&app);

    // Edits are refused with a message.
    send(
        &mut app,
        CreateGate {
            kind: GateKind::Not { w: 1 },
            position: Vec2::ZERO,
            label: Some("extra".into()),
        },
    );
    assert_eq!(feedback(&app).as_deref(), Some(READ_ONLY_MESSAGE));
    assert_eq!(root_node_count(&app), nodes);
    let sum = gate(&mut app, "Sum");
    let sum_id = uuid(&app, sum);
    send(&mut app, DeleteGate { entity: sum });
    send(
        &mut app,
        GateMoved {
            gate_uuid: sum_id,
            position: Vec2::new(999.0, 999.0),
        },
    );
    assert_eq!(root_node_count(&app), nodes);
    assert_ne!(
        app.world().get::<Transform>(sum).unwrap().translation.x,
        999.0
    );

    // Inputs still work.
    set_input(&mut app, "A", 1);
    run(&mut app, 1);
    assert_eq!(value_at(&mut app, "Sum"), 1);

    // The copy: fresh ids everywhere, same circuits, editable.
    let demo = demos().get("half_adder").unwrap();
    let copy = editable_copy(&demo.workspace, "Half adder (copy)");
    assert_ne!(copy.root.id, demo.workspace.root.id);
    assert_eq!(copy.root.name, "Half adder (copy)");
    assert_eq!(copy.definitions.len(), demo.workspace.definitions.len());
    for id in copy.definitions.keys() {
        assert!(!demo.workspace.definitions.contains_key(id));
        assert_eq!(id.get_version_num(), 4);
    }
    for def in copy.definitions.values() {
        let original = demo
            .workspace
            .definitions
            .values()
            .find(|d| d.name == def.name)
            .unwrap();
        assert_eq!(def.nodes.len(), original.nodes.len());
    }
    let dir = temp_dir();
    save_workspace_in(&dir, "copy", &copy).unwrap();
    assert_eq!(read_workspace_in(&dir, "copy").unwrap(), copy);
    std::fs::remove_dir_all(dir).ok();

    load(&mut app, copy);
    assert!(!app.world().resource::<ActiveCircuit>().read_only);
    create_gate(&mut app, GateKind::Not { w: 1 }, "extra");
    assert_eq!(root_node_count(&app), nodes + 1);
    set_input(&mut app, "A", 1);
    set_input(&mut app, "B", 1);
    run(&mut app, 1);
    assert_eq!(
        (value_at(&mut app, "Sum"), value_at(&mut app, "Carry")),
        (0, 1)
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Behaviour of each demo
// ─────────────────────────────────────────────────────────────────────────────

/// Load demo `id` and check it compiled with every wire connected.
fn open(id: &str) -> App {
    let mut app = app();
    load(&mut app, demo(id));
    assert!(
        app.world()
            .resource::<SimDiagnostics>()
            .compile_error
            .is_none(),
        "{id} compiles"
    );
    assert!(
        wires(&mut app).iter().all(|(_, _, s)| s.connected),
        "{id}: every wire simulated"
    );
    app
}

#[test]
fn gates_demo_truth_tables() {
    let mut app = open("gates");
    for a in 0..2 {
        for b in 0..2 {
            let got = eval(
                &mut app,
                &[("A", a), ("B", b)],
                &[
                    "NOT A", "A AND B", "A OR B", "A XOR B", "A NAND B", "A NOR B",
                ],
            );
            let want = [1 - a, a & b, a | b, a ^ b, 1 - (a & b), 1 - (a | b)];
            assert_eq!(got, want, "A={a} B={b}");
        }
    }
}

#[test]
fn universal_demo_truth_tables() {
    let mut app = open("universal");
    for a in 0..2 {
        for b in 0..2 {
            let got = eval(
                &mut app,
                &[("A", a), ("B", b)],
                &[
                    "NOT A (NAND)",
                    "A AND B (NAND)",
                    "A OR B (NAND)",
                    "A XOR B (NAND)",
                    "NOT A (NOR)",
                    "A OR B (NOR)",
                    "A AND B (NOR)",
                    "A XOR B (primitive)",
                ],
            );
            let want = [1 - a, a & b, a | b, a ^ b, 1 - a, a | b, a & b, a ^ b];
            assert_eq!(got, want, "A={a} B={b}");
        }
    }
}

#[test]
fn half_and_full_adder_demos_add() {
    let mut app = open("half_adder");
    for a in 0..2 {
        for b in 0..2 {
            let got = eval(&mut app, &[("A", a), ("B", b)], &["Sum", "Carry"]);
            assert_eq!(got, [(a + b) & 1, (a + b) >> 1], "A={a} B={b}");
        }
    }
    let mut app = open("full_adder");
    for a in 0..2 {
        for b in 0..2 {
            for c in 0..2 {
                let got = eval(
                    &mut app,
                    &[("A", a), ("B", b), ("Cin", c)],
                    &["Sum", "Cout"],
                );
                let s = a + b + c;
                assert_eq!(got, [s & 1, s >> 1], "A={a} B={b} Cin={c}");
            }
        }
    }
}

#[test]
fn ripple_adder_demo_matches_add() {
    let mut app = open("ripple_adder");
    for (a, b, c) in [
        (0, 0, 0),
        (1, 1, 0),
        (0xFF, 1, 0),
        (0x7F, 0x80, 1),
        (200, 100, 1),
        (0x55, 0xAA, 0),
        (0xFF, 0xFF, 1),
        (37, 91, 0),
    ] {
        let got = eval(
            &mut app,
            &[("A", a), ("B", b), ("Cin", c)],
            &["Sum (gates)", "Cout (gates)", "Sum (Add)", "Cout (Add)"],
        );
        let s = a + b + c;
        assert_eq!(got, [s & 0xFF, s >> 8, s & 0xFF, s >> 8], "{a} + {b} + {c}");
    }
}

#[test]
fn decoder_demo_is_one_hot() {
    let mut app = open("decoder");
    for s in 0..4 {
        let got = eval(&mut app, &[("S", s)], &["Y0", "Y1", "Y2", "Y3"]);
        let want: Vec<u64> = (0..4).map(|i| u64::from(i == s)).collect();
        assert_eq!(got, want, "S={s}");
    }
}

#[test]
fn mux_demos_select() {
    let mut app = open("mux");
    for sel in 0..2 {
        for (a, b) in [(0x12, 0xEF), (0xFF, 0x00), (0x5A, 0xA5)] {
            let got = eval(
                &mut app,
                &[("Sel", sel), ("A", a), ("B", b)],
                &["Y (gates)", "Y (Mux)"],
            );
            let want = if sel == 1 { b } else { a };
            assert_eq!(got, [want, want], "Sel={sel}");
        }
    }

    let mut app = open("mux_tree");
    let d = [0x11, 0x22, 0x33, 0x44];
    for (i, v) in d.iter().enumerate() {
        set_input(&mut app, &format!("D{i}"), *v);
    }
    for sel in 0..4 {
        let got = eval(&mut app, &[("Sel", sel)], &["Y (gates)", "Y (Mux)"]);
        assert_eq!(got, [d[sel as usize], d[sel as usize]], "Sel={sel}");
    }
}

/// The SR latch made of NORs and a Delay matches the `SrLatch` primitive at
/// every tick: set, hold, reset, hold, and both high (reset wins).
#[test]
fn sr_latch_demo_remembers_a_bit() {
    let mut app = open("sr_latch");
    let mut history = Vec::new();
    for (s, r) in [
        (0, 0),
        (1, 0),
        (1, 0),
        (0, 0),
        (0, 0),
        (0, 1),
        (0, 0),
        (1, 1),
        (0, 0),
    ] {
        set_input(&mut app, "S", s);
        set_input(&mut app, "R", r);
        for _ in 0..2 {
            run(&mut app, 1);
            let q = value_at(&mut app, "Q (gates)");
            assert_eq!(q, value_at(&mut app, "Q (SrLatch)"), "S={s} R={r}");
            history.push(q);
        }
    }
    // Two ticks per step. A change shows on the second tick (the Delay
    // commits at the end of the first), exactly as for the primitive.
    assert_eq!(
        history,
        [0, 0, 0, 1, 1, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0],
        "powers up reset, sets, holds, resets, holds, both high resets"
    );
    // Qn is the complement while S and R are low.
    assert_eq!(value_at(&mut app, "Qn (gates)"), 1);
}

#[test]
fn d_latch_demo_is_transparent_while_enabled() {
    let mut app = open("d_latch");
    // The gate-built latch answers one tick after the primitive.
    let mut step = |d: u64, e: u64, want: u64| {
        set_input(&mut app, "D", d);
        set_input(&mut app, "E", e);
        run(&mut app, 2);
        assert_eq!(value_at(&mut app, "Q (gates)"), want, "D={d} E={e}");
        assert_eq!(value_at(&mut app, "Q (DLatch)"), want, "D={d} E={e}");
    };
    step(1, 0, 0); // closed: keeps 0
    step(1, 1, 1); // open: follows D
    step(0, 1, 0);
    step(1, 1, 1);
    step(0, 0, 1); // closed: keeps 1
    step(1, 0, 1);
    step(0, 1, 0);
}

#[test]
fn flip_flop_demo_changes_on_rising_edges_only() {
    let mut app = open("flip_flop");
    let mut step = |inputs: &[(&str, u64)], want: u64, what: &str| {
        for &(label, value) in inputs {
            set_input(&mut app, label, value);
        }
        run(&mut app, 3);
        assert_eq!(value_at(&mut app, "Q"), want, "{what}");
    };
    step(&[("D", 1)], 0, "clock low: no edge yet");
    step(&[("Clk", 1)], 1, "rising edge takes D");
    step(&[("D", 0)], 1, "clock high: D is ignored");
    step(&[("Clk", 0)], 1, "falling edge: no change");
    step(&[("D", 1), ("D", 0)], 1, "clock low: D is ignored");
    step(&[("Clk", 1)], 0, "rising edge takes D");
}

#[test]
fn register_demo_loads_on_edges_with_load_set() {
    let mut app = open("register");
    let edge = |app: &mut App| {
        set_input(app, "Clk", 0);
        run(app, 2);
        set_input(app, "Clk", 1);
        run(app, 2);
    };
    set_input(&mut app, "D", 0x5A);
    run(&mut app, 2);
    assert_eq!(value_at(&mut app, "Q (gates)"), 0, "Load is 0");
    set_input(&mut app, "Load", 1);
    run(&mut app, 2);
    assert_eq!(value_at(&mut app, "Q (gates)"), 0, "no clock edge yet");
    assert_eq!(value_at(&mut app, "Q (Register)"), 0x5A, "no clock");
    edge(&mut app);
    assert_eq!(value_at(&mut app, "Q (gates)"), 0x5A);
    set_input(&mut app, "Load", 0);
    set_input(&mut app, "D", 0x33);
    edge(&mut app);
    assert_eq!(value_at(&mut app, "Q (gates)"), 0x5A, "holds without Load");
    assert_eq!(value_at(&mut app, "Q (Register)"), 0x5A);
    set_input(&mut app, "Load", 1);
    edge(&mut app);
    assert_eq!(value_at(&mut app, "Q (gates)"), 0x33);
}

#[test]
fn register_file_demo_writes_and_reads() {
    let mut app = open("register_file");
    let edge = |app: &mut App| {
        set_input(app, "Clk", 0);
        run(app, 2);
        set_input(app, "Clk", 1);
        run(app, 2);
    };
    let values = [0x11, 0xA2, 0x5C, 0xFF];
    set_input(&mut app, "WE", 1);
    for (i, v) in values.iter().enumerate() {
        set_input(&mut app, "WAddr", i as u64);
        set_input(&mut app, "WData", *v);
        edge(&mut app);
    }
    // A write with WE low changes nothing.
    set_input(&mut app, "WE", 0);
    set_input(&mut app, "WAddr", 2);
    set_input(&mut app, "WData", 0);
    edge(&mut app);
    for i in 0..4u64 {
        let got = eval(&mut app, &[("RA", i), ("RB", 3 - i)], &["QA", "QB"]);
        assert_eq!(
            got,
            [values[i as usize], values[3 - i as usize]],
            "r{i} and r{}",
            3 - i
        );
    }
}

#[test]
fn alu_demo_computes() {
    let mut app = open("alu");
    for (a, b) in [
        (0u64, 0u64),
        (5, 3),
        (3, 5),
        (0xFF, 1),
        (0x80, 0x80),
        (0xF0, 0x3C),
        (77, 77),
    ] {
        let add = a + b;
        let sub = a.wrapping_sub(b) & 0xFF;
        let got = eval(
            &mut app,
            &[("A", a), ("B", b), ("Op", 0)],
            &["Y", "Carry", "Zero"],
        );
        assert_eq!(
            got,
            [add & 0xFF, add >> 8, u64::from(add & 0xFF == 0)],
            "{a} + {b}"
        );
        let got = eval(&mut app, &[("Op", 1)], &["Y", "Carry", "Zero"]);
        assert_eq!(
            got,
            [sub, u64::from(a >= b), u64::from(sub == 0)],
            "{a} - {b}"
        );
        let got = eval(&mut app, &[("Op", 2)], &["Y", "Zero"]);
        assert_eq!(got, [a & b, u64::from(a & b == 0)], "{a} & {b}");
        let got = eval(&mut app, &[("Op", 3)], &["Y", "Zero"]);
        assert_eq!(got, [a | b, u64::from(a | b == 0)], "{a} | {b}");
    }
}

/// Reference interpreter of the CPU demo's instruction set (see the demo's
/// description): `(out, pc)` when it halts.
fn run_program(program: &[u64]) -> (u64, u64) {
    let mut regs = [0u64; 4];
    let (mut pc, mut out) = (0u64, 0u64);
    for _ in 0..10_000 {
        let instr = program.get(pc as usize).copied().unwrap_or(0);
        let (op, rd, rs, imm) = (
            instr >> 12,
            (instr >> 10 & 3) as usize,
            (instr >> 8 & 3) as usize,
            instr & 0xFF,
        );
        let rt = (imm & 3) as usize;
        let mut next = (pc + 1) & 0xFF;
        match op {
            1 => regs[rd] = imm,
            2 => next = imm,
            3 if regs[rs] == 0 => next = imm,
            4 => regs[rd] = (regs[rs] + regs[rt]) & 0xFF,
            5 => regs[rd] = regs[rs].wrapping_sub(regs[rt]) & 0xFF,
            6 => regs[rd] = regs[rs] & regs[rt],
            7 => regs[rd] = regs[rs] | regs[rt],
            8 => out = regs[rs],
            15 => return (out, pc),
            _ => {}
        }
        pc = next;
    }
    panic!("the program did not halt");
}

#[test]
fn cpu_demo_runs_its_program() {
    // The program, read from the demo file, and its expected result.
    let cpu = demos().get("cpu").unwrap();
    let program = cpu
        .workspace
        .root
        .nodes
        .iter()
        .find_map(|n| match (&n.kind, n.label.as_deref()) {
            (GateKind::Rom { contents, .. }, Some("program")) => Some(contents.clone()),
            _ => None,
        })
        .expect("a program ROM");
    let (out, halt_pc) = run_program(&program);
    assert_eq!(out, 35, "5 x 7");

    let mut app = open("cpu");
    assert_eq!(value_at(&mut app, "Halted"), 0);
    let mut ticks = 0;
    while value_at(&mut app, "Halted") == 0 {
        run(&mut app, 4);
        ticks += 4;
        assert!(ticks < 1000, "the CPU did not halt");
    }
    run(&mut app, 8);
    assert_eq!(value_at(&mut app, "Out"), out);
    assert_eq!(value_at(&mut app, "PC"), halt_pc);
    assert_eq!(value_at(&mut app, "Halted"), 1, "stays halted");
}
