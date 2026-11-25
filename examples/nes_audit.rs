//! Issue #117 experiments: where the executed ops of the NES console come
//! from (kind, width, chip), what each compile pass contributes, and how
//! often each state word changes.
//!
//! ```sh
//! cargo run --release --features jit --example nes_audit -- nes/workspace [ticks] [--ablate] [--state] [--dump <chip>]
//! ```
//!
//! Executed ops are measured, not estimated: the console runs tick by tick
//! (guards off, so every slot is fresh), and after each tick the guards of
//! the fused-run body of that tick's phase are evaluated on the slots, so
//! each instruction is weighted by how often `Sim::run` really executes it.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

#[path = "nes_demo/mod.rs"]
mod nes_demo;

use gate_quest::mcp::suite;
use gate_quest::sim::netlist::design_of;
use logi_sim::compile::program::{Body, Commit, Op};
use logi_sim::compile::{Options, Program, compile, guard, phases};
use logi_sim::netlist::{Design, NodeKind};
use logi_sim::sim::{Sim, SimOptions};

/// Ticks run before measuring (`NES_WARM`, default 1.5M: nestest turns
/// rendering on after about 1.4M ticks).
fn warm() -> u64 {
    std::env::var("NES_WARM").ok().and_then(|v| v.parse().ok()).unwrap_or(1_500_000)
}

fn bitlen(v: u64) -> u8 {
    (64 - v.leading_zeros()) as u8
}

/// Result width of every slot, inferred forward from the ops.
pub fn widths(p: &Program, bodies: &[&Body]) -> Vec<u8> {
    let mut w = vec![0u8; p.slot_count as usize];
    for port in &p.ports.inputs {
        w[port.slot as usize] = port.width;
    }
    for &(s, v) in &p.consts {
        w[s as usize] = bitlen(v).max(1);
    }
    for k in &p.state_keys {
        if let logi_sim::compile::StateLoc::Slot(s) = k.loc
            && let Some((real, shift, _)) = p.resolve(s)
        {
            let e = &mut w[real as usize];
            *e = (*e).max(shift + k.width);
        }
    }
    for _round in 0..3 {
        for body in bodies {
            for b in &body.blocks {
                for k in b.start..b.start + b.len {
                    let i = body.instrs[k as usize];
                    let imm = body.imms[k as usize];
                    let g = |s: u32| w[s as usize];
                    let r = match b.op {
                        Op::Copy => g(i.a),
                        Op::Not | Op::Nand | Op::Nor | Op::Xnor => bitlen(imm),
                        Op::And | Op::Or | Op::Xor => g(i.a).max(g(i.b)),
                        Op::Mux => g(i.b).max(g(i.c)),
                        Op::Switch => g(i.b),
                        Op::AddSum | Op::Shr | Op::Sext => bitlen(imm),
                        Op::Concat => g(i.a).max(i.c as u8 + g(i.b)),
                        Op::MemRead => p.mems[i.b as usize].width,
                        _ => 1,
                    };
                    w[i.dst as usize] = r.max(1);
                }
            }
            for c in &body.commits {
                if let Commit::Copy { state, next } = *c {
                    let n = w[next as usize];
                    let e = &mut w[state as usize];
                    *e = (*e).max(n);
                }
            }
        }
    }
    w
}

/// The kind of an instruction for the audit.
fn kind(op: Op, width: u8, p_imm: u64, w_a: u8, w_b: u8) -> &'static str {
    use Op::*;
    match op {
        Not | And | Or | Xor | Nand | Nor | Xnor | Switch if width == 1 => "bit gate (1-bit logic)",
        Mux if width == 1 => "bit mux (1-bit)",
        Copy => "copy",
        Not | And | Or | Xor | Nand | Nor | Xnor | Switch => "word logic",
        Mux => "word mux",
        Shr if p_imm == 1 => "bit extract (Shr 1-bit)",
        Shr => "slice (Shr)",
        Concat if w_a == 1 || w_b == 1 => "bit concat",
        Concat => "word concat",
        AddSum | AddCarry => "add/carry",
        Eq | LtU => "compare",
        ReduceOr | ReduceAnd | ReduceXor => "reduce",
        MemRead => "memread (RAM/ROM/table)",
        Sext => "sext",
        Clock => "clock",
        SrNext => "srnext",
    }
}

/// Chip label of each slot (the deepest instance whose own logic produces
/// it), two levels deep, then propagated through temporaries.
fn origins(d: &Design, p: &Program, bodies: &[&Body], depth: usize) -> Vec<String> {
    let mut best: Vec<(usize, String)> = vec![(0, String::new()); p.slot_count as usize];
    let label_of = |inst: u32| -> Vec<String> {
        let mut out = Vec::new();
        let mut i = inst;
        while let Some((parent, node)) = p.debug.instances[i as usize].parent {
            let pdef = &d.defs[p.debug.instances[parent as usize].def];
            let child_def = &d.defs[p.debug.instances[i as usize].def];
            out.push(
                pdef.name_of(node)
                    .map_or_else(|| child_def.name.clone(), |n| format!("{n}")),
            );
            i = parent;
        }
        out.reverse();
        out
    };
    for (inst, map) in p.debug.net_slots.iter().enumerate() {
        let def = &d.defs[p.debug.instances[inst].def];
        let lab = label_of(inst as u32);
        for (net, &s) in map {
            let Some((real, _, _)) = p.resolve(s) else {
                continue;
            };
            let (node, _) = def.nets[net].driver;
            let is_port = matches!(def.nodes[node].kind, NodeKind::Input { .. });
            let dep = lab.len() * 2 + usize::from(!is_port);
            let e = &mut best[real as usize];
            if e.1.is_empty() || dep > e.0 {
                let l: Vec<String> = lab.iter().take(depth).cloned().collect();
                *e = (
                    dep,
                    if l.is_empty() {
                        "(root)".into()
                    } else {
                        l.join("/")
                    },
                );
            }
        }
    }
    let mut o: Vec<String> = best.into_iter().map(|x| x.1).collect();
    // Forward from operands, then backward from consumers.
    for _ in 0..4 {
        for body in bodies {
            for b in &body.blocks {
                let (sa, sb, sc) = b.op.slot_operands();
                for k in b.start..b.start + b.len {
                    let i = body.instrs[k as usize];
                    if !o[i.dst as usize].is_empty() {
                        continue;
                    }
                    for (is, s) in [(sa, i.a), (sb, i.b), (sc, i.c)] {
                        if is && s != 0 && !o[s as usize].is_empty() && o[s as usize] != "(root)" {
                            o[i.dst as usize] = o[s as usize].clone();
                            break;
                        }
                    }
                }
            }
            for b in body.blocks.iter().rev() {
                let (sa, sb, sc) = b.op.slot_operands();
                for k in (b.start..b.start + b.len).rev() {
                    let i = body.instrs[k as usize];
                    if o[i.dst as usize].is_empty() {
                        continue;
                    }
                    let from = o[i.dst as usize].clone();
                    for (is, s) in [(sa, i.a), (sb, i.b), (sc, i.c)] {
                        if is && o[s as usize].is_empty() {
                            o[s as usize] = from.clone();
                        }
                    }
                }
            }
        }
    }
    for x in &mut o {
        if x.is_empty() {
            *x = "(temp)".into();
        }
    }
    o
}

/// The fused-run bodies `Sim::run` executes, one per phase, as `Sim`
/// builds them (pruned to the commits' cone, then guarded).
fn fused_bodies(p: &Program, guarded: bool) -> Vec<Body> {
    let n = p.slot_count as usize;
    let variants: Vec<Body> = match &p.phases {
        Some(ph) => ph.variants.clone(),
        None => vec![Body {
            blocks: p.blocks.clone(),
            instrs: p.instrs.clone(),
            imms: p.imms.clone(),
            commits: p.commits.clone(),
            guards: Vec::new(),
        }],
    };
    variants
        .iter()
        .map(|v| {
            let pruned = phases::prune(p, v).unwrap_or_else(|| v.clone());
            if guarded {
                guard::guard_with(n, &pruned, None, guard::MIN_REGION).unwrap_or(pruned)
            } else {
                pruned
            }
        })
        .collect()
}

/// Per body, per instruction: the fraction of that body's ticks it ran.
fn measure(p: &Arc<Program>, bodies: &[Body], ticks: u64) -> Vec<Vec<f64>> {
    let mut sim = Sim::with_options(
        Arc::clone(p),
        SimOptions {
            guard: false,
            ..SimOptions::jit()
        },
    );
    sim.run(warm());
    let period = bodies.len() as u64;
    let mut held: Vec<Vec<u64>> = bodies.iter().map(|b| vec![0; b.guards.len()]).collect();
    let mut seen = vec![0u64; bodies.len()];
    for _ in 0..ticks {
        sim.tick();
        let k = (sim.tick_count() % period) as usize;
        seen[k] += 1;
        for (g, h) in bodies[k].guards.iter().zip(&mut held[k]) {
            if g.conds.iter().all(|c| (sim.read(c.slot) != 0) == c.when) {
                *h += 1;
            }
        }
    }
    bodies
        .iter()
        .enumerate()
        .map(|(k, b)| {
            let mut f = vec![1.0; b.instrs.len()];
            for (g, &h) in b.guards.iter().zip(&held[k]) {
                let r = h as f64 / seen[k].max(1) as f64;
                for blk in &b.blocks[g.start as usize..g.end as usize] {
                    for i in blk.start..blk.start + blk.len {
                        f[i as usize] = r;
                    }
                }
            }
            f
        })
        .collect()
}

fn main() -> Result<(), String> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |args: &mut Vec<String>, f: &str| {
        let had = args.iter().any(|a| a == f);
        args.retain(|a| a != f);
        had
    };
    let ablate = flag(&mut args, "--ablate");
    let state = flag(&mut args, "--state");
    let report = flag(&mut args, "--report");
    let dump = args.iter().position(|a| a == "--dump").map(|k| {
        let v = args[k + 1].clone();
        args.drain(k..k + 2);
        v
    });
    let folder = args.first().ok_or("usage: nes_audit <workspace> [ticks]")?;
    let ticks: u64 = args.get(1).map_or(Ok(200_000), |t| t.parse()).map_err(|e| format!("{e}"))?;
    let ws = suite::read_workspace(Path::new(folder))?;
    let design = design_of(&ws)?;

    if report {
        let mut cache = logi_sim::compile::func::MatchCache::new();
        for (name, matched, note) in logi_sim::compile::func::report(&design, &mut cache) {
            println!("{} {name}: {note}", if matched { "matched  " } else { "unmatched" });
        }
        return Ok(());
    }
    if ablate {
        for (name, o) in [
            ("default", Options::default()),
            ("no tables", Options { tables: false, ..Options::default() }),
            ("no func_match", Options { func_match: false, tables: false, ..Options::default() }),
            ("no lift", Options { lift: false, ..Options::default() }),
            ("no words", Options { words: false, ..Options::default() }),
            ("no lift, no words", Options { lift: false, words: false, ..Options::default() }),
            (
                "no func/lift/words",
                Options { func_match: false, tables: false, lift: false, words: false, ..Options::default() },
            ),
        ] {
            let p = Arc::new(nes_demo::maybe(compile(&design, &o).map_err(|e| format!("{e:?}"))?));
            let fb = fused_bodies(&p, false);
            let stat: f64 = fb.iter().map(|b| b.ops() as f64).sum::<f64>() / fb.len() as f64;
            let fg = fused_bodies(&p, true);
            let f = measure(&p, &fg, ticks.min(100_000));
            let exec: f64 = fg
                .iter()
                .zip(&f)
                .map(|(b, f)| f.iter().sum::<f64>() + b.commits.len() as f64)
                .sum::<f64>()
                / fg.len() as f64;
            println!(
                "{name:22} program {:6} instrs; fused static {stat:7.0} ops/tick; executed {exec:6.0} ops/tick",
                p.instrs.len()
            );
        }
        return Ok(());
    }

    let program = Arc::new(nes_demo::maybe(compile(&design, &Options::default()).map_err(|e| format!("{e:?}"))?));
    let bodies = fused_bodies(&program, true);
    let refs: Vec<&Body> = bodies.iter().collect();
    let w = widths(&program, &refs);
    let org = origins(&design, &program, &refs, 2);
    let org1 = origins(&design, &program, &refs, 1);

    if let Some(chip) = dump {
        // Executed instructions of one chip, phase 0 body, for reading.
        let f = measure(&program, &bodies, ticks);
        for (bk, body) in bodies.iter().enumerate() {
            for b in &body.blocks {
                for k in b.start..b.start + b.len {
                    let i = body.instrs[k as usize];
                    if !org[i.dst as usize].starts_with(chip.as_str()) {
                        continue;
                    }
                    let names: Vec<String> = program
                        .debug
                        .nets_in_slot(i.dst)
                        .into_iter()
                        .take(1)
                        .map(|(path, net)| {
                            let inst = program.debug.instance(&path).unwrap();
                            let def = &design.defs[program.debug.instances[inst as usize].def];
                            let (node, _) = def.nets[net].driver;
                            format!("{}:{}", def.name, def.name_of(node).unwrap_or("?"))
                        })
                        .collect();
                    println!(
                        "ph{bk} r{} {:?} s{}w{} <- s{}w{} s{}w{} s{}w{} imm={:#x} run={:.3} {} [{}]",
                        b.rank,
                        b.op,
                        i.dst,
                        w[i.dst as usize],
                        i.a,
                        w[i.a as usize],
                        i.b,
                        w[i.b as usize],
                        i.c,
                        w[i.c as usize],
                        body.imms[k as usize],
                        f[bk][k as usize],
                        org[i.dst as usize],
                        names.join(",")
                    );
                }
            }
        }
        return Ok(());
    }

    let f = measure(&program, &bodies, ticks);
    let period = bodies.len() as f64;
    let mut by_kind: BTreeMap<&str, (f64, f64)> = BTreeMap::new();
    let mut by_chip: BTreeMap<String, BTreeMap<&str, f64>> = BTreeMap::new();
    let mut by_chip1: BTreeMap<String, (f64, f64)> = BTreeMap::new();
    let mut total = 0.0;
    let mut commits = 0.0;
    for (bk, body) in bodies.iter().enumerate() {
        for b in &body.blocks {
            for k in b.start..b.start + b.len {
                let i = body.instrs[k as usize];
                let wa = w[i.a as usize];
                let wb = w[i.b as usize];
                let kd = kind(b.op, w[i.dst as usize], body.imms[k as usize], wa, wb);
                let x = f[bk][k as usize] / period;
                let e = by_kind.entry(kd).or_default();
                e.0 += x;
                e.1 += 1.0 / period;
                *by_chip
                    .entry(org[i.dst as usize].clone())
                    .or_default()
                    .entry(kd)
                    .or_default() += x;
                let c1 = by_chip1.entry(org1[i.dst as usize].clone()).or_default();
                c1.0 += x;
                if kd.starts_with("bit") {
                    c1.1 += x;
                }
                total += x;
            }
        }
        commits += body.commits.len() as f64 / period;
    }
    println!(
        "phases: {}; executed instructions {total:.0}/tick + commits {commits:.0}/tick = {:.0} ops/tick",
        bodies.len(),
        total + commits
    );
    println!("\nby kind (executed/tick, static fused/tick):");
    let mut v: Vec<_> = by_kind.into_iter().collect();
    v.sort_by(|a, b| b.1.0.total_cmp(&a.1.0));
    for (k, (x, s)) in v {
        println!("  {x:7.1} {:5.1}%  static {s:6.1}  {k}", x / total * 100.0);
    }
    println!("\nby chip (top level): executed/tick, of which 1-bit residue");
    let mut v: Vec<_> = by_chip1.into_iter().collect();
    v.sort_by(|a, b| b.1.0.total_cmp(&a.1.0));
    for (c, (x, bit)) in v {
        println!("  {x:7.1}  bit {bit:6.1}  {c}");
    }
    println!("\nby chip (two levels), kinds:");
    let mut v: Vec<_> = by_chip
        .into_iter()
        .map(|(c, m)| (m.values().sum::<f64>(), c, m))
        .collect();
    v.sort_by(|a, b| b.0.total_cmp(&a.0));
    for (x, c, m) in v.iter().take(40) {
        let mut kinds: Vec<_> = m.iter().collect();
        kinds.sort_by(|a, b| b.1.total_cmp(a.1));
        let s: Vec<String> = kinds
            .iter()
            .filter(|k| *k.1 >= 0.5)
            .map(|(k, n)| format!("{k} {n:.1}"))
            .collect();
        println!("  {x:7.1}  {c}: {}", s.join(", "));
    }

    if state {
        state_changes(&design, &program, &org, ticks);
    }
    Ok(())
}

/// How often each state word changes over `ticks` ticks.
fn state_changes(d: &Design, p: &Arc<Program>, org: &[String], ticks: u64) {
    let _ = d;
    let mut sim = Sim::with_options(Arc::clone(p), SimOptions::jit());
    // Real state slots (commit targets).
    let mut slots: Vec<(u32, u32)> = p
        .commits
        .iter()
        .filter_map(|c| match *c {
            Commit::Copy { state, next } => Some((state, next)),
            _ => None,
        })
        .collect();
    slots.sort();
    slots.dedup();
    let w = widths(p, &[]);
    sim.run(warm());
    let mut prev: Vec<u64> = slots.iter().map(|&(s, _)| sim.read(s)).collect();
    let mut changes = vec![0u64; slots.len()];
    let mut last_change = vec![0u64; slots.len()];
    let warm = 0;
    for t in 0..ticks {
        sim.run(1);
        for (k, &(s, _)) in slots.iter().enumerate() {
            let v = sim.read(s);
            if v != prev[k] {
                if t >= warm {
                    changes[k] += 1;
                    last_change[k] = t;
                }
                prev[k] = v;
            }
        }
    }
    let n = ticks as f64;
    let mut rows: Vec<(f64, usize)> = changes
        .iter()
        .enumerate()
        .map(|(k, &c)| (c as f64 / n, k))
        .collect();
    rows.sort_by(|a, b| a.0.total_cmp(&b.0));
    let buckets = [0.0, 1e-5, 1e-4, 1e-3, 1e-2, 0.1, 1.01];
    println!("\nstate words by change rate (after {warm} ticks), {} words:", slots.len());
    for win in buckets.windows(2) {
        let (lo, hi) = (win[0], win[1]);
        let sel: Vec<_> = rows
            .iter()
            .filter(|r| if lo == 0.0 { r.0 == 0.0 } else { r.0 > lo && r.0 <= hi } || (lo == 0.0 && false))
            .collect();
        let bits: u32 = sel.iter().map(|r| u32::from(w[slots[r.1].0 as usize])).sum();
        println!("  rate {lo:>7} .. {hi:<5}: {:4} words, {bits:5} bits", sel.len());
    }
    let sel: Vec<_> = rows.iter().filter(|r| r.0 > 0.0 && r.0 <= 0.01).collect();
    println!("\nrarely changing (0 < rate <= 1%):");
    for r in sel.iter().take(80) {
        let (s, _) = slots[r.1];
        let names: Vec<String> = p
            .debug
            .nets_in_slot(s)
            .into_iter()
            .take(2)
            .map(|(path, net)| {
                let inst = p.debug.instance(&path).unwrap();
                let def = &d.defs[p.debug.instances[inst as usize].def];
                let (node, _) = def.nets[net].driver;
                format!("{}:{}", def.name, def.name_of(node).unwrap_or("?"))
            })
            .collect();
        println!(
            "  {:9.2e} changes/tick  s{s} w{} {} [{}]",
            r.0,
            w[s as usize],
            org[s as usize],
            names.join(", ")
        );
    }
    let zero: Vec<_> = rows.iter().filter(|r| r.0 == 0.0).collect();
    println!("\nnever changing after warm-up ({}):", zero.len());
    for r in zero.iter().take(60) {
        let (s, _) = slots[r.1];
        println!("  s{s} w{} {} = {:#x}", w[s as usize], org[s as usize], sim.read(s));
    }
}
