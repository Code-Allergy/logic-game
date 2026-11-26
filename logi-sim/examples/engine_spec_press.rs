//! The threaded engine with and without adaptive specialization
//! (`SimOptions::spec`, issue #117) during SMB3 gameplay: `engine_spec`
//! (which runs from power-on with no input, so it measures the title
//! screen) with the controller.
//!
//! The engine runs unlimited from power-on while pad 1 plays the press
//! script, buttons set through `Engine::set_input` as the tick count
//! crosses each frame (polled, so within a few hundred ticks of the frame
//! edge): Start at frame 451 and 657, Right at 1063 (12 frames), A at
//! 1175, Up at 1481 (12), A at 1593, Right at 1899 (200). That is the
//! title, the file menu and the map into World 1-1, walking right.
//!
//! Measuring starts at `--at` (default tick 170,196,510, frame 1905: in
//! 1-1, holding right) and lasts `--secs`. Per run (spec off, then on, a
//! fresh engine each): the time to reach `--at`, k ticks/s per 5 s and the
//! mean, the spec bodies installed at the start and the end, the first new
//! install, and the longest gap between tick-count updates (a stall).
//!
//! ```sh
//! cargo run -p logi-sim --release --features jit --example engine_spec_press -- \
//!     .gatequest/workspaces/nes [--secs 60] [--rounds 1] [--at TICKS]
//! ```
//!
//! The workspace must be a console with SMB3 in its cartridge and root
//! inputs `p1_start`, `p1_right`, `p1_a` and `p1_up`.
mod workspace;

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use logi_sim::compile::Options;
use logi_sim::engine::{Engine, EngineEvent, Rate};
use logi_sim::sim::{SimOptions, SpecOptions};

const FRAME: u64 = 89_342;
const PRESSES: &[(u64, &str, u64)] = &[
    (451, "p1_start", 6),
    (657, "p1_start", 6),
    (1063, "p1_right", 12),
    (1175, "p1_a", 6),
    (1481, "p1_up", 12),
    (1593, "p1_a", 6),
    (1899, "p1_right", 200),
];
const BUTTONS: &[&str] = &["p1_start", "p1_right", "p1_a", "p1_up"];

fn run(design: &logi_sim::netlist::Design, base: &SimOptions, at: u64, secs: u64, spec: bool) {
    let root = &design.defs[design.root];
    let node = |name: &str| {
        root.nodes
            .iter()
            .find(|(id, _)| root.name_of(*id) == Some(name))
            .map(|(id, _)| id)
            .unwrap_or_else(|| panic!("no root input {name}"))
    };
    let buttons: Vec<_> = BUTTONS.iter().map(|b| (*b, node(b))).collect();
    let sim_options = SimOptions {
        spec: spec.then(SpecOptions::default),
        ..base.clone()
    };
    let e = Engine::spawn_with(Options::default(), sim_options);
    e.load(Arc::new(design.clone()), 1);
    match e.wait_event(Duration::from_secs(600)) {
        Some(EngineEvent::Loaded { .. }) => {}
        other => panic!("load: {other:?}"),
    }
    let mut held = vec![false; buttons.len()];
    let apply = |n: u64, held: &mut Vec<bool>| {
        let f = n / FRAME + 1;
        for (k, (b, id)) in buttons.iter().enumerate() {
            let on = PRESSES
                .iter()
                .any(|(from, i, c)| i == b && (*from..from + c).contains(&f));
            if on != held[k] {
                e.set_input(*id, u64::from(on));
                held[k] = on;
            }
        }
    };
    e.set_rate(Rate::Unlimited);
    let t0 = Instant::now();
    // To the measuring point, pressing.
    loop {
        let n = e.tick_count();
        if n >= at {
            break;
        }
        apply(n, &mut held);
        std::thread::sleep(Duration::from_micros(200));
    }
    let reach = t0.elapsed();
    let installed_at_start = e.spec_installed();
    let t1 = Instant::now();
    let n1 = e.tick_count();
    let (mut last_ticks, mut last_change, mut worst_gap) = (n1, t1, Duration::ZERO);
    let (mut mark_t, mut mark_n) = (t1, n1);
    let mut rates = Vec::new();
    let mut new_install = None;
    while t1.elapsed() < Duration::from_secs(secs) {
        std::thread::sleep(Duration::from_millis(1));
        let now = Instant::now();
        let n = e.tick_count();
        apply(n, &mut held);
        if n != last_ticks {
            worst_gap = worst_gap.max(now - last_change);
            last_ticks = n;
            last_change = now;
        }
        if new_install.is_none() && e.spec_installed() > installed_at_start {
            new_install = Some((now - t1, n));
        }
        if now - mark_t >= Duration::from_secs(5) {
            rates.push((n - mark_n) as f64 / (now - mark_t).as_secs_f64() / 1000.0);
            (mark_t, mark_n) = (now, n);
        }
    }
    let n2 = e.tick_count();
    let r: Vec<String> = rates.iter().map(|r| format!("{r:.0}")).collect();
    println!(
        "spec {:3}: reached tick {at} (frame {}) after {:.1} s; then {secs} s from frame {} to {}: k ticks/s per 5 s [{}], mean {:.0}; installed {} -> {}; first new install {}; worst gap {:.1} ms",
        if spec { "on" } else { "off" },
        at / FRAME,
        reach.as_secs_f64(),
        n1 / FRAME,
        n2 / FRAME,
        r.join(" "),
        (n2 - n1) as f64 / t1.elapsed().as_secs_f64() / 1000.0,
        installed_at_start,
        e.spec_installed(),
        new_install.map_or("none".into(), |(t, n)| format!(
            "{:.1} s in (frame {})",
            t.as_secs_f64(),
            n / FRAME
        )),
        worst_gap.as_secs_f64() * 1e3
    );
}

fn main() -> Result<(), String> {
    let mut folder = None;
    let (mut secs, mut rounds, mut at) = (60, 1, 1905 * FRAME);
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--secs" => secs = it.next().and_then(|v| v.parse().ok()).ok_or("--secs N")?,
            "--rounds" => rounds = it.next().and_then(|v| v.parse().ok()).ok_or("--rounds N")?,
            "--at" => at = it.next().and_then(|v| v.parse().ok()).ok_or("--at TICKS")?,
            f => folder = Some(f.to_string()),
        }
    }
    let folder = folder.ok_or("usage: engine_spec_press <workspace> [--secs N] [--at T]")?;
    let w = workspace::load(Path::new(&folder))?;
    let base = SimOptions {
        jit: cfg!(feature = "jit"),
        ..SimOptions::default()
    };
    for _ in 0..rounds {
        for spec in [false, true] {
            run(&w.design, &base, at, secs, spec);
        }
    }
    Ok(())
}
