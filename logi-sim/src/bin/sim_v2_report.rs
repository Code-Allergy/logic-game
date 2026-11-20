//! Benchmark report and acceptance gates for simulation v2 (design doc
//! section 9; issue #25). Run with:
//!
//! ```text
//! cargo run -p logi-sim --release --bin sim_v2_report [-- --quick]
//! ```
//!
//! Every throughput case changes the root inputs every tick (the cost of
//! that is included) or runs real sequential logic. Single-threaded.

use std::hint::black_box;
use std::sync::Arc;
use std::time::{Duration, Instant};

use logi_sim::circuit::BitPackedCircuit;
use logi_sim::compile::{Options, Program, compile};
use logi_sim::netlist::{Design, mask};
use logi_sim::prelude::{Runner, SimdRunner};
use logi_sim::sim::Sim;
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

/// Throughput cases compile unoptimized, so every instruction counted is
/// work done and the legacy baseline runs identical logic. The simplify
/// section reports what the passes remove.
fn compile_timed(d: &Design) -> (Arc<Program>, f64) {
    compile_with(d, &Options::unoptimized())
}

fn compile_with(d: &Design, opts: &Options) -> (Arc<Program>, f64) {
    let t = Instant::now();
    let p = compile(d, opts).expect("compile");
    (Arc::new(p), t.elapsed().as_secs_f64())
}

/// Seconds per tick for `Sim`, with every input randomized each tick.
fn time_sim(p: &Arc<Program>, budget: Duration) -> f64 {
    let mut sim = Sim::new(Arc::clone(p));
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
    let sizes: &[usize] = if quick { &[10_000, 100_000] } else { &[10_000, 100_000, 1_000_000] };
    let mut gate_ratio = None;
    let mut gate_compile = None;

    println!("== Word-level mixed-op DAGs (32-bit), inputs change every tick ==");
    for &n in sizes {
        let d = generate(&GenConfig::dag(1, n, 32)).design;
        let (p, _) = compile_timed(&d);
        row(&format!("sim dag32 ~{n}"), p.instrs.len(), "instr", time_sim(&p, budget));
    }

    println!("\n== 1-bit gate DAGs: Sim vs legacy SIMD runner (same logic) ==");
    for &n in sizes {
        let d = generate(&GenConfig::gates(2, n, 1)).design;
        let (p, _) = compile_timed(&d);
        let sim_t = time_sim(&p, budget);
        row(&format!("sim gates1 {n}"), p.instrs.len(), "gate", sim_t);

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
        row(&format!("legacy SIMD gates1 {n}"), p.instrs.len(), "gate", simd_t);
        let ratio = simd_t / sim_t;
        println!("{:<34} {ratio:.2}x", "  speedup");
        if n == 1_000_000 {
            gate_ratio = Some(ratio);
        }
    }

    println!("\n== CPU fixture (looping program), word-level vs bit-blasted ==");
    for (label, d) in [
        ("cpu word", fixtures::cpu(&fixtures::LOOP_PROGRAM)),
        ("cpu bit-blasted", bitblast(&fixtures::cpu(&fixtures::LOOP_PROGRAM))),
    ] {
        let (p, _) = compile_timed(&d);
        let t = time_sim(&p, budget);
        println!(
            "{label:<34} {:>9} instr  {:>12.0} cycles/s {:>9.1} Minstr/s",
            p.instrs.len(),
            1.0 / t,
            p.instrs.len() as f64 / t / 1e6
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
    for (label, d) in [("datapath word", dp.clone()), ("datapath bit-blasted", bitblast(&dp))] {
        let (p, _) = compile_timed(&d);
        row(label, p.instrs.len(), "instr", time_sim(&p, budget));
    }

    println!("\n== Simplify passes: instruction count and throughput, unoptimized -> optimized ==");
    for (label, d) in [
        ("cpu word", fixtures::cpu(&fixtures::LOOP_PROGRAM)),
        ("cpu bit-blasted", bitblast(&fixtures::cpu(&fixtures::LOOP_PROGRAM))),
        ("counter32 bit-blasted", fixtures::counter_bitblasted(32)),
        ("datapath bit-blasted", bitblast(&dp)),
    ] {
        let (p0, _) = compile_timed(&d);
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
    let compile_sizes: &[usize] = if quick { &[100_000] } else { &[100_000, 1_000_000] };
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
        let d = generate(&GenConfig::gates(5, if quick { 20_000 } else { 100_000 }, 1)).design;
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
            core.set_probes(nets.iter().take(n).map(|&net| Probe { path: vec![], net }).collect());
            let t = measure(budget / 4, |k| {
                for _ in 0..k {
                    core.publish(&mut frame);
                }
            });
            println!("publish {n:>7} probes ({} nets in design): {:>10.1} us/frame", nets.len(), t * 1e6);
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
