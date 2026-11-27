//! Tests of `expand` with `deep`: a hierarchy of chips to gates.

use serde_json::{Value, json};

use super::{edit, empty, refused};
use crate::mcp::{batch, chips, testing, view};
use crate::sim::demos::catalog;
use crate::sim::{CircuitDocument, GateKind, WorkspaceDocument};

fn chip<'a>(ws: &'a WorkspaceDocument, name: &str) -> &'a CircuitDocument {
    ws.definitions
        .values()
        .find(|d| d.name == name)
        .unwrap_or_else(|| panic!("no chip \"{name}\""))
}

fn has_chip(ws: &WorkspaceDocument, name: &str) -> bool {
    ws.definitions.values().any(|d| d.name == name)
}

/// Whether `doc` has a word-level part, itself or at any depth below.
fn has_word_parts(ws: &WorkspaceDocument, doc: &CircuitDocument) -> bool {
    doc.nodes.iter().any(|n| match n.kind {
        GateKind::SubCircuit { definition } => has_word_parts(ws, &ws.definitions[&definition]),
        ref kind => chips::expandable(kind),
    })
}

/// Three levels of word-level chips: `counter` places `datapath` (and a
/// gate-level `buf`), which places `adder` and `reg`.
fn counter_request() -> Value {
    json!({"edits": [
        {"circuit": "adder", "ops": [
            {"add": [
                {"name": "a", "type": "Input", "w": 8}, {"name": "b", "type": "Input", "w": 8},
                {"name": "add", "type": "Add", "w": 8}, {"name": "sum", "type": "Output", "w": 8}
            ]},
            {"wire": [["a", "add.a"], ["b", "add.b"], ["add.sum", "sum"]]}
        ]},
        {"circuit": "reg", "ops": [
            {"add": [
                {"name": "d", "type": "Input", "w": 8}, {"name": "en", "type": "Input"},
                {"name": "r", "type": "Register", "w": 8}, {"name": "q", "type": "Output", "w": 8}
            ]},
            {"wire": [["d", "r.d"], ["en", "r.en"], ["r", "q"]]}
        ]},
        {"circuit": "buf", "ops": [
            {"add": [
                {"name": "i", "type": "Input"}, {"name": "n", "type": "Not", "count": 2},
                {"name": "o", "type": "Output"}
            ]},
            {"wire": [["i", "n[0]"], ["n[0]", "n[1]"], ["n[1]", "o"]]}
        ]},
        {"circuit": "datapath", "ops": [
            {"add": [
                {"name": "en", "type": "Input"}, {"name": "step", "type": "Input", "w": 8},
                {"name": "inc", "use": "adder"}, {"name": "acc", "use": "reg"},
                {"name": "q", "type": "Output", "w": 8}
            ]},
            {"wire": [
                ["acc.q", "inc.a", "q"], ["step", "inc.b"], ["inc.sum", "acc.d"], ["en", "acc.en"]
            ]}
        ]},
        {"circuit": "counter", "ops": [
            {"add": [
                {"name": "en", "type": "Input"}, {"name": "step", "type": "Input", "w": 8},
                {"name": "limit", "type": "Input", "w": 8},
                {"name": "dp", "use": "datapath"}, {"name": "b", "use": "buf"},
                {"name": "cmp", "type": "Eq", "w": 8},
                {"name": "q", "type": "Output", "w": 8}, {"name": "hit", "type": "Output"}
            ]},
            {"wire": [
                ["en", "b"], ["b", "dp.en"], ["step", "dp.step"],
                ["dp.q", "cmp.a", "q"], ["limit", "cmp.b"], ["cmp", "hit"]
            ]}
        ]}
    ]})
}

/// The recipe of the documentation: a gate-level copy of a chip, tested
/// against the original.
fn gates_of(source: &str, copy: &str) -> Value {
    json!({"circuit": copy, "ops": [
        {"copy_of": source},
        {"expand": {"parts": true, "deep": true}},
        {"tests": [{"name": "same", "equivalent": {"to": source, "samples": 300, "hold": 2}}]}
    ]})
}

#[test]
fn deep_expansion_makes_gate_level_copies_of_a_hierarchy() {
    let (ws, _) = edit(&empty(), counter_request());
    let before = ws.clone();
    let (ws, report) = edit(&ws, gates_of("counter", "counter_gates"));
    assert!(report.build.is_ok(), "{report}");
    // In counter_gates: the Eq and the datapath; the buf has no words.
    let c = &report.circuits[0];
    assert_eq!(c.expanded, 2, "{report}");
    // datapath_gates, adder_gates, reg_gates, Eq_8_gates, Add_8_gates,
    // Register_8_gates, full_adder_cell.
    assert_eq!(c.chips_made, 7, "{report}");
    assert!(
        report
            .to_string()
            .contains("(2 expanded to gates, 7 chips made)"),
        "{report}"
    );
    for name in ["datapath_gates", "adder_gates", "reg_gates"] {
        assert!(has_chip(&ws, name), "{name}");
    }
    assert!(!has_chip(&ws, "buf_gates"));
    // Nothing word-level is left below the copy, and the originals are
    // as they were.
    assert!(!has_word_parts(&ws, chip(&ws, "counter_gates")));
    for name in ["counter", "datapath", "adder", "reg", "buf"] {
        assert_eq!(chip(&ws, name), chip(&before, name), "{name}");
    }
    let text = view::outline(&ws, chip(&ws, "counter_gates"), None, 200);
    assert!(
        text.contains("chips: Eq_8_gates x1, buf x1, datapath_gates x1"),
        "{text}"
    );

    // The copy does what the original does, at every depth.
    let suite = testing::run_suite(&ws, Some("counter_gates"), None).unwrap();
    assert_eq!(suite.failed(), 0, "{}", suite.text());
    assert_eq!(suite.results.len(), 1);

    // Again on a second copy: the copies are reused, nothing is made.
    let (ws2, report) = edit(&ws, gates_of("counter", "counter_gates2"));
    assert_eq!(report.circuits[0].chips_made, 0, "{report}");
    assert_eq!(ws2.definitions.len(), ws.definitions.len() + 1);
    let copy = |ws: &WorkspaceDocument, name: &str| {
        chip(ws, name)
            .nodes
            .iter()
            .find_map(|n| match n.kind {
                GateKind::SubCircuit { definition } if n.label.as_deref() == Some("dp") => {
                    Some(definition)
                }
                _ => None,
            })
            .unwrap()
    };
    assert_eq!(copy(&ws2, "counter_gates2"), copy(&ws2, "counter_gates"));

    // The same request gives the same workspace.
    let (again, _) = edit(
        &edit(&empty(), counter_request()).0,
        gates_of("counter", "counter_gates"),
    );
    assert_eq!(again, ws);
}

#[test]
fn a_gate_level_copy_follows_its_source() {
    let (ws, _) = edit(&empty(), counter_request());
    let (ws, _) = edit(&ws, gates_of("counter", "counter_gates"));
    let old = chip(&ws, "adder_gates").clone();

    // The adder now adds one more: carry in from a constant.
    let (ws, _) = edit(
        &ws,
        json!({"circuit": "adder", "ops": [
            {"add": [{"name": "one", "type": "Const", "value": 1}]},
            {"wire": [["one", "add.cin"]]}
        ]}),
    );
    // The old copy is stale; counter still uses the new adder.
    let (ws, report) = edit(&ws, gates_of("counter", "counter_gates2"));
    // adder_gates made again; datapath_gates is the same circuit.
    assert_eq!(report.circuits[0].chips_made, 1, "{report}");
    let new = chip(&ws, "adder_gates");
    assert_eq!(new.id, old.id);
    assert!(new.revision > old.revision);
    assert!(!new.same_content(&old));
    // Both copies of counter use the new adder_gates, and match counter.
    let suite = testing::run_suite(&ws, None, Some("same")).unwrap();
    assert_eq!(suite.failed(), 0, "{}", suite.text());
    assert_eq!(suite.results.len(), 2);
}

#[test]
fn deep_expansion_keeps_away_from_chips_a_person_made() {
    let (ws, _) = edit(&empty(), counter_request());
    // A chip of the name a copy would take, made by hand.
    let (ws, _) = edit(
        &ws,
        json!({"circuit": "adder_gates", "ops": [{"add": [{"name": "x", "type": "Input"}]}]}),
    );
    let mine = chip(&ws, "adder_gates").clone();
    let (ws, report) = edit(&ws, gates_of("datapath", "datapath_gates"));
    assert!(report.build.is_ok(), "{report}");
    assert_eq!(chip(&ws, "adder_gates"), &mine);
    assert!(has_chip(&ws, "adder_gates_2"));
    let suite = testing::run_suite(&ws, Some("datapath_gates"), None).unwrap();
    assert_eq!(suite.failed(), 0, "{}", suite.text());
}

#[test]
fn expand_takes_its_options_and_says_what_is_wrong() {
    let (ws, _) = edit(&empty(), counter_request());
    // Without deep a chip is refused, as before.
    let e = refused(
        &ws,
        json!({"circuit": "counter", "ops": [{"expand": ["dp"]}]}),
    );
    assert!(e.contains("\"deep\": true"), "{e}");
    let e = refused(
        &ws,
        json!({"circuit": "counter", "ops": [{"expand": {"parts": true, "deeep": true}}]}),
    );
    assert!(e.contains("unknown field \"deeep\""), "{e}");
    let e = refused(
        &ws,
        json!({"circuit": "counter", "ops": [{"expand": {"deep": "yes"}}]}),
    );
    assert!(e.contains("\"deep\" is true or false"), "{e}");

    // Named parts: only those. A chip with nothing to expand stays.
    let (named, report) = edit(
        &ws,
        json!({"circuit": "counter", "ops": [{"expand": {"parts": ["dp", "b"], "deep": true}}]}),
    );
    assert_eq!(report.circuits[0].expanded, 1, "{report}");
    let counter = chip(&named, "counter");
    assert!(
        counter
            .nodes
            .iter()
            .any(|n| n.kind == GateKind::Eq { w: 8 })
    );
    assert!(!has_chip(&named, "buf_gates"));
    // {"parts": true} without deep is the old {"expand": true}.
    let (flat, _) = edit(
        &ws,
        json!({"circuit": "counter", "ops": [{"expand": {"parts": true}}]}),
    );
    let (old, _) = edit(
        &ws,
        json!({"circuit": "counter", "ops": [{"expand": true}]}),
    );
    assert_eq!(flat, old);
    assert!(!has_chip(&flat, "datapath_gates"));
}

/// The gallery's 16-bit CPU of words: its ALU, and its register file
/// (which places a multiplexer chip), to gates, each tested against the
/// original.
#[test]
fn the_word_level_cpu_expands_to_gates_that_do_the_same() {
    let catalog = catalog().unwrap();
    let ws = catalog.get("cpu16_rtl").unwrap().workspace.clone();
    let names: Vec<String> = ws.definitions.values().map(|d| d.name.clone()).collect();
    let alu = names
        .iter()
        .find(|n| n.contains("ALU"))
        .unwrap_or_else(|| panic!("{names:?}"))
        .clone();
    let regs = names
        .iter()
        .find(|n| n.starts_with("Register file"))
        .unwrap_or_else(|| panic!("{names:?}"))
        .clone();
    let (ws, report) = edit(
        &ws,
        json!({"edits": [gates_of(&alu, "alu_gates"), gates_of(&regs, "regs_gates")]}),
    );
    assert!(report.build.is_ok(), "{report}");
    for name in ["alu_gates", "regs_gates"] {
        assert!(!has_word_parts(&ws, chip(&ws, name)), "{name}");
    }
    for def in ws.definitions.values() {
        assert!(
            def.nodes.len() <= batch::ADVISORY_PARTS,
            "{} has {} parts",
            def.name,
            def.nodes.len()
        );
    }
    let suite = testing::run_suite(&ws, None, Some("same")).unwrap();
    assert_eq!(suite.failed(), 0, "{}", suite.text());
    assert_eq!(suite.results.len(), 2);
    let text = view::outline(&ws, chip(&ws, "regs_gates"), None, 200);
    assert!(text.contains("_gates x"), "{text}");
}
