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
    let executed = args.iter().any(|a| a == "--executed");
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
    args.retain(|a| a != "--compare" && a != "--blocks" && a != "--executed");
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
    if blocks {
        return block_costs(&ws);
    }
    #[cfg(not(feature = "counters"))]
    if executed {
        return Err("--executed needs the counters: build with --features counters".into());
    }
    #[cfg(feature = "counters")]
    if executed {
        let program =
            Arc::new(compile(&design, &Options::default()).map_err(|e| format!("{e:?}"))?);
        for (what, guard) in [("guards on", true), ("guards off", false)] {
            let mut sim = Sim::with_options(
                Arc::clone(&program),
                SimOptions {
                    guard,
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
                "{what:10}: {:.1} instructions executed per tick ({} ticks)",
                c.instrs as f64 / ticks as f64,
                ticks
            );
        }
        return Ok(());
    }
    if let Some(depth) = attribute {
        return attribute_ops(&ws, depth);
    }
    if let Some(other) = against {
        let other = design_of(&suite::read_workspace(Path::new(&other))?)?;
        return compare_designs(&design, &other, ticks, &presses, screen.as_deref());
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
        let runs: [(&str, SimOptions, bool); 13] = [
            ("JIT: run(n), variants + fusion", jit.clone(), true),
            (
                "JIT: run(n), no superstep",
                SimOptions {
                    superstep: false,
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
            ("run(n), no guards", no_guard(&SimOptions::default()), true),
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
                "tick() loop, no guards",
                no_guard(&SimOptions::default()),
                false,
            ),
        ];
        for (what, sim_options, batch) in runs {
            if only.as_ref().is_some_and(|o| !what.contains(o.as_str())) {
                continue;
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
fn compare_jit(design: &logi_sim::netlist::Design, ticks: u64) -> Result<(), String> {
    let program = Arc::new(compile(design, &Options::default()).map_err(|e| format!("{e:?}"))?);
    let no_guard = |o: SimOptions| SimOptions { guard: false, ..o };
    let configs = [
        ("interpreter, no guards", no_guard(SimOptions::default())),
        ("interpreter", SimOptions::default()),
        ("JIT", SimOptions::jit()),
        ("JIT, no guards", no_guard(SimOptions::jit())),
        (
            "JIT, no superstep",
            SimOptions {
                superstep: false,
                ..SimOptions::jit()
            },
        ),
    ];
    let mut sims: Vec<Sim> = configs
        .iter()
        .map(|(_, o)| Sim::with_options(Arc::clone(&program), o.clone()))
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
        Sim::with_options(Arc::clone(&program), SimOptions::default()),
        Sim::with_options(Arc::clone(&program), SimOptions::jit()),
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
    println!("{short} ticks in batches of 1 to 41: the same state at every boundary");
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
    let pa = Arc::new(compile(a, &Options::default()).map_err(|e| format!("{e:?}"))?);
    let pb = Arc::new(compile(b, &Options::default()).map_err(|e| format!("{e:?}"))?);
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

/// Ops of the compiled design attributed to instance paths (see `--attribute`).
fn attribute_ops(ws: &gate_quest::sim::WorkspaceDocument, depth: usize) -> Result<(), String> {
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
    let p = compile(&design, &Options::default()).map_err(|e| format!("{e:?}"))?;
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
    // The deepest instance that names each slot.
    let mut owner: std::collections::HashMap<u32, u32> = std::collections::HashMap::new();
    for (inst, nets) in dm.net_slots.iter().enumerate() {
        for (_, &slot) in nets {
            let deeper = owner
                .get(&slot)
                .is_none_or(|&o| paths[o as usize].len() < paths[inst].len());
            if deeper {
                owner.insert(slot, inst as u32);
            }
        }
    }
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
    let fused: Vec<_> = phases
        .variants
        .iter()
        .map(|v| {
            let pruned = logi_sim::compile::phases::prune(&p, v).unwrap_or_else(|| v.clone());
            let guarded = logi_sim::compile::guard::guard_with(
                p.slot_count as usize,
                &pruned,
                None,
                logi_sim::compile::guard::MIN_REGION,
            )
            .unwrap_or(pruned);
            count(&guarded)
        })
        .collect();
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
