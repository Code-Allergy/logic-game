//! Tests of what the compiler did: the `compiled` view and test kind, the
//! `unoptimized` test kind, and build errors that name parts.

use std::sync::Arc;

use logi_sim::compile::Options;
use logi_sim::sim::Sim;
use serde_json::{Value, json};

use super::{adder8, edit, empty, refused, scratch};
use crate::editor::workspace::WorkspaceStore;
use crate::mcp::batch::{Place, find_circuit};
use crate::mcp::compiled::{self, Match};
use crate::mcp::rig::Rig;
use crate::mcp::session::Session;
use crate::mcp::testing::{self, Sampling};
use crate::mcp::view;
use crate::sim::WorkspaceDocument;
use crate::sim::demos::catalog;

/// The gallery's 16-bit CPU built from gates: adders, multiplexers,
/// registers and decoders as chips.
fn cpu16() -> WorkspaceDocument {
    catalog()
        .expect("demos load")
        .get("cpu16")
        .expect("gallery item")
        .workspace
        .as_ref()
        .clone()
}

fn place(ws: &WorkspaceDocument, circuit: &str) -> Place {
    find_circuit(ws, Some(circuit))
        .unwrap()
        .unwrap_or_else(|| panic!("no circuit {circuit}"))
}

fn with_tests(ws: &WorkspaceDocument, circuit: &str, tests: Value) -> WorkspaceDocument {
    let (ws, report) = edit(ws, json!({"circuit": circuit, "ops": [{"tests": tests}]}));
    assert!(report.circuits[0].tests > 0, "{report}");
    ws
}

fn suite(ws: &WorkspaceDocument, circuit: &str) -> String {
    testing::run_suite(ws, Some(circuit), None).unwrap().text()
}

#[test]
fn the_view_says_which_chips_matched_and_how_large_the_program_is() {
    let text = compiled::view(&adder8(), Place::Root).unwrap();
    assert_eq!(
        text,
        "compiled \"main\" as a root (2 chip definition(s) reached; the root itself is never \
         replaced)\n\
         \x20 full_adder: matched as [Add { a: 0, b: 1, inv_b: false, cin: In(2), carry: true }, \
         Add { a: 0, b: 1, inv_b: false, cin: In(2), carry: false }]\n\
         \x20 half_adder: not matched: no cheaper: 2 vs 2\n\
         program: 39 instructions (AddCarry 8, AddSum 8, Concat 7, Shr 16), 0 state words, \
         43 slots, 0 memories, depth 10\n\
         without func_match, lift and words: 63 instructions\n\
         a test that pins this (edit op \"tests\"):\n\
         \x20 {\"name\":\"compiled\",\"compiled\":{\"matched\":[\"full_adder\"],\
         \"instructions\":{\"max\":39}}}\n"
    );
    // The same call twice gives the same text.
    assert_eq!(compiled::view(&adder8(), Place::Root).unwrap(), text);
}

#[test]
fn the_gate_cpu_has_chips_that_match_and_chips_that_do_not() {
    let ws = cpu16();
    let i = compiled::insight(&ws, Place::Root).unwrap();
    assert!(
        matches!(&i.chips["16-bit carry-lookahead adder"], Match::Matched(n) if n.contains("Add {")),
        "{:?}",
        i.chips
    );
    assert!(matches!(
        &i.chips["16-bit 2-to-1 multiplexer"],
        Match::Matched(n) if n.contains("Mux")
    ));
    // A master latch on the clock's low phase, a slave on its high phase.
    assert!(
        matches!(
            &i.chips["D flip-flop (rising edge)"],
            Match::Matched(n) if n == "state [1-bit Register { en: !In(0), d: In(1) }, \
                1-bit Register { en: In(0), d: r0 }], outputs [r1]"
        ),
        "{:?}",
        i.chips["D flip-flop (rising edge)"]
    );
    assert!(
        matches!(&i.chips["16-bit ALU"], Match::Unmatched(why) if why.contains("no catalogue function fits"))
    );
    // A small chip with no catalogue function runs as a truth table.
    assert!(matches!(
        &i.chips["4-to-16 decoder with enable"],
        Match::Matched(n) if n.starts_with("table of 5 input bits")
    ));
    // The program ROM and the RAM, and the decoder's table (a ROM the
    // compiler adds).
    assert!(
        i.size.state_words > 0 && i.size.memories == 3,
        "{:?}",
        i.size
    );
    // The word-level passes are most of the story.
    assert!(
        i.plain_instructions > 10 * i.size.instructions,
        "{} vs {}",
        i.plain_instructions,
        i.size.instructions
    );
    // The view tool serves it for any circuit.
    let dir = scratch("compiled-view");
    let mut s = Session::new(WorkspaceStore::in_dir(&dir));
    s.workspace_tool("open_demo", Some("cpu16"), false).unwrap();
    let text = s
        .view_tool(Some("16-bit ALU"), Some("compiled"), None, None)
        .unwrap();
    assert!(
        text.starts_with("compiled \"16-bit ALU\" as a root (13 chip definition(s) reached"),
        "{text}"
    );
    assert!(
        text.contains("  16-bit carry-lookahead adder: matched as [Add {"),
        "{text}"
    );
    assert!(
        text.contains("without func_match, lift and words: "),
        "{text}"
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_chip_with_memory_is_not_analysed() {
    let (ws, _) = edit(
        &empty(),
        json!({"edits": [
            {"circuit": "store", "ops": [
                {"add": [{"name": "A", "type": "Input", "w": 2}, {"name": "D", "type": "Input", "w": 4},
                         {"name": "W", "type": "Input"}, {"name": "m", "type": "Ram", "addr_bits": 2, "w": 4},
                         {"name": "Q", "type": "Output", "w": 4}]},
                {"wire": [["A", "m.addr"], ["D", "m.wdata"], ["W", "m.we"], ["m", "Q"]]}
            ]},
            {"ops": [
                {"add": [{"name": "A", "type": "Input", "w": 2}, {"name": "D", "type": "Input", "w": 4},
                         {"name": "W", "type": "Input"}, {"name": "s", "use": "store"},
                         {"name": "Q", "type": "Output", "w": 4}]},
                {"wire": [["A", "s.A"], ["D", "s.D"], ["W", "s.W"], ["s.Q", "Q"]]}
            ]}
        ]}),
    );
    let text = compiled::view(&ws, Place::Root).unwrap();
    assert!(
        text.contains("  store: not analysed (a clock or memory is inside it"),
        "{text}"
    );
    assert!(text.contains("1 memories"), "{text}");
}

#[test]
fn a_compiled_test_pins_what_the_compiler_does() {
    let ws = cpu16();
    let ws = with_tests(
        &ws,
        "16-bit ALU",
        json!([{"name": "word level", "compiled": {
            "matched": {"16-bit carry-lookahead adder": "Add", "16-bit 2-to-1 multiplexer": true,
                        "Carry lookahead unit": "table"},
            "unmatched": ["16-bit 8-to-1 multiplexer"],
            "instructions": {"max": 60}
        }}]),
    );
    assert_eq!(
        suite(&ws, "16-bit ALU"),
        "1 of 1 tests passed in 1 circuit(s) (5 cases)\n"
    );
    // The test the view prints passes as it is.
    let i = compiled::insight(&ws, place(&ws, "16-bit ALU")).unwrap();
    let ws = with_tests(&ws, "16-bit ALU", json!([i.pinning_test()]));
    let text = testing::run_suite(&ws, Some("16-bit ALU"), Some("compiled"))
        .unwrap()
        .text();
    assert!(text.starts_with("1 of 1 tests passed"), "{text}");

    // Too tight, and wrong about what matches: every broken promise is said.
    let ws = with_tests(
        &ws,
        "16-bit ALU",
        json!([{"name": "too tight", "compiled": {
            "matched": {"16-bit carry-lookahead adder": {"exactly": "an adder"}, "16-bit 8-to-1 multiplexer": true},
            "unmatched": ["16-bit 2-to-1 multiplexer"],
            "instructions": {"max": 10}
        }}]),
    );
    let text = testing::run_suite(&ws, Some("16-bit ALU"), Some("too tight"))
        .unwrap()
        .text();
    assert!(
        text.starts_with("FAILED 16-bit ALU / too tight: "),
        "{text}"
    );
    for said in [
        "\"16-bit carry-lookahead adder\" matched as [Add {",
        "not exactly as an adder",
        "\"16-bit 8-to-1 multiplexer\" did not match: output 0: no catalogue function fits",
        "\"16-bit 2-to-1 multiplexer\" must not match, but matched as [Mux",
        "32 instructions, more than the 10 allowed",
    ] {
        assert!(text.contains(said), "{said}: {text}");
    }
    assert!(
        text.ends_with("0 of 1 tests passed in 1 circuit(s) (0 cases)\n"),
        "{text}"
    );

    // An exact count breaks on any change to the program.
    let ws = with_tests(
        &adder8(),
        "main",
        json!([{"name": "exact", "compiled": {"matched": ["full_adder"], "instructions": {"exactly": 39}}}]),
    );
    assert!(suite(&ws, "main").starts_with("1 of 1"));
    let (ws, _) = edit(
        &ws,
        json!({"ops": [
            {"add": [{"name": "n", "type": "Not"}, {"name": "NotCout", "type": "Output"}]},
            {"wire": [["fa[7].Cout", "n"], ["n", "NotCout"]]}
        ]}),
    );
    let text = suite(&ws, "main");
    assert!(
        text.starts_with("FAILED main / exact: 40 instructions, not exactly 39\n"),
        "{text}"
    );
}

#[test]
fn a_compiled_test_is_checked_when_it_is_set() {
    let ws = adder8();
    for (test, said) in [
        (
            json!({"name": "t", "compiled": {"matched": ["nope"]}}),
            "this circuit places no chip \"nope\"",
        ),
        (
            json!({"name": "t", "compiled": {"matched": ["half_adder"], "unmatched": ["half_adder"]}}),
            "\"half_adder\" is both matched and unmatched",
        ),
        (
            json!({"name": "t", "compiled": {"instructions": {"most": 3}}}),
            "unknown field \"most\"",
        ),
        (
            json!({"name": "t", "compiled": {"instructions": 3}}),
            "\"instructions\" takes {\"max\": n}",
        ),
        (
            json!({"name": "t", "compiled": {"matched": {"full_adder": 3}}}),
            "takes true, a text the match must contain",
        ),
        (json!({"name": "t", "compiled": {}}), "\"compiled\" takes"),
        (
            json!({"name": "t", "unoptimized": {"off": ["dce"]}}),
            "\"off\" lists passes: func_match, lift, words",
        ),
        (
            json!({"name": "t", "unoptimized": {"samples": 0}}),
            "\"samples\" must be 1 to",
        ),
        (
            json!({"name": "t", "unoptimized": {"to": "x"}}),
            "unknown field \"to\" (unoptimized has",
        ),
    ] {
        let e = refused(&ws, json!({"ops": [{"tests": [test.clone()]}]}));
        assert!(e.contains(said), "{test}: {e}");
    }
    // A chip is not tested on itself: its own root is never replaced.
    let e = refused(
        &ws,
        json!({"circuit": "full_adder", "ops": [{"tests": [
            {"name": "t", "compiled": {"matched": ["full_adder"]}}
        ]}]}),
    );
    assert!(e.contains("\"full_adder\" is this circuit"), "{e}");
}

#[test]
fn designs_behave_the_same_without_the_word_level_passes() {
    let ws = with_tests(
        &adder8(),
        "main",
        json!([
            {"name": "same", "unoptimized": {"samples": 2000, "seed": 1}},
            {"name": "same without matching", "unoptimized": {"samples": 500, "off": ["func_match"]}}
        ]),
    );
    assert_eq!(
        suite(&ws, "main"),
        "2 of 2 tests passed in 1 circuit(s) (2500 cases)\n"
    );

    // A counter with state, held for a few ticks per input.
    let (counter, _) = edit(
        &empty(),
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
                      ["inc.sum", "r.d"], ["En", "r.en"]]},
            {"tests": [{"name": "same", "unoptimized": {"samples": 500, "hold": 3}}]}
        ]}),
    );
    assert_eq!(
        suite(&counter, "main"),
        "1 of 1 tests passed in 1 circuit(s) (500 cases)\n"
    );

    // The gate CPU's ALU and its program counter (gate-built registers).
    let ws = cpu16();
    let ws = with_tests(
        &ws,
        "16-bit ALU",
        json!([{"name": "same", "unoptimized": {"samples": 2000, "seed": 7}}]),
    );
    let ws = with_tests(
        &ws,
        "Program counter",
        json!([{"name": "same", "unoptimized": {"samples": 300, "hold": 2}}]),
    );
    assert_eq!(
        suite(&ws, "16-bit ALU"),
        "1 of 1 tests passed in 1 circuit(s) (2000 cases)\n"
    );
    assert_eq!(
        suite(&ws, "Program counter"),
        "1 of 1 tests passed in 1 circuit(s) (300 cases)\n"
    );
}

/// The comparison an `unoptimized` test makes, on two programs that do
/// differ: an adder, and the same adder with its carry chain cut.
#[test]
fn the_comparison_reports_the_first_differing_case() {
    let ws = adder8();
    let (broken, _) = edit(
        &ws,
        json!({"ops": [{"unwire": ["fa[4].Cin"]}, {"add": [{"name": "nc", "type": "Const", "value": 0}]},
                       {"wire": [["nc", "fa[4].Cin"]]}]}),
    );
    let sim = |ws: &WorkspaceDocument, options: &Options| {
        let built = compiled::compile(ws, options).unwrap();
        Sim::new(Arc::new(built.program))
    };
    let (inputs, outputs) = testing::ports(&ws.root);
    let mut a = sim(&ws, &Options::default());
    let mut b = sim(
        &broken,
        &compiled::options_without(&["func_match".into(), "lift".into(), "words".into()]),
    );
    let e = testing::compare(
        &mut a,
        &mut b,
        &inputs,
        &outputs,
        &Sampling {
            samples: 1000,
            seed: 1,
            hold: 1,
        },
        ("as compiled", "with func_match, lift, words off"),
    )
    .unwrap_err();
    assert!(e.starts_with("case "), "{e}");
    assert!(
        e.contains(": as compiled gives ")
            && e.contains(", with func_match, lift, words off gives "),
        "{e}"
    );
    assert!(e.contains("inputs A="), "{e}");
}

/// Two NOT gates in a ring, with a way in and out.
fn ring_ops() -> Value {
    json!([
        {"add": [{"name": "x", "type": "Input"}, {"name": "a", "type": "Xor"},
                 {"name": "b", "type": "Not"}, {"name": "y", "type": "Output"}]},
        {"wire": [["x", "a.a"], ["a", "b"], ["b", "a.b", "y"]]}
    ])
}

#[test]
fn a_loop_names_its_parts_at_the_root() {
    let (ws, report) = edit(&empty(), json!({"ops": ring_ops()}));
    let errors = report.build.unwrap_err();
    assert_eq!(
        errors,
        [
            "a loop of gates with no state in it: a -> b -> a (put a Delay or a Register on the \
          loop)"
        ]
    );
    let text = view::diagnostics(&ws);
    assert!(
        text.contains("  a loop of gates with no state in it: a -> b -> a"),
        "{text}"
    );
}

#[test]
fn a_loop_two_chips_down_is_named_by_its_path() {
    let (ws, report) = edit(
        &empty(),
        json!({"edits": [
            {"circuit": "inner", "ops": ring_ops()},
            {"circuit": "mid", "ops": [
                {"add": [{"name": "x", "type": "Input"}, {"name": "in1", "use": "inner"},
                         {"name": "y", "type": "Output"}]},
                {"wire": [["x", "in1.x"], ["in1.y", "y"]]}
            ]},
            {"ops": [
                {"add": [{"name": "X", "type": "Input"}, {"name": "m", "use": "mid", "count": 2},
                         {"name": "Y", "type": "Output"}]},
                {"wire": [["X", "m[0].x"], ["m[0].y", "m[1].x"], ["m[1].y", "Y"]]},
                {"tests": [{"name": "t", "unoptimized": {"samples": 10}}]}
            ]}
        ]}),
    );
    let looped = |path: &str| {
        format!("a loop of gates with no state in it: {path}/a -> {path}/b -> {path}/a")
    };
    let errors = report.build.unwrap_err();
    assert_eq!(errors.len(), 2, "{errors:?}");
    assert!(errors[0].starts_with(&looped("m[0]/in1")), "{errors:?}");
    assert!(errors[1].starts_with(&looped("m[1]/in1")), "{errors:?}");
    assert!(view::status(&ws).contains(&looped("m[0]/in1")));

    // The private simulations say the same.
    let e = Rig::build(&ws, Place::Root).err().unwrap();
    assert!(
        e.starts_with(&format!(
            "the circuit does not build: {}",
            looped("m[0]/in1")
        )),
        "{e}"
    );
    let text = suite(&ws, "main");
    assert!(
        text.contains(&format!(
            "FAILED main / t: it does not build: {}",
            looped("m[0]/in1")
        )),
        "{text}"
    );
    // A chip compiled alone names its parts from itself.
    let e = Rig::build(&ws, place(&ws, "mid")).err().unwrap();
    assert!(e.contains(&looped("in1")), "{e}");
}

#[test]
fn the_compiled_view_of_a_loop_says_so() {
    let (ws, _) = edit(&empty(), json!({"ops": ring_ops()}));
    let e = compiled::view(&ws, Place::Root).unwrap_err();
    assert!(
        e.starts_with("it does not build: a loop of gates with no state in it: a -> b -> a"),
        "{e}"
    );
}

/// An 8-bit register of eight `dffe` chips (a `Delay` behind an enable
/// from gates), as a player builds one, used by `main`.
fn reg8_of_dffe_chips() -> WorkspaceDocument {
    let (ws, report) = edit(
        &empty(),
        json!({"edits": [
            {"circuit": "dffe", "ops": [
                {"add": [
                    {"name": "D", "type": "Input"}, {"name": "En", "type": "Input"},
                    {"name": "q", "type": "Delay"}, {"name": "ne", "type": "Not"},
                    {"name": "load", "type": "And"}, {"name": "keep", "type": "And"},
                    {"name": "next", "type": "Or"}, {"name": "Q", "type": "Output"}
                ]},
                {"wire": [["D", "load.a"], ["En", "load.b", "ne"], ["q", "keep.a", "Q"],
                          ["ne", "keep.b"], ["load", "next.a"], ["keep", "next.b"], ["next", "q"]]}
            ]},
            {"circuit": "reg8", "ops": [
                {"add": [
                    {"name": "D", "type": "Input", "w": 8}, {"name": "Load", "type": "Input"},
                    {"name": "ff", "use": "dffe", "count": 8},
                    {"name": "Q", "type": "Output", "w": 8}
                ]},
                {"wire": [["D", "ff.D"], ["Load", "ff.En"], ["ff.Q", "Q"]]}
            ]},
            {"ops": [
                {"add": [
                    {"name": "D", "type": "Input", "w": 8}, {"name": "Load", "type": "Input"},
                    {"name": "r", "use": "reg8"}, {"name": "Q", "type": "Output", "w": 8}
                ]},
                {"wire": [["D", "r.D"], ["Load", "r.Load"], ["r", "Q"]]}
            ]}
        ]}),
    );
    assert!(report.build.is_ok(), "{report}");
    ws
}

#[test]
fn a_register_of_flip_flop_chips_is_reported_and_pinned_as_a_register() {
    // The report of issue #117: `reg8` of `dffe` chips was "not matched:
    // state element 0: not a register".
    let ws = reg8_of_dffe_chips();
    let text = compiled::view(&ws, Place::Root).unwrap();
    for said in [
        "  dffe: matched as state [1-bit Register { en: In(1), d: In(0) }], outputs [r0]\n",
        "  reg8: matched as state [8-bit Register { en: In(1), d: In(0) }], outputs [r0]\n",
    ] {
        assert!(text.contains(said), "{said}: {text}");
    }
    // What a player would pin passes: by kind, by kind and width, and the
    // test the view prints.
    let i = compiled::insight(&ws, Place::Root).unwrap();
    let ws = with_tests(
        &ws,
        "main",
        json!([
            {"name": "a register", "compiled": {"matched": {"reg8": "Register"}}},
            {"name": "8 bits", "compiled": {"matched": {"reg8": "8-bit Register", "dffe": true}}},
            i.pinning_test()
        ]),
    );
    assert_eq!(
        suite(&ws, "main"),
        "3 of 3 tests passed in 1 circuit(s) (6 cases)\n"
    );
    // The register behaves as its gates do.
    let ws = with_tests(
        &ws,
        "main",
        json!([{"name": "same", "unoptimized": {"samples": 500, "hold": 2}}]),
    );
    assert!(suite(&ws, "main").starts_with("4 of 4 tests passed"));
}
