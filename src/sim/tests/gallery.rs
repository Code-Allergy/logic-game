//! The gallery (`assets/gallery/`): its files are embedded and canonical,
//! it only builds on the curriculum and on items listed before it, every
//! item compiles and is laid out without overlapping boxes (the curriculum
//! demos too), and each one
//! behaves as its description says (checked against an oracle: arithmetic,
//! a truth table, a reference state machine or an instruction-set
//! interpreter).

use std::collections::BTreeSet;

use super::*;
use crate::sim::demos::{GALLERY_DIR, GALLERY_EMBEDDED, MANIFEST_FILE, gallery_key};
use crate::sim::layout::{GRID, overlaps};
use crate::sim::source::{self, circuit_id};

// ─────────────────────────────────────────────────────────────────────────────
// Files, format and structure
// ─────────────────────────────────────────────────────────────────────────────

/// Every file in `assets/gallery/` is embedded, unchanged, and listed.
#[test]
fn embedded_files_match_the_gallery_folder() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(GALLERY_DIR);
    let mut on_disk: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    on_disk.sort();
    let mut embedded: Vec<String> = GALLERY_EMBEDDED
        .iter()
        .map(|(n, _)| n.to_string())
        .collect();
    embedded.sort();
    assert_eq!(embedded, on_disk);
    for (name, text) in GALLERY_EMBEDDED {
        assert_eq!(
            std::fs::read_to_string(dir.join(name)).unwrap(),
            *text,
            "{name}"
        );
    }
    let listed: BTreeSet<String> = demos()
        .gallery
        .iter()
        .map(|d| format!("{}.json", d.id))
        .chain([MANIFEST_FILE.to_string()])
        .collect();
    assert_eq!(listed, on_disk.into_iter().collect());
}

/// Canonical text (what `demo_fmt` writes), readable lines, deterministic
/// expansion and ids derived from `gallery/<id>/<circuit>`.
#[test]
fn gallery_files_are_canonical_and_expand_deterministically() {
    for item in &demos().gallery {
        let (_, text) = GALLERY_EMBEDDED
            .iter()
            .find(|(n, _)| *n == format!("{}.json", item.id))
            .unwrap();
        let parsed = source::parse(text).unwrap();
        assert_eq!(source::to_text(&parsed), *text, "{} is canonical", item.id);
        assert!(
            text.lines().all(|l| l.len() <= 100),
            "{}: a line is longer than 100 columns",
            item.id
        );
        let again = source::expand(&parsed, &gallery_key(&item.id), &demos().externals()).unwrap();
        assert_eq!(again.workspace, *item.workspace, "{}", item.id);
        for (name, doc) in &item.circuits {
            assert_eq!(doc.id, circuit_id(&gallery_key(&item.id), name));
        }
    }
}

/// Between 20 and 35 items in four groups, simple to complex; each uses
/// only curriculum demos and items listed before it, and has a title and
/// a description.
#[test]
fn gallery_builds_on_the_curriculum_and_earlier_items() {
    let catalog = demos();
    assert!(
        (20..=35).contains(&catalog.gallery.len()),
        "{} items",
        catalog.gallery.len()
    );
    let categories: Vec<&str> = catalog.categories.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(
        categories,
        ["combinational", "sequential", "systems", "displays"]
    );
    let mut last = 0;
    for (i, item) in catalog.gallery.iter().enumerate() {
        assert_eq!(item.level, 0);
        let category = item.category.as_deref().unwrap();
        let c = categories.iter().position(|&x| x == category).unwrap();
        assert!(c >= last, "{}: categories are grouped", item.id);
        last = c;
        assert!(
            !item.title.is_empty() && item.description.len() > 40,
            "{}",
            item.id
        );
        assert!(catalog.demos.iter().all(|d| d.id != item.id));
        assert_eq!(
            item.source.external_uses(),
            item.uses.iter().cloned().collect::<BTreeSet<_>>()
        );
        for r in &item.uses {
            let (other, circuit) = r.split_once('/').unwrap();
            let earlier = catalog.demos.iter().any(|d| d.id == other)
                || catalog.gallery[..i].iter().any(|d| d.id == other);
            assert!(earlier, "{}: {r} is not listed before it", item.id);
            let key = if catalog.demos.iter().any(|d| d.id == other) {
                crate::sim::demos::demo_key(other)
            } else {
                gallery_key(other)
            };
            assert!(
                item.workspace
                    .definitions
                    .contains_key(&circuit_id(&key, circuit)),
                "{}: {r} is not instanced",
                item.id
            );
        }
    }
    // The pyramid continues: several items build on earlier gallery items.
    let reuse = catalog
        .gallery
        .iter()
        .filter(|g| {
            g.uses.iter().any(|r| {
                catalog
                    .gallery
                    .iter()
                    .any(|o| r.starts_with(&format!("{}/", o.id)))
            })
        })
        .count();
    assert!(reuse >= 8, "{reuse} items reuse gallery items");
}

/// Every circuit of every curriculum demo and gallery item (generated
/// adapters included) is laid out on the 20-unit grid with no two node
/// boxes overlapping.
#[test]
fn demo_and_gallery_layouts_have_no_overlapping_boxes() {
    let catalog = demos();
    assert!(!catalog.demos.is_empty() && !catalog.gallery.is_empty());
    for item in catalog.demos.iter().chain(&catalog.gallery) {
        for (name, doc) in &item.circuits {
            let boxes: Vec<(Vec2, Vec2)> = doc
                .nodes
                .iter()
                .map(|n| (Vec2::new(n.x, n.y), n.kind.size()))
                .collect();
            if let Some((a, b)) = overlaps(&boxes) {
                panic!(
                    "{}/{name}: {:?} at {:?} overlaps {:?} at {:?}",
                    item.id, doc.nodes[a].label, boxes[a].0, doc.nodes[b].label, boxes[b].0
                );
            }
            for n in &doc.nodes {
                assert!(
                    n.x % GRID == 0.0 && n.y % GRID == 0.0,
                    "{}/{name}: {:?} at ({}, {}) is off the grid",
                    item.id,
                    n.label,
                    n.x,
                    n.y
                );
            }
        }
    }
}

/// Gallery items save and load as version 5 workspaces unchanged.
#[test]
fn gallery_round_trips_through_workspace_files() {
    let dir = temp_dir();
    for item in &demos().gallery {
        save_workspace_in(&dir, &item.id, &item.workspace).unwrap();
        assert_eq!(
            read_workspace_in(&dir, &item.id).unwrap(),
            *item.workspace,
            "{}",
            item.id
        );
    }
    std::fs::remove_dir_all(dir).ok();
}

/// Every item compiles (no combinational loops) and simulates with every
/// wire connected.
#[test]
fn every_gallery_item_compiles() {
    for item in &demos().gallery {
        let mut app = open(&item.id);
        run(&mut app, 2);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Helpers
// ─────────────────────────────────────────────────────────────────────────────

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

/// Run `ticks` ticks one at a time and record `read` after each, dropping
/// repeats of the previous value: the sequence of states a clocked
/// circuit goes through, independent of the clock's phase.
fn trace<T: PartialEq>(app: &mut App, ticks: u64, mut read: impl FnMut(&mut App) -> T) -> Vec<T> {
    let mut out: Vec<T> = Vec::new();
    for _ in 0..ticks {
        run(app, 1);
        let v = read(app);
        if out.last() != Some(&v) {
            out.push(v);
        }
    }
    out
}

/// Press a button input: 1 for two ticks, then 0 for two ticks.
fn press(app: &mut App, label: &str) {
    set_input(app, label, 1);
    run(app, 2);
    set_input(app, label, 0);
    run(app, 2);
}

// ─────────────────────────────────────────────────────────────────────────────
// Combinational
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn majority_votes() {
    let mut app = open("majority");
    for v in 0..8u64 {
        let (a, b, c) = (v & 1, v >> 1 & 1, v >> 2 & 1);
        let got = eval(&mut app, &[("A", a), ("B", b), ("C", c)], &["Majority"]);
        assert_eq!(got, [u64::from(a + b + c >= 2)], "{a}{b}{c}");
    }
}

#[test]
fn comparators_compare() {
    let mut app = open("comparator");
    for a in 0..2 {
        for b in 0..2 {
            let got = eval(&mut app, &[("A", a), ("B", b)], &["Gt", "Eq", "Lt"]);
            assert_eq!(got, [u64::from(a > b), u64::from(a == b), u64::from(a < b)]);
        }
    }
    let mut app = open("magnitude");
    for a in 0..16 {
        for b in 0..16 {
            let got = eval(
                &mut app,
                &[("A", a), ("B", b)],
                &["A > B", "A = B", "A < B", "A = B (Eq)", "A < B (LtU)"],
            );
            let (gt, eq, lt) = (u64::from(a > b), u64::from(a == b), u64::from(a < b));
            assert_eq!(got, [gt, eq, lt, eq, lt], "{a} vs {b}");
        }
    }
}

#[test]
fn parity_counts_ones() {
    let mut app = open("parity");
    for d in [0u64, 1, 3, 0x80, 0xFF, 0xA5, 0x7F, 0x10, 0x6E, 0xFE] {
        let p = u64::from(d.count_ones() % 2);
        let got = eval(
            &mut app,
            &[("D", d)],
            &["Parity (gates)", "Parity (ReduceXor)"],
        );
        assert_eq!(got, [p, p], "{d:#x}");
    }
}

#[test]
fn gray_code_round_trips() {
    let mut app = open("gray_code");
    let mut previous: Option<u64> = None;
    for b in 0..16u64 {
        let got = eval(&mut app, &[("Binary", b)], &["Gray code", "Binary again"]);
        assert_eq!(got, [b ^ (b >> 1), b], "{b}");
        if let Some(p) = previous {
            assert_eq!((p ^ got[0]).count_ones(), 1, "neighbours differ in one bit");
        }
        previous = Some(got[0]);
    }
}

#[test]
fn priority_encoder_finds_the_highest_bit() {
    let mut app = open("priority_encoder");
    for d in [0u64, 1, 2, 3, 0x10, 0x11, 0x80, 0xFF, 0x46, 0x2C] {
        let want = if d == 0 {
            0
        } else {
            63 - u64::from(d.leading_zeros())
        };
        let got = eval(&mut app, &[("D", d)], &["Y", "Valid"]);
        assert_eq!(got, [want, u64::from(d != 0)], "{d:#x}");
    }
}

#[test]
fn seven_segment_shows_hex_digits() {
    // Lit segments of each hex digit, the usual way: 0 to 9, A, b, C, d, E, F.
    let digits = [
        "abcdef", "bc", "abdeg", "abcdg", "bcfg", "acdfg", "acdefg", "abc", "abcdefg", "abcdfg",
        "abcefg", "cdefg", "adef", "bcdeg", "adefg", "aefg",
    ];
    let mut app = open("seven_segment");
    for (d, lit) in digits.iter().enumerate() {
        let segments = ["a", "b", "c", "d", "e", "f", "g"];
        let got = eval(&mut app, &[("Digit", d as u64)], &segments);
        let want: Vec<u64> = segments
            .iter()
            .map(|s| u64::from(lit.contains(*s)))
            .collect();
        assert_eq!(got, want, "digit {d:X}");
        assert_eq!(value_at(&mut app, "Digit (hex)"), d as u64);
    }
}

#[test]
fn bcd_converts_to_binary() {
    let mut app = open("bcd_to_binary");
    for t in 0..10 {
        for o in [0, 1, 5, 9] {
            let got = eval(&mut app, &[("Tens", t), ("Ones", o)], &["Binary", "Value"]);
            assert_eq!(got, [10 * t + o, 10 * t + o], "{t}{o}");
        }
    }
}

#[test]
fn barrel_shifter_shifts_left() {
    let mut app = open("barrel_shifter");
    for a in [0x01u64, 0xFF, 0xA5, 0x3C] {
        for s in 0..8 {
            let got = eval(&mut app, &[("A", a), ("S", s)], &["Y"]);
            assert_eq!(got, [(a << s) & 0xFF], "{a:#x} << {s}");
        }
    }
}

#[test]
fn carry_lookahead_matches_ripple_and_arithmetic() {
    let mut app = open("cla_adder");
    for a in 0..16 {
        for b in 0..16 {
            let c = (a + b) & 1;
            let got = eval(
                &mut app,
                &[("A", a), ("B", b), ("Cin", c)],
                &[
                    "Sum (lookahead)",
                    "Cout (lookahead)",
                    "Sum (ripple)",
                    "Cout (ripple)",
                ],
            );
            let s = a + b + c;
            assert_eq!(got, [s & 15, s >> 4, s & 15, s >> 4], "{a} + {b} + {c}");
        }
    }
}

#[test]
fn multiplier_multiplies() {
    let mut app = open("multiplier");
    for a in 0..16 {
        for b in 0..16 {
            let got = eval(&mut app, &[("A", a), ("B", b)], &["P", "A x B"]);
            assert_eq!(got, [a * b, a * b], "{a} x {b}");
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Sequential
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn edge_detector_flashes_for_one_tick() {
    let mut app = open("edge_detector");
    run(&mut app, 2);
    set_input(&mut app, "Signal", 1);
    let rising = trace(&mut app, 4, |app| {
        (value_at(app, "Rising edge"), value_at(app, "Falling edge"))
    });
    assert_eq!(rising, [(1, 0), (0, 0)], "one tick on the rising edge");
    set_input(&mut app, "Signal", 0);
    let falling = trace(&mut app, 4, |app| {
        (value_at(app, "Rising edge"), value_at(app, "Falling edge"))
    });
    assert_eq!(falling, [(0, 1), (0, 0)], "one tick on the falling edge");
}

fn lights(app: &mut App, labels: &[&str]) -> u64 {
    labels
        .iter()
        .enumerate()
        .map(|(i, l)| value_at(app, l) << i)
        .sum()
}

#[test]
fn shift_register_moves_data_along() {
    let mut app = open("shift_register");
    let q = ["Q0", "Q1", "Q2", "Q3"];
    // One clock period is 8 ticks: set Data, let one rising edge pass.
    let data = [1, 0, 1, 1, 0, 0, 0];
    let mut want = 0u64;
    for d in data {
        set_input(&mut app, "Data", d);
        run(&mut app, 8);
        want = (want << 1 | d) & 0xF;
        // Q0 holds the newest bit.
        assert_eq!(lights(&mut app, &q), want, "after shifting in {d}");
    }
}

#[test]
fn johnson_counter_walks_eight_states() {
    let mut app = open("johnson_counter");
    let states = trace(&mut app, 80, |app| lights(app, &["Q0", "Q1", "Q2", "Q3"]));
    let want = [
        0b0000, 0b0001, 0b0011, 0b0111, 0b1111, 0b1110, 0b1100, 0b1000, 0b0000,
    ];
    assert!(states.len() >= 9, "{states:?}");
    assert_eq!(states[..9], want);
}

/// The LFSR of the gallery: shift left, feed in XNOR of bits 7, 5, 4, 3.
fn lfsr_next(q: u64) -> u64 {
    let fb = 1 ^ ((q >> 7 ^ q >> 5 ^ q >> 4 ^ q >> 3) & 1);
    (q << 1 | fb) & 0xFF
}

#[test]
fn lfsr_matches_its_polynomial() {
    // The sequence has period 255.
    let mut q = 0;
    let mut seen = BTreeSet::new();
    while seen.insert(q) {
        q = lfsr_next(q);
    }
    assert_eq!(seen.len(), 255);
    assert!(!seen.contains(&0xFF));

    let mut app = open("lfsr");
    let got = trace(&mut app, 4 * 40, |app| value_at(app, "Random number"));
    let want: Vec<u64> = std::iter::successors(Some(0), |&q| Some(lfsr_next(q)))
        .take(got.len())
        .collect();
    assert!(got.len() >= 30, "{got:?}");
    assert_eq!(got, want);
    // The lights show the same number bit by bit.
    let leds: Vec<String> = (0..8).map(|i| format!("Q{i}")).collect();
    let leds: Vec<&str> = leds.iter().map(String::as_str).collect();
    assert_eq!(lights(&mut app, &leds), value_at(&mut app, "Random number"));
}

#[test]
fn clock_divider_halves_each_bit() {
    let mut app = open("clock_divider");
    let labels = [
        "Clock in",
        "Clock / 2",
        "Clock / 4",
        "Clock / 8",
        "Clock / 16",
    ];
    let mut rises = [0u32; 5];
    let mut last = [0u64; 5];
    for _ in 0..256 {
        run(&mut app, 1);
        for (i, l) in labels.iter().enumerate() {
            let v = value_at(&mut app, l);
            if v == 1 && last[i] == 0 {
                rises[i] += 1;
            }
            last[i] = v;
        }
    }
    // 64 clock periods in 256 ticks; each output has half the rising edges
    // of the one before (give or take the one in progress).
    assert!(rises[0].abs_diff(64) <= 1, "{rises:?}");
    for i in 1..5 {
        assert!(
            (rises[i - 1] / 2).abs_diff(rises[i]) <= 1,
            "{} rises {} times, {} {} times",
            labels[i - 1],
            rises[i - 1],
            labels[i],
            rises[i]
        );
    }
    let count = value_at(&mut app, "Count");
    assert_eq!(value_at(&mut app, "Clock / 2"), count & 1);
    assert_eq!(value_at(&mut app, "Clock / 16"), count >> 3 & 1);
}

#[test]
fn up_down_counter_counts_both_ways() {
    let mut app = open("updown_counter");
    set_input(&mut app, "Enable", 1);
    set_input(&mut app, "Up", 1);
    let up = trace(&mut app, 4 * 5, |app| value_at(app, "Count"));
    assert_eq!(up[..5], [0, 1, 2, 3, 4]);
    let top = *up.last().unwrap();
    set_input(&mut app, "Up", 0);
    let down = trace(&mut app, 4 * 10, |app| value_at(app, "Count"));
    let want: Vec<u64> = (0..down.len() as u64)
        .map(|i| top.wrapping_sub(i) & 0xFF)
        .collect();
    assert_eq!(down, want, "down through 0 to 255");
    assert!(down.contains(&255));
    set_input(&mut app, "Enable", 0);
    let held = value_at(&mut app, "Count");
    run(&mut app, 12);
    assert_eq!(value_at(&mut app, "Count"), held, "holds without Enable");
}

#[test]
fn bcd_counter_counts_in_decimal() {
    let mut app = open("bcd_counter");
    set_input(&mut app, "Enable", 1);
    let got = trace(&mut app, 4 * 30, |app| {
        10 * value_at(app, "Tens") + value_at(app, "Ones")
    });
    let want: Vec<u64> = (0..got.len() as u64).collect();
    assert!(got.len() >= 25, "{got:?}");
    assert_eq!(got, want, "00, 01, ... 09, 10, ...");
    for v in [value_at(&mut app, "Ones"), value_at(&mut app, "Tens")] {
        assert!(v <= 9);
    }
    set_input(&mut app, "Reset", 1);
    run(&mut app, 4);
    assert_eq!(
        (value_at(&mut app, "Tens"), value_at(&mut app, "Ones")),
        (0, 0)
    );
}

#[test]
fn debouncer_ignores_bounces() {
    let mut app = open("debouncer");
    // Bounces shorter than four samples never reach the output.
    for pattern in [[1, 0, 1, 1, 0, 1, 0, 0], [1, 1, 1, 0, 1, 1, 0, 0]] {
        for b in pattern {
            set_input(&mut app, "Button", b);
            run(&mut app, 1);
            assert_eq!(value_at(&mut app, "Clean"), 0, "bouncing {pattern:?}");
        }
    }
    // A steady press gets through after the samples agree.
    set_input(&mut app, "Button", 1);
    let pressed = trace(&mut app, 8, |app| value_at(app, "Clean"));
    assert_eq!(pressed, [0, 1]);
    // Bouncing low while held does not release it.
    for b in [0, 1, 0, 0, 1] {
        set_input(&mut app, "Button", b);
        run(&mut app, 1);
        assert_eq!(value_at(&mut app, "Clean"), 1);
    }
    set_input(&mut app, "Button", 0);
    let released = trace(&mut app, 8, |app| value_at(app, "Clean"));
    assert_eq!(released, [1, 0]);
}

#[test]
fn pwm_is_on_for_duty_out_of_sixteen() {
    let mut app = open("pwm");
    for duty in [0u64, 1, 4, 9, 15] {
        set_input(&mut app, "Duty", duty);
        run(&mut app, 16);
        let mut on = 0;
        for _ in 0..16 {
            run(&mut app, 1);
            on += value_at(&mut app, "PWM");
        }
        assert_eq!(on, duty, "duty {duty}");
    }
}

#[test]
fn sequence_detector_finds_1011() {
    let mut app = open("sequence_detector");
    let bits = [1, 0, 1, 1, 0, 1, 1, 1, 0, 1, 0, 1, 1, 0, 0, 1, 0, 1, 1];
    // The reference: found when the last four bits are 1011.
    let mut history = 0u64;
    for (i, &b) in bits.iter().enumerate() {
        set_input(&mut app, "Bit", b);
        run(&mut app, 1);
        press(&mut app, "Step");
        history = (history << 1 | b) & 0xF;
        let want = u64::from(i >= 3 && history == 0b1011);
        assert_eq!(value_at(&mut app, "Found 1011"), want, "after bit {i}");
    }
}

#[test]
fn traffic_light_cycles_through_its_phases() {
    let mut app = open("traffic_light");
    let names = [
        "North-south red",
        "North-south yellow",
        "North-south green",
        "East-west red",
        "East-west yellow",
        "East-west green",
    ];
    let got = trace(&mut app, 8 * 22, |app| lights(app, &names));
    // NS green / EW red, NS yellow / EW red, NS red / EW green,
    // NS red / EW yellow, and again.
    let cycle = [0b001100, 0b001010, 0b100001, 0b010001];
    let want: Vec<u64> = cycle.iter().cycle().copied().take(got.len()).collect();
    assert!(got.len() >= 8, "{got:?}");
    assert_eq!(got, want);
    // Exactly one light of each direction is on.
    for g in got {
        assert_eq!((g & 7).count_ones(), 1);
        assert_eq!((g >> 3).count_ones(), 1);
    }
    let phases = trace(&mut app, 8 * 12, |app| value_at(app, "Phase"));
    assert!(phases.iter().all(|&p| p <= 9));
    assert!(
        phases.windows(2).all(|w| w[1] == (w[0] + 1) % 10),
        "{phases:?}"
    );
}

#[test]
fn stopwatch_counts_tenths_and_stops() {
    let mut app = open("stopwatch");
    let read = |app: &mut App| {
        100 * value_at(app, "Tens of seconds")
            + 10 * value_at(app, "Seconds")
            + value_at(app, "Tenths")
    };
    run(&mut app, 12);
    assert_eq!(read(&mut app), 0, "stopped until Run");
    set_input(&mut app, "Run", 1);
    let got = trace(&mut app, 4 * 130, read);
    let want: Vec<u64> = (0..got.len() as u64).collect();
    assert!(got.len() >= 120, "{}", got.len());
    assert_eq!(got, want);
    // Up to 59.9, then 00.0.
    run(&mut app, 4 * (600 - got.len() as u64));
    let wrap = trace(&mut app, 4 * 4, read);
    assert!(wrap.windows(2).any(|w| w == [599, 0]), "{wrap:?}");
    set_input(&mut app, "Run", 0);
    let held = read(&mut app);
    run(&mut app, 20);
    assert_eq!(read(&mut app), held, "Run off holds the time");
    set_input(&mut app, "Reset", 1);
    run(&mut app, 2);
    assert_eq!(read(&mut app), 0);
}

// ─────────────────────────────────────────────────────────────────────────────
// Systems
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn stack_is_last_in_first_out() {
    let mut app = open("stack");
    let state = |app: &mut App| {
        (
            value_at(app, "Top"),
            value_at(app, "Depth"),
            value_at(app, "Empty"),
            value_at(app, "Full"),
        )
    };
    run(&mut app, 2);
    assert_eq!(state(&mut app), (0, 0, 1, 0));
    let mut model: Vec<u64> = Vec::new();
    let check = |app: &mut App, model: &Vec<u64>| {
        let want = (
            model.last().copied().unwrap_or(0),
            model.len() as u64,
            u64::from(model.is_empty()),
            u64::from(model.len() == 8),
        );
        assert_eq!(state(app), want, "{model:?}");
    };
    for v in [0x11u64, 0x22, 0x33] {
        set_input(&mut app, "Data", v);
        press(&mut app, "Push");
        model.push(v);
        check(&mut app, &model);
    }
    press(&mut app, "Pop");
    model.pop();
    check(&mut app, &model);
    // Fill it: pushing a ninth byte is ignored.
    for v in 0x40..0x48u64 {
        set_input(&mut app, "Data", v);
        press(&mut app, "Push");
        if model.len() < 8 {
            model.push(v);
        }
        check(&mut app, &model);
    }
    // Empty it: popping when empty is ignored.
    for _ in 0..10 {
        press(&mut app, "Pop");
        model.pop();
        check(&mut app, &model);
    }
}

#[test]
fn uart_sends_a_frame() {
    let mut app = open("uart_tx");
    let data = 0xA5u64;
    set_input(&mut app, "Data", data);
    run(&mut app, 8);
    assert_eq!(value_at(&mut app, "TX line"), 1, "idle line is 1");
    assert_eq!(value_at(&mut app, "Busy"), 0);
    set_input(&mut app, "Send", 1);
    // Record the line every tick while busy.
    let mut line = Vec::new();
    let mut started = false;
    for _ in 0..80 {
        run(&mut app, 1);
        let busy = value_at(&mut app, "Busy");
        if busy == 1 {
            started = true;
            set_input(&mut app, "Send", 0);
            line.push(value_at(&mut app, "TX line"));
        } else if started {
            break;
        }
    }
    // Ten bits of four ticks each: start 0, data from bit 0, stop 1.
    assert_eq!(line.len(), 40, "{line:?}");
    let bits: Vec<u64> = line.chunks(4).map(|c| c[0]).collect();
    for c in line.chunks(4) {
        assert!(c.iter().all(|&b| b == c[0]), "each bit lasts a period");
    }
    let mut want = vec![0];
    want.extend((0..8).map(|i| data >> i & 1));
    want.push(1);
    assert_eq!(bits, want);
    run(&mut app, 8);
    assert_eq!(value_at(&mut app, "TX line"), 1);
}

#[test]
fn reaction_timer_measures_the_reaction() {
    let mut app = open("reaction_timer");
    run(&mut app, 3);
    press(&mut app, "Start");
    // GO! within 8 to 15 periods (plus the latches' tick).
    let mut waited = 4;
    while value_at(&mut app, "GO!") == 0 {
        run(&mut app, 1);
        waited += 1;
        assert!(waited <= 4 * 16 + 8, "GO! never lit");
        assert_eq!(value_at(&mut app, "Too soon!"), 0);
    }
    assert!(waited >= 4 * 7, "waited only {waited} ticks");
    // React after 13 periods.
    run(&mut app, 4 * 13);
    press(&mut app, "Stop");
    assert_eq!(value_at(&mut app, "GO!"), 0);
    let time = 10 * value_at(&mut app, "Time (tens)") + value_at(&mut app, "Time (ones)");
    assert!((13..=15).contains(&time), "reaction time {time}");
    run(&mut app, 40);
    let after = 10 * value_at(&mut app, "Time (tens)") + value_at(&mut app, "Time (ones)");
    assert_eq!(after, time, "the time stays on the display");

    // Stop before GO!: too soon, and no GO!.
    press(&mut app, "Start");
    run(&mut app, 8);
    press(&mut app, "Stop");
    assert_eq!(value_at(&mut app, "Too soon!"), 1);
    run(&mut app, 4 * 20);
    assert_eq!(value_at(&mut app, "GO!"), 0);
}

/// Reference interpreter of the extended instruction set (the curriculum
/// CPU's ops plus 12 ADDI, 13 SUBI, 14 ANDI): the values written by OUT,
/// and the pc of HLT.
fn run_program(program: &[u64]) -> (Vec<u64>, u64) {
    let mut regs = [0u64; 4];
    let mut pc = 0u64;
    let mut outs = Vec::new();
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
            8 => outs.push(regs[rs]),
            12 => regs[rd] = (regs[rs] + imm) & 0xFF,
            13 => regs[rd] = regs[rs].wrapping_sub(imm) & 0xFF,
            14 => regs[rd] = regs[rs] & imm,
            15 => return (outs, pc),
            _ => {}
        }
        pc = next;
    }
    panic!("the program did not halt");
}

#[test]
fn fibonacci_cpu_prints_the_sequence() {
    let item = demos().get("cpu_fibonacci").unwrap();
    let program = item
        .workspace
        .root
        .nodes
        .iter()
        .find_map(|n| match (&n.kind, n.label.as_deref()) {
            (GateKind::Rom { contents, .. }, Some("program")) => Some(contents.clone()),
            _ => None,
        })
        .expect("a program ROM");
    let (outs, halt_pc) = run_program(&program);
    assert_eq!(outs, [0, 1, 1, 2, 3, 5, 8, 13, 21, 34, 55, 89, 144]);

    let mut app = open("cpu_fibonacci");
    let mut seen: Vec<u64> = vec![value_at(&mut app, "Out value")];
    let mut ticks = 0;
    while value_at(&mut app, "Halted") == 0 {
        run(&mut app, 1);
        ticks += 1;
        assert!(ticks < 2000, "the CPU did not halt");
        let v = value_at(&mut app, "Out value");
        if seen.last() != Some(&v) {
            seen.push(v);
        }
    }
    run(&mut app, 8);
    let mut want = outs.clone();
    want.dedup();
    assert_eq!(seen, want, "Out shows each number in turn");
    assert_eq!(value_at(&mut app, "Out"), 144);
    assert_eq!(value_at(&mut app, "PC"), halt_pc);
}

/// The CPU's output sequence until it halts (one tick at a time).
fn fibonacci_until_halt(app: &mut App) -> Vec<u64> {
    let mut seen: Vec<u64> = vec![value_at(app, "Out value")];
    let mut ticks = 0;
    while value_at(app, "Halted") == 0 {
        run(app, 1);
        ticks += 1;
        assert!(ticks < 2000, "the CPU did not halt");
        let v = value_at(app, "Out value");
        if seen.last() != Some(&v) {
            seen.push(v);
        }
    }
    seen
}

/// Reset (the transport's Reset button and `R`) returns a halted CPU to
/// power-on: tick 0, registers and RAM cleared, the program runs again
/// from the start.
#[test]
fn reset_restarts_the_fibonacci_cpu() {
    let mut app = open("cpu_fibonacci");
    let first = fibonacci_until_halt(&mut app);
    assert_eq!(first, [0, 1, 2, 3, 5, 8, 13, 21, 34, 55, 89, 144]);
    assert!(app.world().resource::<SimBridge>().tick_count() > 0);
    let running = app.world().resource::<SimBridge>().is_running();

    app.world().resource::<SimBridge>().reset();
    settle(&mut app);
    assert_eq!(app.world().resource::<SimBridge>().tick_count(), 0);
    assert_eq!(app.world().resource::<SimBridge>().is_running(), running);
    assert_eq!(value_at(&mut app, "Halted"), 0);
    assert_eq!(value_at(&mut app, "PC"), 0);
    assert_eq!(value_at(&mut app, "Out value"), 0);

    let again = fibonacci_until_halt(&mut app);
    assert_eq!(
        again, first,
        "it prints 0, 1, 1, 2, … again (repeats show once)"
    );
}

/// A gallery item placed as a chip (the Chips tab of the parts bar):
/// its definitions are merged into the library, it builds, and it runs
/// inside the chip as it does on its own.
#[test]
fn gallery_item_places_and_runs_as_a_chip() {
    let mut app = app();
    let item = demo("cpu_fibonacci");
    let (registration, kind) = crate::sim::RegisterDefinitions::chip(&item);
    send(&mut app, registration);
    {
        let library = app.world().resource::<crate::sim::CircuitLibrary>();
        for id in item.definitions.keys().chain([&item.root.id]) {
            assert!(library.get(*id).is_some(), "definition merged");
        }
    }
    let chip = create_gate(&mut app, kind.clone(), "Fib");
    let (_, outs) = app
        .world()
        .resource::<crate::sim::CircuitLibrary>()
        .pin_names(&kind);
    let pin = |name: &str| outs.iter().position(|n| n == name).unwrap() as u8;
    let out = create_gate(&mut app, GateKind::Output { w: 8 }, "Out");
    let halted = create_gate(&mut app, GateKind::Output { w: 1 }, "Done");
    create_wire(&mut app, chip, pin("Out value"), out, 0);
    create_wire(&mut app, chip, pin("Halted"), halted, 0);
    settle(&mut app);
    assert!(
        app.world()
            .resource::<SimDiagnostics>()
            .compile_error
            .is_none(),
        "the chip builds"
    );
    let mut seen = vec![value_at(&mut app, "Out")];
    let mut ticks = 0;
    while value_at(&mut app, "Done") == 0 {
        run(&mut app, 1);
        ticks += 1;
        assert!(ticks < 2000, "the chip's CPU did not halt");
        let v = value_at(&mut app, "Out");
        if seen.last() != Some(&v) {
            seen.push(v);
        }
    }
    assert_eq!(seen, [0, 1, 2, 3, 5, 8, 13, 21, 34, 55, 89, 144]);
}

// ─────────────────────────────────────────────────────────────────────────────
// The 16-bit CPUs (`cpu16` of gates, `cpu16_rtl` of words)
// ─────────────────────────────────────────────────────────────────────────────

/// What a run of the cpu16 instruction set produced: the values written by
/// OUT, the number of instructions executed and the address of HLT.
#[derive(Debug, PartialEq)]
struct Cpu16Run {
    outs: Vec<u64>,
    steps: u64,
    halt_pc: u64,
}

/// Reference interpreter of the cpu16 instruction set (see the gallery
/// manifest and `docs/save-format.md`).
fn run_cpu16(program: &[u64], max_steps: u64) -> Option<Cpu16Run> {
    let mut r = [0u64; 8];
    let (mut pc, mut sp) = (0u64, 0u64);
    let (mut z, mut n, mut c) = (0u64, 0u64, 0u64);
    let mut ram = [0u64; 256];
    let mut stack = [0u64; 16];
    let mut outs = Vec::new();
    let alu = |a: u64, b: u64, f: u64| -> (u64, u64) {
        let sub = u64::from(f == 1);
        let t = a + if sub == 1 { b ^ 0xFFFF } else { b } + sub;
        let (y, carry) = match f {
            0 | 1 => (t, t >> 16 & 1),
            2 => (a & b, 0),
            3 => (a | b, 0),
            4 => (a ^ b, 0),
            5 => (!a, 0),
            6 => (a << 1, a >> 15 & 1),
            _ => (a >> 1, a & 1),
        };
        (y & 0xFFFF, carry)
    };
    for step in 1..=max_steps {
        let ins = program.get((pc & 0xFF) as usize).copied().unwrap_or(0);
        let here = pc;
        pc = (pc + 1) & 0xFFFF;
        let (op, rd, rs, rt, f) = (
            ins >> 12,
            (ins >> 9 & 7) as usize,
            (ins >> 6 & 7) as usize,
            (ins >> 3 & 7) as usize,
            ins & 7,
        );
        let imm8 = ins & 0xFF;
        let simm6 = if ins & 32 != 0 {
            (ins & 63) | 0xFFC0
        } else {
            ins & 63
        };
        let mut flags = |y: u64, carry: u64| {
            z = u64::from(y == 0);
            n = y >> 15;
            c = carry;
        };
        match op {
            1 => {
                let (y, carry) = alu(r[rs], r[rt], f);
                flags(y, carry);
                r[rd] = y;
            }
            2 => {
                let (y, carry) = alu(r[rs], r[rt], 1);
                flags(y, carry);
            }
            3 => r[rd] = imm8,
            4 => {
                let (y, carry) = alu(r[rs], simm6, 0);
                flags(y, carry);
                r[rd] = y;
            }
            5 => r[rd] = ram[((r[rs] + simm6) & 0xFF) as usize],
            6 => ram[((r[rs] + simm6) & 0xFF) as usize] = r[rd],
            8 => pc = imm8,
            9 => {
                let flag = [z, c, n, 1][rd >> 1];
                if flag ^ (rd as u64 & 1) == 1 {
                    pc = imm8;
                }
            }
            10 => {
                sp = sp.wrapping_sub(1) & 0xFFFF;
                stack[(sp & 15) as usize] = pc;
                pc = imm8;
            }
            11 => {
                pc = stack[(sp & 15) as usize];
                sp = (sp + 1) & 0xFFFF;
            }
            12 => outs.push(r[rs]),
            15 => {
                return Some(Cpu16Run {
                    outs,
                    steps: step,
                    halt_pc: here,
                });
            }
            _ => {}
        }
    }
    None
}

/// A tiny assembler for the cpu16 test programs.
#[derive(Default)]
struct Asm(Vec<u64>);

impl Asm {
    fn at(&self) -> u64 {
        self.0.len() as u64
    }
    fn alu(&mut self, f: u64, rd: u64, rs: u64, rt: u64) {
        self.0.push(1 << 12 | rd << 9 | rs << 6 | rt << 3 | f);
    }
    fn cmp(&mut self, rs: u64, rt: u64) {
        self.0.push(2 << 12 | rs << 6 | rt << 3 | 1);
    }
    fn ldi(&mut self, rd: u64, imm: u64) {
        self.0.push(3 << 12 | rd << 9 | imm);
    }
    fn rsi(&mut self, op: u64, rd: u64, rs: u64, imm: i64) {
        assert!((-32..32).contains(&imm));
        self.0
            .push(op << 12 | rd << 9 | rs << 6 | (imm as u64 & 63));
    }
    fn addi(&mut self, rd: u64, rs: u64, imm: i64) {
        self.rsi(4, rd, rs, imm);
    }
    fn ld(&mut self, rd: u64, rs: u64, imm: i64) {
        self.rsi(5, rd, rs, imm);
    }
    fn st(&mut self, rd: u64, rs: u64, imm: i64) {
        self.rsi(6, rd, rs, imm);
    }
    fn jump(&mut self, op: u64, cond: u64, target: u64) {
        self.0.push(op << 12 | cond << 9 | target);
    }
    fn out(&mut self, rs: u64) {
        self.0.push(12 << 12 | rs << 6);
    }
    fn raw(&mut self, word: u64) {
        self.0.push(word);
    }
}

/// A program that uses every instruction, every ALU function and every
/// branch condition, and prints what it computes. No two consecutive
/// values printed are equal, so the Out display shows each of them.
fn cpu16_self_test() -> Vec<u64> {
    let mut a = Asm::default();
    a.ldi(1, 0x5A);
    a.ldi(2, 0x0F);
    a.addi(4, 0, -1); // 0xFFFF
    // Neighbouring results differ.
    for f in 0..8 {
        for (x, y) in [(1, 2), (4, 1), (2, 1)] {
            a.alu(f, 3, x, y);
            a.out(3);
        }
    }
    // Every condition after each kind of flags: print 2 * k + taken (LDI
    // leaves the flags alone).
    let mut k = 0;
    for setup in 0..4 {
        match setup {
            0 => a.alu(1, 5, 1, 2), // 0x4B: C
            1 => a.cmp(2, 1),       // negative, borrow: N
            2 => a.alu(6, 5, 4, 0), // 0xFFFE: N, C
            _ => a.cmp(4, 4),       // Z, C
        }
        for cond in 0..8 {
            a.ldi(6, 2 * k + 1);
            let skip = a.at() + 2;
            a.jump(9, cond, skip);
            a.ldi(6, 2 * k);
            a.out(6);
            k += 1;
        }
    }
    // ADDI flags and carry.
    a.addi(5, 4, 1);
    a.ldi(6, 0xA0);
    a.jump(9, 3, a.at() + 2); // no carry: skip
    a.addi(6, 6, 7);
    a.out(6);
    // Memory: stores with offsets, loads back.
    a.ldi(7, 20);
    a.st(1, 7, -3);
    a.st(2, 7, 5);
    a.st(4, 7, 31);
    a.ldi(5, 0x53);
    a.ld(3, 7, -3); // 17
    a.out(3);
    a.ld(3, 5, -32); // 51
    a.out(3);
    a.ld(3, 7, 5); // 25
    a.out(3);
    a.ld(3, 7, 31); // 51
    a.out(3);
    // Unused opcodes do nothing.
    a.raw(0x0000);
    a.raw(0x7FFF);
    a.raw(0xD123);
    a.raw(0xE456);
    // Nested calls: f adds 1 and calls g, g shifts left; recursion 20
    // deep through h (deeper than the 16-word stack only if it wraps).
    a.ldi(3, 3);
    let call_f = a.at();
    a.jump(10, 0, 0); // patched below
    a.out(3);
    a.ldi(7, 12);
    let call_h = a.at();
    a.jump(10, 0, 0);
    a.addi(3, 3, -3);
    a.out(3);
    let jmp_end = a.at();
    a.jump(8, 0, 0);
    let f = a.at();
    a.addi(3, 3, 1);
    let call_g = a.at();
    a.jump(10, 0, 0);
    a.raw(11 << 12);
    let g = a.at();
    a.alu(6, 3, 3, 0);
    a.raw(11 << 12);
    let h = a.at(); // r3 += r7 + (r7 - 1) + ... recursively
    a.cmp(7, 0);
    let ret_h = a.at() + 6;
    a.jump(9, 0, ret_h);
    a.alu(0, 3, 3, 7);
    a.addi(7, 7, -1);
    a.jump(10, 0, h);
    a.addi(3, 3, 1);
    a.out(3);
    a.raw(11 << 12);
    let end = a.at();
    a.raw(15 << 12);
    a.0[call_f as usize] |= f;
    a.0[call_h as usize] |= h;
    a.0[call_g as usize] |= g;
    a.0[jmp_end as usize] |= end;
    assert_eq!(a.0[ret_h as usize], 11 << 12);
    assert!(a.0.len() <= 256);
    a.0
}

/// The program ROM of a CPU workspace.
fn program_of(workspace: &WorkspaceDocument) -> Vec<u64> {
    workspace
        .root
        .nodes
        .iter()
        .find_map(|n| match (&n.kind, n.label.as_deref()) {
            (GateKind::Rom { contents, .. }, Some("program")) => Some(contents.clone()),
            _ => None,
        })
        .expect("a program ROM")
}

/// `workspace` with its program ROM holding `program`.
fn with_program(workspace: &WorkspaceDocument, program: &[u64]) -> WorkspaceDocument {
    let mut w = workspace.clone();
    for n in &mut w.root.nodes {
        if let (GateKind::Rom { contents, .. }, Some("program")) = (&mut n.kind, n.label.as_deref())
        {
            *contents = program.to_vec();
        }
    }
    w
}

fn sim_of(workspace: &WorkspaceDocument) -> logi_sim::sim::Sim {
    let design = crate::sim::netlist::design_of(workspace).unwrap();
    let program = logi_sim::compile::compile(&design, &logi_sim::compile::Options::default())
        .unwrap_or_else(|e| panic!("{e:?}"));
    logi_sim::sim::Sim::new(Arc::new(program))
}

/// Run a CPU until Halted (at most `max_ticks`): the values Out showed,
/// in order, the ticks taken and the final PC.
fn run_cpu(workspace: &WorkspaceDocument, max_ticks: u64) -> (Vec<u64>, u64, u64) {
    let mut sim = sim_of(workspace);
    let mut seen = vec![sim.get("Out value")];
    while sim.get("Halted") == 0 {
        sim.tick();
        assert!(sim.tick_count() < max_ticks, "did not halt");
        let v = sim.get("Out value");
        if seen.last() != Some(&v) {
            seen.push(v);
        }
    }
    let ticks = sim.tick_count();
    // Halted stays, and so does everything else.
    let pc = sim.get("PC");
    sim.run(20);
    assert_eq!((sim.get("Halted"), sim.get("PC")), (1, pc));
    assert_eq!(sim.get("Out value"), *seen.last().unwrap());
    (seen, ticks, pc)
}

/// What the Out display shows for a run: 0 first, then each value OUT
/// writes (repeats of the previous value are not seen).
fn shown(outs: &[u64]) -> Vec<u64> {
    let mut want = vec![0];
    want.extend_from_slice(outs);
    want.dedup();
    want
}

fn primes_below(limit: u64) -> Vec<u64> {
    (2..limit)
        .filter(|&n| (2..n).take_while(|d| d * d <= n).all(|d| n % d != 0))
        .collect()
}

/// The two CPUs hold the same program, and it prints the primes below 250.
#[test]
fn cpu16_program_prints_the_primes() {
    let gates = demos().get("cpu16").unwrap();
    let words = demos().get("cpu16_rtl").unwrap();
    let program = program_of(&gates.workspace);
    assert_eq!(program, program_of(&words.workspace));
    let run = run_cpu16(&program, 1_000_000).expect("halts");
    assert_eq!(run.outs, primes_below(250));
    assert!(run.steps > 50_000, "{} instructions", run.steps);
}

/// Every input pin of every node of both CPUs is connected, in every
/// definition.
#[test]
fn cpu16_has_no_unconnected_pins() {
    for id in ["cpu16", "cpu16_rtl"] {
        let design = crate::sim::netlist::design_of(&demos().get(id).unwrap().workspace).unwrap();
        for (_, def) in &design.defs {
            for (node, n) in &def.nodes {
                assert!(
                    n.inputs.iter().all(Option::is_some),
                    "{id}: {}: {:?} {:?} has an unconnected input",
                    def.name,
                    def.name_of(node),
                    n.kind
                );
            }
        }
    }
}

/// The word CPU runs the whole program, one instruction per tick, and
/// prints what the interpreter prints.
#[test]
fn cpu16_rtl_matches_the_interpreter() {
    let words = &demos().get("cpu16_rtl").unwrap().workspace;
    let run = run_cpu16(&program_of(words), 1_000_000).unwrap();
    let (seen, ticks, pc) = run_cpu(words, 100_000);
    assert_eq!(seen, shown(&run.outs));
    assert_eq!(ticks, run.steps, "one instruction per tick");
    assert_eq!(pc, run.halt_pc + 1);
}

/// The gate CPU runs the whole program (about 360 000 ticks: three or
/// four clock periods of two ticks per instruction) and prints the same
/// sequence as the interpreter and the word CPU.
#[test]
fn cpu16_of_gates_matches_the_interpreter_and_the_word_cpu() {
    let gates = &demos().get("cpu16").unwrap().workspace;
    let words = &demos().get("cpu16_rtl").unwrap().workspace;
    let run = run_cpu16(&program_of(gates), 1_000_000).unwrap();
    let (seen, ticks, pc) = run_cpu(gates, 1_000_000);
    assert_eq!(seen, shown(&run.outs));
    assert_eq!(seen, run_cpu(words, 100_000).0, "the same Out sequence");
    assert_eq!(pc, run.halt_pc + 1);
    let per = ticks as f64 / run.steps as f64;
    assert!((6.0..=8.5).contains(&per), "{per} ticks per instruction");
}

/// Both CPUs run a program that uses every instruction, ALU function and
/// branch condition, and print what the interpreter prints.
#[test]
fn cpu16_self_test_program_runs_on_both_cpus() {
    let program = cpu16_self_test();
    let run = run_cpu16(&program, 100_000).unwrap();
    assert!(run.outs.windows(2).all(|w| w[0] != w[1]), "{:x?}", run.outs);
    assert!(run.outs.len() > 70, "{} values", run.outs.len());
    for id in ["cpu16_rtl", "cpu16"] {
        let w = with_program(&demos().get(id).unwrap().workspace, &program);
        let (seen, _, pc) = run_cpu(&w, 100_000);
        assert_eq!(seen, shown(&run.outs), "{id}");
        assert_eq!(pc, run.halt_pc + 1, "{id}");
    }
}

/// The 16-bit ALUs of both CPUs, alone, against the interpreter's ALU.
#[test]
fn cpu16_alus_compute_every_function() {
    let cases = [
        (0u64, 0u64),
        (0x5A, 0x0F),
        (0x0F, 0x5A),
        (0xFFFF, 1),
        (0x8000, 0x8000),
        (0x1234, 0xFEDC),
        (0x7FFF, 0x7FFF),
        (0xAAAA, 0x5555),
    ];
    for (id, alu) in [("cpu16", "alu16"), ("cpu16_rtl", "rtl_alu")] {
        let item = demos().get(id).unwrap();
        let doc = &item.circuits.iter().find(|(n, _)| n == alu).unwrap().1;
        let w = WorkspaceDocument {
            root: (**doc).clone(),
            definitions: item.workspace.definitions.clone(),
            tests: Default::default(),
        };
        let mut sim = sim_of(&w);
        for &(a, b) in &cases {
            for f in 0..8 {
                sim.set("A", a);
                sim.set("B", b);
                sim.set("Fn", f);
                sim.tick();
                let sub = u64::from(f == 1);
                let t = a + if sub == 1 { b ^ 0xFFFF } else { b } + sub;
                let (y, c) = match f {
                    0 | 1 => (t & 0xFFFF, t >> 16 & 1),
                    2 => (a & b, 0),
                    3 => (a | b, 0),
                    4 => (a ^ b, 0),
                    5 => (!a & 0xFFFF, 0),
                    6 => (a << 1 & 0xFFFF, a >> 15),
                    _ => (a >> 1, a & 1),
                };
                let got = [sim.get("Y"), sim.get("Z"), sim.get("N"), sim.get("C")];
                assert_eq!(
                    got,
                    [y, u64::from(y == 0), y >> 15, c],
                    "{alu}: {a:#x} fn {f} {b:#x}"
                );
            }
        }
    }
}

/// Whether the segment `a`-`b` passes through the box at `p` of size `s`
/// (shrunk by 4 on each side, as `render_svg` checks it).
fn segment_hits_box(a: Vec2, b: Vec2, p: Vec2, s: Vec2) -> bool {
    let half = s / 2.0 - Vec2::splat(4.0);
    let (lo, hi) = (p - half, p + half);
    let d = b - a;
    let (mut t0, mut t1) = (0.0f32, 1.0f32);
    for (pv, qv) in [
        (-d.x, a.x - lo.x),
        (d.x, hi.x - a.x),
        (-d.y, a.y - lo.y),
        (d.y, hi.y - a.y),
    ] {
        if pv == 0.0 {
            if qv < 0.0 {
                return false;
            }
            continue;
        }
        let t = qv / pv;
        if pv < 0.0 {
            t0 = t0.max(t);
        } else {
            t1 = t1.min(t);
        }
        if t0 > t1 {
            return false;
        }
    }
    true
}

/// In every circuit of both CPUs, no wire (drawn straight from centre to
/// centre, as the editor draws it) passes through a box it does not
/// connect.
#[test]
fn cpu16_wires_do_not_pass_through_boxes() {
    for id in ["cpu16", "cpu16_rtl"] {
        for (name, doc) in &demos().get(id).unwrap().circuits {
            let index: std::collections::HashMap<_, _> = doc
                .nodes
                .iter()
                .enumerate()
                .map(|(i, n)| (n.id, i))
                .collect();
            let pos = |i: usize| Vec2::new(doc.nodes[i].x, doc.nodes[i].y);
            for w in &doc.wires {
                let (a, b) = (index[&w.from.node], index[&w.to.node]);
                for (k, n) in doc.nodes.iter().enumerate() {
                    assert!(
                        k == a
                            || k == b
                            || !segment_hits_box(pos(a), pos(b), pos(k), n.kind.size()),
                        "{id}/{name}: {:?} -> {:?} crosses {:?}",
                        doc.nodes[a].label,
                        doc.nodes[b].label,
                        n.label
                    );
                }
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Displays
// ─────────────────────────────────────────────────────────────────────────────

/// The video memory of the screen labelled `label`, as the app reads it
/// back (the probe frame's copy), one word per pixel or cell.
fn screen_memory(app: &mut App, label: &str) -> Vec<u64> {
    let e = gate(app, label);
    let probe = app
        .world()
        .get::<crate::sim::GateProbes>(e)
        .unwrap()
        .memory
        .expect("a screen has a memory probe");
    app.world()
        .resource::<SimBridge>()
        .with_frame(|f| f.mems[probe as usize].as_deref().map(<[u64]>::to_vec))
        .expect("the screen memory is read back")
}

/// The pen bounces: x = c[0:5] folded at c[5], y = c[0:4] folded at c[4],
/// colour c[6:10], one step every 16 ticks; every pixel drawn holds the
/// colour of the last lap through it.
#[test]
fn screen_plotter_bounces() {
    let mut app = open("screen_plotter");
    // Just short of 64 steps: lap 0.
    run(&mut app, 16 * 64 - 8);
    let m = screen_memory(&mut app, "Plotter");
    assert_eq!(m.len(), 32 * 16);
    let pen = |c: u64| {
        let x = (c & 31) ^ if c >> 5 & 1 == 1 { 31 } else { 0 };
        let y = (c & 15) ^ if c >> 4 & 1 == 1 { 15 } else { 0 };
        (x as usize, y as usize)
    };
    // Lap 0 draws colour 0 (dark): nothing lit yet.
    assert!(m.iter().all(|&v| v == 0), "lap 0 draws in colour 0");
    // Lap 1 (steps 64 to 127) paints in colour 1.
    run(&mut app, 16 * 64);
    let m = screen_memory(&mut app, "Plotter");
    let lit: Vec<usize> = (0..m.len()).filter(|&i| m[i] != 0).collect();
    assert!(lit.len() >= 32, "{} pixels lit", lit.len());
    for c in 0..63u64 {
        let (x, y) = pen(c);
        assert_eq!(m[y * 32 + x], 1, "lap 1 paints colour 1 at ({x}, {y})");
    }
    assert!(lit.iter().all(|&i| m[i] == 1));
    // Clear blanks it.
    set_input(&mut app, "Clear", 1);
    run(&mut app, 3);
    set_input(&mut app, "Clear", 0);
    run(&mut app, 1);
    let m = screen_memory(&mut app, "Plotter");
    assert!(m.iter().filter(|&&v| v != 0).count() <= 2, "cleared");
}

#[test]
fn screen_xor_fills_the_framebuffer() {
    let mut app = open("screen_xor");
    run(&mut app, 1024 + 4);
    let m = screen_memory(&mut app, "Framebuffer");
    assert_eq!(m.len(), 1024);
    // (The sweep is a few ticks into frame 1: addresses 0 to 3 are newer.)
    for (x, y) in [(3, 5), (31, 0), (17, 30), (31, 31)] {
        assert_eq!(
            m[y * 32 + x],
            ((x ^ y) & 15) as u64,
            "({x}, {y}) in frame 0"
        );
    }
    run(&mut app, 1024);
    let m = screen_memory(&mut app, "Framebuffer");
    assert_eq!(m[5 * 32 + 3], ((3 ^ 5) + 1) & 15, "frame 1 adds one");
}

#[test]
fn screen_bars_paint_a_test_pattern() {
    let mut app = open("screen_bars");
    // One whole frame (1024 ticks) after the first vsync.
    run(&mut app, 2 * 1024 + 4);
    let m = screen_memory(&mut app, "Scanline");
    assert_eq!(m.len(), 32 * 16);
    // Frame 1: bar k (4 pixels wide) has colour k + 1 (mod 8), bright (+8)
    // in lines 8 to 15. Line 0 is the first line after vsync.
    // (Line 0 of frame 2 has begun.)
    for y in [1usize, 7, 8, 15] {
        for x in [0usize, 5, 13, 31] {
            let bar = (x / 4) as u64;
            let want = (bar + 1) % 8 + if y >= 8 { 8 } else { 0 };
            assert_eq!(m[y * 32 + x], want, "({x}, {y})");
        }
    }
}

#[test]
fn screen_text_shows_the_message_and_typing() {
    let mut app = open("screen_text");
    run(&mut app, 130);
    let m = screen_memory(&mut app, "Text");
    assert_eq!(m.len(), 128);
    let row = |r: usize| -> String {
        m[r * 32..r * 32 + 32]
            .iter()
            .map(|&c| (c as u8 & 0x7F) as char)
            .collect()
    };
    assert_eq!(row(2).trim_end(), "  HELLO, GATE QUEST!");
    assert!(row(0).contains("GATE QUEST"));
    assert!(
        m[..32].iter().all(|&c| c & 0x80 != 0),
        "the title row is inverted"
    );
    // Type a '#' into row 1, column 5.
    set_input(&mut app, "Cell", 32 + 5);
    set_input(&mut app, "Char", u64::from(b'#'));
    set_input(&mut app, "Write", 1);
    run(&mut app, 3);
    set_input(&mut app, "Write", 0);
    run(&mut app, 2);
    let m = screen_memory(&mut app, "Text");
    assert_eq!(m[32 + 5], u64::from(b'#'));
}

// ─────────────────────────────────────────────────────────────────────────────
// Word lifting (logi-sim issue #117)
// ─────────────────────────────────────────────────────────────────────────────

/// Run `design` compiled with and without word lifting on the same random
/// inputs for `ticks` ticks and compare, every tick, the outputs, every
/// state element and every authored net (all observable under the editor's
/// options). Halfway, state is carried from each program to the other.
/// Returns the instruction counts (lifted, unlifted).
fn lifting_matches(
    name: &str,
    design: &logi_sim::netlist::Design,
    debug: bool,
    ticks: u64,
) -> (usize, usize) {
    use logi_sim::compile::{Options, compile};
    use logi_sim::sim::Sim;
    let on_opts = if debug {
        Options::debug()
    } else {
        Options::default()
    };
    let off_opts = Options {
        lift: false,
        ..on_opts.clone()
    };
    let p_on = Arc::new(compile(design, &on_opts).unwrap());
    let p_off = Arc::new(compile(design, &off_opts).unwrap());
    let (mut on, mut off) = (Sim::new(Arc::clone(&p_on)), Sim::new(Arc::clone(&p_off)));
    let mut x = 0x9E37_79B9_7F4A_7C15u64 ^ ticks;
    for t in 0..=ticks {
        if t > 0 {
            for (port, p) in p_on.ports.inputs.iter().enumerate() {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                let v = x & logi_sim::netlist::mask(p.width);
                on.set_input(port, v);
                off.set_input(port, v);
            }
            on.tick();
            off.tick();
        }
        if t == ticks / 2 {
            let (a, b) = (on.export_state(), off.export_state());
            assert_eq!(a.words, b.words, "{name}: tick {t}: state");
            on.import_state(&b);
            off.import_state(&a);
        }
        for port in 0..p_on.ports.outputs.len() {
            assert_eq!(
                on.output(port),
                off.output(port),
                "{name}: tick {t}: output {port}"
            );
        }
        assert_eq!(
            on.export_state().words,
            off.export_state().words,
            "{name}: tick {t}: state"
        );
        for inst in 0..p_off.debug.instances.len() {
            let path = p_off.debug.path(inst as u32);
            for (net, _) in &p_off.debug.net_slots[inst] {
                let (got, want) = (on.read_net(&path, net), off.read_net(&path, net));
                if debug {
                    assert_eq!(got, want, "{name}: tick {t}: net {path:?}/{net:?}");
                } else if let (Some(g), Some(w)) = (got, want) {
                    assert_eq!(g, w, "{name}: tick {t}: net {path:?}/{net:?}");
                }
            }
        }
    }
    (p_on.instrs.len(), p_off.instrs.len())
}

/// Lifting changes nothing observable in any demo or gallery item, under
/// the editor's options (every net a probe could show) and the defaults.
#[test]
fn word_lifting_preserves_every_demo_and_gallery_item() {
    let catalog = demos();
    for item in catalog.demos.iter().chain(&catalog.gallery) {
        if item.id == "cpu16" {
            continue; // below, for longer
        }
        let design = crate::sim::netlist::design_of(&item.workspace).unwrap();
        for debug in [true, false] {
            lifting_matches(&item.id, &design, debug, 150);
        }
    }
}

/// The gate-level CPU, lifted and not, for 3000 ticks (a few hundred
/// instructions of its program): identical outputs, state and nets every
/// tick. Lifting must also make it much smaller.
#[test]
fn word_lifting_preserves_the_gate_cpu() {
    let design = crate::sim::netlist::design_of(&demos().get("cpu16").unwrap().workspace).unwrap();
    for debug in [true, false] {
        let (on, off) = lifting_matches("cpu16", &design, debug, 3000);
        assert!(on * 3 < off, "debug {debug}: {off} -> {on} instructions");
    }
}
