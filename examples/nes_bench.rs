//! Throughput of a saved workspace's root circuit (the NES console of
//! nes/) under the compile and runtime options that decide whether
//! clock-phase variants and tick fusion apply.
//!
//! ```sh
//! cargo run --release --example nes_bench -- .gatequest/workspaces/nes_test [ticks]
//! cargo run --release --features jit --example nes_bench -- .gatequest/workspaces/nes_test
//! cargo run --release --features jit --example nes_bench -- .gatequest/workspaces/nes --compare [ticks]
//! ```
//!
//! `--compare` runs the same ticks on the interpreter and on the JIT (each
//! with variants and fusion) and requires every state element and every
//! memory (the screen, RAMs, OAM, palette) to be the same at the end.
//! `--blocks` reports where the ops of a tick are: each chip under the
//! root (and inside large ones) compiled on its own, with its op mix. The
//! settle is straight-line code that runs every op of its variant each
//! tick whatever the data, so a block's share of ops is its share of time.

use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use gate_quest::mcp::suite;
use gate_quest::sim::netlist::design_of;
use logi_sim::compile::{Options, Program, compile};
use logi_sim::sim::{Sim, SimOptions};

fn main() -> Result<(), String> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let compare = args.iter().any(|a| a == "--compare");
    let blocks = args.iter().any(|a| a == "--blocks");
    args.retain(|a| a != "--compare" && a != "--blocks");
    let folder = args
        .first()
        .ok_or("usage: nes_bench <workspace folder> [--compare] [ticks]")?;
    let ticks: u64 = args
        .get(1)
        .map_or(Ok(3_000_000), |t| t.parse())
        .map_err(|e| format!("{e}"))?;
    let ws = suite::read_workspace(Path::new(folder))?;
    let design = design_of(&ws)?;
    if compare {
        return compare_jit(&design, ticks);
    }
    if blocks {
        return block_costs(&ws);
    }
    for (name, options) in [
        ("default", Options::default()),
        ("debug (editor)", Options::debug()),
    ] {
        let start = Instant::now();
        let program = Arc::new(compile(&design, &options).map_err(|e| format!("{e:?}"))?);
        println!(
            "\n{name}: compiled in {:.2} s; {}",
            start.elapsed().as_secs_f64(),
            describe(&program)
        );
        let jit = SimOptions::jit();
        let runs: [(&str, SimOptions, bool); 8] = [
            ("JIT: run(n), variants + fusion", jit.clone(), true),
            (
                "JIT: run(n), neither",
                SimOptions {
                    fuse: false,
                    phases: false,
                    ..jit.clone()
                },
                true,
            ),
            ("JIT: tick() loop", jit, false),
            ("run(n), variants + fusion", SimOptions::default(), true),
            (
                "run(n), variants only",
                SimOptions {
                    fuse: false,
                    ..SimOptions::default()
                },
                true,
            ),
            (
                "run(n), fusion only",
                SimOptions {
                    phases: false,
                    ..SimOptions::default()
                },
                true,
            ),
            (
                "run(n), neither",
                SimOptions {
                    fuse: false,
                    phases: false,
                    ..SimOptions::default()
                },
                true,
            ),
            ("tick() loop, defaults", SimOptions::default(), false),
        ];
        for (what, sim_options, batch) in runs {
            let start = Instant::now();
            let jit_on = sim_options.jit;
            let mut sim = Sim::with_options(Arc::clone(&program), sim_options);
            let built = start.elapsed().as_secs_f64();
            if jit_on && !sim.jit_active() {
                println!("  {what:32} JIT not active (build with --features jit)");
                continue;
            }
            let start = Instant::now();
            if batch {
                sim.run(ticks);
            } else {
                for _ in 0..ticks {
                    sim.tick();
                }
            }
            let rate = ticks as f64 / start.elapsed().as_secs_f64();
            let jit_note = if jit_on {
                format!(", JIT compiled in {built:.2} s")
            } else {
                String::new()
            };
            println!(
                "  {what:32} {:6.0} k ticks/s ({:5.1}% of NES speed{jit_note})",
                rate / 1e3,
                rate / 5_369_318.0 * 100.0
            );
        }
    }
    Ok(())
}

fn describe(p: &Program) -> String {
    let base = p.work();
    match &p.phases {
        Some(ph) => {
            let per: Vec<String> = ph.variants.iter().map(|b| b.ops().to_string()).collect();
            format!(
                "{base} ops unspecialized; phases: period {}, variants [{}] ops, {:.0} per tick on average",
                ph.period,
                per.join(", "),
                ph.ops_per_tick()
            )
        }
        None => format!("{base} ops; no clock-phase variants"),
    }
}

/// The interpreter and the JIT, `ticks` ticks from power-on: the same state
/// and memories, or an error naming the first difference.
fn compare_jit(design: &logi_sim::netlist::Design, ticks: u64) -> Result<(), String> {
    let program = Arc::new(compile(design, &Options::default()).map_err(|e| format!("{e:?}"))?);
    let mut interp = Sim::with_options(Arc::clone(&program), SimOptions::default());
    let mut jit = Sim::with_options(Arc::clone(&program), SimOptions::jit());
    if !jit.jit_active() {
        return Err("the JIT is not active (build with --features jit)".into());
    }
    interp.run(ticks);
    jit.run(ticks);
    let (a, b) = (interp.export_state(), jit.export_state());
    let mut differ = 0;
    for (path, v) in &a.words {
        if b.words.get(path) != Some(v) {
            differ += 1;
        }
    }
    for (path, m) in &a.mems {
        if b.mems.get(path) != Some(m) {
            differ += 1;
            println!("memory {path:?} differs");
        }
    }
    let words = a.words.len();
    let mems = a.mems.len();
    if differ == 0 && a.tick == b.tick {
        println!(
            "{ticks} ticks: interpreter and JIT agree on {words} state words and {mems} memories"
        );
        Ok(())
    } else {
        Err(format!(
            "{differ} of {} state elements differ",
            words + mems
        ))
    }
}

/// Ops per block: the root's chips, and the chips inside any of them that
/// costs at least 150 ops, each compiled as a root.
fn block_costs(ws: &gate_quest::sim::WorkspaceDocument) -> Result<(), String> {
    use gate_quest::mcp::batch::Place;
    use gate_quest::mcp::compiled::insight;
    use gate_quest::sim::{CircuitDocument, GateKind};

    fn children(c: &CircuitDocument) -> Vec<(String, uuid::Uuid)> {
        let mut out: Vec<(String, uuid::Uuid)> = c
            .nodes
            .iter()
            .filter_map(|n| match n.kind {
                GateKind::SubCircuit { definition } => {
                    Some((n.label.clone().unwrap_or_default(), definition))
                }
                _ => None,
            })
            .collect();
        out.sort();
        out
    }
    fn mix(by_op: &std::collections::BTreeMap<String, usize>) -> String {
        let mut v: Vec<(&String, &usize)> = by_op.iter().collect();
        v.sort_by(|a, b| b.1.cmp(a.1));
        v.iter()
            .take(6)
            .map(|(k, n)| format!("{k} {n}"))
            .collect::<Vec<_>>()
            .join(", ")
    }
    let total = insight(ws, Place::Root)?;
    println!(
        "console: {} ops per tick ({})",
        total.size.instructions,
        mix(&total.size.by_op)
    );
    let t = total.size.instructions as f64;
    let mut listed = 0usize;
    for (label, id) in children(&ws.root) {
        let Some(def) = ws.definitions.get(&id) else {
            continue;
        };
        let i = insight(ws, Place::Definition(id))?;
        let n = i.size.instructions;
        listed += n;
        println!(
            "\n{label} ({}): {n} ops, {:.1}% ({})",
            def.name,
            n as f64 / t * 100.0,
            mix(&i.size.by_op)
        );
        if n >= 150 {
            let mut inner = 0;
            for (sub, sid) in children(def) {
                let Some(sdef) = ws.definitions.get(&sid) else {
                    continue;
                };
                let si = insight(ws, Place::Definition(sid))?;
                let m = si.size.instructions;
                inner += m;
                if m >= 10 {
                    println!(
                        "    {sub:14} {:18} {m:5} ops {:5.1}%  ({})",
                        sdef.name,
                        m as f64 / t * 100.0,
                        mix(&si.size.by_op)
                    );
                }
            }
            println!(
                "    {:33} {:5} ops (the block's own wiring and logic)",
                "glue",
                n.saturating_sub(inner)
            );
        }
    }
    println!(
        "\nroot glue: {} ops (the console's own decode and wiring, and what compiling blocks together saves or adds)",
        total.size.instructions as i64 - listed as i64
    );
    Ok(())
}
