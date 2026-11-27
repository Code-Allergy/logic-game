//! Simulation cost of the gallery's two 16-bit CPUs: `cpu16`, built of
//! gates, and `cpu16_rtl`, the same instruction set and program built of
//! word primitives.
//!
//! ```sh
//! cargo run --release --example cpu_bench               # 2 000 000 ticks each
//! cargo run --release --example cpu_bench -- 500000     # N ticks each
//! cargo run --release --example cpu_bench -- 500000 cpu16_rtl  # one CPU
//! ```
//!
//! Naming one CPU runs only that one (no comparison); this is the target
//! for profiling a CPU's tick loop with `cargo xtask perf flamegraph
//! example:cpu_bench -- 500000 cpu16` (see `docs/performance.md`).
//!
//! For each CPU it prints the size of the design (primitive nodes after
//! flattening) and of the compiled `Program` (instructions, packed lanes,
//! slots, depth), with the compiler's defaults and with the editor's
//! debug options; then ticks per second over N ticks, single-threaded, and
//! instructions of the CPU per second (how many ticks one instruction takes
//! is measured by running the program to its HLT); and the slowdown of the
//! gate CPU against the word CPU. For the gate CPU it also lists the
//! blocks: each definition compiled on its own.

use std::sync::Arc;
use std::time::Instant;

use gate_quest::sim::demos::DemoCatalog;
use gate_quest::sim::netlist::design_of;
use gate_quest::sim::{Demo, WorkspaceDocument};
use logi_sim::compile::{Options, Program, compile};
use logi_sim::netlist::Design;
use logi_sim::sim::{Sim, SimOptions};

fn program(design: &Design, options: &Options) -> Arc<Program> {
    Arc::new(compile(design, options).expect("compiles"))
}

/// Ticks from reset to Halted.
fn ticks_to_halt(p: &Arc<Program>) -> u64 {
    let mut sim = Sim::new(Arc::clone(p));
    while sim.get("Halted") == 0 {
        sim.tick();
        assert!(sim.tick_count() < 10_000_000, "no HLT");
    }
    sim.tick_count()
}

/// Ticks per second over `n` ticks on one thread (the program restarts
/// from reset whenever it halts, so the CPU is always running).
fn ticks_per_second(p: &Arc<Program>, n: u64, run_length: u64) -> f64 {
    let mut sim = Sim::with_options(Arc::clone(p), SimOptions::default());
    sim.run(1000.min(n)); // warm up
    let mut left = n;
    let start = Instant::now();
    while left > 0 {
        let chunk = left.min(run_length.saturating_sub(sim.tick_count()).max(1));
        sim.run(chunk);
        left -= chunk;
        if sim.tick_count() >= run_length {
            sim = Sim::with_options(Arc::clone(p), SimOptions::default());
        }
    }
    n as f64 / start.elapsed().as_secs_f64()
}

fn describe(p: &Program) -> String {
    format!(
        "{} instructions + {} packed lanes = {} work, {} slots, depth {}",
        p.instrs.len(),
        p.packed_lanes(),
        p.work(),
        p.slot_count,
        p.depth()
    )
}

struct Row {
    ticks_per_s: f64,
    instr_per_s: f64,
}

fn bench(demo: &Demo, n: u64, instructions: u64) -> Row {
    let design = design_of(&demo.workspace).expect("design");
    let fast = program(&design, &Options::default());
    let debug = program(&design, &Options::debug());
    let halt = ticks_to_halt(&fast);
    println!("{} ({})", demo.title, demo.id);
    println!(
        "  design:   {} primitive nodes after flattening",
        design.flat_primitives(design.root)
    );
    println!("  compiled: {}", describe(&fast));
    println!("  editor:   {} (debug options)", describe(&debug));
    println!(
        "  program:  {halt} ticks to HLT, {:.2} ticks per instruction",
        halt as f64 / instructions as f64
    );
    let tps = ticks_per_second(&fast, n, halt);
    let tps_debug = ticks_per_second(&debug, n, halt);
    let ips = tps * instructions as f64 / halt as f64;
    println!(
        "  speed:    {:.0} ticks/s, {:.0} CPU instructions/s ({:.0} ticks/s with the editor's options)",
        tps, ips, tps_debug
    );
    println!(
        "  work:     {:.0} M simulator instructions/s",
        tps * fast.work() as f64 / 1e6
    );
    Row {
        ticks_per_s: tps,
        instr_per_s: ips,
    }
}

fn blocks(demo: &Demo) {
    println!("  blocks of {} (each definition compiled alone):", demo.id);
    println!(
        "    {:<14} {:>9} {:>14}",
        "definition", "nodes", "instructions"
    );
    for (name, doc) in &demo.circuits {
        let w = WorkspaceDocument {
            root: (**doc).clone(),
            definitions: demo.workspace.definitions.clone(),
            tests: Default::default(),
        };
        let design = design_of(&w).expect("design");
        let p = program(&design, &Options::default());
        println!(
            "    {:<14} {:>9} {:>14}",
            name,
            design.flat_primitives(design.root),
            p.work()
        );
    }
}

fn main() {
    let n: u64 = std::env::args()
        .nth(1)
        .map_or(2_000_000, |a| a.parse().expect("N ticks"));
    let only = std::env::args().nth(2);
    let catalog = DemoCatalog::embedded().expect("the demos load");
    let gates = catalog.get("cpu16").expect("cpu16");
    let words = catalog.get("cpu16_rtl").expect("cpu16_rtl");
    // The word CPU runs one instruction per tick: its ticks to HLT are the
    // program's instruction count.
    let instructions = {
        let design = design_of(&words.workspace).expect("design");
        ticks_to_halt(&program(&design, &Options::default()))
    };
    println!(
        "{n} ticks per measurement, one thread; the program runs {instructions} instructions\n"
    );
    if let Some(id) = only {
        let demo = match id.as_str() {
            "cpu16" => gates,
            "cpu16_rtl" => words,
            other => panic!("no CPU {other:?}: cpu16 or cpu16_rtl"),
        };
        bench(demo, n, instructions);
        return;
    }
    let g = bench(gates, n, instructions);
    blocks(gates);
    println!();
    let w = bench(words, n, instructions);
    println!();
    println!(
        "gate-level slowdown: {:.1}x per tick, {:.1}x per CPU instruction",
        w.ticks_per_s / g.ticks_per_s,
        w.instr_per_s / g.instr_per_s
    );
}
