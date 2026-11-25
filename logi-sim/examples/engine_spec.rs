//! The threaded engine with and without adaptive specialization
//! (`SimOptions::spec`, issue #117) on a workspace: ticks per second over
//! a long unlimited run, the time of the first bodies, and the longest gap
//! between two tick-count updates (a stall of the worker or of the
//! frames).
//!
//! ```sh
//! cargo run -p logi-sim --release --features jit --example engine_spec -- \
//!     nes/workspace [--secs 40] [--debug] [--no-jit] [--rounds 2]
//! ```
//!
//! Each round runs spec off, then spec on, on a fresh engine; the numbers
//! are per 5 s interval after the first 5 s.

mod workspace;

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use logi_sim::compile::Options;
use logi_sim::engine::{Engine, EngineEvent, Rate};
use logi_sim::sim::{SimOptions, SpecOptions};

fn run(design: &logi_sim::netlist::Design, base: &SimOptions, debug: bool, secs: u64, spec: bool) {
    let options = if debug {
        Options::debug()
    } else {
        Options::default()
    };
    let sim_options = SimOptions {
        spec: spec.then(SpecOptions::default),
        ..base.clone()
    };
    let e = Engine::spawn_with(options, sim_options);
    e.load(Arc::new(design.clone()), 1);
    match e.wait_event(Duration::from_secs(600)) {
        Some(EngineEvent::Loaded { .. }) => {}
        other => panic!("load: {other:?}"),
    }
    e.set_rate(Rate::Unlimited);
    let t0 = Instant::now();
    let (mut last_ticks, mut last_change, mut worst_gap) = (0, t0, Duration::ZERO);
    let (mut first, mut third) = (None, None);
    let (mut mark_t, mut mark_n) = (t0, 0u64);
    let mut rates = Vec::new();
    while t0.elapsed() < Duration::from_secs(secs) {
        std::thread::sleep(Duration::from_millis(1));
        let now = Instant::now();
        let n = e.tick_count();
        if n != last_ticks {
            // Only gaps of a running engine count.
            if last_ticks != 0 {
                worst_gap = worst_gap.max(now - last_change);
            }
            last_ticks = n;
            last_change = now;
        }
        let s = e.spec_installed();
        if s >= 1 && first.is_none() {
            first = Some((now - t0, n));
        }
        if s >= 3 && third.is_none() {
            third = Some((now - t0, n));
        }
        if now - mark_t >= Duration::from_secs(5) {
            rates.push((n - mark_n) as f64 / (now - mark_t).as_secs_f64() / 1000.0);
            (mark_t, mark_n) = (now, n);
        }
    }
    let fmt = |x: Option<(Duration, u64)>| {
        x.map_or("never".to_string(), |(t, n)| {
            format!("{:.1} s ({:.1}M ticks)", t.as_secs_f64(), n as f64 / 1e6)
        })
    };
    let r: Vec<String> = rates.iter().map(|r| format!("{r:.0}")).collect();
    println!(
        "spec {:3}: k ticks/s per 5 s: [{}]; installed {}; first {}; third {}; worst gap {:.1} ms",
        if spec { "on" } else { "off" },
        r.join(" "),
        e.spec_installed(),
        fmt(first),
        fmt(third),
        worst_gap.as_secs_f64() * 1e3
    );
}

fn main() -> Result<(), String> {
    let mut folder = None;
    let (mut secs, mut rounds, mut debug, mut jit) = (40, 1, false, true);
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--secs" => secs = it.next().and_then(|v| v.parse().ok()).ok_or("--secs N")?,
            "--rounds" => rounds = it.next().and_then(|v| v.parse().ok()).ok_or("--rounds N")?,
            "--debug" => debug = true,
            "--no-jit" => jit = false,
            f => folder = Some(f.to_string()),
        }
    }
    let folder = folder.ok_or(
        "usage: engine_spec <workspace folder> [--secs N] [--rounds N] [--debug] [--no-jit]",
    )?;
    let w = workspace::load(Path::new(&folder))?;
    let base = SimOptions {
        jit: jit && cfg!(feature = "jit"),
        ..SimOptions::default()
    };
    println!(
        "{folder}: jit {}, {} compile options, {secs} s per run",
        base.jit,
        if debug { "debug" } else { "default" }
    );
    for _ in 0..rounds {
        for spec in [false, true] {
            run(&w.design, &base, debug, secs, spec);
        }
    }
    Ok(())
}
