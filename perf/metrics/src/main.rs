//! Deterministic performance metrics, for the regression gate
//! (`cargo xtask perf check`, baseline in `perf/baseline.json`).
//!
//! ```sh
//! cargo run -p perf-metrics                 # every metric, JSON on stdout
//! cargo run -p perf-metrics -- ticks cpu16 100000   # just run ticks (perf stat, profiling)
//! ```
//!
//! Every metric is a count that does not depend on the machine, the load
//! or the build profile: the same source gives the same numbers anywhere,
//! so they can be gated tightly where wall-clock times cannot.
//!
//! - `nodes.<case>`: primitive nodes after flattening.
//! - `program.<case>.<options>.<field>`: compiled program size
//!   (instructions, slots, packed words and lanes, commits, blocks).
//! - `tick.<case>.<options>.ops`: operations executed per tick
//!   (instructions + packed words + commits; logi-sim feature `counters`).
//! - `tick.<case>.<options>.allocs`: heap allocations in 1000 ticks
//!   (counted by this binary's global allocator).
//! - `engine.<case>.allocs`: heap allocations in 100 engine frames (100
//!   ticks and a probe frame of every root net each).
//!
//! `<options>` is `default` (the compiler's defaults) or `editor`
//! (`Options::debug`, what the game runs).

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use logi_sim::compile::{Options, Program, compile};
use logi_sim::engine::{EngineCore, Probe, ProbeFrame};
use logi_sim::netlist::Design;
use logi_sim::sim::{Sim, SimOptions};
use logi_sim::testkit::{GenConfig, fixtures, generate};
use serde_json::{Map, Value, json};

// ── Counting allocator ───────────────────────────────────────────────────

/// Counts allocations (and reallocations) of the whole process.
struct Counting;

static ALLOCS: AtomicU64 = AtomicU64::new(0);

// SAFETY: forwards every call to `System` unchanged; only counts.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.realloc(ptr, layout, new_size) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

/// Allocations made by `f`.
fn allocations(f: impl FnOnce()) -> u64 {
    let before = ALLOCS.load(Ordering::SeqCst);
    f();
    ALLOCS.load(Ordering::SeqCst) - before
}

// ── Cases ────────────────────────────────────────────────────────────────

/// The fixed designs measured. Changing this list changes the metric
/// names: bless the baseline afterwards.
fn cases() -> Vec<(&'static str, Design)> {
    CASES.iter().map(|&c| (c, case(c))).collect()
}

/// The gated cases (see [`case`]).
const CASES: [&str; 5] = [
    "cpu16",
    "cpu16_rtl",
    "testkit_cpu",
    "dag32_10k",
    "gates1_100k",
];

/// The design of case `name`: a gated case, or one of the larger
/// profiling-only designs `gates1_1m` and `dag32_1m` (for `ticks`).
fn case(name: &str) -> Design {
    match name {
        "cpu16" | "cpu16_rtl" => perf::design(name),
        "testkit_cpu" => fixtures::cpu(&fixtures::LOOP_PROGRAM),
        "dag32_10k" => generate(&GenConfig::dag(1, 10_000, 32)).design,
        "gates1_100k" => generate(&GenConfig::gates(2, 100_000, 1)).design,
        "gates1_1m" => generate(&GenConfig::gates(2, 1_000_000, 1)).design,
        "dag32_1m" => generate(&GenConfig::dag(1, 1_000_000, 32)).design,
        other => panic!("no case {other:?}"),
    }
}

/// The options each case is compiled with: the gallery CPUs both ways,
/// the rest with the defaults only.
fn options_for(case: &str) -> Vec<(&'static str, Options)> {
    let mut v = vec![("default", Options::default())];
    if perf::CPUS.contains(&case) {
        v.push(("editor", Options::debug()));
    }
    v
}

const TICKS: u64 = 1000;
const FRAMES: u64 = 100;

struct Metrics(Map<String, Value>);

impl Metrics {
    fn put(&mut self, name: String, kind: &str, value: u64) {
        self.0.insert(name, json!({ "kind": kind, "value": value }));
    }
}

fn program_metrics(m: &mut Metrics, prefix: &str, p: &Program) {
    let fields = [
        ("instrs", p.instrs.len()),
        ("slots", p.slot_count as usize),
        ("packed_words", p.packs.len()),
        ("packed_lanes", p.packed_lanes()),
        ("commits", p.commits.len()),
        ("blocks", p.blocks.len()),
    ];
    for (field, v) in fields {
        m.put(format!("program.{prefix}.{field}"), "size", v as u64);
    }
}

/// One tick with root inputs changing (an LFSR), as the benchmarks do.
fn tick(sim: &mut Sim, lfsr: &mut u64) {
    for port in 0..sim.program().ports.inputs.len() {
        *lfsr ^= *lfsr << 13;
        *lfsr ^= *lfsr >> 7;
        *lfsr ^= *lfsr << 17;
        sim.set_input(port, *lfsr);
    }
    sim.tick();
}

fn tick_metrics(m: &mut Metrics, prefix: &str, p: &Arc<Program>) {
    let mut sim = Sim::new(Arc::clone(p));
    let mut lfsr = 0x9E37_79B9_7F4A_7C15u64;
    for _ in 0..10 {
        tick(&mut sim, &mut lfsr); // warm up
    }
    sim.reset_counters();
    let allocs = allocations(|| {
        for _ in 0..TICKS {
            tick(&mut sim, &mut lfsr);
        }
    });
    let c = *sim.counters();
    assert_eq!(c.ticks, TICKS);
    m.put(format!("tick.{prefix}.ops"), "ops", c.ops() / TICKS);
    m.put(format!("tick.{prefix}.allocs"), "allocs", allocs);
}

fn engine_metrics(m: &mut Metrics, case: &str, d: &Design) {
    let mut core = EngineCore::new(Options::debug());
    core.load(d, 1);
    core.set_probes(
        d.root_def()
            .nets
            .keys()
            .map(|net| Probe { path: vec![], net })
            .collect(),
    );
    // A fake clock that never advances: every `run` finishes its steps.
    let clock = || Duration::ZERO;
    let mut frame = ProbeFrame::default();
    let mut frame_of = |core: &mut EngineCore| {
        core.step(100);
        core.run(Duration::from_secs(1), &clock);
        core.publish(&mut frame);
    };
    for _ in 0..3 {
        frame_of(&mut core); // warm up: the frame's buffers grow once
    }
    let allocs = allocations(|| {
        for _ in 0..FRAMES {
            frame_of(&mut core);
        }
    });
    m.put(format!("engine.{case}.allocs"), "allocs", allocs);
}

fn all_metrics() -> Value {
    let mut m = Metrics(Map::new());
    for (case, d) in cases() {
        m.put(format!("nodes.{case}"), "nodes", d.flat_primitives(d.root));
        for (name, opts) in options_for(case) {
            let p = Arc::new(compile(&d, &opts).unwrap_or_else(|e| panic!("{case}: {e:?}")));
            let prefix = format!("{case}.{name}");
            program_metrics(&mut m, &prefix, &p);
            tick_metrics(&mut m, &prefix, &p);
        }
        engine_metrics(&mut m, case, &d);
    }
    json!({ "schema": 1, "metrics": Value::Object(m.0) })
}

/// Run `n` ticks of `case` and nothing else: the target for `perf stat`
/// (two runs of different lengths cancel start-up out) and for profiling
/// the tick loop (`cargo xtask perf flamegraph bin:perf-metrics -- ticks
/// cpu16 2000000`). `options` is `default`, `editor` or `raw` (simplify
/// off, as the throughput benchmarks); `threads` lays the
/// program out for and runs it on that many threads.
fn run_ticks(name: &str, n: u64, options: &str, threads: usize) {
    let d = case(name);
    let opts = named_options(options);
    let p = Arc::new(compile(&d, &opts.for_threads(threads)).unwrap());
    let mut sim = Sim::with_options(p, SimOptions::threaded(threads));
    let mut lfsr = 0x9E37_79B9_7F4A_7C15u64;
    for _ in 0..n {
        tick(&mut sim, &mut lfsr);
    }
    std::hint::black_box(sim.output(0));
}

/// Compiler options by name (`ticks`, `breakdown`).
fn named_options(options: &str) -> Options {
    match options {
        "default" => Options::default(),
        "editor" => Options::debug(),
        // Word lifting (issue #117) off, for comparison.
        "default-nolift" => Options {
            lift: false,
            ..Options::default()
        },
        "editor-nolift" => Options {
            lift: false,
            ..Options::debug()
        },
        // As the throughput benchmarks: simplify off (every node is an
        // instruction), default layout and packing.
        "raw" => Options {
            slot_order: Default::default(),
            pack: Default::default(),
            ..Options::unoptimized()
        },
        other => panic!("options {other:?}: default, editor, raw, default-nolift or editor-nolift"),
    }
}

/// What the compiled program of `case` executes, by op and by the
/// definition each instruction's result belongs to (the definition of an
/// authored net in its slot; `(temp)` for compiler temporaries): the data
/// for planning the next lifting stages (issue #117).
fn breakdown(name: &str, options: &str) {
    use std::collections::{BTreeMap, HashMap};
    let d = case(name);
    let p = compile(&d, &named_options(options)).unwrap();
    // Compile time per pass (median of 11 compiles).
    let opts = named_options(options);
    let mut runs: Vec<Vec<(logi_sim::compile::Pass, f64)>> = Vec::new();
    for _ in 0..11 {
        let mut t = std::time::Instant::now();
        let mut v = Vec::new();
        logi_sim::compile::compile_observed(&d, &opts, |pass| {
            v.push((pass, t.elapsed().as_secs_f64() * 1e3));
            t = std::time::Instant::now();
        })
        .unwrap();
        runs.push(v);
    }
    let mut line = String::from("compile ms:");
    let mut total = Vec::new();
    for i in 0..runs[0].len() {
        let mut xs: Vec<f64> = runs.iter().map(|r| r[i].1).collect();
        xs.sort_by(f64::total_cmp);
        line += &format!(" {} {:.2}", runs[0][i].0.name(), xs[xs.len() / 2]);
    }
    for r in &runs {
        total.push(r.iter().map(|x| x.1).sum::<f64>());
    }
    total.sort_by(f64::total_cmp);
    println!("{line}; total {:.2}", total[total.len() / 2]);
    let mut def_of_slot: HashMap<u32, &str> = HashMap::new();
    for (inst, map) in p.debug.net_slots.iter().enumerate() {
        let def = &d.defs[p.debug.instances[inst].def];
        for (_, &s) in map {
            // A view (a lifted net) counts for the word holding it.
            let s = p.resolve(s).map_or(s, |(real, _, _)| real);
            def_of_slot.entry(s).or_insert(def.name.as_str());
        }
    }
    let mut by_op: BTreeMap<String, usize> = BTreeMap::new();
    let mut by_def: BTreeMap<&str, usize> = BTreeMap::new();
    let mut by_def_op: BTreeMap<(&str, String), usize> = BTreeMap::new();
    for b in &p.blocks {
        let n = b.len as usize;
        let op = if b.packed {
            format!("{:?} (packed)", b.op)
        } else {
            format!("{:?}", b.op)
        };
        *by_op.entry(op.clone()).or_default() += n;
        if b.packed {
            continue;
        }
        for ins in &p.instrs[b.start as usize..(b.start + b.len) as usize] {
            let def = def_of_slot.get(&ins.dst).copied().unwrap_or("(temp)");
            *by_def.entry(def).or_default() += 1;
            *by_def_op.entry((def, op.clone())).or_default() += 1;
        }
    }
    println!(
        "{name} {options}: {} instructions, {} packed words, {} commits, {} slots, {} views",
        p.instrs.len(),
        p.packs.len(),
        p.commits.len(),
        p.slot_count,
        p.views.len()
    );
    let sorted = |m: BTreeMap<String, usize>| {
        let mut v: Vec<_> = m.into_iter().collect();
        v.sort_by(|a, b| b.1.cmp(&a.1));
        v
    };
    println!("\nby op:");
    for (op, n) in sorted(by_op) {
        println!("  {n:6}  {op}");
    }
    println!("\nby definition:");
    let mut defs: Vec<_> = by_def.into_iter().collect();
    defs.sort_by(|a, b| b.1.cmp(&a.1));
    for (def, n) in defs {
        let ops: Vec<String> = {
            let mut v: Vec<_> = by_def_op
                .iter()
                .filter(|((d, _), _)| *d == def)
                .map(|((_, op), n)| (op.clone(), *n))
                .collect();
            v.sort_by(|a, b| b.1.cmp(&a.1));
            v.into_iter().map(|(op, n)| format!("{op} {n}")).collect()
        };
        println!("  {n:6}  {def}: {}", ops.join(", "));
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        None => println!("{}", serde_json::to_string_pretty(&all_metrics()).unwrap()),
        Some("cases") => {
            for case in CASES {
                println!("{case}");
            }
        }
        Some("breakdown") if (2..=3).contains(&args.len()) => {
            breakdown(&args[1], args.get(2).map_or("default", String::as_str));
        }
        Some("ticks") if (3..=5).contains(&args.len()) => {
            run_ticks(
                &args[1],
                args[2].parse().expect("a tick count"),
                args.get(3).map_or("default", String::as_str),
                args.get(4)
                    .map_or(1, |t| t.parse().expect("a thread count")),
            );
        }
        _ => {
            eprintln!(
                "usage: perf-metrics [cases | ticks <case> <n> [OPTIONS] [threads] | breakdown <case> [OPTIONS]]\n\
                 OPTIONS: default, editor, raw, default-nolift, editor-nolift"
            );
            std::process::exit(2);
        }
    }
}
