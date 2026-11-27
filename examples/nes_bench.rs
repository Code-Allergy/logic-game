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
//! `--against <other workspace>` runs both workspaces from power-on and
//! compares every memory they share (screen, RAMs, OAM, palette, CHR) at
//! each frame boundary: a redesigned block must leave them all unchanged.
//! `--press frame:input:frames,...` holds a root input (a `p1_*` button)
//! at 1 from that frame for that many frames, in both; `--screen file`
//! stores the last frame's screen memory.
//! `--attribute [depth]` compiles the whole workspace once and attributes
//! every op of its clock-phase variants to the block whose net it writes
//! (a slot shared by a chip's port and its parent counts for the deeper
//! one): ops per tick in context, and how many sit in guarded ranges that
//! may be skipped.
//! `--executed` (build with `--features counters`) counts the instructions
//! the interpreter actually executes per tick in `run(n)` batches, with
//! guards on and off: what guards skip at run time.
//! `--unnamed` lists the ops that write no authored net, `--ops <block>`
//! the ops writing a block's nets (op, width, operands, readers).
//! `--live <frames> [--skip <frames>] [--press ...] [--every <k>]
//! [--dead-ops <prefix>] [--attribute <depth>]` measures the running
//! console instead of the static bodies: the interpreter plays the presses
//! to frame `--skip`, then ticks `<frames>` frames one by one. Per tick it
//! evaluates the guards of that phase's fused body on the live slots, so
//! each block's executed ops per tick count what the guards really skip;
//! ticks are split by place in the frame (dot and scanline ranges, vblank,
//! found from the PPU's dot and line counters) and rendering off (PPU mask
//! bits 3 and 4). Every `--every` ticks (default 5) a backward liveness
//! pass from the roots (commits, a RAM write's address and data only when
//! it writes, guard conditions, outputs) follows only the operands the
//! current values select (a `Mux` or `Switch` its taken input, an `And`
//! with a 0 operand that operand): an executed op nothing live reads is
//! dead that tick. It also counts the ops that depend on the dot and line
//! counters alone (foldable by a decode table or keys on them).
//! `--dead-ops <prefix>` lists the most often dead ops of the blocks under
//! it, by region. E.g., SMB3 in World 1-1:
//!
//! ```sh
//! nes_bench .gatequest/workspaces/nes --live 20 --skip 1905 \
//!     --press 451:p1_start:6,657:p1_start:6,1063:p1_right:12,1175:p1_a:6,1481:p1_up:12,1593:p1_a:6,1899:p1_right:200
//! ```
//!
//! `--no-shifts` compiles without the shift rewrites (`Options::shifts`
//! and `Options::barrels`) everywhere, for A/B runs; `--compare` also
//! runs the interpreter on a program compiled without them and requires
//! the same state.
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
use logi_sim::sim::{FallbackOptions, Sim, SimOptions, SpecOptions};

/// `--no-shifts`: compile without the shift rewrites.
static NO_SHIFTS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The compile options of every run: the defaults, without the shift
/// rewrites under `--no-shifts`.
fn base_options() -> Options {
    if NO_SHIFTS.load(std::sync::atomic::Ordering::Relaxed) {
        without_shifts(Options::default())
    } else {
        Options::default()
    }
}

fn without_shifts(o: Options) -> Options {
    Options {
        shifts: false,
        barrels: false,
        ..o
    }
}

fn main() -> Result<(), String> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(k) = args.iter().position(|a| a == "--no-shifts") {
        args.remove(k);
        NO_SHIFTS.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    let compare = args.iter().any(|a| a == "--compare");
    let blocks = args.iter().any(|a| a == "--blocks");
    let units = args.iter().any(|a| a == "--units");
    let executed = args.iter().any(|a| a == "--executed");
    let unnamed = args.iter().any(|a| a == "--unnamed");
    let ops_of = args.iter().position(|a| a == "--ops").map(|k| {
        let block = args.get(k + 1).cloned().unwrap_or_default();
        args.drain(k..(k + 2).min(args.len()));
        block
    });
    let attribute = args.iter().position(|a| a == "--attribute").map(|k| {
        let depth = args.get(k + 1).and_then(|d| d.parse::<usize>().ok());
        args.drain(k..(k + 1 + usize::from(depth.is_some())).min(args.len()));
        depth.unwrap_or(2)
    });
    // `--only <text>`: run only the configurations whose name contains it.
    let only = args.iter().position(|a| a == "--only").map(|k| {
        let v = args.get(k + 1).cloned().unwrap_or_default();
        args.drain(k..(k + 2).min(args.len()));
        v
    });
    let against = args.iter().position(|a| a == "--against").map(|i| {
        let other = args.get(i + 1).cloned().unwrap_or_default();
        args.drain(i..(i + 2).min(args.len()));
        other
    });
    let mut value_of = |flag: &str| {
        args.iter().position(|a| a == flag).map(|i| {
            let v = args.get(i + 1).cloned().unwrap_or_default();
            args.drain(i..(i + 2).min(args.len()));
            v
        })
    };
    let presses = parse_presses(&value_of("--press").unwrap_or_default())?;
    let screen = value_of("--screen");
    let live = match value_of("--live") {
        Some(frames) => {
            let num = |v: Option<String>, d: u64, flag: &str| -> Result<u64, String> {
                v.map_or(Ok(d), |v| v.parse().map_err(|_| format!("{flag} <number>")))
            };
            let frames = num(Some(frames), 0, "--live")?;
            let skip = num(value_of("--skip"), 0, "--skip")?;
            let every = num(value_of("--every"), 5, "--every")?.max(1);
            Some(Live {
                frames,
                skip,
                every,
                presses: presses.clone(),
                dead_ops: value_of("--dead-ops"),
            })
        }
        None => None,
    };
    // `--live` reports through the attribution (depth 2 unless given).
    let attribute = attribute.or(live.as_ref().map(|_| 2));
    args.retain(|a| {
        a != "--compare"
            && a != "--units"
            && a != "--blocks"
            && a != "--executed"
            && a != "--unnamed"
    });
    let folder = args.first().ok_or(
        "usage: nes_bench <workspace folder> [--compare | --blocks | --against <workspace> [--press ...] [--screen file]] [--only <name>] [ticks]",
    )?;
    let ticks: u64 = args
        .get(1)
        .map_or(Ok(3_000_000), |t| t.parse())
        .map_err(|e| format!("{e}"))?;
    let ws = suite::read_workspace(Path::new(folder))?;
    let design = design_of(&ws)?;
    if compare {
        return compare_jit(&design, ticks);
    }
    if units {
        return unit_costs(&design);
    }
    if blocks {
        return block_costs(&ws);
    }
    #[cfg(not(feature = "counters"))]
    if executed {
        return Err("--executed needs the counters: build with --features counters".into());
    }
    #[cfg(feature = "counters")]
    if executed {
        let program = Arc::new(compile(&design, &base_options()).map_err(|e| format!("{e:?}"))?);
        for (what, guard, known_bits) in [
            ("guards on", true, true),
            ("guards off", false, true),
            ("no known bits", true, false),
        ] {
            let mut sim = Sim::with_options(
                Arc::clone(&program),
                SimOptions {
                    guard,
                    known_bits,
                    guard_interpreter: guard,
                    guard_min: std::env::var("GUARD_MIN")
                        .ok()
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(SimOptions::default().guard_min),
                    ..SimOptions::default()
                },
            );
            sim.reset_counters();
            sim.run(ticks);
            let c = sim.counters();
            if c.instrs == 0 {
                return Err("no counts: build with --features counters".into());
            }
            println!(
                "{what:13}: {:.1} instructions, {:.1} ops executed per tick ({} ticks)",
                c.instrs as f64 / ticks as f64,
                c.ops() as f64 / ticks as f64,
                ticks
            );
        }
        return Ok(());
    }
    if let Some(depth) = attribute {
        return attribute_ops(&ws, depth, None, live.as_ref());
    }
    if unnamed {
        return attribute_ops(&ws, 2, Some(String::new()), None);
    }
    if let Some(block) = ops_of {
        return attribute_ops(&ws, 2, Some(block), None);
    }
    if let Some(other) = against {
        let other = design_of(&suite::read_workspace(Path::new(&other))?)?;
        return compare_designs(&design, &other, ticks, &presses, screen.as_deref());
    }
    for (name, options) in [
        ("default", base_options()),
        (
            "debug (editor)",
            Options {
                debug: true,
                ..base_options()
            },
        ),
    ] {
        let start = Instant::now();
        let program = Arc::new(compile(&design, &options).map_err(|e| format!("{e:?}"))?);
        println!(
            "\n{name}: compiled in {:.2} s; {}",
            start.elapsed().as_secs_f64(),
            describe(&program)
        );
        // `SUPERSTEP_TICKS` and `SUPERSTEP_CHUNK` override the superstep's
        // length and chunk size (for sweeps).
        let env = |k: &str| std::env::var(k).ok().and_then(|v| v.parse::<u64>().ok());
        let jit = SimOptions {
            superstep_ticks: env("SUPERSTEP_TICKS")
                .unwrap_or(SimOptions::default().superstep_ticks),
            superstep_chunk: env("SUPERSTEP_CHUNK")
                .map_or(SimOptions::default().superstep_chunk, |c| c as usize),
            ..SimOptions::jit()
        };
        let no_guard = |o: &SimOptions| SimOptions {
            guard: false,
            ..o.clone()
        };
        let runs: [(&str, SimOptions, bool); 14] = [
            ("JIT: run(n), variants + fusion", jit.clone(), true),
            (
                "JIT: run(n), no superstep",
                SimOptions {
                    superstep: false,
                    ..jit.clone()
                },
                true,
            ),
            (
                "JIT: run(n), no known bits",
                SimOptions {
                    known_bits: false,
                    ..jit.clone()
                },
                true,
            ),
            ("JIT: run(n), no guards", no_guard(&jit), true),
            ("JIT: tick() loop, no guards", no_guard(&jit), false),
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
                "run(n), guards in the interpreter",
                SimOptions {
                    guard_interpreter: true,
                    ..SimOptions::default()
                },
                true,
            ),
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
            (
                "tick() loop, guards in the interpreter",
                SimOptions {
                    guard_interpreter: true,
                    ..SimOptions::default()
                },
                false,
            ),
        ];
        for (what, mut sim_options, batch) in runs {
            if only.as_ref().is_some_and(|o| !what.contains(o.as_str())) {
                continue;
            }
            // GUARD_MIN overrides the guard threshold of every row.
            if let Some(min) = std::env::var("GUARD_MIN").ok().and_then(|v| v.parse().ok()) {
                sim_options.guard_min = min;
            }
            let start = Instant::now();
            let jit_on = sim_options.jit;
            let mut sim = Sim::with_options(Arc::clone(&program), sim_options);
            let built = start.elapsed().as_secs_f64();
            if jit_on && !sim.jit_active() {
                println!("  {what:32} JIT not active (build with --features jit)");
                continue;
            }
            #[cfg(feature = "counters")]
            sim.reset_counters();
            let start = Instant::now();
            if batch {
                sim.run(ticks);
            } else {
                for _ in 0..ticks {
                    sim.tick();
                }
            }
            let rate = ticks as f64 / start.elapsed().as_secs_f64();
            #[cfg(feature = "jit")]
            let code = sim.jit_code().map_or(String::new(), |j| {
                let sup = j.superstep().map_or(String::new(), |s| {
                    let s = &s.info;
                    format!(
                        "; superstep of {} ticks: {} items, {} chunks, {} KB, {} of {} writes stored",
                        s.ticks,
                        s.items - s.dead,
                        s.chunks,
                        s.code_bytes / 1024,
                        s.stores,
                        s.writes
                    )
                });
                format!(", {} KB{sup}", j.code_bytes / 1024)
            });
            #[cfg(not(feature = "jit"))]
            let code = String::new();
            let jit_note = if jit_on {
                format!(", JIT compiled in {built:.2} s{code}")
            } else {
                String::new()
            };
            #[cfg(feature = "counters")]
            let ops = {
                let c = sim.counters();
                let t = c.ticks.max(1) as f64;
                format!(
                    ", {:.0} ops/tick, {:.0} blocks/tick",
                    c.ops() as f64 / t,
                    c.blocks as f64 / t
                )
            };
            #[cfg(not(feature = "counters"))]
            let ops = String::new();
            println!(
                "  {what:32} {:6.0} k ticks/s ({:5.1}% of NES speed{jit_note}{ops})",
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

/// The interpreter and the JIT, with guards on and off, `ticks` ticks
/// from power-on: the same state and memories, or an error naming the
/// first difference. Then a shorter run compared at every batch boundary:
/// fused batches of 1 to 7 ticks (guards on, interpreter and JIT) against
/// single ticks with guards off.
/// `--units`: compile every body of the console as a `JitUnit` of its own
/// (what adaptive specialization does per hot key) and report each one's
/// instructions, native code size and compile time.
#[cfg(not(feature = "jit"))]
fn unit_costs(_: &logi_sim::netlist::Design) -> Result<(), String> {
    Err("--units needs the JIT: build with --features jit".into())
}

#[cfg(feature = "jit")]
fn unit_costs(design: &logi_sim::netlist::Design) -> Result<(), String> {
    let program = Arc::new(compile(design, &Options::default()).map_err(|e| format!("{e:?}"))?);
    let sim = Sim::new(Arc::clone(&program));
    // Warm-up: the first compile pays one-time costs (ISA lookup, page faults).
    drop(sim.compile_unit_of(0));
    let (mut bytes, mut instrs, mut n) = (0, 0, 0);
    let mut total = std::time::Duration::ZERO;
    for k in 0..sim.body_count() {
        let t = Instant::now();
        let u = sim.compile_unit_of(k);
        let took = t.elapsed();
        match u {
            None => println!("body {k}: not compiled"),
            Some(Err(e)) => println!("body {k}: error {}", e.0),
            Some(Ok(u)) => {
                println!(
                    "body {k}: {} instrs, {} settle chunks, {} KiB native, {:.1} ms",
                    u.instrs(),
                    u.settle_chunks(),
                    u.code_bytes() / 1024,
                    took.as_secs_f64() * 1e3
                );
                bytes += u.code_bytes();
                instrs += u.instrs();
                total += took;
                n += 1;
            }
        }
    }
    println!(
        "{n} units, {instrs} instrs, {} KiB native, {:.1} ms to compile them one by one",
        bytes / 1024,
        total.as_secs_f64() * 1e3
    );
    Ok(())
}

fn compare_jit(design: &logi_sim::netlist::Design, ticks: u64) -> Result<(), String> {
    let program = Arc::new(compile(design, &base_options()).map_err(|e| format!("{e:?}"))?);
    // The same design without the shift rewrites, interpreted.
    let unshifted =
        Arc::new(compile(design, &without_shifts(base_options())).map_err(|e| format!("{e:?}"))?);
    let no_guard = |o: SimOptions| SimOptions { guard: false, ..o };
    let configs = [
        ("interpreter, no guards", no_guard(SimOptions::default())),
        (
            "interpreter",
            SimOptions {
                guard_interpreter: true,
                ..SimOptions::default()
            },
        ),
        ("JIT", SimOptions::jit()),
        ("JIT, no guards", no_guard(SimOptions::jit())),
        (
            "JIT, no known bits",
            SimOptions {
                known_bits: false,
                ..SimOptions::jit()
            },
        ),
        (
            "JIT, no superstep",
            SimOptions {
                superstep: false,
                ..SimOptions::jit()
            },
        ),
        (
            "interpreter, shift rewrites off",
            no_guard(SimOptions::default()),
        ),
    ];
    let mut sims: Vec<Sim> = configs
        .iter()
        .enumerate()
        .map(|(k, (_, o))| {
            let p = if k == 5 { &unshifted } else { &program };
            Sim::with_options(Arc::clone(p), o.clone())
        })
        .collect();
    if !sims[2].jit_active() {
        return Err("the JIT is not active (build with --features jit)".into());
    }
    for s in &mut sims {
        s.run(ticks);
    }
    let a = sims[0].export_state();
    for (k, s) in sims.iter().enumerate().skip(1) {
        let b = s.export_state();
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
        if differ > 0 || a.tick != b.tick {
            return Err(format!(
                "{}: {differ} of {} state elements differ from {}",
                configs[k].0,
                a.words.len() + a.mems.len(),
                configs[0].0
            ));
        }
    }
    println!(
        "{ticks} ticks: {} agree on {} state words and {} memories",
        configs.map(|c| c.0).join(", "),
        a.words.len(),
        a.mems.len()
    );
    // Batch boundaries.
    let short = (ticks / 20).max(1000);
    let mut want = Sim::with_options(Arc::clone(&program), no_guard(SimOptions::default()));
    let mut got = [
        Sim::with_options(
            Arc::clone(&program),
            SimOptions {
                guard_interpreter: true,
                ..SimOptions::default()
            },
        ),
        Sim::with_options(Arc::clone(&program), SimOptions::jit()),
        // Specialization with an automatic plan chosen early, and the
        // fallback to supersteps on windows of a few batches: modes flip
        // as the wall clock says, at any batch boundary.
        Sim::with_options(
            Arc::clone(&program),
            SimOptions {
                spec: Some(SpecOptions {
                    plan_window: 1 << 12,
                    samples: 64,
                    fallback: Some(FallbackOptions {
                        window: 64,
                        sample: 64,
                        resample: 2,
                        margin: 0,
                        patience: 1,
                        probe_min: 64,
                        probe_max: 1024,
                        min_run: 1,
                    }),
                    ..SpecOptions::interactive()
                }),
                ..SimOptions::jit()
            },
        ),
    ];
    let mut t = 0;
    let mut n = 0u64;
    while t < short {
        // 1 to 7 ticks, and every few batches up to 41 (supersteps).
        n = n * 5 % 7 + 1;
        if t % 5 == 0 {
            n = n * 6 - 1;
        }
        for _ in 0..n {
            want.tick();
        }
        for g in &mut got {
            g.run(n);
        }
        t += n;
        let w = want.export_state();
        for g in &got {
            if g.export_state() != w {
                return Err(format!("a fused batch differs at tick {t}"));
            }
        }
    }
    let st = got[2].spec_stats();
    println!(
        "{short} ticks in batches of 1 to 41: the same state at every boundary \
         (specialized: {} keyed phases, {} hits, {} misses, {} ticks as supersteps, \
         {} fallbacks, {} probes)",
        st.keyed_phases, st.hits, st.misses, st.super_ticks, st.fallbacks, st.probes
    );
    Ok(())
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

/// Two designs from power-on, frame by frame (89,342 ticks, rendering off;
/// the odd-frame skip makes frames one tick shorter, so this is a fixed
/// tick grid): every memory both have must be the same at every boundary.
fn compare_designs(
    a: &logi_sim::netlist::Design,
    b: &logi_sim::netlist::Design,
    ticks: u64,
    presses: &[(u64, String, u64)],
    screen: Option<&str>,
) -> Result<(), String> {
    const FRAME: u64 = 89_342;
    let pa = Arc::new(compile(a, &base_options()).map_err(|e| format!("{e:?}"))?);
    let pb = Arc::new(compile(b, &base_options()).map_err(|e| format!("{e:?}"))?);
    // The Screen is not a state element; it is the one writable memory of
    // 2^16 8-bit words (256 x 256, Rgb332) in the console.
    let screen_of = |p: &Program| -> Option<u32> {
        let mut found = p
            .mems
            .iter()
            .enumerate()
            .filter(|(_, m)| m.writable && m.addr_bits == 16 && m.width == 8);
        match (found.next(), found.next()) {
            (Some((i, _)), None) => Some(i as u32),
            _ => None,
        }
    };
    let (screen_a, screen_b) = (screen_of(&pa), screen_of(&pb));
    let mut sa = Sim::with_options(pa, SimOptions::default());
    let mut sb = Sim::with_options(pb, SimOptions::default());
    let frames = ticks.div_ceil(FRAME);
    let mut shared = 0;
    for f in 1..=frames {
        // An input is held while any of its presses is on.
        let mut held: std::collections::BTreeMap<&str, bool> = std::collections::BTreeMap::new();
        for (from, input, count) in presses {
            *held.entry(input.as_str()).or_default() |= (*from..from + count).contains(&f);
        }
        for (input, on) in held {
            sa.set(input, u64::from(on));
            sb.set(input, u64::from(on));
        }
        sa.run(FRAME);
        sb.run(FRAME);
        let (ma, mb) = (sa.export_state().mems, sb.export_state().mems);
        shared = 0;
        if let (Some(x), Some(y)) = (screen_a, screen_b) {
            shared += 1;
            if sa.mem(x) != sb.mem(y) {
                let (ma, mb) = (sa.mem(x), sb.mem(y));
                let diff: Vec<usize> = (0..ma.len()).filter(|&i| ma[i] != mb[i]).collect();
                let (xs, ys): (Vec<usize>, Vec<usize>) =
                    diff.iter().map(|i| (i % 256, i / 256)).unzip();
                if let Some(file) = screen {
                    let bytes = |m: &[u64]| m.iter().map(|w| *w as u8).collect::<Vec<u8>>();
                    std::fs::write(file, bytes(ma)).map_err(|e| format!("{file}: {e}"))?;
                    std::fs::write(format!("{file}.other"), bytes(mb))
                        .map_err(|e| format!("{file}: {e}"))?;
                }
                return Err(format!(
                    "frame {f}: the screens differ in {} pixels, x {}..={}, y {}..={}",
                    diff.len(),
                    xs.iter().min().unwrap_or(&0),
                    xs.iter().max().unwrap_or(&0),
                    ys.iter().min().unwrap_or(&0),
                    ys.iter().max().unwrap_or(&0)
                ));
            }
        }
        for (path, m) in &ma {
            let Some(n) = mb.get(path) else { continue };
            shared += 1;
            if m != n {
                let first = m.2.iter().zip(&n.2).position(|(x, y)| x != y);
                return Err(format!(
                    "frame {f}: memory {path:?} differs (first at word {first:?})"
                ));
            }
        }
    }
    println!(
        "{frames} frames: the {shared} memories both designs have are identical at every frame"
    );
    if screen_a.is_none() || screen_b.is_none() {
        println!("(no screen found in one design: screens not compared)");
    }
    if let (Some(file), Some(x)) = (screen, screen_a) {
        let bytes: Vec<u8> = sa.mem(x).iter().map(|w| *w as u8).collect();
        std::fs::write(file, bytes).map_err(|e| format!("{file}: {e}"))?;
        println!("screen stored in {file}");
    }
    Ok(())
}

/// `frame:input:frames,...`.
fn parse_presses(text: &str) -> Result<Vec<(u64, String, u64)>, String> {
    text.split(',')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let f: Vec<&str> = p.split(':').collect();
            match f.as_slice() {
                [from, input, count] => Ok((
                    from.parse()
                        .map_err(|_| format!("--press {p}: bad frame"))?,
                    input.to_string(),
                    count
                        .parse()
                        .map_err(|_| format!("--press {p}: bad count"))?,
                )),
                _ => Err(format!("--press {p}: expected frame:input:frames")),
            }
        })
        .collect()
}

/// `--live`: what to run and sample (see the header).
struct Live {
    frames: u64,
    skip: u64,
    every: u64,
    presses: Vec<(u64, String, u64)>,
    /// `--dead-ops <prefix>`: list the dead ops of blocks under it.
    dead_ops: Option<String>,
}

/// Ops of the compiled design attributed to instance paths (see `--attribute`).
fn attribute_ops(
    ws: &gate_quest::sim::WorkspaceDocument,
    depth: usize,
    // Some(""): list the ops that write no authored net; Some(block): the
    // ops writing that block's nets (a path prefix).
    list: Option<String>,
    live: Option<&Live>,
) -> Result<(), String> {
    use std::collections::BTreeMap;
    let (design, map) = gate_quest::sim::netlist::design_map_of(ws)?;
    // (definition, node) -> the part's label in the workspace.
    let mut labels: std::collections::HashMap<
        (logi_sim::netlist::DefId, logi_sim::netlist::NodeId),
        String,
    > = std::collections::HashMap::new();
    let circuits = std::iter::once(&ws.root).chain(ws.definitions.values().map(|d| &**d));
    for c in circuits {
        let (Some(&def), Some(nodes)) = (map.defs.get(&c.id), map.nodes.get(&c.id)) else {
            continue;
        };
        for n in &c.nodes {
            if let (Some(&node), Some(l)) = (nodes.get(&n.id), &n.label) {
                labels.insert((def, node), l.clone());
            }
        }
    }
    let p = compile(&design, &base_options()).map_err(|e| format!("{e:?}"))?;
    let dm = &p.debug;
    // Label path of every instance, cut to `depth` levels.
    let label = |inst: u32| -> Vec<String> {
        let mut names = Vec::new();
        let mut i = inst;
        while let Some((parent, node)) = dm.instances[i as usize].parent {
            let def = dm.instances[parent as usize].def;
            names.push(
                labels
                    .get(&(def, node))
                    .cloned()
                    .unwrap_or_else(|| "?".into()),
            );
            i = parent;
        }
        names.reverse();
        names
    };
    let paths: Vec<Vec<String>> = (0..dm.instances.len() as u32).map(label).collect();
    // The deepest instance that names each slot, and that net.
    let mut owner: std::collections::HashMap<u32, u32> = std::collections::HashMap::new();
    let mut net_of: std::collections::HashMap<u32, logi_sim::netlist::NetId> =
        std::collections::HashMap::new();
    for (inst, nets) in dm.net_slots.iter().enumerate() {
        for (net, &slot) in nets {
            let deeper = owner
                .get(&slot)
                .is_none_or(|&o| paths[o as usize].len() < paths[inst].len());
            if deeper {
                owner.insert(slot, inst as u32);
                net_of.insert(slot, net);
            }
        }
    }
    // "path/part.pin" of a slot's net: the part that drives it.
    let slot_name = |slot: u32| -> Option<String> {
        let inst = *owner.get(&slot)?;
        let net = *net_of.get(&slot)?;
        let def_id = dm.instances[inst as usize].def;
        let def = design.def(def_id).ok()?;
        let (node, pin) = def.nets.get(net)?.driver;
        let part = labels
            .get(&(def_id, node))
            .cloned()
            .unwrap_or_else(|| "?".into());
        let path = paths[inst as usize].join("/");
        let pin = if pin == 0 {
            String::new()
        } else {
            format!(".{pin}")
        };
        Some(if path.is_empty() {
            format!("{part}{pin}")
        } else {
            format!("{path}/{part}{pin}")
        })
    };
    let Some(phases) = &p.phases else {
        return Err("no clock-phase variants: this mode counts the variants' ops".into());
    };
    let key_of = |dst: u32| -> String {
        match owner.get(&dst) {
            Some(&o) => {
                let path = &paths[o as usize];
                if path.is_empty() {
                    "(root)".to_string()
                } else {
                    path[..path.len().min(depth)].join("/")
                }
            }
            None => "(no authored net)".to_string(),
        }
    };
    // Ops of `body` per block: (ops, ops inside guarded ranges).
    let count = |body: &logi_sim::compile::Body| -> BTreeMap<String, (u32, u32)> {
        let mut by: BTreeMap<String, (u32, u32)> = BTreeMap::new();
        for (bi, block) in body.blocks.iter().enumerate() {
            let guarded = body
                .guards
                .iter()
                .any(|g| (g.start..g.end).contains(&(bi as u32)));
            let dsts: Vec<u32> = if block.packed {
                vec![u32::MAX; block.len as usize]
            } else {
                body.instrs[block.start as usize..(block.start + block.len) as usize]
                    .iter()
                    .map(|i| i.dst)
                    .collect()
            };
            for dst in dsts {
                let e = by.entry(key_of(dst)).or_default();
                e.0 += 1;
                e.1 += u32::from(guarded);
            }
        }
        by
    };
    // As the Sim builds them: a full settle runs the variant; a fused tick
    // (every tick of a run(n) batch but the last) runs the variant pruned
    // to its commits' cone, then guarded.
    let full: Vec<_> = phases.variants.iter().map(count).collect();
    let fused_bodies: Vec<logi_sim::compile::Body> = phases
        .variants
        .iter()
        .map(|v| {
            let pruned = logi_sim::compile::phases::prune(&p, v).unwrap_or_else(|| v.clone());
            logi_sim::compile::guard::guard_with(
                p.slot_count as usize,
                &pruned,
                None,
                logi_sim::compile::guard::MIN_REGION,
            )
            .unwrap_or(pruned)
        })
        .collect();
    if let Some(block) = list {
        // A real slot no net names may hold named nets as views (bits of a
        // lifted or merged word): name it by them, low bits first.
        let mut parts: std::collections::HashMap<u32, Vec<(u8, String)>> =
            std::collections::HashMap::new();
        for &named in owner.keys() {
            if let Some((real, shift, mask)) = p.resolve(named)
                && real != named
                && !owner.contains_key(&real)
            {
                let w = mask.count_ones();
                let n = slot_name(named).unwrap_or_else(|| format!("#{named}"));
                parts
                    .entry(real)
                    .or_default()
                    .push((shift, format!("{n}[{shift}+{w}]")));
            }
        }
        let word_name = |slot: u32| -> Option<String> {
            let mut v = parts.get(&slot)?.clone();
            v.sort();
            v.dedup_by(|a, b| a.0 == b.0);
            Some(format!(
                "{{{}}}",
                v.into_iter().map(|(_, n)| n).collect::<Vec<_>>().join(", ")
            ))
        };
        let full_name = |slot: u32| slot_name(slot).or_else(|| word_name(slot));
        let wanted = |dst: u32| -> bool {
            match owner.get(&dst) {
                None => block.is_empty(),
                Some(&o) => {
                    !block.is_empty() && paths[o as usize].join("/").starts_with(block.as_str())
                }
            }
        };
        return list_ops(&fused_bodies[0], &wanted, &full_name);
    }
    // `--live`: run the console to `skip` frames, then tick by tick,
    // evaluating each fused body's guards on the live slots (executed ops
    // per block and frame region) and, every `every` ticks, which executed
    // ops compute a value no live op reads.
    if let Some(live) = live {
        let Live {
            frames,
            skip,
            every,
            ref presses,
            ref dead_ops,
        } = *live;
        const FRAME: u64 = 89_342;
        let mut sim = Sim::with_options(Arc::new(p.clone()), SimOptions::default());
        // An input is held while any of its presses is on.
        let press_at = |sim: &mut Sim, f: u64| {
            let mut held: BTreeMap<&str, bool> = BTreeMap::new();
            for (from, name, len) in presses {
                *held.entry(name.as_str()).or_default() |= (*from..from + len).contains(&f);
            }
            for (name, on) in held {
                sim.set(name, u64::from(on));
            }
        };
        for f in 1..=skip {
            press_at(&mut sim, f);
            sim.run(FRAME);
        }
        // The dot counter: a named slot stepping by 1 per tick, wrapping
        // after 340; the line counter: one that steps when it wraps.
        // Candidates in a fixed order, the timing block's first (other
        // slots can step in lockstep, e.g. the line plus one).
        // Found on the running console, then put back: measurement starts
        // at frame `skip + 1`, with that frame's presses.
        let at_skip = sim.export_state();
        let mut cands: Vec<u32> = owner.keys().copied().collect();
        cands.sort_by_key(|&s| (!slot_name(s).is_some_and(|n| n.starts_with("timing/")), s));
        let mut hist: Vec<Vec<u64>> = vec![Vec::new(); cands.len()];
        for _ in 0..1400 {
            sim.tick();
            for (i, &s) in cands.iter().enumerate() {
                hist[i].push(sim.read(s));
            }
        }
        let dot_i = (0..cands.len())
            .find(|&i| {
                let h = &hist[i];
                // (339 → 0 is the odd frame's short pre-render line)
                h.iter().max() == Some(&340)
                    && h.windows(2)
                        .all(|w| w[1] == w[0] + 1 || (w[0] >= 339 && w[1] == 0))
            })
            .ok_or("no dot counter")?;
        let wraps: Vec<usize> = (1..1400).filter(|&k| hist[dot_i][k] == 0).collect();
        let line_cands: Vec<usize> = (0..cands.len())
            .filter(|&i| {
                let h = &hist[i];
                *h.iter().max().unwrap_or(&999) < 262
                    && wraps.iter().all(|&k| h[k] == (h[k - 1] + 1) % 262)
                    && (1..1400).all(|k| wraps.contains(&k) || h[k] == h[k - 1])
            })
            .collect();
        let mut seen: Vec<std::collections::BTreeSet<u64>> =
            vec![Default::default(); line_cands.len()];
        for _ in 0..FRAME {
            sim.tick();
            for (j, &i) in line_cands.iter().enumerate() {
                seen[j].insert(sim.read(cands[i]));
            }
        }
        let line_i = line_cands
            .iter()
            .zip(&seen)
            .find(|(_, s)| s.len() == 262)
            .map(|(&i, _)| i)
            .ok_or("no line counter")?;
        let (dot, line) = (cands[dot_i], cands[line_i]);
        sim.import_state(&at_skip);
        eprintln!(
            "dot {dot} ({:?}), line {line} ({:?})",
            slot_name(dot),
            slot_name(line)
        );
        // The PPU's mask register (rendering is off when bits 3 and 4 are).
        let mask = cands
            .iter()
            .copied()
            .find(|&s| slot_name(s).is_some_and(|n| n.ends_with("regs/mask_reg/dffe")));
        // Per variant: for each block, (key index, ops, guard index).
        let mut key_ix: BTreeMap<String, usize> = BTreeMap::new();
        let mut plan: Vec<Vec<(usize, u32, Option<usize>)>> = Vec::new();
        for body in &fused_bodies {
            let mut v = Vec::new();
            for (bi, block) in body.blocks.iter().enumerate() {
                let g = body
                    .guards
                    .iter()
                    .position(|g| (g.start..g.end).contains(&(bi as u32)));
                let dsts: Vec<u32> = if block.packed {
                    vec![u32::MAX; block.len as usize]
                } else {
                    body.instrs[block.start as usize..(block.start + block.len) as usize]
                        .iter()
                        .map(|i| i.dst)
                        .collect()
                };
                let mut per: BTreeMap<usize, u32> = BTreeMap::new();
                for d in dsts {
                    let n = key_ix.len();
                    let k = *key_ix.entry(key_of(d)).or_insert(n);
                    *per.entry(k).or_default() += 1;
                }
                for (k, n) in per {
                    v.push((k, n, g));
                }
            }
            // Commits count as ops (as `Body::ops` does): a copy of the
            // block owning the state slot, a memory write or clear of the
            // block owning its enable (or bit mask).
            for c in &body.commits {
                use logi_sim::compile::Commit;
                let at = match *c {
                    Commit::Copy { state, .. } => state,
                    Commit::RamWrite { we, .. } => we,
                    Commit::RamWriteMasked { mask, .. } => mask,
                    Commit::MemClear { en, .. } => en,
                };
                let n = key_ix.len();
                let k = *key_ix.entry(key_of(at)).or_insert(n);
                v.push((k, 1, None));
            }
            plan.push(v);
        }
        let names: Vec<String> = {
            let mut v = vec![String::new(); key_ix.len()];
            for (k, &i) in &key_ix {
                v[i] = k.clone();
            }
            v
        };
        const REGIONS: [&str; 7] = [
            "visible dot 0",
            "visible 1-64",
            "visible 65-256",
            "visible 257-320",
            "visible 321-340",
            "line 240 / pre-render 261",
            "vblank 241-260",
        ];
        let region = |l: u64, d: u64| -> usize {
            if l < 240 {
                match d {
                    0 => 0,
                    1..=64 => 1,
                    65..=256 => 2,
                    257..=320 => 3,
                    _ => 4,
                }
            } else if l == 240 || l == 261 {
                5
            } else {
                6
            }
        };
        let nk = names.len();
        let mut exec = vec![[0u64; 7]; nk];
        let mut static_ops = vec![0u64; nk];
        let mut guarded = vec![0u64; nk];
        let mut reg_ticks = [0u64; 7];
        let mut off_ticks = 0u64;
        let mut off_exec = vec![0u64; nk];
        let period = phases.variants.len() as u64;
        // Dynamic liveness (every `every`-th tick): from the roots (commits,
        // guard conditions, outputs), back through the body, following only
        // the operands the current values select.
        let n_slots = p.slot_count as usize;
        let real = |s: u32| -> u32 {
            if (s as usize) < n_slots {
                s
            } else {
                p.resolve(s).map_or(s, |(r, _, _)| r)
            }
        };
        // Timing-only ops (statically): every slot operand is a constant,
        // the dot or line word, or another timing-only result.
        let timing_roots: std::collections::HashSet<u32> =
            [real(dot), real(line)].into_iter().collect();
        let consts: std::collections::HashSet<u32> =
            p.consts.iter().map(|&(s, _)| s).chain([0]).collect();
        let mut timing_only = vec![0u64; names.len()];
        let mut timing_frontier = 0u32;
        {
            let body = &fused_bodies[0];
            // A read of a memory something writes depends on its contents,
            // not only its address: never timing-only.
            let written: std::collections::HashSet<u32> = fused_bodies
                .iter()
                .flat_map(|b| &b.commits)
                .filter_map(|c| match *c {
                    logi_sim::compile::Commit::RamWrite { mem, .. }
                    | logi_sim::compile::Commit::RamWriteMasked { mem, .. }
                    | logi_sim::compile::Commit::MemClear { mem, .. } => Some(mem),
                    logi_sim::compile::Commit::Copy { .. } => None,
                })
                .collect();
            let mut t: std::collections::HashSet<u32> = std::collections::HashSet::new();
            let mut readers: std::collections::HashMap<u32, Vec<bool>> = Default::default();
            for block in &body.blocks {
                if block.packed {
                    continue;
                }
                let (ua, ub, uc) = block.op.slot_operands();
                for k in block.start..block.start + block.len {
                    let i = body.instrs[k as usize];
                    let ops: Vec<u32> = [(ua, i.a), (ub, i.b), (uc, i.c)]
                        .iter()
                        .filter(|(u, _)| *u)
                        .map(|(_, s)| real(*s))
                        .collect();
                    let only = !ops.is_empty()
                        && !(block.op == logi_sim::compile::Op::MemRead && written.contains(&i.b))
                        && ops.iter().all(|s| {
                            timing_roots.contains(s) || consts.contains(s) || t.contains(s)
                        });
                    for s in &ops {
                        readers.entry(*s).or_default().push(only);
                    }
                    if only {
                        t.insert(i.dst);
                        timing_only[key_ix[&key_of(i.dst)]] += 1;
                    }
                }
            }
            // Frontier: timing-only results read by an op that is not.
            for s in &t {
                if readers.get(s).is_some_and(|r| r.iter().any(|x| !x)) {
                    timing_frontier += 1;
                }
            }
        }
        let mut dead = vec![[0u64; 7]; names.len()];
        let mut dead_off = vec![0u64; names.len()];
        let mut dead_op: std::collections::HashMap<(u32, String), [u64; 7]> = Default::default();
        let mut samples = [0u64; 7];
        let mut samples_off = 0u64;
        let total = frames * FRAME;
        for t in 0..total {
            if t % FRAME == 0 {
                press_at(&mut sim, skip + 1 + t / FRAME);
            }
            sim.tick();
            let v = (sim.tick_count() % period) as usize;
            let body = &fused_bodies[v];
            let holds: Vec<bool> = body
                .guards
                .iter()
                .map(|g| g.conds.iter().all(|c| (sim.read(c.slot) != 0) == c.when))
                .collect();
            let r = region(sim.read(line), sim.read(dot));
            reg_ticks[r] += 1;
            let off = mask.is_some_and(|m| sim.read(m) & 0x18 == 0);
            off_ticks += u64::from(off);
            if t % every == 0 {
                samples[r] += 1;
                samples_off += u64::from(off);
                let mut need = vec![false; n_slots + 1];
                let mark = |s: u32, need: &mut Vec<bool>| {
                    let r = real(s) as usize;
                    if r < need.len() {
                        need[r] = true;
                    }
                };
                for c in &body.commits {
                    match *c {
                        logi_sim::compile::Commit::RamWrite { we, addr, data, .. } => {
                            mark(we, &mut need);
                            if sim.read(we) != 0 {
                                mark(addr, &mut need);
                                mark(data, &mut need);
                            }
                        }
                        logi_sim::compile::Commit::RamWriteMasked {
                            mask, addr, data, ..
                        } => {
                            mark(mask, &mut need);
                            if sim.read(mask) != 0 {
                                mark(addr, &mut need);
                                mark(data, &mut need);
                            }
                        }
                        _ => {
                            for s in c.reads() {
                                mark(s, &mut need);
                            }
                        }
                    }
                }
                // A guard's tests stop at the first that fails.
                for g in &body.guards {
                    for c in &g.conds {
                        mark(c.slot, &mut need);
                        if (sim.read(c.slot) != 0) != c.when {
                            break;
                        }
                    }
                }
                for o in &p.ports.outputs {
                    mark(o.slot, &mut need);
                }
                let gholds: Vec<bool> = body
                    .guards
                    .iter()
                    .map(|g| g.conds.iter().all(|c| (sim.read(c.slot) != 0) == c.when))
                    .collect();
                for (bi, block) in body.blocks.iter().enumerate().rev() {
                    let runs = body
                        .guards
                        .iter()
                        .position(|g| (g.start..g.end).contains(&(bi as u32)))
                        .is_none_or(|gi| gholds[gi]);
                    if block.packed {
                        for k in block.start..block.start + block.len {
                            let pk = &p.packs[k as usize];
                            for op in [pk.a, pk.b, pk.c] {
                                mark(op.base, &mut need);
                                mark(op.splat, &mut need);
                                for gth in
                                    &p.gathers[op.gather_start as usize..][..op.gather_len as usize]
                                {
                                    mark(gth.slot, &mut need);
                                }
                            }
                        }
                        continue;
                    }
                    let (ua, ub, uc) = block.op.slot_operands();
                    for k in (block.start..block.start + block.len).rev() {
                        let i = body.instrs[k as usize];
                        // A skipped op neither counts nor makes its
                        // operands needed: its slot keeps an older value.
                        if !runs {
                            continue;
                        }
                        if !need[i.dst as usize] {
                            let key = key_ix[&key_of(i.dst)];
                            dead[key][r] += 1;
                            let e = dead_op
                                .entry((i.dst, format!("{:?}", block.op)))
                                .or_insert([0u64; 7]);
                            e[r] += 1;
                            if off {
                                dead_off[key] += 1;
                            }
                            continue;
                        }
                        match block.op {
                            logi_sim::compile::Op::Mux => {
                                mark(i.a, &mut need);
                                mark(if sim.read(i.a) != 0 { i.c } else { i.b }, &mut need);
                            }
                            logi_sim::compile::Op::Switch => {
                                mark(i.a, &mut need);
                                if sim.read(i.a) != 0 {
                                    mark(i.b, &mut need);
                                }
                            }
                            logi_sim::compile::Op::And if sim.read(i.a) == 0 => {
                                mark(i.a, &mut need)
                            }
                            logi_sim::compile::Op::And if sim.read(i.b) == 0 => {
                                mark(i.b, &mut need)
                            }
                            _ => {
                                if ua {
                                    mark(i.a, &mut need);
                                }
                                if ub {
                                    mark(i.b, &mut need);
                                }
                                if uc {
                                    mark(i.c, &mut need);
                                }
                            }
                        }
                    }
                }
            }
            for &(k, n, g) in &plan[v] {
                static_ops[k] += u64::from(n);
                if g.is_some() {
                    guarded[k] += u64::from(n);
                }
                if g.is_none_or(|g| holds[g]) {
                    exec[k][r] += u64::from(n);
                    if off {
                        off_exec[k] += u64::from(n);
                    }
                }
            }
        }
        let s_all: u64 = samples.iter().sum();
        let dead_all: u64 = dead.iter().map(|d| d.iter().sum::<u64>()).sum();
        println!(
            "LIVENESS ({s_all} sampled ticks): {:.1} ops per tick compute a value no live op reads (executed instructions only: guard-skipped ones excluded); timing-only ops (fold with dot/line keys): {}, frontier values {timing_frontier}",
            dead_all as f64 / s_all as f64,
            timing_only.iter().sum::<u64>()
        );
        let mut dorder: Vec<usize> = (0..nk).collect();
        dorder.sort_by_key(|&k| std::cmp::Reverse(dead[k].iter().sum::<u64>()));
        println!(
            "| block | dead ops/tick | {} | rendering off | timing-only ops |",
            REGIONS.join(" | ")
        );
        for &k in &dorder {
            let d: u64 = dead[k].iter().sum();
            if d == 0 && timing_only[k] == 0 {
                continue;
            }
            let per: Vec<String> = (0..7)
                .map(|r| format!("{:.1}", dead[k][r] as f64 / samples[r].max(1) as f64))
                .collect();
            println!(
                "| {} | {:.1} | {} | {} | {} |",
                names[k],
                d as f64 / s_all as f64,
                per.join(" | "),
                if samples_off > 0 {
                    format!("{:.1}", dead_off[k] as f64 / samples_off as f64)
                } else {
                    "-".into()
                },
                timing_only[k]
            );
        }
        if let Some(prefix) = dead_ops {
            let mut v: Vec<(&(u32, String), &[u64; 7])> = dead_op
                .iter()
                .filter(|((d, _), _)| {
                    key_of(*d).starts_with(prefix.as_str())
                        || slot_name(*d).is_some_and(|n| n.starts_with(prefix.as_str()))
                })
                .collect();
            v.sort_by_key(|(_, c)| std::cmp::Reverse(c.iter().sum::<u64>()));
            println!(
                "| op | writes | dead share of sampled ticks | {} |",
                REGIONS.join(" | ")
            );
            for ((d, op), c) in v.iter().take(120) {
                let per: Vec<String> = (0..7)
                    .map(|r| format!("{:.0}%", 100.0 * c[r] as f64 / samples[r].max(1) as f64))
                    .collect();
                println!(
                    "| {op} | {} | {:.0}% | {} |",
                    slot_name(*d).unwrap_or_else(|| format!("#{d}")),
                    100.0 * c.iter().sum::<u64>() as f64 / s_all as f64,
                    per.join(" | ")
                );
            }
        }
        let mut order: Vec<usize> = (0..nk).collect();
        let tot = |k: usize| exec[k].iter().sum::<u64>();
        order.sort_by_key(|&k| std::cmp::Reverse(tot(k)));
        let all: u64 = (0..nk).map(tot).sum();
        println!(
            "{frames} frames from frame {skip}, {total} ticks: {:.1} ops executed per tick (fused bodies, guards on live slots), {:.1} in the fused bodies; rendering off {:.1}% of ticks",
            all as f64 / total as f64,
            static_ops.iter().sum::<u64>() as f64 / total as f64,
            100.0 * off_ticks as f64 / total as f64
        );
        println!(
            "region share of ticks: {}",
            REGIONS
                .iter()
                .zip(reg_ticks)
                .map(|(n, c)| format!("{n} {:.1}%", 100.0 * c as f64 / total as f64))
                .collect::<Vec<_>>()
                .join(", ")
        );
        println!(
            "| block | executed/tick | share | in fused body/tick | guarded/tick | guarded skipped | {} | rendering off |",
            REGIONS.join(" | ")
        );
        for &k in &order {
            let e = tot(k);
            if e == 0 && static_ops[k] == 0 {
                continue;
            }
            let g = guarded[k];
            let skipped = static_ops[k] - e;
            let per_region: Vec<String> = (0..7)
                .map(|r| format!("{:.1}", exec[k][r] as f64 / reg_ticks[r].max(1) as f64))
                .collect();
            println!(
                "| {} | {:.1} | {:.1}% | {:.1} | {:.1} | {} | {} | {} |",
                names[k],
                e as f64 / total as f64,
                100.0 * e as f64 / all as f64,
                static_ops[k] as f64 / total as f64,
                g as f64 / total as f64,
                if g > 0 {
                    format!("{:.0}%", 100.0 * skipped as f64 / g as f64)
                } else {
                    "-".into()
                },
                per_region.join(" | "),
                if off_ticks > 0 {
                    format!("{:.1}", off_exec[k] as f64 / off_ticks as f64)
                } else {
                    "-".into()
                }
            );
        }
        return Ok(());
    }
    let fused: Vec<_> = fused_bodies.iter().map(count).collect();
    let mut keys: Vec<String> = full
        .iter()
        .chain(&fused)
        .flat_map(|m| m.keys().cloned())
        .collect();
    keys.sort();
    keys.dedup();
    let get = |m: &BTreeMap<String, (u32, u32)>, k: &str| m.get(k).copied().unwrap_or((0, 0));
    keys.sort_by_key(|k| std::cmp::Reverse(get(&full[0], k).0));
    let sum = |ms: &[BTreeMap<String, (u32, u32)>]| -> Vec<u32> {
        ms.iter().map(|m| m.values().map(|v| v.0).sum()).collect()
    };
    println!(
        "full settle: {:?} ops per variant; fused tick: {:?} ops per variant (phase where the CPU's ce is 1 is the one that differs)",
        sum(&full),
        sum(&fused)
    );
    println!(
        "  {:32} {:>6}  {:>22}  {:>22}",
        "block", "full", "fused ops per phase", "of them guarded"
    );
    for k in keys {
        let f = get(&full[0], &k).0;
        let fu: Vec<u32> = fused.iter().map(|m| get(m, &k).0).collect();
        let gu: Vec<u32> = fused.iter().map(|m| get(m, &k).1).collect();
        if f == 0 && fu.iter().all(|&x| x == 0) {
            continue;
        }
        println!(
            "  {k:32} {f:6}  {:>22}  {:>22}",
            format!("{fu:?}"),
            format!("{gu:?}")
        );
    }
    Ok(())
}

/// Every op of `body` whose result `wanted` selects: op, width, operands
/// (named where they are authored nets), and what reads the result.
fn list_ops(
    body: &logi_sim::compile::Body,
    wanted: &dyn Fn(u32) -> bool,
    slot_name: &dyn Fn(u32) -> Option<String>,
) -> Result<(), String> {
    use std::collections::BTreeMap;
    // Each instruction's index, op and the block it sits in.
    let mut ops: Vec<(usize, logi_sim::compile::Op)> = Vec::new();
    for block in &body.blocks {
        if block.packed {
            continue;
        }
        for k in block.start..block.start + block.len {
            ops.push((k as usize, block.op));
        }
    }
    // Readers of each slot: instructions (by index) and commits.
    let mut readers: std::collections::HashMap<u32, Vec<String>> = std::collections::HashMap::new();
    let name_or = |slot: u32| slot_name(slot).unwrap_or_else(|| format!("#{slot}"));
    for &(k, op) in &ops {
        let i = body.instrs[k];
        let (sa, sb, sc) = op.slot_operands();
        for (is, slot) in [(sa, i.a), (sb, i.b), (sc, i.c)] {
            if is {
                readers
                    .entry(slot)
                    .or_default()
                    .push(format!("{op:?} -> {}", name_or(i.dst)));
            }
        }
    }
    for c in &body.commits {
        for slot in c.reads() {
            readers.entry(slot).or_default().push("commit".into());
        }
    }
    let mut by_op: BTreeMap<String, usize> = BTreeMap::new();
    let mut lines = Vec::new();
    for &(k, op) in &ops {
        let i = body.instrs[k];
        if !wanted(i.dst) {
            continue;
        }
        *by_op.entry(format!("{op:?}")).or_default() += 1;
        let imm = body.imms.get(k).copied().unwrap_or(0);
        let width = if imm != 0 && (imm & imm.wrapping_add(1)) == 0 {
            format!("w{}", imm.count_ones())
        } else {
            "w?".into()
        };
        let (sa, sb, sc) = op.slot_operands();
        let mut args = Vec::new();
        for (k, (is, slot, lit)) in [(sa, i.a, i.a), (sb, i.b, i.b), (sc, i.c, i.c)]
            .into_iter()
            .enumerate()
        {
            if is {
                args.push(name_or(slot));
            } else if matches!(
                op,
                logi_sim::compile::Op::Shr | logi_sim::compile::Op::Concat
            ) && lit != 0
            {
                args.push(format!("<<{lit}"));
            } else if op == logi_sim::compile::Op::Shl && lit != 0 {
                // `((a >> c) << b) & imm`.
                let dir = if k == 1 { "<<" } else { ">>" };
                args.push(format!("{dir}{lit}"));
            }
        }
        let used = readers
            .get(&i.dst)
            .map(|r| r.join("; "))
            .unwrap_or_else(|| "nothing".into());
        lines.push(format!(
            "#{:<5} {op:?} {width}  ({})  -> read by: {used}",
            i.dst,
            args.join(", ")
        ));
    }
    println!("{} ops (fused tick, CPU idle phase):", lines.len());
    for (op, n) in &by_op {
        println!("  {op}: {n}");
    }
    println!();
    for l in lines {
        println!("{l}");
    }
    Ok(())
}
