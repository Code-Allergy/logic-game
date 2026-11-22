//! Tests of state targets (`Rig::target`, `Rig::poke`, `Rig::peek`) and
//! the `poke` step of experiment scripts.

use logi_sim::compile::{Options, PackMode};
use serde_json::{Map, Value, json};

use super::{edit, empty};
use crate::mcp::batch::{self, Place};
use crate::mcp::rig::{Rig, StateTarget, Stored};
use crate::mcp::run::{self, RunState};
use crate::sim::WorkspaceDocument;
use crate::sim::demos::catalog;

/// The compile options a poke must be right under: the defaults, nothing,
/// and each of `lift`, `func_match` and `words` off and alone.
fn variants() -> Vec<(&'static str, Options)> {
    let d = Options::default();
    vec![
        ("default", d.clone()),
        ("unoptimized", Options::unoptimized()),
        (
            "no lift",
            Options {
                lift: false,
                ..d.clone()
            },
        ),
        (
            "no func_match",
            Options {
                func_match: false,
                ..d.clone()
            },
        ),
        (
            "no words",
            Options {
                words: false,
                ..d.clone()
            },
        ),
        (
            "lift only",
            Options {
                func_match: false,
                words: false,
                ..d.clone()
            },
        ),
        (
            "func_match only",
            Options {
                lift: false,
                words: false,
                ..d.clone()
            },
        ),
        (
            "words only",
            Options {
                lift: false,
                func_match: false,
                ..d.clone()
            },
        ),
        (
            "packed",
            Options {
                pack: PackMode::All,
                pack_min: 2,
                ..d
            },
        ),
    ]
}

fn object(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        other => panic!("not an object: {other}"),
    }
}

fn script_on(rig: Rig, script: Value) -> run::Outcome {
    let mut state = RunState::new(rig);
    run::run(&mut state, &object(script)).unwrap_or_else(|e| panic!("{e}"))
}

fn script_error(ws: &WorkspaceDocument, script: Value) -> String {
    let mut state = RunState::new(Rig::build(ws, Place::Root).unwrap());
    match run::run(&mut state, &object(script)) {
        Ok(o) => panic!("the script should not run: {}", o.text),
        Err(e) => e,
    }
}

/// Every kind of state in one root: word and 1-bit registers, a Delay, an
/// SR latch, a D latch, a RAM and a ROM, the gallery's gate-level 8-bit
/// register, and small chips that are, or are not, registers.
fn fixture() -> WorkspaceDocument {
    let (ws, report) = batch::apply(
        &empty(),
        &json!({"edits": [
            // A flip-flop on the rising edge: the previous clock is state
            // the output depends on, and no copy of it.
            {"circuit": "edge_ff", "ops": [
                {"add": [
                    {"name": "D", "type": "Input"}, {"name": "Clk", "type": "Input"},
                    {"name": "prev", "type": "Delay"}, {"name": "np", "type": "Not"},
                    {"name": "rise", "type": "And"}, {"name": "q", "type": "Register"},
                    {"name": "Q", "type": "Output"}
                ]},
                {"wire": [["Clk", "prev", "rise.a"], ["prev", "np"], ["np", "rise.b"],
                          ["rise", "q.en"], ["D", "q.d"], ["q", "Q"]]}
            ]},
            {"circuit": "with_ram", "ops": [
                {"add": [
                    {"name": "A", "type": "Input", "w": 4},
                    {"name": "r", "type": "Register", "w": 8},
                    {"name": "mem", "type": "Ram", "addr_bits": 4, "w": 8},
                    {"name": "Q", "type": "Output", "w": 8}
                ]},
                {"wire": [["A", "mem.addr"], ["mem", "r.d"], ["r", "Q"]]}
            ]},
            {"circuit": "through", "ops": [
                {"add": [{"name": "A", "type": "Input", "w": 8},
                         {"name": "Y", "type": "Output", "w": 8}]},
                {"wire": [["A", "Y"]]}
            ]},
            {"circuit": "inverted", "ops": [
                {"add": [{"name": "D", "type": "Input", "w": 4},
                         {"name": "r", "type": "Register", "w": 4},
                         {"name": "n", "type": "Not", "w": 4},
                         {"name": "Q", "type": "Output", "w": 4}]},
                {"wire": [["D", "r.d"], ["r", "n"], ["n", "Q"]]}
            ]},
            // A register whose chip also counts ticks: the count is state
            // the output does not depend on.
            {"circuit": "counted", "ops": [
                {"add": [{"name": "D", "type": "Input", "w": 4}, {"name": "En", "type": "Input"},
                         {"name": "r", "type": "Register", "w": 4},
                         {"name": "cnt", "type": "Delay", "w": 4},
                         {"name": "one", "type": "Const", "w": 4, "value": 1},
                         {"name": "inc", "type": "Add", "w": 4},
                         {"name": "Q", "type": "Output", "w": 4}]},
                {"wire": [["D", "r.d"], ["En", "r.en"], ["r", "Q"], ["cnt", "inc.a"],
                          ["one", "inc.b"], ["inc.sum", "cnt"]]}
            ]},
            // A master-slave flip-flop whose master holds the complement.
            {"circuit": "nff", "ops": [
                {"add": [{"name": "D", "type": "Input"}, {"name": "Clk", "type": "Input"},
                         {"name": "nd", "type": "Not"}, {"name": "mm", "type": "Mux"},
                         {"name": "m", "type": "Delay"}, {"name": "nm", "type": "Not"},
                         {"name": "sm", "type": "Mux"}, {"name": "s", "type": "Delay"},
                         {"name": "Q", "type": "Output"}]},
                {"wire": [["D", "nd"], ["Clk", "mm.sel", "sm.sel"], ["nd", "mm.a"],
                          ["m", "mm.b", "nm"], ["mm", "m"], ["s", "sm.a", "Q"], ["nm", "sm.b"],
                          ["sm", "s"]]}
            ]},
            {"ops": [
                {"add": [
                    {"name": "D", "type": "Input", "w": 8},
                    {"name": "En", "type": "Input"},
                    {"name": "S", "type": "Input"}, {"name": "R", "type": "Input"},
                    {"name": "E", "type": "Input"},
                    {"name": "Clk", "type": "Input"},
                    {"name": "Load", "type": "Input"},
                    {"name": "r", "type": "Register", "w": 8},
                    {"name": "nr", "type": "Not", "w": 8},
                    {"name": "Q", "type": "Output", "w": 8},
                    {"name": "NQ", "type": "Output", "w": 8},
                    {"name": "bit", "type": "Register", "count": 8},
                    {"name": "B", "type": "Output", "w": 8},
                    {"name": "dl", "type": "Delay", "w": 4},
                    {"name": "sr", "type": "SrLatch"},
                    {"name": "lat", "type": "DLatch", "w": 4},
                    {"name": "L", "type": "Output", "w": 4},
                    {"name": "g", "type": "And"},
                    {"name": "rom", "type": "Rom", "addr_bits": 4, "w": 8},
                    {"name": "ram", "type": "Ram", "addr_bits": 4, "w": 8},
                    {"name": "M", "type": "Output", "w": 8},
                    {"name": "RO", "type": "Output", "w": 8},
                    {"name": "R8", "use": "register/register8"},
                    {"name": "G", "type": "Output", "w": 8},
                    {"name": "e", "use": "edge_ff"},
                    {"name": "mc", "use": "with_ram"},
                    {"name": "th", "use": "through"},
                    {"name": "inv", "use": "inverted"},
                    {"name": "cnt", "use": "counted"},
                    {"name": "nf", "use": "nff", "count": 4},
                    {"name": "NF", "type": "Output", "w": 4}
                ]},
                {"wire": [
                    ["D", "r.d", "bit.d", "R8.D", "th.A"],
                    ["En", "r.en", "bit.en", "cnt.En"],
                    ["r", "nr", "Q"], ["nr", "NQ"], ["bit", "B"],
                    ["dl", "dl"],
                    ["S", "sr.s", "g.a"], ["R", "sr.r", "g.b"],
                    ["E", "lat.en"], ["D:0..4", "lat.d", "inv.D", "nf.D", "cnt.D"],
                    ["lat", "L"],
                    ["r:0..4", "rom.addr", "ram.addr"], ["ram", "M"], ["rom", "RO"],
                    ["Load", "R8.Load"], ["Clk", "R8.Clk", "e.Clk", "nf.Clk"], ["R8", "G"],
                    ["nf", "NF"]
                ]}
            ]}
        ]}),
        &catalog().expect("demos load").externals(),
    )
    .unwrap_or_else(|e| panic!("{e}"));
    assert!(report.build.is_ok(), "{report}");
    ws
}

fn target(rig: &mut Rig, text: &str) -> StateTarget {
    rig.target(text).unwrap_or_else(|e| panic!("{e}"))
}

fn refused(rig: &mut Rig, text: &str) -> String {
    match rig.target(text) {
        Ok(t) => panic!("\"{text}\" should be refused: {t:?}"),
        Err(e) => e,
    }
}

/// What a fixture shows: every root output and the value of every target.
fn observe(rig: &Rig, targets: &[StateTarget]) -> Vec<u64> {
    let sim = rig.sim();
    (0..sim.program().ports.outputs.len())
        .map(|i| sim.output(i))
        .chain(targets.iter().map(|t| rig.peek(t)))
        .collect()
}

const POKES: [(&str, u64); 10] = [
    ("r", 0xA5),
    ("bit", 0x5A),
    ("dl", 0x9),
    ("sr", 1),
    ("lat.q", 0x6),
    ("ram@5", 0x77),
    ("R8", 0xC3),
    ("nf", 0xA),
    ("cnt", 0xB),
    ("e/q", 1),
];

#[test]
fn every_target_form_is_written_read_back_and_seen_by_the_logic() {
    let ws = fixture();
    let mut reference: Option<Vec<Vec<u64>>> = None;
    for (variant, options) in variants() {
        let mut rig = Rig::with_options(ws.clone(), options).unwrap();
        // The clock high: the flip-flops' slaves follow their masters.
        rig.set("Clk", 1).unwrap();
        rig.tick();
        let targets: Vec<StateTarget> = POKES.iter().map(|(t, _)| target(&mut rig, t)).collect();
        let pokes: Vec<(&StateTarget, u64)> = targets
            .iter()
            .zip(POKES)
            .map(|(t, (_, v))| (t, v))
            .collect();
        rig.poke(&pokes).unwrap();
        assert_eq!(rig.tick_count(), 1, "{variant}: a poke does not tick");
        for (t, (text, v)) in targets.iter().zip(POKES) {
            assert_eq!(rig.peek(t), v, "{variant}: {text} reads back");
        }
        // The logic sees the new state before the next tick.
        let sim = rig.sim();
        let outs = ["Q", "NQ", "B", "L", "M", "G", "NF"].map(|o| sim.get(o));
        assert_eq!(
            outs,
            [0xA5, 0x5A, 0x5A, 0x6, 0x77, 0xC3, 0xA],
            "{variant}: outputs after the poke"
        );
        // Other forms of the same state.
        for (text, want) in [
            ("r:4..8", 0xA),
            ("bit[2..6]", 0x6),
            ("bit[7]", 0),
            ("R8.Q:4..8", 0xC),
            ("G", 0xC3),
            ("Q", 0xA5),
            ("nf[1..3]", 0x1),
            ("cnt/r", 0xB),
            ("cnt/cnt", 1),
        ] {
            let t = target(&mut rig, text);
            assert_eq!(rig.peek(&t), want, "{variant}: {text}");
        }
        let low = target(&mut rig, "r:0..4");
        rig.poke(&[(&low, 3)]).unwrap();
        assert_eq!(rig.sim().get("Q"), 0xA3, "{variant}: a bit field");

        // Run on: hold for a while, then load, set and reset.
        let all: Vec<StateTarget> = targets.clone();
        let mut seen = vec![observe(&rig, &all)];
        for step in 0..12u64 {
            rig.set("Clk", (step + 1) % 2).unwrap();
            if step == 6 {
                for (i, v) in [("Load", 1), ("D", 0x3C), ("En", 1), ("S", 1)] {
                    rig.set(i, v).unwrap();
                }
            }
            if step == 8 {
                for (i, v) in [("Load", 0), ("En", 0), ("S", 0), ("R", 1), ("E", 1)] {
                    rig.set(i, v).unwrap();
                }
            }
            rig.tick();
            seen.push(observe(&rig, &all));
            if step == 0 {
                let sim = rig.sim();
                let outs = ["Q", "B", "L", "M", "G", "NF"].map(|o| sim.get(o));
                assert_eq!(
                    outs,
                    [0xA3, 0x5A, 0x6, 0, 0xC3, 0xA],
                    "{variant}: the state holds a tick on"
                );
            }
        }
        assert_eq!(
            rig.sim().get("G"),
            0x3C,
            "{variant}: the gate register loaded"
        );
        match &reference {
            None => reference = Some(seen),
            Some(r) => {
                for (tick, (a, b)) in r.iter().zip(&seen).enumerate() {
                    assert_eq!(a, b, "{variant}: step {tick} differs from the defaults");
                }
            }
        }
    }
}

#[test]
fn chip_targets_write_the_copies_of_their_bits() {
    let ws = fixture();
    let mut rig = Rig::build(&ws, Place::Root).unwrap();
    // The gate register shows its slave latches and writes its masters.
    let r8 = target(&mut rig, "R8");
    let Stored::Bits(bits) = &r8.stored else {
        panic!("bits")
    };
    let shown = bits.iter().filter(|b| b.shown).count();
    let copies: Vec<_> = bits.iter().filter(|b| !b.shown).collect();
    assert_eq!(shown, 8, "{bits:?}");
    assert_eq!(copies.len(), 8, "{bits:?}");
    assert!(copies.iter().all(|b| !b.invert));
    // The complement master is written inverted.
    let nf = target(&mut rig, "nf");
    let Stored::Bits(bits) = &nf.stored else {
        panic!("bits")
    };
    assert_eq!(bits.iter().filter(|b| b.invert).count(), 4, "{bits:?}");
    // State the output does not depend on is left alone.
    let counted = target(&mut rig, "cnt");
    let Stored::Bits(bits) = &counted.stored else {
        panic!("bits")
    };
    assert!(bits.iter().all(|b| b.shown), "{bits:?}");
    rig.run(5);
    let count = target(&mut rig, "cnt/cnt");
    assert_eq!(rig.peek(&count), 5);
    rig.poke(&[(&counted, 9)]).unwrap();
    assert_eq!(rig.peek(&count), 5, "the count is not touched");
    assert_eq!(rig.peek(&counted), 9);
}

#[test]
fn what_is_not_state_is_refused_with_what_to_use() {
    let ws = fixture();
    let mut rig = Rig::build(&ws, Place::Root).unwrap();
    for (text, said) in [
        ("D", "set it with \"set\""),
        ("rom@1", "\"load\""),
        ("rom", "a ROM"),
        ("ram", "name one word"),
        ("g", "not state"),
        ("nope", "no part \"nope\""),
        ("e", "depends on"),
        ("mc", "holds a memory"),
        ("th", "comes from an input"),
        ("inv", "computed by a Not"),
        ("NQ", "computed by a Not"),
        ("r.x", "has no output \"x\""),
        ("r:4..12", "past its width"),
        ("ram@99", "past the memory"),
        ("th/A", "an input port"),
    ] {
        let e = refused(&mut rig, text);
        assert!(e.contains(said), "{text}: {e}");
        assert!(e.starts_with(&format!("state \"{text}\"")), "{text}: {e}");
    }
    let r = target(&mut rig, "r");
    let e = rig.poke(&[(&r, 0x100)]).unwrap_err();
    assert!(e.contains("does not fit \"r\" (8 bits)"), "{e}");
    // Nothing was written when one value is wrong.
    let bit = target(&mut rig, "bit");
    assert!(rig.poke(&[(&bit, 3), (&r, 0x100)]).is_err());
    assert_eq!(rig.peek(&bit), 0);
}

#[test]
fn a_poke_settles_at_once_and_leaves_staged_inputs_for_the_next_tick() {
    let ws = fixture();
    for (variant, options) in [
        ("default", Options::default()),
        ("unoptimized", Options::unoptimized()),
    ] {
        let mut rig = Rig::with_options(ws.clone(), options).unwrap();
        let r = target(&mut rig, "r");
        rig.set("En", 1).unwrap();
        rig.set("D", 7).unwrap();
        rig.poke(&[(&r, 5)]).unwrap();
        assert_eq!(
            rig.sim().get("NQ"),
            0xFA,
            "{variant}: combinational at once"
        );
        assert_eq!(rig.tick_count(), 0);
        // The inputs set before the poke reach the register a tick later,
        // as they would without it.
        rig.tick();
        assert_eq!(rig.peek(&r), 5, "{variant}");
        rig.tick();
        assert_eq!(rig.peek(&r), 7, "{variant}");
    }
}

#[test]
fn targets_survive_a_recompile() {
    let ws = fixture();
    let mut rig = Rig::build(&ws, Place::Root).unwrap();
    let r8 = target(&mut rig, "R8");
    let ram = target(&mut rig, "ram@3");
    rig.poke(&[(&r8, 0x42), (&ram, 9)]).unwrap();
    // A new probe recompiles; the state is carried and the targets work.
    let probe = rig.resolve("R8/bit3/master").unwrap();
    rig.watch(&[probe]).unwrap();
    assert_eq!(rig.peek(&r8), 0x42);
    assert_eq!(rig.peek(&ram), 9);
    rig.poke(&[(&r8, 0x24)]).unwrap();
    assert_eq!(rig.sim().get("G"), 0x24);
}

#[test]
fn the_poke_step_takes_paths_aliases_and_word_lists() {
    let ws = fixture();
    let out = script_on(
        Rig::build(&ws, Place::Root).unwrap(),
        json!({
            "signals": {"reg": "r", "bits": "bit"},
            "steps": [
                {"poke": {"reg": "0x0A", "bits": 0x5A, "ram": [[10, 7], ["0xB", 8]],
                          "R8": 0xC3, "dl": 3}},
                {"expect": {"Q": 10, "bits": 0x5A, "bit[0..4]": 0xA, "M": 7, "G": 0xC3,
                            "ram@11": 8, "R8/bit3": 0}},
                {"read": ["bit", "dl", "ram@0..0x10"]},
                {"until": "bits == 0x5A", "max_ticks": 1},
                {"set": {"En": 1, "D": 1}}, {"ticks": 2},
                {"expect": {"bit": 1, "reg": 1}}
            ]
        }),
    );
    assert!(!out.failed, "{}", out.text);
    assert!(
        out.text.contains("poked 6 value(s) at tick 0"),
        "{}",
        out.text
    );
    assert!(out.text.contains("  bit = 0x5A (90)"), "{}", out.text);

    for (bad, said) in [
        (json!({"poke": {"D": 1}}), "set it with \"set\""),
        (
            json!({"poke": {"ram": [[1]]}}),
            "\"ram\"[0]: a word is [address, value]",
        ),
        (json!({"poke": {"ram": [[16, 1]]}}), "past the memory"),
        (json!({"poke": {"rom": [[1, 2]]}}), "ROM"),
        (json!({"poke": {"r": 256}}), "does not fit \"r\" (8 bits)"),
        (json!({"poke": {"ram": [[1, 256]]}}), "does not fit"),
        (json!({"poke": 3}), "\"poke\" takes"),
    ] {
        let e = script_error(&ws, json!({"steps": [{"ticks": 3}, bad]}));
        assert!(e.starts_with("steps[1]: "), "{e}");
        assert!(e.contains(said), "{said}: {e}");
    }
}

// ── The gallery's gate-level CPU ─────────────────────────────────────────

fn cpu16() -> WorkspaceDocument {
    catalog()
        .expect("demos load")
        .get("cpu16")
        .expect("gallery item")
        .workspace
        .as_ref()
        .clone()
}

#[test]
fn a_poked_register_and_ram_word_drive_the_gate_cpu() {
    let ws = cpu16();
    // At 0 a halt; at 8: r1 = ram[0x10]; r1 += r2; ram[0x11] = r1; out r1;
    // halt. Only a poked program counter reaches 8, and only a poked r2
    // and RAM word give 0x1005.
    let mut program = vec![0xF000u64];
    program.resize(8, 0);
    program.extend([0x5210, 0x1250, 0x6211, 0xC040, 0xF000]);
    let mut probe = Rig::build(&ws, Place::Root).unwrap();
    let r2 = ["registers_r0_r7/r2", "registers_r0_r7/r[2]"]
        .into_iter()
        .find(|t| probe.target(t).is_ok())
        .unwrap_or_else(|| panic!("{}", probe.target("registers_r0_r7/r2").unwrap_err()));
    let r1 = r2.replace('2', "1");
    for (variant, options) in variants() {
        let rig = Rig::with_options(ws.clone(), options).unwrap();
        let out = script_on(
            rig,
            json!({"steps": [
                {"load": {"part": "program", "data": program}},
                {"poke": {"program_counter/pc": 8, r2: 5, "data_RAM@0x10": 0x1000}},
                {"expect": {"PC": 8, r2: 5, "program_counter/pc": 8}},
                {"until": "Halted == 1", "max_ticks": 2000},
                {"expect": {"Out_value": 0x1005, "data_RAM@0x11": 0x1005,
                            r1.as_str(): 0x1005, "output_register": 0x1005}}
            ]}),
        );
        assert!(!out.failed, "{variant}: {}", out.text);
    }
    // Without the pokes it halts at once.
    let out = script_on(
        Rig::build(&ws, Place::Root).unwrap(),
        json!({"steps": [
            {"load": {"part": "program", "data": program}},
            {"until": "Halted == 1", "max_ticks": 2000},
            {"expect": {"Out_value": 0}}
        ]}),
    );
    assert!(!out.failed, "{}", out.text);
}

// ── A Harte-shaped case ──────────────────────────────────────────────────

/// One instruction per tick: at `pc`, opcode 1 adds the byte at the
/// operand's address to `a` (carry into `c`), opcode 2 stores `a` there,
/// anything else halts.
fn accumulator() -> WorkspaceDocument {
    let (ws, report) = edit(
        &empty(),
        json!({"ops": [
            {"add": [
                {"name": "pc", "type": "Register", "w": 8},
                {"name": "a", "type": "Register", "w": 8},
                {"name": "c", "type": "Register"},
                {"name": "mem", "type": "Ram2", "addr_bits": 8, "w": 8, "reads": 3},
                {"name": "one", "type": "Const", "w": 8, "value": 1},
                {"name": "two", "type": "Const", "w": 8, "value": 2},
                {"name": "next", "type": "Add", "w": 8},
                {"name": "oper", "type": "Add", "w": 8},
                {"name": "is_add", "type": "Eq", "w": 8},
                {"name": "is_st", "type": "Eq", "w": 8},
                {"name": "go", "type": "Or"},
                {"name": "sum", "type": "Add", "w": 8},
                {"name": "A", "type": "Output", "w": 8},
                {"name": "PC", "type": "Output", "w": 8}
            ]},
            {"wire": [
                ["pc", "mem.raddr0", "oper.a", "next.a", "PC"],
                ["one", "oper.b", "is_add.b"],
                ["two", "next.b", "is_st.b"],
                ["oper.sum", "mem.raddr1"],
                ["mem.rdata1", "mem.raddr2", "mem.waddr"],
                ["mem.rdata0", "is_add.a", "is_st.a"],
                ["is_add", "go.a", "a.en", "c.en"], ["is_st", "go.b", "mem.we"],
                ["go", "pc.en"], ["next.sum", "pc.d"],
                ["a", "sum.a", "mem.wdata", "A"], ["mem.rdata2", "sum.b"],
                ["sum.sum", "a.d"], ["sum.cout", "c.d"]
            ]}
        ]}),
    );
    assert!(report.build.is_ok(), "{report}");
    ws
}

/// Cases as Harte writes them: initial registers and RAM bytes, final
/// registers and RAM bytes.
type Case = ([u64; 3], Vec<[u64; 2]>, [u64; 3], Vec<[u64; 2]>);

fn cases() -> Vec<Case> {
    vec![
        (
            [0x10, 0x30, 1],
            vec![
                [0x10, 1],
                [0x11, 0x80],
                [0x80, 0x22],
                [0x12, 2],
                [0x13, 0x81],
            ],
            [0x14, 0x52, 0],
            vec![[0x80, 0x22], [0x81, 0x52]],
        ),
        (
            [0x40, 0xF0, 0],
            vec![
                [0x40, 1],
                [0x41, 0x90],
                [0x90, 0x20],
                [0x42, 2],
                [0x43, 0x91],
            ],
            [0x44, 0x10, 1],
            vec![[0x90, 0x20], [0x91, 0x10]],
        ),
    ]
}

#[test]
fn harte_shaped_cases_run_as_a_script() {
    let ws = accumulator();
    let mut steps = Vec::new();
    for (regs, ram, want, want_ram) in cases() {
        steps.push(json!({"poke": {"pc": regs[0], "acc": regs[1], "c": regs[2], "mem": ram}}));
        steps.push(json!({"ticks": 3}));
        let mut expect = json!({"pc": want[0], "acc": want[1], "c": want[2], "A": want[1]});
        for [addr, value] in want_ram {
            expect[format!("mem@{addr}")] = json!(value);
        }
        steps.push(json!({"expect": expect}));
    }
    for (variant, options) in [
        ("default", Options::default()),
        ("unoptimized", Options::unoptimized()),
    ] {
        let rig = Rig::with_options(ws.clone(), options).unwrap();
        let out = script_on(
            rig,
            json!({"signals": {"acc": "a"}, "steps": steps.clone()}),
        );
        assert!(!out.failed, "{variant}: {}", out.text);
    }
}

#[test]
fn harte_shaped_cases_run_through_the_rust_api() {
    let ws = accumulator();
    for (variant, options) in variants() {
        let mut rig = Rig::with_options(ws.clone(), options).unwrap();
        // Resolved once for the suite.
        let regs = ["pc", "a", "c"].map(|t| target(&mut rig, t));
        let mem = rig.memory("mem").unwrap();
        let mut touched: Vec<usize> = Vec::new();
        for _ in 0..2 {
            for (init, ram, want, want_ram) in cases() {
                // Clear what the last case left, then set this one.
                let clear: Vec<(usize, u64)> = touched.drain(..).map(|a| (a, 0)).collect();
                rig.write_words(&mem, &clear).unwrap();
                let pokes: Vec<(&StateTarget, u64)> = regs.iter().zip(init).collect();
                rig.write_state(&pokes).unwrap();
                let words: Vec<(usize, u64)> = ram.iter().map(|[a, v]| (*a as usize, *v)).collect();
                rig.write_words(&mem, &words).unwrap();
                rig.settle();
                rig.run(3);
                let got = regs.each_ref().map(|t| rig.peek(t));
                assert_eq!(got, want, "{variant}: registers");
                let words = rig.words(&mem);
                for [addr, value] in &want_ram {
                    assert_eq!(words[*addr as usize], *value, "{variant}: ram@{addr}");
                }
                touched.extend(ram.iter().map(|[a, _]| *a as usize));
                touched.extend(want_ram.iter().map(|[a, _]| *a as usize));
            }
        }
    }
}
