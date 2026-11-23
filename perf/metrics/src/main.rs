//! Deterministic performance metrics, for the regression gate
//! (`cargo xtask perf check`, baseline in `perf/baseline.json`).
//!
//! ```sh
//! cargo run -p perf-metrics                 # every metric, JSON on stdout
//! cargo run -p perf-metrics -- ticks cpu16 100000   # just run ticks (perf stat, profiling)
//! cargo run -p perf-metrics --release --features jit -- jit cpu16   # interpreter vs JIT
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
//!   With clock-phase variants (issue #117) every tick runs its phase's
//!   variant, so this is the average over whole clock periods.
//! - `tick.<case>.<options>.run_ops`: the same for one `Sim::run` of 1000
//!   ticks, where every tick but the last settles only what its commits
//!   read (tick fusion).
//! - `tick.<case>.<options>.allocs`: heap allocations in 1000 ticks
//!   (counted by this binary's global allocator).
//! - `program.<case>.<options>.phase_ops`: with clock-phase variants, the
//!   average instructions plus commits per tick over one period (the
//!   unspecialized `instrs + commits` without).
//! - `cpu.<case>.ticks_per_instr_x100`: for the gallery CPUs, ticks per
//!   retired CPU instruction (a change of `PC`) over a run of the program
//!   from reset to `Halted`, times 100. Wall-clock µs per tick and per
//!   instruction: `perf-metrics cpi <case>`.
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
        (
            "phase_ops",
            p.phases
                .as_ref()
                .map_or(p.instrs.len() + p.commits.len(), |ph| {
                    ph.ops_per_tick().round() as usize
                }),
        ),
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
    // One batch of the same length: fused.
    sim.reset_counters();
    sim.run(TICKS);
    let c = *sim.counters();
    assert_eq!(c.ticks, TICKS);
    m.put(format!("tick.{prefix}.run_ops"), "ops", c.ops() / TICKS);
}

/// A gallery CPU's program from reset to `Halted` (at most `max` ticks),
/// tick by tick, inputs at 0: `(ticks, retired instructions)`, where an
/// instruction retires when `PC` changes.
fn cpu_run(p: &Arc<Program>, max: u64) -> (u64, u64) {
    let mut sim = Sim::new(Arc::clone(p));
    let mut pc = sim.get("PC");
    let mut retired = 0;
    let mut ticks = 0;
    while ticks < max && sim.get("Halted") == 0 {
        sim.tick();
        ticks += 1;
        let now = sim.get("PC");
        retired += (now != pc) as u64;
        pc = now;
    }
    (ticks, retired)
}

/// Ticks per retired instruction of a gallery CPU, times 100.
fn cpu_metrics(m: &mut Metrics, case: &str, p: &Arc<Program>) {
    let (ticks, retired) = cpu_run(p, 2_000_000);
    m.put(
        format!("cpu.{case}.ticks_per_instr_x100"),
        "ticks",
        100 * ticks / retired.max(1),
    );
}

/// Wall-clock µs per tick and per retired CPU instruction of a gallery
/// CPU running its program from reset to `Halted`, again and again for
/// about `secs` seconds: tick by tick (`Sim::tick`, what single steps and
/// probes see) and in one `Sim::run` per program run (what the engine
/// does at speed).
fn cpi(name: &str, options: &str, secs: f64) {
    let d = case(name);
    let p = Arc::new(compile(&d, &named_options(options)).unwrap());
    let (ticks, retired) = cpu_run(&p, 10_000_000);
    println!(
        "{name} {options}: {ticks} ticks, {retired} instructions per program run ({:.2} ticks/instruction)",
        ticks as f64 / retired.max(1) as f64
    );
    if let Some(ph) = &p.phases {
        println!(
            "  phases: period {}, {:.1} ops/tick (unspecialized {})",
            ph.period,
            ph.ops_per_tick(),
            p.instrs.len() + p.commits.len()
        );
        for (k, v) in ph.variants.iter().enumerate() {
            let pruned =
                logi_sim::compile::phases::prune(&p, v).map_or(v.instrs.len(), |b| b.instrs.len());
            println!(
                "  variant {k}: {} instructions ({pruned} when fused), {} commits",
                v.instrs.len(),
                v.commits.len()
            );
        }
    }
    for mode in ["tick", "run"] {
        let mut best = f64::MAX;
        for _ in 0..3 {
            let mut sim = Sim::new(Arc::clone(&p));
            let start = std::time::Instant::now();
            let mut runs = 0u64;
            while start.elapsed().as_secs_f64() < secs / 6.0 {
                sim.reset();
                if mode == "tick" {
                    for _ in 0..ticks {
                        sim.tick();
                    }
                } else {
                    sim.run(ticks);
                }
                runs += 1;
            }
            let us = start.elapsed().as_secs_f64() * 1e6;
            best = best.min(us / runs as f64);
            std::hint::black_box(sim.output(0));
        }
        println!(
            "  {mode:4}: {:.3} µs/tick, {:.3} µs/instruction",
            best / ticks as f64,
            best / retired.max(1) as f64
        );
    }
}

/// Interpreter against JIT (`SimOptions::jit`, logi-sim feature `jit`):
/// µs per tick tick by tick (inputs changing every tick, as `ticks`) and
/// in `run(1000)` batches, best of 3 over about `secs` seconds; for the
/// gallery CPUs also µs per retired CPU instruction over a program run;
/// and the JIT's compile time for this program.
fn jit_compare(name: &str, options: &str, secs: f64) {
    let d = case(name);
    let t0 = std::time::Instant::now();
    let p = Arc::new(compile(&d, &named_options(options)).unwrap());
    let compile_ms = t0.elapsed().as_secs_f64() * 1e3;
    let interp = SimOptions::default();
    let jit = SimOptions {
        jit: true,
        ..SimOptions::default()
    };
    let probe = Sim::with_options(Arc::clone(&p), jit.clone());
    if !probe.jit_active() {
        println!("{name} {options}: the JIT is not available (build with --features jit)");
        return;
    }
    let mut best_jit = f64::MAX;
    for _ in 0..5 {
        let t = std::time::Instant::now();
        let code = probe.compile_jit().expect("JIT");
        best_jit = best_jit.min(t.elapsed().as_secs_f64() * 1e3);
        std::hint::black_box(code.instrs);
    }
    println!(
        "{name} {options}: {} instrs, {} commits; program compile {compile_ms:.2} ms, JIT compile {best_jit:.2} ms (every body)",
        p.instrs.len(),
        p.commits.len()
    );
    let cpu = perf::CPUS.contains(&name);
    let (ticks, retired) = if cpu { cpu_run(&p, 10_000_000) } else { (0, 0) };
    for (label, o) in [("interp", &interp), ("jit", &jit)] {
        let mut row = format!("  {label:6}");
        for mode in ["tick", "run"] {
            let mut best = f64::MAX;
            for _ in 0..3 {
                let mut sim = Sim::with_options(Arc::clone(&p), o.clone());
                let mut lfsr = 0x9E37_79B9_7F4A_7C15u64;
                let start = std::time::Instant::now();
                let mut n = 0u64;
                while start.elapsed().as_secs_f64() < secs / 12.0 {
                    if mode == "tick" {
                        for _ in 0..1000 {
                            tick(&mut sim, &mut lfsr);
                        }
                    } else {
                        sim.run(1000);
                    }
                    n += 1000;
                }
                best = best.min(start.elapsed().as_secs_f64() * 1e6 / n as f64);
                std::hint::black_box(sim.output(0));
            }
            row += &format!("  {mode}: {best:.4} µs/tick");
        }
        if cpu {
            for mode in ["tick", "run"] {
                let mut best = f64::MAX;
                for _ in 0..3 {
                    let mut sim = Sim::with_options(Arc::clone(&p), o.clone());
                    let start = std::time::Instant::now();
                    let mut runs = 0u64;
                    while start.elapsed().as_secs_f64() < secs / 12.0 {
                        sim.reset();
                        if mode == "tick" {
                            for _ in 0..ticks {
                                sim.tick();
                            }
                        } else {
                            sim.run(ticks);
                        }
                        runs += 1;
                    }
                    best = best.min(start.elapsed().as_secs_f64() * 1e6 / runs as f64);
                    std::hint::black_box(sim.output(0));
                }
                row += &format!(
                    "  cpu {mode}: {:.4} µs/tick {:.4} µs/instr",
                    best / ticks as f64,
                    best / retired.max(1) as f64
                );
            }
        }
        println!("{row}");
    }
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
            if name == "default" && perf::CPUS.contains(&case) {
                cpu_metrics(&mut m, case, &p);
            }
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
        // Function matching (issue #117, stage 2) off, for comparison.
        "default-nomatch" => Options {
            func_match: false,
            ..Options::default()
        },
        "editor-nomatch" => Options {
            func_match: false,
            ..Options::debug()
        },
        // Clock-phase specialization (issue #117, stage 4) off.
        "default-nophases" => Options {
            clock_phases: false,
            ..Options::default()
        },
        "editor-nophases" => Options {
            clock_phases: false,
            ..Options::debug()
        },
        // Word-level cleanup (issue #117, stage 3) off, for comparison.
        "default-nowords" => Options {
            words: false,
            ..Options::default()
        },
        "editor-nowords" => Options {
            words: false,
            ..Options::debug()
        },
        // As the throughput benchmarks: simplify off (every node is an
        // instruction), default layout and packing.
        "raw" => Options {
            slot_order: Default::default(),
            pack: Default::default(),
            ..Options::unoptimized()
        },
        other => panic!(
            "options {other:?}: default, editor, raw, default-nolift, editor-nolift, default-nomatch, editor-nomatch, default-nowords, editor-nowords, default-nophases or editor-nophases"
        ),
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
    // Recompile after a one-gate edit of the root (what the editor does on
    // every edit), with the function-matching cache of the previous
    // compile: median of 11.
    {
        use logi_sim::compile::{compile_cached, func::MatchCache};
        use logi_sim::netlist::NodeKind;
        let mut cache = MatchCache::new();
        compile_cached(&d, &opts, &mut cache, |_| {}).unwrap();
        let mut edited = d.clone();
        let root = edited.root;
        let net = edited.defs[root]
            .nets
            .iter()
            .find(|(_, n)| n.width == 1)
            .map(|(id, _)| id)
            .expect("a 1-bit root net");
        let not = edited.add_node(root, NodeKind::Not { w: 1 }).unwrap();
        edited.connect(root, net, (not, 0)).unwrap();
        let mut runs: Vec<Vec<(logi_sim::compile::Pass, f64)>> = Vec::new();
        for _ in 0..11 {
            let mut t = std::time::Instant::now();
            let mut v = Vec::new();
            compile_cached(&edited, &opts, &mut cache, |pass| {
                v.push((pass, t.elapsed().as_secs_f64() * 1e3));
                t = std::time::Instant::now();
            })
            .unwrap();
            runs.push(v);
        }
        let mut line = String::from("recompile after a one-gate root edit, ms:");
        for i in 0..runs[0].len() {
            let mut xs: Vec<f64> = runs.iter().map(|r| r[i].1).collect();
            xs.sort_by(f64::total_cmp);
            line += &format!(" {} {:.2}", runs[0][i].0.name(), xs[xs.len() / 2]);
        }
        let mut total: Vec<f64> = runs.iter().map(|r| r.iter().map(|x| x.1).sum()).collect();
        total.sort_by(f64::total_cmp);
        println!("{line}; total {:.2}", total[total.len() / 2]);
        println!(
            "match cache: {} definitions analysed, {} reused",
            cache.analysed, cache.reused
        );
        if opts.func_match {
            for (name, matched, note) in logi_sim::compile::func::report(&d, &mut cache) {
                println!(
                    "  {} {name}: {note}",
                    if matched { "matched  " } else { "unmatched" }
                );
            }
        }
    }
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
        v.sort_by_key(|a| std::cmp::Reverse(a.1));
        v
    };
    println!("\nby op:");
    for (op, n) in sorted(by_op) {
        println!("  {n:6}  {op}");
    }
    println!("\nby definition:");
    let mut defs: Vec<_> = by_def.into_iter().collect();
    defs.sort_by_key(|a| std::cmp::Reverse(a.1));
    for (def, n) in defs {
        let ops: Vec<String> = {
            let mut v: Vec<_> = by_def_op
                .iter()
                .filter(|((d, _), _)| *d == def)
                .map(|((_, op), n)| (op.clone(), *n))
                .collect();
            v.sort_by_key(|a| std::cmp::Reverse(a.1));
            v.into_iter().map(|(op, n)| format!("{op} {n}")).collect()
        };
        println!("  {n:6}  {def}: {}", ops.join(", "));
    }
}

/// Print the compiled program of `case` block by block: each instruction
/// with its operands, each slot named by an authored net that lives in it
/// (`def/net`) or by its number and width. For reading what the
/// optimizer left (issue #117).
fn dump(name: &str, options: &str) {
    use std::collections::HashMap;
    let d = case(name);
    let p = compile(&d, &named_options(options)).unwrap();
    let mut names: HashMap<u32, String> = HashMap::new();
    for (inst, map) in p.debug.net_slots.iter().enumerate() {
        let def = &d.defs[p.debug.instances[inst].def];
        for (net, &s) in map {
            if s < p.slot_count {
                names.entry(s).or_insert_with(|| {
                    let (node, pin) = def.nets[net].driver;
                    let n = def
                        .name_of(node)
                        .map_or_else(|| format!("{:?}", def.nodes[node].kind), str::to_string);
                    format!("{}/{}.{pin}#{inst}", def.name, n)
                });
            }
        }
    }
    let consts: HashMap<u32, u64> = p.consts.iter().copied().collect();
    let name_of = |s: u32| -> String {
        if let Some(v) = consts.get(&s) {
            return format!("#{v:#x}");
        }
        match names.get(&s) {
            Some(n) => format!("s{s}({n})"),
            None => format!("s{s}"),
        }
    };
    for b in &p.blocks {
        if b.packed {
            println!("rank {} {:?} packed x{}", b.rank, b.op, b.len);
            continue;
        }
        for k in b.start..b.start + b.len {
            let ins = p.instrs[k as usize];
            let (sa, sb, sc) = b.op.slot_operands();
            let o = |is: bool, s: u32| if is { name_of(s) } else { format!("{s}") };
            println!(
                "r{} {:?} {} <- {} {} {} imm={:#x}",
                b.rank,
                b.op,
                name_of(ins.dst),
                o(sa, ins.a),
                o(sb, ins.b),
                o(sc, ins.c),
                p.imms[k as usize]
            );
        }
    }
    for k in &p.state_keys {
        if let logi_sim::compile::StateLoc::Slot(s) = k.loc {
            let (real, shift, mask) = p.resolve(s).unwrap();
            println!(
                "state {} = s{real}[{shift}+{}] w{} {:?}",
                d.defs
                    [p.debug.instances[p.debug.instance(&k.path.instances).unwrap() as usize].def]
                    .name,
                mask.count_ones(),
                k.width,
                k.path.node
            );
        }
    }
    for c in &p.commits {
        match *c {
            logi_sim::compile::Commit::Copy { state, next } => {
                println!("commit {} <- {}", name_of(state), name_of(next))
            }
            other => println!("commit {other:?}"),
        }
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
        Some("dump") if (2..=3).contains(&args.len()) => {
            dump(&args[1], args.get(2).map_or("default", String::as_str));
        }
        Some("cpi") if (2..=4).contains(&args.len()) => {
            cpi(
                &args[1],
                args.get(2).map_or("default", String::as_str),
                args.get(3).map_or(3.0, |s| s.parse().expect("seconds")),
            );
        }
        Some("jit") if (2..=4).contains(&args.len()) => {
            jit_compare(
                &args[1],
                args.get(2).map_or("default", String::as_str),
                args.get(3).map_or(3.0, |s| s.parse().expect("seconds")),
            );
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
                "usage: perf-metrics [cases | jit <case> [OPTIONS] [seconds] | ticks <case> <n> [OPTIONS] [threads] | cpi <cpu case> [OPTIONS] [seconds] | breakdown <case> [OPTIONS] | dump <case> [OPTIONS]]\n\
                 OPTIONS: default, editor, raw, default-nolift, editor-nolift, default-nomatch, editor-nomatch, default-nowords, editor-nowords, default-nophases, editor-nophases"
            );
            std::process::exit(2);
        }
    }
}
