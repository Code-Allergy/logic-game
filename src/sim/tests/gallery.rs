//! The gallery (`assets/gallery/`): its files are embedded and canonical,
//! it only builds on the curriculum and on items listed before it, every
//! item compiles and is laid out without overlapping boxes, and each one
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

/// Between 20 and 30 items in three groups, simple to complex; each uses
/// only curriculum demos and items listed before it, and has a title and
/// a description.
#[test]
fn gallery_builds_on_the_curriculum_and_earlier_items() {
    let catalog = demos();
    assert!(
        (20..=30).contains(&catalog.gallery.len()),
        "{} items",
        catalog.gallery.len()
    );
    let categories: Vec<&str> = catalog.categories.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(categories, ["combinational", "sequential", "systems"]);
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

/// Every circuit of every item (generated adapters included) is laid out
/// on the 20-unit grid with no two node boxes overlapping.
#[test]
fn gallery_layouts_have_no_overlapping_boxes() {
    for item in &demos().gallery {
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
