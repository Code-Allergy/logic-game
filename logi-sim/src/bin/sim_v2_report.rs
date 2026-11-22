//! Benchmark report and acceptance gates for simulation v2 (design doc
//! section 9; issue #25). Run with:
//!
//! ```text
//! cargo run -p logi-sim --release --bin sim_v2_report [-- --quick]
//! ```
//!
//! Every throughput case changes the root inputs every tick (the cost of
//! that is included) or runs real sequential logic. Throughput counts the
//! same logic in every configuration: scalar instructions plus packed lanes
//! ([`Program::work`]).
//!
//! The first section shows each step of the performance pass (#41)
//! separately, as the median of `--runs N` runs (default 5; 3 with
//! `--quick`): the M3 layout and runtime, then slot ordering, the streaming
//! runtime, lane packing and 2/3/4 threads, each added on top of the last.
//! The other sections run the current single-threaded defaults.

use std::hint::black_box;
use std::sync::Arc;
use std::time::{Duration, Instant};

use logi_sim::circuit::BitPackedCircuit;
use logi_sim::compile::{Options, PackMode, Program, SlotOrder, compile};
use logi_sim::netlist::{Design, mask};
use logi_sim::prelude::{Runner, SimdRunner};
use logi_sim::sim::{Sim, SimOptions};
use logi_sim::testkit::{GenConfig, bitblast::bitblast, fixtures, generate, legacy::to_legacy};

struct XorShift(u64);
impl XorShift {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

/// Run `step(n)` with growing `n` until it takes at least `budget`; return
/// seconds per unit.
fn measure(budget: Duration, mut step: impl FnMut(u64)) -> f64 {
    step(1); // warm up
    let mut n = 1u64;
    loop {
        let t = Instant::now();
        step(n);
        let dt = t.elapsed();
        if dt >= budget || n >= 1 << 40 {
            return dt.as_secs_f64() / n as f64;
        }
        let scale = (budget.as_secs_f64() / dt.as_secs_f64().max(1e-9)).clamp(2.0, 100.0);
        n = (n as f64 * scale) as u64;
    }
}

/// Throughput cases compile with the simplify passes off, so every
/// instruction counted is work done and the legacy baseline runs identical
/// logic; the memory layout passes (slot order, packing) are at their
/// defaults. The simplify section reports what the passes remove.
fn bench_options() -> Options {
    Options {
        slot_order: SlotOrder::default(),
        pack: PackMode::default(),
        ..Options::unoptimized()
    }
}

fn compile_timed(d: &Design) -> (Arc<Program>, f64) {
    compile_with(d, &bench_options())
}

fn compile_with(d: &Design, opts: &Options) -> (Arc<Program>, f64) {
    let t = Instant::now();
    let p = compile(d, opts).expect("compile");
    (Arc::new(p), t.elapsed().as_secs_f64())
}

/// Seconds per tick for `Sim`, with every input randomized each tick.
fn time_sim(p: &Arc<Program>, budget: Duration) -> f64 {
    time_sim_with(p, &SimOptions::default(), budget)
}

fn time_sim_with(p: &Arc<Program>, so: &SimOptions, budget: Duration) -> f64 {
    let mut sim = Sim::with_options(Arc::clone(p), so.clone());
    let widths: Vec<u8> = p.ports.inputs.iter().map(|x| x.width).collect();
    let mut rng = XorShift(0x9E37_79B9_7F4A_7C15);
    measure(budget, |n| {
        for _ in 0..n {
            for (port, &w) in widths.iter().enumerate() {
                sim.set_input(port, rng.next() & mask(w));
            }
            sim.tick();
        }
        black_box(sim.output(0));
    })
}

/// Seconds per tick for `Sim::run` with no input changes (engine overhead
/// baseline).
fn time_sim_idle(p: &Arc<Program>, budget: Duration) -> f64 {
    let mut sim = Sim::new(Arc::clone(p));
    measure(budget, |n| sim.run(n))
}

fn row(label: &str, work: usize, unit: &str, secs_per_tick: f64) {
    let per_s = work as f64 / secs_per_tick;
    println!(
        "{label:<34} {work:>9} {unit:<6} {:>12.0} ticks/s {:>9.1} M{unit}/s {:>7.2} ns/{unit}",
        1.0 / secs_per_tick,
        per_s / 1e6,
        secs_per_tick * 1e9 / work as f64,
    );
}

fn main() {
    let quick = std::env::args().any(|a| a == "--quick");
    let budget = Duration::from_millis(if quick { 200 } else { 1000 });
    let sizes: &[usize] = if quick {
        &[10_000, 100_000]
    } else {
        &[10_000, 100_000, 1_000_000]
    };
    let runs: usize = std::env::args()
        .skip_while(|a| a != "--runs")
        .nth(1)
        .map_or(if quick { 3 } else { 5 }, |n| n.parse().expect("--runs N"));
    let mut gate_ratio = None;
    let mut gate_compile = None;

    perf_steps(quick, runs, budget);

    println!("== Word-level mixed-op DAGs (32-bit), inputs change every tick ==");
    for &n in sizes {
        let d = generate(&GenConfig::dag(1, n, 32)).design;
        let (p, _) = compile_timed(&d);
        row(
            &format!("sim dag32 ~{n}"),
            p.work(),
            "instr",
            time_sim(&p, budget),
        );
    }

    println!("\n== 1-bit gate DAGs: Sim vs legacy SIMD runner (same logic) ==");
    for &n in sizes {
        let d = generate(&GenConfig::gates(2, n, 1)).design;
        let (p, _) = compile_timed(&d);
        let sim_t = time_sim(&p, budget);
        row(&format!("sim gates1 {n}"), p.work(), "gate", sim_t);

        let legacy = to_legacy(&d);
        let mut simd = SimdRunner::new(BitPackedCircuit::compile(&legacy.circuit));
        let mut rng = XorShift(0x1234_5678_9ABC_DEF1);
        let simd_t = measure(budget, |k| {
            for _ in 0..k {
                for &node in &legacy.inputs {
                    simd.set_input(node, 0, rng.next() & 1 == 1);
                }
                simd.tick();
            }
            black_box(simd.output(legacy.outputs[0], 0));
        });
        row(&format!("legacy SIMD gates1 {n}"), p.work(), "gate", simd_t);
        let ratio = simd_t / sim_t;
        println!("{:<34} {ratio:.2}x", "  speedup");
        if n == 1_000_000 {
            gate_ratio = Some(ratio);
        }
    }

    println!("\n== CPU fixture (looping program), word-level vs bit-blasted ==");
    for (label, d) in [
        ("cpu word", fixtures::cpu(&fixtures::LOOP_PROGRAM)),
        (
            "cpu bit-blasted",
            bitblast(&fixtures::cpu(&fixtures::LOOP_PROGRAM)),
        ),
    ] {
        let (p, _) = compile_timed(&d);
        let t = time_sim(&p, budget);
        println!(
            "{label:<34} {:>9} instr  {:>12.0} cycles/s {:>9.1} Minstr/s",
            p.work(),
            1.0 / t,
            p.work() as f64 / t / 1e6
        );
    }

    println!("\n== Generated word-level datapath (flat, stateful, 32-bit) vs bit-blasted ==");
    let dp = generate(&GenConfig {
        seed: 3,
        nodes: if quick { 2_000 } else { 20_000 },
        child_defs: 0,
        max_depth: 1,
        max_width: 32,
        state_percent: 20,
        unconnected_percent: 0,
        inputs: 16,
        outputs: 16,
        ..Default::default()
    })
    .design;
    for (label, d) in [
        ("datapath word", dp.clone()),
        ("datapath bit-blasted", bitblast(&dp)),
    ] {
        let (p, _) = compile_timed(&d);
        row(label, p.work(), "instr", time_sim(&p, budget));
    }

    println!("\n== Simplify passes: instruction count and throughput, unoptimized -> optimized ==");
    for (label, d) in [
        ("cpu word", fixtures::cpu(&fixtures::LOOP_PROGRAM)),
        (
            "cpu bit-blasted",
            bitblast(&fixtures::cpu(&fixtures::LOOP_PROGRAM)),
        ),
        ("counter32 bit-blasted", fixtures::counter_bitblasted(32)),
        ("datapath bit-blasted", bitblast(&dp)),
    ] {
        let (p0, _) = compile_with(&d, &Options::unoptimized());
        let (p1, _) = compile_with(&d, &Options::default());
        let (t0, t1) = (time_sim(&p0, budget), time_sim(&p1, budget));
        println!(
            "{label:<34} {:>9} -> {:>9} instr ({:>5.1}% fewer)  {:>10.0} -> {:>10.0} ticks/s",
            p0.instrs.len(),
            p1.instrs.len(),
            100.0 * (1.0 - p1.instrs.len() as f64 / p0.instrs.len() as f64),
            1.0 / t0,
            1.0 / t1
        );
    }

    println!("\n== Compile time (all passes) ==");
    let compile_sizes: &[usize] = if quick {
        &[100_000]
    } else {
        &[100_000, 1_000_000]
    };
    for &n in compile_sizes {
        for (label, d) in [
            ("gates1", generate(&GenConfig::gates(4, n, 1)).design),
            ("dag32", generate(&GenConfig::dag(4, n, 32)).design),
        ] {
            let nodes: usize = d.defs.values().map(|x| x.nodes.len()).sum();
            let (p, secs) = compile_with(&d, &Options::default());
            println!(
                "compile {label} {n:<9} {nodes:>9} nodes -> {:>9} instr  {:>8.1} ms",
                p.instrs.len(),
                secs * 1e3
            );
            if n == 1_000_000 && label == "gates1" {
                gate_compile = Some(secs);
            }
        }
    }

    println!("\n== Engine (engine_v2) ==");
    {
        use logi_sim::engine_v2::{Engine, Probe, ProbeFrame, Rate};
        let d = generate(&GenConfig::gates(
            5,
            if quick { 20_000 } else { 100_000 },
            1,
        ))
        .design;
        let (p, _) = compile_with(&d, &Options::debug());
        let direct = 1.0 / time_sim_idle(&p, budget);
        let e = Engine::spawn(Options::debug());
        e.load(Arc::new(d.clone()), 1);
        let _ = e.wait_event(Duration::from_secs(10));
        e.set_rate(Rate::Unlimited);
        e.sync();
        let (t0, n0) = (Instant::now(), e.tick_count());
        std::thread::sleep(budget * 2);
        e.sync();
        let engine = (e.tick_count() - n0) as f64 / t0.elapsed().as_secs_f64();
        println!(
            "unlimited: Sim::run {direct:.0} ticks/s, engine {engine:.0} ticks/s ({:.1}%)",
            100.0 * engine / direct
        );
        drop(e);

        // Readback cost per published frame, by probe count.
        let mut core = logi_sim::engine_v2::EngineCore::new(Options::debug());
        core.load(&d, 1);
        let nets: Vec<_> = d.root_def().nets.keys().collect();
        let mut frame = ProbeFrame::default();
        for n in [16usize, 1_000, nets.len()] {
            core.set_probes(
                nets.iter()
                    .take(n)
                    .map(|&net| Probe { path: vec![], net })
                    .collect(),
            );
            let t = measure(budget / 4, |k| {
                for _ in 0..k {
                    core.publish(&mut frame);
                }
            });
            println!(
                "publish {n:>7} probes ({} nets in design): {:>10.1} us/frame",
                nets.len(),
                t * 1e6
            );
        }
    }

    println!("\n== Gates (section 9) ==");
    match gate_ratio {
        Some(r) => println!(
            "1M gate DAG throughput >= 3x legacy SIMD: {r:.2}x  {}",
            if r >= 3.0 { "PASS" } else { "FAIL" }
        ),
        None => println!("1M gate DAG throughput: skipped (--quick)"),
    }
    match gate_compile {
        Some(s) => println!(
            "compile 1M nodes < 2 s: {s:.2} s  {}",
            if s < 2.0 { "PASS" } else { "FAIL" }
        ),
        None => println!("compile 1M nodes: skipped (--quick)"),
    }
}

/// Median of `runs` throughput measurements (work items per second).
fn median_rate(
    p: &Arc<Program>,
    so: &SimOptions,
    work: usize,
    runs: usize,
    budget: Duration,
) -> f64 {
    let mut v: Vec<f64> = (0..runs)
        .map(|_| work as f64 / time_sim_with(p, so, budget))
        .collect();
    v.sort_by(|a, b| a.total_cmp(b));
    v[v.len() / 2]
}

/// Per-step gains of the performance pass (#41), single- and
/// multi-threaded, on the cases the issue names.
fn perf_steps(quick: bool, runs: usize, budget: Duration) {
    let dp = generate(&GenConfig {
        seed: 3,
        nodes: if quick { 2_000 } else { 20_000 },
        child_defs: 0,
        max_depth: 1,
        max_width: 32,
        state_percent: 20,
        unconnected_percent: 0,
        inputs: 16,
        outputs: 16,
        ..Default::default()
    })
    .design;
    let big = if quick { 100_000 } else { 1_000_000 };
    let cases: Vec<(String, Design)> = vec![
        (
            format!("gates1 {big} (1-bit gate DAG)"),
            generate(&GenConfig::gates(2, big, 1)).design,
        ),
        (
            "cpu bit-blasted".into(),
            bitblast(&fixtures::cpu(&fixtures::LOOP_PROGRAM)),
        ),
        (
            format!("dag32 ~{big} (word-level DAG)"),
            generate(&GenConfig::dag(1, big, 32)).design,
        ),
        ("datapath bit-blasted".into(), bitblast(&dp)),
        (
            "gates1 10000".into(),
            generate(&GenConfig::gates(2, 10_000, 1)).design,
        ),
    ];
    let m3 = Options::unoptimized();
    let order = Options {
        slot_order: SlotOrder::FirstUse,
        ..m3.clone()
    };
    let pack = Options {
        pack: PackMode::Auto,
        ..order.clone()
    };
    let pack_all = Options {
        pack: PackMode::All,
        ..order.clone()
    };
    let single = SimOptions::default();
    // (label, compile options, runtime options)
    let steps: Vec<(&str, Options, SimOptions)> = vec![
        ("M3", m3.clone(), SimOptions::baseline()),
        ("+order", order.clone(), SimOptions::baseline()),
        ("+stream", order.clone(), single.clone()),
        ("+pack", pack.clone(), single.clone()),
        ("2T", pack.clone(), SimOptions::threaded(2)),
        ("3T", pack.clone(), SimOptions::threaded(3)),
        ("4T", pack.clone(), SimOptions::threaded(4)),
        (
            "4T part",
            pack.clone().for_threads(4),
            SimOptions::threaded(4),
        ),
        ("pack=All", pack_all.clone(), single.clone()),
    ];
    println!(
        "== Performance pass (#41): each step added to the previous, M work/s, median of {runs} \
         runs =="
    );
    println!(
        "   (M3: the M3 layout and runtime; +order: SlotOrder::FirstUse; +stream: streaming \
         runtime;\n    +pack: PackMode::Auto; nT: SimOptions::threaded(n), par_min {}; \
         4T part: also Options::partitions = 4;\n    pack=All: +stream with every 1-bit \
         instruction packed. Threads in use are shown when fewer than asked.)",
        logi_sim::sim::DEFAULT_PAR_MIN
    );
    print!("{:<32}", "case");
    for (label, _, _) in &steps {
        print!(" {label:>9}");
    }
    println!();
    for (label, d) in &cases {
        let work = compile(d, &m3).expect("compile").work();
        let mut progs: Vec<(Options, Arc<Program>)> = Vec::new();
        let mut rates = Vec::new();
        let mut notes = Vec::new();
        for (_, opts, so) in &steps {
            let p = match progs.iter().find(|(o, _)| o == opts) {
                Some((_, p)) => Arc::clone(p),
                None => {
                    let p = Arc::new(compile(d, opts).expect("compile"));
                    progs.push((opts.clone(), Arc::clone(&p)));
                    p
                }
            };
            let used = Sim::with_options(Arc::clone(&p), so.clone()).threads();
            rates.push(median_rate(&p, so, work, runs, budget));
            notes.push(if used < so.threads {
                format!("({used}T)")
            } else {
                String::new()
            });
        }
        let one = rates[3];
        print!("{label:<32}");
        for (r, note) in rates.iter().zip(&notes) {
            print!(" {:>9}", format!("{:.0}{note}", r / 1e6));
        }
        print!("\n{:<32}", "  speedup vs M3");
        for r in &rates {
            print!(" {:>8.2}x", r / rates[0]);
        }
        print!("\n{:<32}", "  thread scaling vs +pack");
        for (k, r) in rates.iter().enumerate() {
            if (4..=7).contains(&k) {
                print!(" {:>8.2}x", r / one);
            } else {
                print!(" {:>9}", "");
            }
        }
        println!();
    }
    println!();
}
