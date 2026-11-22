//! The build-it challenges: every challenge has a reference solution that
//! passes, wrong circuits fail with the first failing case, and the
//! manifest entries are validated when the catalog loads.

use super::*;
use crate::sim::challenges::{
    Challenge, ChallengeTest, Failure, PRIMITIVE_TYPES, Port, formula_cases, type_name,
};
use crate::sim::demos::{EMBEDDED, MANIFEST_FILE};
use crate::sim::source;

fn challenge(id: &str) -> &'static Challenge {
    demos()
        .challenge(id)
        .unwrap_or_else(|| panic!("no challenge {id}"))
}

/// A circuit written as the root circuit `main` of a version 6 file.
fn circuit(nodes: &str, wires: &str) -> WorkspaceDocument {
    let text = format!(
        r#"{{"format": "gatequest-workspace", "version": 6, "root": "main",
            "circuits": [{{"name": "main", "nodes": [{nodes}], "wires": [{wires}]}}]}}"#
    );
    let file = source::parse(&text).unwrap_or_else(|e| panic!("{e}\n{text}"));
    source::expand(&file, "tests/challenge", &demos().externals())
        .unwrap_or_else(|e| panic!("{e}\n{text}"))
        .workspace
}

/// The reference solution of each challenge.
fn solution(id: &str) -> WorkspaceDocument {
    match id {
        "and_from_nand" => circuit(
            r#"{"name": "A", "type": "Input", "at": [0, 40]},
               {"name": "B", "type": "Input", "at": [0, -40]},
               {"name": "n1", "type": "Nand", "at": [160, 0]},
               {"name": "n2", "type": "Nand", "at": [320, 0]},
               {"name": "Y", "type": "Output", "at": [480, 0]}"#,
            r#"["A", "n1.a"], ["B", "n1.b"], ["n1", "n2.a", "n2.b"], ["n2", "Y"]"#,
        ),
        "xor_from_nand" => circuit(
            r#"{"name": "A", "type": "Input", "at": [0, 40]},
               {"name": "B", "type": "Input", "at": [0, -40]},
               {"name": "n1", "type": "Nand", "at": [160, 0]},
               {"name": "n2", "type": "Nand", "at": [320, 80]},
               {"name": "n3", "type": "Nand", "at": [320, -80]},
               {"name": "n4", "type": "Nand", "at": [480, 0]},
               {"name": "Y", "type": "Output", "at": [640, 0]}"#,
            r#"["A", "n1.a", "n2.a"], ["B", "n1.b", "n3.a"], ["n1", "n2.b", "n3.b"],
               ["n2", "n4.a"], ["n3", "n4.b"], ["n4", "Y"]"#,
        ),
        "half_adder" => circuit(
            r#"{"name": "A", "type": "Input", "at": [0, 40]},
               {"name": "B", "type": "Input", "at": [0, -40]},
               {"name": "x", "type": "Xor", "at": [160, 40]},
               {"name": "c", "type": "And", "at": [160, -40]},
               {"name": "Sum", "type": "Output", "at": [480, 40]},
               {"name": "Carry", "type": "Output", "at": [480, -40]}"#,
            r#"["A", "x.a", "c.a"], ["B", "x.b", "c.b"], ["x", "Sum"], ["c", "Carry"]"#,
        ),
        // Two half adders (the level's own demo) and an OR.
        "full_adder" => circuit(
            r#"{"name": "A", "type": "Input", "at": [0, 80]},
               {"name": "B", "type": "Input", "at": [0, 0]},
               {"name": "Cin", "type": "Input", "at": [0, -80]},
               {"name": "ha1", "use": "half_adder/half_adder", "at": [160, 40]},
               {"name": "ha2", "use": "half_adder/half_adder", "at": [320, 0]},
               {"name": "o", "type": "Or", "at": [480, -120]},
               {"name": "Sum", "type": "Output", "at": [640, 40]},
               {"name": "Cout", "type": "Output", "at": [640, -40]}"#,
            r#"["A", "ha1.A"], ["B", "ha1.B"], ["ha1.Sum", "ha2.A"], ["Cin", "ha2.B"],
               ["ha1.Carry", "o.a"], ["ha2.Carry", "o.b"], ["ha2.Sum", "Sum"], ["o", "Cout"]"#,
        ),
        // Eight full adders, the carries chained.
        "adder8" => circuit(
            r#"{"name": "A", "type": "Input", "w": 8, "at": [0, 80]},
               {"name": "B", "type": "Input", "w": 8, "at": [0, 0]},
               {"name": "Cin", "type": "Input", "at": [0, -80]},
               {"name": "fa", "use": "full_adder/full_adder", "at": [320, 280],
                "count": 8, "step": [0, -80]},
               {"name": "Sum", "type": "Output", "w": 8, "at": [640, 40]},
               {"name": "Cout", "type": "Output", "at": [640, -40]}"#,
            r#"["A", "fa.A"], ["B", "fa.B"], ["Cin", "fa[0].Cin"],
               ["fa[0..7].Cout", "fa[1..8].Cin"], ["fa.Sum", "Sum"], ["fa[7].Cout", "Cout"]"#,
        ),
        "mux2" => circuit(
            r#"{"name": "A", "type": "Input", "at": [0, 80]},
               {"name": "B", "type": "Input", "at": [0, 0]},
               {"name": "Sel", "type": "Input", "at": [0, -80]},
               {"name": "n", "type": "Not", "at": [160, -80]},
               {"name": "a1", "type": "And", "at": [320, 80]},
               {"name": "a2", "type": "And", "at": [320, -40]},
               {"name": "o", "type": "Or", "at": [480, 0]},
               {"name": "Y", "type": "Output", "at": [640, 0]}"#,
            r#"["Sel", "n", "a2.b"], ["A", "a1.a"], ["n", "a1.b"], ["B", "a2.a"],
               ["a1", "o.a"], ["a2", "o.b"], ["o", "Y"]"#,
        ),
        "decoder2" => circuit(
            r#"{"name": "S", "type": "Input", "w": 2, "at": [0, 0]},
               {"name": "s0", "type": "Split", "w": 2, "lo": 0, "len": 1, "at": [160, 80]},
               {"name": "s1", "type": "Split", "w": 2, "lo": 1, "len": 1, "at": [160, -80]},
               {"name": "n0", "type": "Not", "at": [320, 80]},
               {"name": "n1", "type": "Not", "at": [320, -80]},
               {"name": "y0", "type": "And", "at": [480, 120]},
               {"name": "y1", "type": "And", "at": [480, 40]},
               {"name": "y2", "type": "And", "at": [480, -40]},
               {"name": "y3", "type": "And", "at": [480, -120]},
               {"name": "Y0", "type": "Output", "at": [640, 120]},
               {"name": "Y1", "type": "Output", "at": [640, 40]},
               {"name": "Y2", "type": "Output", "at": [640, -40]},
               {"name": "Y3", "type": "Output", "at": [640, -120]}"#,
            r#"["S", "s0", "s1"], ["s0", "n0", "y1.b", "y3.b"], ["s1", "n1", "y2.a", "y3.a"],
               ["n0", "y0.b", "y2.b"], ["n1", "y0.a", "y1.a"],
               ["y0", "Y0"], ["y1", "Y1"], ["y2", "Y2"], ["y3", "Y3"]"#,
        ),
        // Two NORs with a Delay in the loop (as the level's demo).
        "sr_latch" => circuit(
            r#"{"name": "S", "type": "Input", "at": [0, 40]},
               {"name": "R", "type": "Input", "at": [0, -40]},
               {"name": "nor_s", "type": "Nor", "at": [160, 40]},
               {"name": "nor_r", "type": "Nor", "at": [320, -40]},
               {"name": "state", "type": "Delay", "at": [480, 120]},
               {"name": "Q", "type": "Output", "at": [640, 40]}"#,
            r#"["S", "nor_s.a"], ["state", "nor_s.b", "Q"], ["R", "nor_r.a"],
               ["nor_s", "nor_r.b"], ["nor_r", "state"]"#,
        ),
        // Q = En ? D : Q, through a Delay.
        "d_latch" => circuit(
            r#"{"name": "D", "type": "Input", "at": [0, 40]},
               {"name": "En", "type": "Input", "at": [0, -40]},
               {"name": "m", "type": "Mux", "at": [160, 0]},
               {"name": "state", "type": "Delay", "at": [320, 0]},
               {"name": "Q", "type": "Output", "at": [480, 0]}"#,
            r#"["En", "m.sel"], ["state", "m.a", "Q"], ["D", "m.b"], ["m", "state"]"#,
        ),
        "register4" => circuit(
            r#"{"name": "D", "type": "Input", "w": 4, "at": [0, 40]},
               {"name": "Load", "type": "Input", "at": [0, -40]},
               {"name": "m", "type": "Mux", "w": 4, "at": [160, 0]},
               {"name": "state", "type": "Delay", "w": 4, "at": [320, 0]},
               {"name": "Q", "type": "Output", "w": 4, "at": [480, 0]}"#,
            r#"["Load", "m.sel"], ["state", "m.a", "Q"], ["D", "m.b"], ["m", "state"]"#,
        ),
        "counter4" => counter(1),
        other => panic!("no reference solution for challenge {other}"),
    }
}

/// A Register fed its own value plus `step`.
fn counter(step: u64) -> WorkspaceDocument {
    circuit(
        &format!(
            r#"{{"name": "En", "type": "Input", "at": [0, 0]}},
               {{"name": "one", "type": "Const", "w": 4, "value": {step}, "at": [0, -80]}},
               {{"name": "inc", "type": "Add", "w": 4, "at": [160, -40]}},
               {{"name": "reg", "type": "Register", "w": 4, "at": [320, 0]}},
               {{"name": "Count", "type": "Output", "w": 4, "at": [480, 0]}}"#
        ),
        r#"["En", "reg.en"], ["reg", "inc.a", "Count"], ["one", "inc.b"], ["inc.sum", "reg.d"]"#,
    )
}

fn case(f: Option<Failure>) -> crate::sim::challenges::FailedCase {
    match f {
        Some(Failure::Case(c)) => *c,
        other => panic!("expected a failing case, got {other:?}"),
    }
}

fn names(signals: &[crate::sim::challenges::Signal]) -> Vec<(String, u64)> {
    signals.iter().map(|s| (s.name.clone(), s.value)).collect()
}

fn pairs(list: &[(&str, u64)]) -> Vec<(String, u64)> {
    list.iter().map(|(n, v)| (n.to_string(), *v)).collect()
}

#[test]
fn levels_one_to_seven_have_challenges() {
    let catalog = demos();
    for level in 1..=7 {
        assert!(
            catalog.challenges.iter().any(|c| c.level == level),
            "level {level} has no challenge"
        );
    }
    // In level order, at known levels, with briefs and starters.
    assert!(
        catalog
            .challenges
            .windows(2)
            .all(|w| w[0].level <= w[1].level)
    );
    for c in &catalog.challenges {
        assert!(catalog.levels.iter().any(|l| l.level == c.level));
        assert!(!c.brief.is_empty() && !c.title.is_empty());
        assert!(!c.inputs.is_empty() && !c.outputs.is_empty(), "{}", c.id);
        // The starter is just the labelled ports.
        assert!(c.starter.root.nodes.iter().all(|n| n.kind.is_port()));
        assert!(c.starter.root.wires.is_empty());
        assert!(c.starter.definitions.is_empty());
        assert!(
            catalog
                .challenge_for_workspace(&c.workspace_name())
                .is_some()
        );
        assert!(
            catalog
                .get(&c.id)
                .is_none_or(|d| d.id != c.workspace_name())
        );
    }
}

#[test]
fn every_reference_solution_passes() {
    for c in &demos().challenges {
        let report = c.check(&solution(&c.id));
        assert!(report.passed(), "{}: {}", c.id, report.summary());
        assert!(report.cases > 0);
        assert!(
            report.summary().starts_with("Passed"),
            "{}",
            report.summary()
        );
    }
}

#[test]
fn every_starter_fails() {
    for c in &demos().challenges {
        let report = c.check(&c.starter);
        assert!(!report.passed(), "{}: the empty starter passed", c.id);
        assert!(
            matches!(report.failure, Some(Failure::Case(_))),
            "{}: {}",
            c.id,
            report.summary()
        );
    }
}

#[test]
fn a_wrong_half_adder_shows_the_first_failing_row() {
    let c = challenge("half_adder");
    // OR for the carry: wrong on the second row.
    let wrong = circuit(
        r#"{"name": "A", "type": "Input", "at": [0, 40]},
           {"name": "B", "type": "Input", "at": [0, -40]},
           {"name": "x", "type": "Xor", "at": [160, 40]},
           {"name": "c", "type": "Or", "at": [160, -40]},
           {"name": "Sum", "type": "Output", "at": [480, 40]},
           {"name": "Carry", "type": "Output", "at": [480, -40]}"#,
        r#"["A", "x.a", "c.a"], ["B", "x.b", "c.b"], ["x", "Sum"], ["c", "Carry"]"#,
    );
    let report = c.check(&wrong);
    assert_eq!(report.cases, 2);
    let f = case(report.failure.clone());
    assert_eq!(f.case, 2);
    assert_eq!(f.tick, None);
    assert_eq!(names(&f.inputs), pairs(&[("A", 0), ("B", 1)]));
    assert_eq!(names(&f.expected), pairs(&[("Sum", 1), ("Carry", 0)]));
    assert_eq!(names(&f.got), pairs(&[("Sum", 1), ("Carry", 1)]));
    assert_eq!(
        report.summary(),
        "Case 2: inputs A=0, B=1; expected Sum=1, Carry=0, got Sum=1, Carry=1."
    );
}

#[test]
fn a_wrong_adder_fails_on_a_sample() {
    let c = challenge("adder8");
    // Every full adder gets Cin: no carry chain.
    let wrong = circuit(
        r#"{"name": "A", "type": "Input", "w": 8, "at": [0, 80]},
           {"name": "B", "type": "Input", "w": 8, "at": [0, 0]},
           {"name": "Cin", "type": "Input", "at": [0, -80]},
           {"name": "fa", "use": "full_adder/full_adder", "at": [320, 280],
            "count": 8, "step": [0, -80]},
           {"name": "Sum", "type": "Output", "w": 8, "at": [640, 40]},
           {"name": "Cout", "type": "Output", "w": 8, "at": [640, -40]}"#,
        r#"["A", "fa.A"], ["B", "fa.B"], ["Cin", "fa.Cin"], ["fa.Sum", "Sum"]"#,
    );
    // Cout is 8 bits wide here: the ports are checked first.
    let report = c.check(&wrong);
    assert!(
        matches!(&report.failure, Some(Failure::Port(p)) if p.contains("Cout")),
        "{}",
        report.summary()
    );
    let wrong = circuit(
        r#"{"name": "A", "type": "Input", "w": 8, "at": [0, 80]},
           {"name": "B", "type": "Input", "w": 8, "at": [0, 0]},
           {"name": "Cin", "type": "Input", "at": [0, -80]},
           {"name": "fa", "use": "full_adder/full_adder", "at": [320, 280],
            "count": 8, "step": [0, -80]},
           {"name": "Sum", "type": "Output", "w": 8, "at": [640, 40]},
           {"name": "Cout", "type": "Output", "at": [640, -40]}"#,
        r#"["A", "fa.A"], ["B", "fa.B"], ["Cin", "fa.Cin"], ["fa.Sum", "Sum"],
           ["fa[7].Cout", "Cout"]"#,
    );
    let report = c.check(&wrong);
    let f = case(report.failure.clone());
    // All-zero and all-ones pass (every bit then carries the same); the
    // first random sample does not.
    assert_eq!(f.case, 3);
    let input = |n: &str| f.inputs.iter().find(|s| s.name == n).unwrap().value;
    let total = input("A") + input("B") + input("Cin");
    assert_eq!(
        names(&f.expected),
        pairs(&[("Sum", total & 0xFF), ("Cout", total >> 8)])
    );
    assert_ne!(f.expected, f.got);
    assert!(
        report.summary().starts_with("Case 3: inputs A="),
        "{}",
        report.summary()
    );
}

#[test]
fn a_latch_without_memory_fails_on_the_hold_step() {
    // Q wired straight to S: right until S drops.
    let c = challenge("sr_latch");
    let wrong = circuit(
        r#"{"name": "S", "type": "Input", "at": [0, 40]},
           {"name": "R", "type": "Input", "at": [0, -40]},
           {"name": "Q", "type": "Output", "at": [480, 0]}"#,
        r#"["S", "Q"]"#,
    );
    let f = case(c.check(&wrong).failure);
    assert_eq!(f.case, 4);
    assert_eq!(f.tick, Some(12));
    assert_eq!(names(&f.inputs), pairs(&[("S", 0), ("R", 0)]));
    assert_eq!(names(&f.expected), pairs(&[("Q", 1)]));
    assert_eq!(names(&f.got), pairs(&[("Q", 0)]));

    // A D latch that is always transparent.
    let c = challenge("d_latch");
    let wrong = circuit(
        r#"{"name": "D", "type": "Input", "at": [0, 40]},
           {"name": "En", "type": "Input", "at": [0, -40]},
           {"name": "Q", "type": "Output", "at": [480, 0]}"#,
        r#"["D", "Q"]"#,
    );
    assert_eq!(case(c.check(&wrong).failure).case, 5);

    // A D latch around the SrLatch primitive is allowed and passes.
    let with_sr = circuit(
        r#"{"name": "D", "type": "Input", "at": [0, 40]},
           {"name": "En", "type": "Input", "at": [0, -40]},
           {"name": "nd", "type": "Not", "at": [160, -80]},
           {"name": "s", "type": "And", "at": [320, 40]},
           {"name": "r", "type": "And", "at": [320, -40]},
           {"name": "l", "type": "SrLatch", "at": [480, 0]},
           {"name": "Q", "type": "Output", "at": [640, 0]}"#,
        r#"["D", "s.a", "nd"], ["En", "s.b", "r.b"], ["nd", "r.a"], ["s", "l.s"], ["r", "l.r"],
           ["l", "Q"]"#,
    );
    let report = c.check(&with_sr);
    assert!(report.passed(), "{}", report.summary());
}

#[test]
fn a_counter_that_counts_by_two_fails() {
    let c = challenge("counter4");
    let f = case(c.check(&counter(2)).failure);
    assert_eq!(f.case, 3);
    assert_eq!(f.tick, Some(4));
    assert_eq!(names(&f.expected), pairs(&[("Count", 1)]));
    assert_eq!(names(&f.got), pairs(&[("Count", 2)]));
    assert_eq!(names(&f.inputs), pairs(&[("En", 1)]));
}

#[test]
fn parts_outside_the_rules_are_refused() {
    // XOR from NAND, built with an XOR.
    let c = challenge("xor_from_nand");
    let cheat = circuit(
        r#"{"name": "A", "type": "Input", "at": [0, 40]},
           {"name": "B", "type": "Input", "at": [0, -40]},
           {"name": "x", "type": "Xor", "at": [160, 0]},
           {"name": "Y", "type": "Output", "at": [480, 0]}"#,
        r#"["A", "x.a"], ["B", "x.b"], ["x", "Y"]"#,
    );
    let report = c.check(&cheat);
    match &report.failure {
        Some(Failure::Part { part, circuit, .. }) => {
            assert_eq!(part, "XOR");
            assert_eq!(*circuit, None);
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(
        report.summary(),
        "Not allowed here: XOR. Only NAND (and the ports)."
    );
    // Hidden in a chip: the gates demo's XOR definition is an Xor inside.
    let chip = circuit(
        r#"{"name": "A", "type": "Input", "at": [0, 40]},
           {"name": "B", "type": "Input", "at": [0, -40]},
           {"name": "x", "use": "gates/xor", "at": [160, 0]},
           {"name": "Y", "type": "Output", "at": [480, 0]}"#,
        r#"["A", "x.A"], ["B", "x.B"], ["x", "Y"]"#,
    );
    let report = c.check(&chip);
    assert!(
        matches!(&report.failure, Some(Failure::Part { circuit: Some(c), .. }) if c == "XOR"),
        "{}",
        report.summary()
    );
    // NAND chips are fine: the universal demo's NAND-only XOR.
    let nand_chip = circuit(
        r#"{"name": "A", "type": "Input", "at": [0, 40]},
           {"name": "B", "type": "Input", "at": [0, -40]},
           {"name": "x", "use": "universal/xor", "at": [160, 0]},
           {"name": "Y", "type": "Output", "at": [480, 0]}"#,
        r#"["A", "x.A"], ["B", "x.B"], ["x", "Y"]"#,
    );
    let report = c.check(&nand_chip);
    assert!(report.passed(), "{}", report.summary());

    // The level's own demo, placed as a chip, is the answer: refused.
    let c = challenge("full_adder");
    let demo_chip = circuit(
        r#"{"name": "A", "type": "Input", "at": [0, 80]},
           {"name": "B", "type": "Input", "at": [0, 0]},
           {"name": "Cin", "type": "Input", "at": [0, -80]},
           {"name": "fa", "use": "full_adder/full_adder", "at": [160, 0]},
           {"name": "Sum", "type": "Output", "at": [480, 40]},
           {"name": "Cout", "type": "Output", "at": [480, -40]}"#,
        r#"["A", "fa.A"], ["B", "fa.B"], ["Cin", "fa.Cin"], ["fa.Sum", "Sum"],
           ["fa.Cout", "Cout"]"#,
    );
    let report = c.check(&demo_chip);
    assert!(
        matches!(&report.failure, Some(Failure::Part { part, .. })
            if part == "the Full adder chip"),
        "{}",
        report.summary()
    );
    // And the Add primitive.
    let c = challenge("adder8");
    let add = circuit(
        r#"{"name": "A", "type": "Input", "w": 8, "at": [0, 80]},
           {"name": "B", "type": "Input", "w": 8, "at": [0, 0]},
           {"name": "Cin", "type": "Input", "at": [0, -80]},
           {"name": "add", "type": "Add", "w": 8, "at": [160, 0]},
           {"name": "Sum", "type": "Output", "w": 8, "at": [480, 40]},
           {"name": "Cout", "type": "Output", "at": [480, -40]}"#,
        r#"["A", "add.a"], ["B", "add.b"], ["Cin", "add.cin"], ["add.sum", "Sum"],
           ["add.cout", "Cout"]"#,
    );
    assert!(matches!(
        c.check(&add).failure,
        Some(Failure::Part { part, .. }) if part == "ADD"
    ));

    // What the parts bar greys out.
    let c = challenge("xor_from_nand");
    assert!(c.allows_kind(&GateKind::Nand { w: 1 }));
    assert!(c.allows_kind(&GateKind::Input { w: 1 }));
    assert!(!c.allows_kind(&GateKind::And { w: 1 }));
    let catalog = demos();
    assert!(!c.allows_chip(&catalog.get("universal").unwrap().workspace));
    let c = challenge("adder8");
    assert!(c.allows_chip(&catalog.get("full_adder").unwrap().workspace));
    assert!(!c.allows_chip(&catalog.get("ripple_adder").unwrap().workspace));
    assert!(c.allows_kind(&GateKind::Xor { w: 1 }));
    assert!(!c.allows_kind(&GateKind::Add { w: 8 }));
    assert_eq!(
        c.parts_rule().as_deref(),
        Some("No ADD, the 8-bit ripple-carry adder chip")
    );
}

#[test]
fn missing_ports_and_loops_are_explained() {
    let c = challenge("half_adder");
    // "Carry" relabelled.
    let renamed = circuit(
        r#"{"name": "A", "type": "Input", "at": [0, 40]},
           {"name": "B", "type": "Input", "at": [0, -40]},
           {"name": "Sum", "type": "Output", "at": [480, 40]},
           {"name": "C", "type": "Output", "at": [480, -40]}"#,
        "",
    );
    let report = c.check(&renamed);
    assert_eq!(
        report.failure,
        Some(Failure::Port("there is no output \"Carry\"".into()))
    );
    // A loop of gates with no state in it.
    let looped = circuit(
        r#"{"name": "A", "type": "Input", "at": [0, 40]},
           {"name": "B", "type": "Input", "at": [0, -40]},
           {"name": "x", "type": "Xor", "at": [160, 40]},
           {"name": "Sum", "type": "Output", "at": [480, 40]},
           {"name": "Carry", "type": "Output", "at": [480, -40]}"#,
        r#"["A", "x.a"], ["x", "x.b", "Sum"]"#,
    );
    let report = c.check(&looped);
    assert!(
        matches!(&report.failure, Some(Failure::Build(e)) if e.contains("loop")),
        "{}",
        report.summary()
    );
}

#[test]
fn formula_cases_are_exhaustive_or_seeded() {
    let ports = [
        Port {
            name: "A".into(),
            w: 2,
        },
        Port {
            name: "C".into(),
            w: 1,
        },
    ];
    let all = formula_cases(&ports, None, 0);
    assert_eq!(all.len(), 8);
    assert_eq!(all[0], [0, 0]);
    assert_eq!(all[1], [1, 0]);
    assert_eq!(all[4], [0, 1]);
    assert_eq!(all[7], [3, 1]);
    let wide = [
        Port {
            name: "A".into(),
            w: 8,
        },
        Port {
            name: "B".into(),
            w: 8,
        },
    ];
    let a = formula_cases(&wide, Some(50), 95);
    assert_eq!(a.len(), 52, "all-zero, all-ones and the samples");
    assert_eq!(a[0], [0, 0]);
    assert_eq!(a[1], [255, 255]);
    assert_eq!(a, formula_cases(&wide, Some(50), 95), "the same every time");
    assert_ne!(a, formula_cases(&wide, Some(50), 96));
    assert!(a.iter().flatten().all(|&v| v <= 255));
    // The 8-bit adder's test samples instead of trying 2^17 cases.
    assert!(matches!(
        challenge("adder8").test,
        ChallengeTest::Formula {
            samples: Some(_),
            ..
        }
    ));
}

#[test]
fn primitive_type_names_cover_every_part() {
    for (_, kinds) in GateKind::palette(1) {
        for kind in kinds {
            let name = type_name(&kind).unwrap();
            assert!(PRIMITIVE_TYPES.contains(&name.as_str()), "{name}");
        }
    }
    assert_eq!(type_name(&GateKind::SrLatch).as_deref(), Some("SrLatch"));
    assert_eq!(
        type_name(&GateKind::SubCircuit {
            definition: Uuid::nil()
        }),
        None
    );
}

/// Load the catalog with `edit` applied to the manifest's text.
fn load_with(edit: impl Fn(String) -> String) -> Result<DemoCatalog, String> {
    let read = |file: &str| -> Result<String, String> {
        let text = EMBEDDED
            .iter()
            .find(|(n, _)| *n == file)
            .map(|(_, t)| t.to_string())
            .ok_or_else(|| format!("no {file}"))?;
        Ok(if file == MANIFEST_FILE {
            edit(text)
        } else {
            text
        })
    };
    DemoCatalog::load(read, |file| {
        crate::sim::demos::GALLERY_EMBEDDED
            .iter()
            .find(|(n, _)| *n == file)
            .map(|(_, t)| t.to_string())
            .ok_or_else(|| format!("no {file}"))
    })
}

#[test]
fn bad_challenge_entries_are_refused_with_a_message() {
    assert!(load_with(|t| t).is_ok());
    let cases: [(&str, &str, &str); 8] = [
        (
            r#""allowed": ["Nand"]"#,
            r#""allowed": ["Nandd"]"#,
            "not a primitive type",
        ),
        (
            r#""forbidden": ["Add", "half_adder/half_adder"]"#,
            r#""forbidden": ["Add", "half_adder/nope"]"#,
            "no circuit \"half_adder/nope\"",
        ),
        (
            r#""Sum": "A ^ B ^ Cin""#,
            r#""Sum": "A ^ X""#,
            "no input \"X\"",
        ),
        (
            r#""Sum": "A ^ B ^ Cin""#,
            r#""Sum": "A ^""#,
            "unexpected end",
        ),
        (r#", "samples": 300"#, "", "too many to try them all"),
        (
            r#"[0, 1, 1, 0], [1, 0"#,
            r#"[0, 1, 1], [1, 0"#,
            "row 2 has 3 values",
        ),
        (
            r#""id": "mux2",
      "level": 4"#,
            r#""id": "mux2",
      "level": 40"#,
            "level 40 is not in",
        ),
        (
            r#"{"set": {"R": 1}, "ticks": 3, "expect": {"Q": 0}}"#,
            r#"{"set": {"R": 1}, "ticks": 3, "expect": {"Qx": 0}}"#,
            "no output \"Qx\"",
        ),
    ];
    for (from, to, message) in cases {
        let e = load_with(|t| {
            assert!(t.contains(from), "{from}");
            t.replacen(from, to, 1)
        })
        .err()
        .unwrap_or_else(|| panic!("{to} loaded"));
        assert!(e.contains(message), "{e}");
        assert!(e.starts_with("challenge \""), "{e}");
    }
    // A manifest without challenges still loads.
    let catalog = load_with(|t| {
        let at = t.find(",\n  \"challenges\"").unwrap();
        format!("{}\n}}\n", &t[..at])
    })
    .unwrap();
    assert!(catalog.challenges.is_empty());
}
