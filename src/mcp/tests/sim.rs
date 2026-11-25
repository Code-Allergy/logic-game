//! Tests of stored tests, the rig and experiment scripts.

use serde_json::{Map, Value, json};

use super::{adder8, edit, empty, refused, scratch};
use crate::editor::workspace::WorkspaceStore;
use crate::mcp::batch::Place;
use crate::mcp::rig::Rig;
use crate::mcp::run::{self, RunState};
use crate::mcp::session::Session;
use crate::mcp::testing;
use crate::sim::WorkspaceDocument;
use crate::sim::demos::catalog;

fn object(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        other => panic!("not an object: {other}"),
    }
}

/// Run a script on a fresh simulation of the root.
fn script(ws: &WorkspaceDocument, script: Value) -> run::Outcome {
    let mut state = RunState::new(Rig::build(ws, Place::Root).unwrap());
    run::run(&mut state, &object(script)).unwrap_or_else(|e| panic!("{e}"))
}

fn script_error(ws: &WorkspaceDocument, script: Value) -> String {
    let mut state = RunState::new(Rig::build(ws, Place::Root).unwrap());
    match run::run(&mut state, &object(script)) {
        Ok(o) => panic!("the script should not run: {}", o.text),
        Err(e) => e,
    }
}

/// An 8-bit counter with an enable, a chip `counter` placed twice in the
/// root (`lo` counts, `hi` counts when `lo` is 255), and a RAM and a ROM.
fn machine() -> WorkspaceDocument {
    let (ws, report) = edit(
        &empty(),
        json!({"edits": [
            {"circuit": "counter", "ops": [
                {"add": [
                    {"name": "En", "type": "Input"},
                    {"name": "one", "type": "Const", "w": 8, "value": 1},
                    {"name": "zero", "type": "Const", "value": 0},
                    {"name": "inc", "type": "Add", "w": 8},
                    {"name": "r", "type": "Register", "w": 8},
                    {"name": "Q", "type": "Output", "w": 8},
                    {"name": "Wrap", "type": "Output"}
                ]},
                {"wire": [["r", "inc.a", "Q"], ["one", "inc.b"], ["zero", "inc.cin"],
                          ["inc.sum", "r.d"], ["En", "r.en"], ["inc.cout", "Wrap"]]}
            ]},
            {"ops": [
                {"add": [
                    {"name": "Go", "type": "Input"},
                    {"name": "lo", "use": "counter"},
                    {"name": "hi", "use": "counter"},
                    {"name": "carry", "type": "And"},
                    {"name": "Count", "type": "Output", "w": 16},
                    {"name": "rom", "type": "Rom", "addr_bits": 8, "w": 8},
                    {"name": "Data", "type": "Output", "w": 8},
                    {"name": "ram", "type": "Ram", "addr_bits": 4, "w": 8},
                    {"name": "we", "type": "Input"},
                    {"name": "Mem", "type": "Output", "w": 8}
                ]},
                {"wire": [["Go", "lo.En", "carry.a"], ["lo.Wrap", "carry.b"], ["carry", "hi.En"],
                          ["lo.Q", "Count:0..8", "rom.addr"], ["hi.Q", "Count:8..16"],
                          ["rom", "Data"], ["lo.Q:0..4", "ram.addr"], ["lo.Q", "ram.wdata"],
                          ["we", "ram.we"], ["ram", "Mem"]]}
            ]}
        ]}),
    );
    assert!(report.build.is_ok(), "{report}");
    ws
}

#[test]
fn a_script_sets_runs_expects_and_reads() {
    let ws = machine();
    let out = script(
        &ws,
        json!({"steps": [
            {"set": {"Go": 1}},
            {"until": "Count == 5", "max_ticks": 100},
            {"expect": {"Count": 5, "lo/r": 5, "hi/r.q": 0}},
            {"read": ["Count", "lo/inc.sum", "lo/r:0..2"]},
            {"until": "Count == 0x0102", "max_ticks": 1000},
            {"expect": {"hi/r": 1}}
        ]}),
    );
    assert!(!out.failed, "{}", out.text);
    assert!(
        out.text.contains("until Count == 5: reached at tick"),
        "{}",
        out.text
    );
    assert!(out.text.contains("  Count = 0x0005 (5)"), "{}", out.text);
    assert!(out.text.contains("  lo/inc.sum = 0x06 (6)"), "{}", out.text);
    assert!(out.text.contains("  lo/r:0..2 = 1 (1)"), "{}", out.text);
    assert!(out.text.ends_with("ok\n"), "{}", out.text);

    // A failed expectation ends the run and says what it saw.
    let out = script(
        &ws,
        json!({"steps": [{"set": {"Go": 1}}, {"ticks": 10}, {"expect": {"Count": 3}}, {"ticks": 5}]}),
    );
    assert!(out.failed);
    assert!(
        out.text
            .contains("FAILED steps[2]: expect at tick 10: Count: expected 0x0003, got 0x"),
        "{}",
        out.text
    );
    assert!(out.text.contains("ran 10 ticks"), "{}", out.text);

    let out = script(
        &ws,
        json!({"steps": [{"until": "Count == 9", "max_ticks": 20}]}),
    );
    assert!(out.failed);
    assert!(
        out.text
            .contains("until Count == 9: not reached in 20 ticks (tick 20; Count=0x0000)"),
        "{}",
        out.text
    );
}

#[test]
fn a_misspelt_script_runs_nothing_and_says_where() {
    let ws = machine();
    for (bad, said) in [
        (
            json!({"steps": [{"ticks": 5}, {"expect": {"Cuont": 1}}]}),
            "steps[1]: signal \"Cuont\": no part \"Cuont\" in \"test\"",
        ),
        (
            json!({"steps": [{"read": ["lo/nothing"]}]}),
            "no part \"nothing\" in \"counter\"",
        ),
        (
            json!({"steps": [{"read": ["Go/r"]}]}),
            "\"Go\" is not a chip",
        ),
        (
            json!({"steps": [{"set": {"Nope": 1}}]}),
            "there is no input \"Nope\" (inputs: Go, we)",
        ),
        (
            json!({"steps": [{"set": {"Go": 2}}]}),
            "does not fit input \"Go\" (1 bits)",
        ),
        (json!({"steps": [{"jump": 1}]}), "unknown step"),
        (
            json!({"steps": [{"until": "Count == 1"}]}),
            "\"until\" needs \"max_ticks\"",
        ),
        (
            json!({"steps": [{"until": "Count ==", "max_ticks": 5}]}),
            "\"Count ==\"",
        ),
        (
            json!({"steps": [{"compare": {"rows": [[1]]}}]}),
            "put a \"record\" step first",
        ),
        (
            json!({"steps": [{"read": ["ram@0x00..0x200"]}]}),
            "inside the memory's 16 words",
        ),
        (
            json!({"steps": [{"read": ["lo@3"]}]}),
            "\"lo\" is a chip, not a memory",
        ),
        (json!({"steps": [], "speed": 1}), "unknown field \"speed\""),
        (json!({"steps": [{"ticks": 2000000}]}), "limit of ticks"),
        (
            json!({"steps": [{"restore": "never"}]}),
            "no snapshot \"never\" (snapshots: none)",
        ),
    ] {
        let e = script_error(&ws, bad.clone());
        assert!(e.contains(said), "{bad}: {e}");
    }
}

#[test]
fn recordings_go_to_a_trace_and_compare_with_expected_rows() {
    let ws = machine();
    let dir = scratch("trace");
    let csv = dir.join("out/run.csv");
    let out = script(
        &ws,
        json!({"signals": {"n": "lo/r.q"}, "trace": csv, "steps": [
            {"set": {"Go": 1}},
            {"record": ["n", "Count"], "when": "n & 1"},
            {"ticks": 12}
        ]}),
    );
    assert!(!out.failed, "{}", out.text);
    assert!(out.text.contains("recorded 6 rows:"), "{}", out.text);
    assert!(out.text.contains("n=0x01 Count=0x0001"), "{}", out.text);
    let text = std::fs::read_to_string(&csv).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines[0], "tick,n,Count");
    assert_eq!(lines.len(), 7, "{text}");
    assert!(lines[1].ends_with(",0x01,0x0001"), "{text}");

    // The trace is what a later run is compared with: by header names.
    let again = json!({"signals": {"n": "lo/r.q"}, "steps": [
        {"set": {"Go": 1}},
        {"record": ["Count", "n"], "when": "n & 1"},
        {"compare": {"file": csv, "columns": [null, "n", "Count"], "skip_lines": 1}},
        {"ticks": 12}
    ]});
    let out = script(&ws, again.clone());
    assert!(!out.failed, "{}", out.text);
    assert!(out.text.contains("compared 6 rows with"), "{}", out.text);
    assert!(out.text.contains("all match"), "{}", out.text);

    // A changed circuit (the counter adds 2) differs at the first row.
    let (broken, _) = edit(
        &ws,
        json!({"circuit": "counter", "ops": [{"set": {"part": "one", "value": 3}}]}),
    );
    let out = script(&broken, again);
    assert!(out.failed, "{}", out.text);
    assert!(
        out.text.contains("first difference at row 1 of"),
        "{}",
        out.text
    );
    assert!(
        out.text.contains("  n: expected 0x01, got 0x03"),
        "{}",
        out.text
    );
    assert!(
        out.text.contains("rows before it (and the differing one):"),
        "{}",
        out.text
    );

    // Inline rows, with nothing expected where null; a log with names.
    let out = script(
        &ws,
        json!({"steps": [
            {"set": {"Go": 1}}, {"record": ["Count", "Data"]},
            {"compare": {"rows": [[0, null], [1, 0], [2, 0], [9, 0]], "context": 1}},
            {"ticks": 10}
        ]}),
    );
    assert!(out.failed);
    assert!(
        out.text
            .contains("first difference at row 4 of rows (tick 4)"),
        "{}",
        out.text
    );
    assert!(
        out.text.contains("Count: expected 0x0009, got 0x0003"),
        "{}",
        out.text
    );
    let log = dir.join("cpu.log");
    std::fs::write(&log, "# pc then registers\n0000  NOP   COUNT:0000 X:00\n0001  NOP   COUNT:0001 X:00\n0002  NOP   COUNT:0003\n").unwrap();
    let out = script(
        &ws,
        json!({"steps": [
            {"set": {"Go": 1}}, {"record": ["Count"]},
            {"compare": {"file": log, "radix": 16, "columns": [null]}},
            {"ticks": 10}
        ]}),
    );
    assert!(out.failed);
    assert!(
        out.text.contains("first difference at row 3"),
        "{}",
        out.text
    );

    // A VCD trace holds changes only.
    let vcd = dir.join("run.vcd");
    let out = script(
        &ws,
        json!({"trace": vcd, "steps": [{"record": ["Go", "Count"]}, {"ticks": 3}, {"set": {"Go": 1}}, {"ticks": 4}]}),
    );
    assert!(!out.failed, "{}", out.text);
    let text = std::fs::read_to_string(&vcd).unwrap();
    assert!(
        text.contains("$var wire 1 ! Go $end") && text.contains("$var wire 16 \" Count $end"),
        "{text}"
    );
    assert_eq!(
        text.matches("\n0!").count() + text.matches("\n1!").count(),
        2,
        "{text}"
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn memories_are_read_loaded_and_stored() {
    let ws = machine();
    let dir = scratch("memory");
    // A file with a 4-byte header, as a ROM dump has.
    let image = dir.join("image.bin");
    let bytes: Vec<u8> = (0..36u8)
        .map(|i| if i < 4 { 0xEE } else { 0xA0 + (i - 4) })
        .collect();
    std::fs::write(&image, &bytes).unwrap();
    let dump = dir.join("ram.hex");
    let out = script(
        &ws,
        json!({"steps": [
            {"load": {"part": "rom", "file": image, "skip": 4, "length": 16, "at": 2}},
            {"load": {"part": "ram", "data": [7, "0x08", 9], "at": 13}},
            {"set": {"Go": 1}}, {"ticks": 4},
            {"expect": {"Count": 3, "Data": 0xA1, "rom@2": 0xA0, "ram@14": 8}},
            {"set": {"we": 1}}, {"ticks": 3},
            {"read": ["rom@0x00..0x06", "ram@0..16"]},
            {"store": {"part": "ram", "file": dump}}
        ]}),
    );
    assert!(!out.failed, "{}", out.text);
    assert!(out.text.contains("loaded 16 words at 2"), "{}", out.text);
    assert!(
        out.text
            .contains("rom@0x00..0x06 0x0000: 00 00 A0 A1 A2 A3"),
        "{}",
        out.text
    );
    assert!(
        out.text.contains("ram@0..16 0x0000: 00 00 00 00 04 05 "),
        "{}",
        out.text
    );
    let stored = std::fs::read_to_string(&dump).unwrap();
    assert!(stored.starts_with("00 00 00 00 04 05 "), "{stored}");
    assert!(stored.trim_end().ends_with("07 08 09"), "{stored}");

    // Loading for a run leaves the workspace alone; `set` with `load`
    // changes the part itself.
    let e = script_error(
        &ws,
        json!({"steps": [{"load": {"part": "rom", "file": image, "at": 250}}]}),
    );
    assert!(e.contains("past the memory's 256 words"), "{e}");
    let (loaded, _) = edit(
        &ws,
        json!({"ops": [{"set": {"part": "rom", "load": {"file": image, "skip": 4, "length": 3}}}]}),
    );
    let rom = loaded
        .root
        .nodes
        .iter()
        .find(|n| n.label.as_deref() == Some("rom"))
        .unwrap();
    assert_eq!(
        rom.kind,
        crate::sim::GateKind::Rom {
            addr_bits: 8,
            w: 8,
            contents: vec![0xA0, 0xA1, 0xA2]
        }
    );
    let e = refused(
        &ws,
        json!({"ops": [{"set": {"part": "lo", "load": {"data": [1]}}}]}),
    );
    assert!(e.contains("is a chip"), "{e}");
    let e = refused(
        &ws,
        json!({"ops": [{"set": {"part": "carry", "load": {"data": [1]}}}]}),
    );
    assert!(e.contains("\"load\" sets the contents of a Rom"), "{e}");
    let e = refused(
        &ws,
        json!({"ops": [{"set": {"part": "rom", "load": {"data": [256]}}}]}),
    );
    assert!(e.contains("wider than the memory's 8 bits"), "{e}");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn snapshots_rewind_and_sessions_continue() {
    let dir = scratch("sessions");
    let mut s = Session::new(WorkspaceStore::in_dir(&dir));
    s.workspace_tool("new", Some("machine"), false).unwrap();
    // The session's workspace is the machine.
    let request = json!({"edits": [
        {"circuit": "counter", "ops": [
            {"add": [{"name": "En", "type": "Input"}, {"name": "one", "type": "Const", "w": 8, "value": 1},
                     {"name": "zero", "type": "Const", "value": 0}, {"name": "inc", "type": "Add", "w": 8},
                     {"name": "r", "type": "Register", "w": 8}, {"name": "Q", "type": "Output", "w": 8}]},
            {"wire": [["r", "inc.a", "Q"], ["one", "inc.b"], ["zero", "inc.cin"], ["inc.sum", "r.d"], ["En", "r.en"]]}
        ]},
        {"ops": [{"add": [{"name": "Go", "type": "Input"}, {"name": "c", "use": "counter"},
                          {"name": "Count", "type": "Output", "w": 8}]},
                 {"wire": [["Go", "c.En"], ["c.Q", "Count"]]}]}
    ]});
    s.edit_tool(object(request), None, false).unwrap();

    let run = |s: &mut Session, session: Option<&str>, steps: Value| {
        s.run_tool(None, session, object(json!({"steps": steps})), None)
    };
    let out = run(&mut s, Some("a"), json!([
        {"set": {"Go": 1}}, {"until": "Count == 10", "max_ticks": 50}, {"snapshot": "ten"}, {"ticks": 20}
    ])).unwrap();
    assert!(out.contains("snapshot \"ten\" at tick"), "{out}");
    // The next call goes on from there, and can go back.
    let out = run(
        &mut s,
        Some("a"),
        json!([{"expect": {"Count": 30}}, {"restore": "ten"}, {"expect": {"Count": 10}},
                                            {"ticks": 1}, {"expect": {"Count": 11}}]),
    )
    .unwrap();
    assert!(out.starts_with("session \"a\" continues at tick"), "{out}");
    // A new signal in a later call is readable too, with the state kept.
    let out = run(&mut s, Some("a"), json!([{"expect": {"c/inc.sum": 12}}])).unwrap();
    assert!(out.contains("ok"), "{out}");
    // Without a session every call starts at power-on.
    let out = run(&mut s, None, json!([{"expect": {"Count": 0}}])).unwrap();
    assert!(out.contains("now at tick 0"), "{out}");
    // A failed experiment is the tool's error, with the report.
    let e = run(&mut s, Some("a"), json!([{"expect": {"Count": 99}}])).unwrap_err();
    assert!(e.contains("FAILED steps[0]: expect"), "{e}");

    // An edit drops the session: its simulation is of the old circuit.
    s.edit_tool(
        object(json!({"ops": [{"add": [{"name": "extra", "type": "Not"}]}]})),
        None,
        false,
    )
    .unwrap();
    let out = run(&mut s, Some("a"), json!([{"expect": {"Count": 0}}])).unwrap();
    assert!(out.starts_with("session \"a\" was dropped"), "{out}");

    // A script from a file names its own circuit.
    let file = dir.join("count.json");
    std::fs::write(&file, r#"{"circuit": "counter", "steps": [{"set": {"En": 1}}, {"ticks": 4}, {"expect": {"Q": 3}}]}"#).unwrap();
    let out = s.run_tool(None, None, Map::new(), Some(&file)).unwrap();
    assert!(out.contains("ran 4 ticks"), "{out}");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn stored_tests_run_as_a_suite_and_report_the_first_failing_case() {
    let ws = adder8();
    let (ws, report) = edit(
        &ws,
        json!({"edits": [
            {"circuit": "half_adder", "ops": [{"tests": [
                {"name": "truth", "table": {"inputs": ["A", "B"], "outputs": ["Sum", "Carry"],
                                            "rows": [[0, 0, 0, 0], [0, 1, 1, 0], [1, 0, 1, 0], [1, 1, 0, 1]]}}
            ]}]},
            {"circuit": "full_adder", "ops": [{"tests": [
                {"name": "adds", "formula": {"expect": {"Sum": "A ^ B ^ Cin", "Cout": "(A + B + Cin) >> 1"}}}
            ]}]},
            {"ops": [{"tests": [
                {"name": "adds", "formula": {"expect": {"Sum": "A + B + Cin", "Cout": "(A + B + Cin) >> 8"},
                                             "samples": 200, "seed": 4}},
                {"name": "carries", "sequence": {"steps": [
                    {"set": {"A": 255, "B": 1}, "expect": {"Sum": 0, "Cout": 1}},
                    {"set": {"B": 0}, "expect": {"Sum": 255, "Cout": 0}}]}},
                {"name": "by script", "script": {"steps": [
                    {"set": {"A": 3, "B": 4}}, {"ticks": 1}, {"expect": {"Sum": 7, "fa[0]/h1.Sum": 1}}]}}
            ]}]}
        ]}),
    );
    assert_eq!(
        report.circuits.iter().map(|c| c.tests).collect::<Vec<_>>(),
        [1, 1, 3],
        "{report}"
    );
    assert!(
        report
            .to_string()
            .contains("main: +3 tests -> 30 parts, 50 wires, 3 tests"),
        "{report}"
    );

    let suite = testing::run_suite(&ws, None, None).unwrap();
    assert_eq!(suite.failed(), 0, "{}", suite.text());
    assert_eq!(
        suite.text(),
        "5 of 5 tests passed in 3 circuit(s) (219 cases)\n"
    );
    let one = testing::run_suite(&ws, Some("full_adder"), None).unwrap();
    assert_eq!(
        one.text(),
        "1 of 1 tests passed in 1 circuit(s) (8 cases)\n"
    );

    // The outline and the status list them.
    let text = crate::mcp::view::outline(&ws, &ws.root, None, 5);
    assert!(text.contains("tests: adds, carries, by script"), "{text}");
    assert!(
        crate::mcp::view::status(&ws)
            .contains("chip half_adder: 6 parts, 6 wires, placed 2 time(s), 1 test(s)")
    );

    // Break the half adder: its own test fails, and every test above it.
    let (broken, _) = edit(
        &ws,
        json!({"circuit": "half_adder", "ops": [
            {"remove": ["c"]}, {"add": [{"name": "c", "type": "Or"}]},
            {"wire": [["A", "c.a"], ["B", "c.b"], ["c", "Carry"]]}
        ]}),
    );
    let suite = testing::run_suite(&broken, None, None).unwrap();
    let text = suite.text();
    assert_eq!(suite.failed(), 5, "{text}");
    assert!(text.contains("FAILED half_adder / truth: Case 2: inputs A=0, B=1; expected Sum=1, Carry=0, got Sum=1, Carry=1."), "{text}");
    assert!(
        text.contains(
            "FAILED main / by script: FAILED steps[2]: expect at tick 1: Sum: expected 0x07, got 0x"
        ),
        "{text}"
    );
    assert!(
        text.ends_with("0 of 5 tests passed in 3 circuit(s) (0 cases)\n"),
        "{text}"
    );
    let named = testing::run_suite(&broken, None, Some("truth")).unwrap();
    assert_eq!(named.results.len(), 1);

    // Tests are replaced by name and removed by name.
    let (ws2, report) = edit(
        &ws,
        json!({"ops": [{"remove_tests": ["carries"]},
                       {"tests": {"name": "adds", "table": {"inputs": ["A"], "outputs": ["Sum"], "rows": [[5, 5]]}}}]}),
    );
    assert_eq!(
        (
            report.circuits[0].tests_set,
            report.circuits[0].tests_removed,
            report.circuits[0].tests
        ),
        (1, 1, 2)
    );
    assert_eq!(
        testing::run_suite(&ws2, Some("main"), Some("adds"))
            .unwrap()
            .text(),
        "1 of 1 tests passed in 1 circuit(s) (1 cases)\n"
    );
    let (ws3, _) = edit(&ws2, json!({"ops": [{"remove_tests": true}]}));
    assert!(!ws3.tests.contains_key(&ws3.root.id));
    assert!(
        testing::run_suite(&ws3, Some("main"), None)
            .unwrap_err()
            .contains("has no tests")
    );
}

#[test]
fn a_test_that_cannot_run_is_refused_when_it_is_set() {
    let ws = adder8();
    for (test, said) in [
        (
            json!({"name": "t", "table": {"inputs": ["X"], "outputs": ["Sum"], "rows": [[0, 0]]}}),
            "tests: test \"t\": there is no input \"X\"",
        ),
        (
            json!({"name": "t", "formula": {"expect": {"Sum": "A +"}}}),
            "\"A +\"",
        ),
        (
            json!({"name": "t", "formula": {"expect": {"Sum": "A + B"}}}),
            "give \"samples\"",
        ),
        (
            json!({"name": "t", "wiggle": {}}),
            "unknown kind \"wiggle\"",
        ),
        (json!({"table": {}}), "a test needs a \"name\""),
        (
            json!({"name": "t", "equivalent": {"to": "nothing"}}),
            "no chip \"nothing\"",
        ),
        (
            json!({"name": "t", "equivalent": {"to": "full_adder"}}),
            "the ports differ: this chip has in A[8] B[8] Cin[1] | out Cout[1] Sum[8]; \"full_adder\" has in A[1] B[1] Cin[1]",
        ),
        (
            json!({"name": "t", "script": {"moves": []}}),
            "\"script\" takes",
        ),
    ] {
        let e = refused(&ws, json!({"ops": [{"tests": [test.clone()]}]}));
        assert!(e.contains(said), "{test}: {e}");
    }
    let e = refused(&ws, json!({"ops": [{"remove_tests": ["never"]}]}));
    assert!(e.contains("no test \"never\""), "{e}");
}

#[test]
fn a_gate_level_chip_is_checked_against_its_word_level_reference() {
    // The reference: one Add part with the adder's ports.
    let (ws, _) = edit(
        &adder8(),
        json!({"edits": [
            {"circuit": "add_ref", "ops": [
                {"add": [{"name": "A", "type": "Input", "w": 8}, {"name": "B", "type": "Input", "w": 8},
                         {"name": "Cin", "type": "Input"}, {"name": "add", "type": "Add", "w": 8},
                         {"name": "Sum", "type": "Output", "w": 8}, {"name": "Cout", "type": "Output"}]},
                {"wire": [["A", "add.a"], ["B", "add.b"], ["Cin", "add.cin"], ["add.sum", "Sum"], ["add.cout", "Cout"]]}
            ]},
            {"ops": [{"tests": [{"name": "same as Add", "equivalent": {"to": "add_ref", "samples": 500}}]}]}
        ]}),
    );
    assert_eq!(
        testing::run_suite(&ws, Some("main"), None).unwrap().text(),
        "1 of 1 tests passed in 1 circuit(s) (500 cases)\n"
    );
    // Cut the carry chain between bits 3 and 4.
    let (broken, _) = edit(
        &ws,
        json!({"ops": [{"unwire": ["fa[4].Cin"]}, {"add": [{"name": "nc", "type": "Const", "value": 0}]},
                       {"wire": [["nc", "fa[4].Cin"]]}]}),
    );
    let text = testing::run_suite(&broken, Some("main"), None)
        .unwrap()
        .text();
    assert!(
        text.starts_with("FAILED main / same as Add: case "),
        "{text}"
    );
    assert!(
        text.contains("Sum: this chip gives ") && text.contains("\"add_ref\" gives "),
        "{text}"
    );
}

/// The gallery's 16-bit CPU of gates and its word-level twin: the scale a
/// real design has.
fn cpu(id: &str) -> WorkspaceDocument {
    catalog()
        .expect("demos load")
        .get(id)
        .expect("gallery item")
        .workspace
        .as_ref()
        .clone()
}

#[test]
fn the_gate_level_alu_matches_the_word_level_one() {
    let mut ws = cpu("cpu16");
    ws.absorb(&cpu("cpu16_rtl"));
    let (ws, report) = edit(
        &ws,
        json!({"circuit": "16-bit ALU", "ops": [{"tests": [
            {"name": "same as rtl", "equivalent": {"to": "16-bit ALU (word primitives)", "samples": 3000, "seed": 16}}
        ]}]}),
    );
    assert_eq!(report.circuits[0].tests, 1, "{report}");
    let text = testing::run_suite(&ws, Some("16-bit ALU"), None)
        .unwrap()
        .text();
    assert_eq!(text, "1 of 1 tests passed in 1 circuit(s) (3000 cases)\n");
}

#[test]
fn both_cpus_print_the_primes_and_a_trace_pins_one_down() {
    let dir = scratch("cpu");
    let trace = dir.join("rtl.csv");
    // The word-level CPU runs its program to the halt; its PC goes to a file.
    let rtl = cpu("cpu16_rtl");
    let out = script(
        &rtl,
        json!({"trace": trace, "max_ticks": 200000, "steps": [
            {"record": ["PC", "Out_value"]},
            {"until": "Halted", "max_ticks": 100000},
            {"expect": {"Out_value": 241}}
        ]}),
    );
    assert!(!out.failed, "{}", out.text);
    // The same run again matches its own trace, row for row.
    let replay = json!({"max_ticks": 200000, "steps": [
        {"record": ["PC", "Out_value"]},
        {"compare": {"file": trace, "columns": [null, "PC", "Out_value"], "skip_lines": 1}},
        {"until": "Halted", "max_ticks": 100000}
    ]});
    let out = script(&rtl, replay.clone());
    assert!(!out.failed, "{}", out.text);
    assert!(out.text.contains("all match"), "{}", out.text);
    // With one word of the program changed, the first difference is found.
    let mut state = RunState::new(Rig::build(&rtl, Place::Root).unwrap());
    let mut patched = object(replay);
    let Value::Array(steps) = &mut patched["steps"] else {
        unreachable!()
    };
    steps.insert(
        0,
        json!({"load": {"part": "program", "data": [0, 0, 0, 0, 0, 0], "at": 0}}),
    );
    let out = run::run(&mut state, &patched).unwrap();
    assert!(out.failed, "{}", out.text);
    assert!(out.text.contains("first difference at row"), "{}", out.text);

    // The CPU of gates (thousands of parts) reaches the same answer, and
    // a register deep inside it is readable by path.
    let gates = cpu("cpu16");
    let out = script(
        &gates,
        json!({"max_ticks": 2000000, "steps": [
            {"until": "Halted", "max_ticks": 1000000},
            {"expect": {"Out_value": 241}},
            {"read": ["PC", "data_RAM@0..8"]}
        ]}),
    );
    assert!(!out.failed, "{}", out.text);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_run_can_compile_without_the_optimising_passes() {
    let dir = scratch("off");
    let mut s = Session::new(WorkspaceStore::in_dir(&dir));
    s.workspace_tool("open_demo", Some("cpu16"), false).unwrap();
    let steps = json!([{"until": "Halted", "max_ticks": 1000000}, {"expect": {"Out_value": 241}}]);
    let on = s
        .run_tool(
            None,
            None,
            object(json!({"max_ticks": 2000000, "steps": steps})),
            None,
        )
        .unwrap();
    let off = s
        .run_tool(
            None,
            None,
            object(json!({"max_ticks": 2000000, "off": true, "steps": steps})),
            None,
        )
        .unwrap();
    assert!(
        off.starts_with("compiled with func_match, lift, words off\n"),
        "{off}"
    );
    // The same answer at the same tick, either way.
    assert_eq!(
        off.trim_start_matches("compiled with func_match, lift, words off\n"),
        on
    );
    let one = s
        .run_tool(
            None,
            None,
            object(json!({"off": ["lift"], "steps": [{"ticks": 3}]})),
            None,
        )
        .unwrap();
    assert!(one.starts_with("compiled with lift off\n"), "{one}");
    let e = s
        .run_tool(
            None,
            None,
            object(json!({"off": ["fast"], "steps": []})),
            None,
        )
        .unwrap_err();
    assert!(e.contains("\"off\" takes true, or passes of"), "{e}");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn an_until_is_charged_the_ticks_it_ran() {
    let ws = machine();
    // Reached after 2 of its 10 ticks: the 5 after it fit a budget of 10.
    let out = script(
        &ws,
        json!({"max_ticks": 10, "steps": [
            {"set": {"Go": 1}}, {"until": "Count == 1", "max_ticks": 10}, {"ticks": 5}
        ]}),
    );
    assert!(out.text.contains("ran 7 ticks"), "{}", out.text);
    // The budget still ends a wait that is not reached.
    let e = script_error(
        &ws,
        json!({"max_ticks": 10, "steps": [
            {"ticks": 4}, {"until": "Count == 1", "max_ticks": 10}
        ]}),
    );
    assert!(e.contains("limit of ticks (0 left)"), "{e}");
}

#[test]
fn each_recording_is_reported_with_its_own_columns() {
    let ws = machine();
    let out = script(
        &ws,
        json!({"steps": [
            {"set": {"Go": 1}}, {"record": ["Count"]}, {"ticks": 2},
            {"record": ["Data", "Go"]}, {"ticks": 3}
        ]}),
    );
    let text = &out.text;
    assert!(
        text.contains("recorded 2 rows:") && text.contains("recorded 3 rows:"),
        "{text}"
    );
    assert!(text.contains("tick 1: Count=0x0000\n"), "{text}");
    assert!(text.contains("tick 3: Data=0x00 Go=1\n"), "{text}");
    assert!(!text.contains("tick 1: Data="), "{text}");
}

#[test]
fn expected_rows_are_read_strictly() {
    let ws = machine();
    let dir = scratch("compare-strict");
    let run = |compare: Value| {
        json!({"signals": {"A": "Count", "B": "Data"}, "steps": [
            {"set": {"Go": 1}}, {"record": ["A", "B"]}, {"compare": compare}, {"ticks": 3}
        ]})
    };
    // No rows: refused, where it used to panic at the first recorded row.
    let e = script_error(&ws, run(json!({"rows": []})));
    assert!(e.contains("\"rows\" is empty"), "{e}");
    // A header of names that read as hex values is still a header.
    let log = dir.join("named.log");
    std::fs::write(&log, "A B\n0000 00\n0001 00\n").unwrap();
    let out = script(&ws, run(json!({"file": log, "radix": 16})));
    assert!(!out.failed, "{}", out.text);
    assert!(out.text.contains("compared 2 rows with"), "{}", out.text);
    // A value that cannot be read is an error, not a column left unchecked.
    std::fs::write(&log, "A:GG B:00\n").unwrap();
    let e = script_error(&ws, run(json!({"file": log, "radix": 16})));
    assert!(e.contains("\"A:GG\" is not a value"), "{e}");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_bad_set_or_load_costs_no_simulation() {
    let ws = machine();
    let mut state = RunState::new(Rig::build(&ws, Place::Root).unwrap());
    for set in [json!({"Nope": 1}), json!({"Go": 2})] {
        let e = run::run(
            &mut state,
            &object(json!({"steps": [{"ticks": 5}, {"set": set}]})),
        )
        .unwrap_err();
        assert!(e.starts_with("steps[1]: "), "{e}");
        assert_eq!(state.rig.tick_count(), 0, "{e}");
    }
    // A load past the end of the memory is refused before anything runs.
    let e = run::run(
        &mut state,
        &object(json!({"steps": [
            {"ticks": 5}, {"load": {"part": "ram", "data": [1], "at": 16}}
        ]})),
    )
    .unwrap_err();
    assert!(
        e.starts_with("steps[1]: ") && e.contains("past the memory"),
        "{e}"
    );
    assert_eq!(state.rig.tick_count(), 0, "{e}");
    // An address at the end of the address space is refused, not a panic.
    let e = script_error(
        &ws,
        json!({"steps": [{"load": {"part": "ram", "data": [1], "at": "0xFFFFFFFFFFFFFFFF"}}]}),
    );
    assert!(e.contains("past the memory's 16 words"), "{e}");
}

#[test]
fn inputs_keep_their_timing_over_a_recompile_and_a_restore() {
    let ws = machine();
    let mut state = RunState::new(Rig::build(&ws, Place::Root).unwrap());
    let mut run = |steps: Value| {
        let out = run::run(&mut state, &object(json!({"steps": steps}))).unwrap();
        assert!(!out.failed, "{}", out.text);
    };
    // `Go` is set but not seen yet. Reading a new net recompiles: the
    // counter must still not count at the next tick.
    run(json!([{"set": {"Go": 1}}]));
    run(json!([
        {"read": ["lo/inc.sum"]},
        {"ticks": 1}, {"expect": {"Count": 0}},
        {"ticks": 1}, {"expect": {"Count": 1}}
    ]));
    // A snapshot holds the inputs: restored, the counter counts again.
    run(json!([
        {"snapshot": "counting"}, {"set": {"Go": 0}}, {"ticks": 3}, {"expect": {"Count": 2}},
        {"restore": "counting"}, {"expect": {"Count": 1}},
        {"ticks": 2}, {"expect": {"Count": 3}}
    ]));
}

#[test]
fn an_equivalence_test_fails_when_the_ports_no_longer_match() {
    let chip = |name: &str| {
        json!({"circuit": name, "ops": [
            {"add": [{"name": "A", "type": "Input"}, {"name": "X", "type": "Output"},
                     {"name": "Y", "type": "Output"}]},
            {"wire": [["A", "X", "Y"]]}
        ]})
    };
    let (ws, _) = edit(&empty(), json!({"edits": [chip("ref"), chip("dut")]}));
    let (ws, _) = edit(
        &ws,
        json!({"circuit": "dut", "ops": [{"tests": [{"name": "same", "equivalent": {"to": "ref"}}]}]}),
    );
    let text = testing::run_suite(&ws, Some("dut"), None).unwrap().text();
    assert!(text.starts_with("1 of 1 tests passed"), "{text}");
    // The tested chip loses an output: the test no longer covers it.
    let (ws, _) = edit(&ws, json!({"circuit": "dut", "ops": [{"remove": ["Y"]}]}));
    let text = testing::run_suite(&ws, Some("dut"), None).unwrap().text();
    assert!(
        text.starts_with("FAILED dut / same: the ports differ"),
        "{text}"
    );
}

#[test]
fn the_roots_tests_follow_a_save_as() {
    let dir = scratch("save-as-tests");
    let mut s = Session::new(WorkspaceStore::in_dir(&dir));
    s.workspace_tool("new", Some("first"), false).unwrap();
    let request = json!({"ops": [
        {"add": [{"name": "A", "type": "Input"}, {"name": "Y", "type": "Output"}]},
        {"wire": [["A", "Y"]]},
        {"tests": [{"name": "wire", "table": {"inputs": ["A"], "outputs": ["Y"], "rows": [[0, 0], [1, 1]]}}]}
    ]});
    s.edit_tool(object(request), None, false).unwrap();
    s.workspace_tool("save", None, false).unwrap();
    s.workspace_tool("save", Some("second"), false).unwrap();
    let out = s.test_tool(None, None).unwrap();
    assert!(out.starts_with("1 of 1 tests passed"), "{out}");
    // And they are in the next save too.
    s.workspace_tool("save", None, false).unwrap();
    let saved = WorkspaceStore::in_dir(&dir).read("second").unwrap();
    assert_eq!(saved.tests.get(&saved.root.id).map(Vec::len), Some(1));
    let _ = std::fs::remove_dir_all(dir);
}
