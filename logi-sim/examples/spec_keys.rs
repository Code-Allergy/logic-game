//! State-keyed specialization report (issue #117, `docs/adaptive-spec.md`):
//! the key candidates the compile-side analysis finds in each fused-run
//! body of a workspace's design, their widths (whole slot, exact support,
//! fold support), and the operations each would save per tick, from the
//! states a run gives them. Then the key `choose_plan` picks per phase (greedy, pruned), and the
//! prototype's hand-picked keys (`cpu6502/ref_decode/cw` on the CPU
//! phase; `ppu_render_ctl/*`, `ppu_sprites/{fetching,clr_oam_addr,set_ovf}`
//! and `ppu_regs/{mask,ctrl}` on the others), whole and masked, including
//! whether bodies built in one scene still hit in another.
//!
//! ```sh
//! cargo run -p logi-sim --release --example spec_keys -- nes/workspace \
//!     [--demo] [--warm 1500000] [--sample 600000] [--bodies 64] [--top 12] [--min-loss 2] [--check 300]
//!     [--bench 1000000 --rounds 3] [--period-keys 3000000]
//! ```
//!
//! `--bench N` then runs the greedy keys (bodies for the `--bodies` most
//! frequent keys of each phase) in the runtime (`SimOptions::spec`, JIT
//! with the feature `jit`) against the runtime without: `--rounds`
//! alternating rounds of `N` ticks after `--warm`, ticks per second (and
//! executed operations per tick with the feature `counters`), and the
//! state and memories must be equal at the end.
//!
//! `--adaptive N` instead compares the runtime's tables (all JIT-compiled
//! with the feature `jit`): spec off (supersteps), the fixed plan with
//! bodies for the 16 and 64 most frequent keys of the profile (P4), the
//! adaptive tables on the plan's keys with caps of 8, 16 and 32 bodies per
//! phase, and an automatic plan (keys chosen from the `Sim`'s own first
//! window, `--auto-window` fused ticks per phase, `--auto-samples` states
//! per phase) with 16. Adaptive builds run on a thread of their own. Each
//! runs to the end of the profile (`--warm` + `--sample`), then `N` ticks
//! are timed in `--rounds` interleaved rounds, then again from `--later`;
//! state and memories must agree at the end.
//!
//! `--cross-tick N [--trace-len L]` estimates cross-tick constant
//! propagation (a trace of `L` consecutive fused ticks): on `N` sampled
//! traces after the profile, each tick after the first is specialized on
//! its plan key plus the known bits of state the previous ticks' settles
//! and commits imply (forward known bits through the general fused body
//! under the same assumptions), against its plan key alone; executed ops
//! per tick, guarded, on the tick's real state.
//!
//! `--period-keys N` reports how the plan's keys of whole clock periods
//! repeat over `N` ticks after the profile (what supersteps keyed per
//! period could cover; `docs/adaptive-spec.md`, "P8 as built").
//!
//! `--demo` patches a small program into the PRG ROM that renders like a
//! game (sprites and background on, OAM DMA every frame), writing `$1E`
//! to `PPUMASK` (a title screen's value); the transfer check then runs it
//! with `$18` (left-column clipping on, a typical in-game value).
//!
//! Gains are operations (instructions plus commits) per tick of
//! `Sim::run`: the guarded general fused body's executed operations on
//! each sampled state, minus the key prefix's and the guarded specialized
//! body's ("executed"); "static" counts every instruction of the bodies.

mod workspace;

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use logi_sim::compile::guard::{MIN_REGION, guard_with};
use logi_sim::compile::spec::{
    Candidate, KeyClass, KeyField, KeyLayout, Known, PlanOptions, SpecContext, choose_plan,
    possible_bits, transfer,
};
use logi_sim::compile::{Body, Commit, Instr, Op, Options, Program, ZERO_SLOT, compile, phases};
use logi_sim::sim::{PhasePlan, Sim, SimOptions, SpecOptions, SpecPlan};

struct Args {
    folder: String,
    demo: bool,
    warm: u64,
    sample: u64,
    bodies: usize,
    top: usize,
    min_loss: f64,
    check: usize,
    bench: u64,
    rounds: u64,
    adaptive: u64,
    later: u64,
    auto_window: u64,
    auto_samples: usize,
    period_keys: u64,
    cross_tick: u64,
    trace_len: u64,
}

fn args() -> Result<Args, String> {
    let mut it = std::env::args().skip(1);
    let mut a = Args {
        folder: String::new(),
        demo: false,
        warm: 1_500_000,
        sample: 600_000,
        bodies: 64,
        top: 12,
        min_loss: 2.0,
        check: 300,
        bench: 0,
        rounds: 3,
        adaptive: 0,
        later: 20_000_000,
        auto_window: SpecOptions::default().plan_window,
        auto_samples: SpecOptions::default().samples,
        period_keys: 0,
        cross_tick: 0,
        trace_len: 3,
    };
    while let Some(x) = it.next() {
        let mut num = |what: &str| -> Result<u64, String> {
            it.next()
                .and_then(|v| v.parse().ok())
                .ok_or_else(|| format!("{what} needs a number"))
        };
        match x.as_str() {
            "--demo" => a.demo = true,
            "--warm" => a.warm = num("--warm")?,
            "--sample" => a.sample = num("--sample")?,
            "--bodies" => a.bodies = num("--bodies")? as usize,
            "--top" => a.top = num("--top")? as usize,
            "--min-loss" => a.min_loss = num("--min-loss")? as f64,
            "--check" => a.check = num("--check")? as usize,
            "--bench" => a.bench = num("--bench")?,
            "--rounds" => a.rounds = num("--rounds")?,
            "--adaptive" => a.adaptive = num("--adaptive")?,
            "--later" => a.later = num("--later")?,
            "--auto-window" => a.auto_window = num("--auto-window")?,
            "--auto-samples" => a.auto_samples = num("--auto-samples")? as usize,
            "--period-keys" => a.period_keys = num("--period-keys")?,
            "--cross-tick" => a.cross_tick = num("--cross-tick")?,
            "--trace-len" => a.trace_len = num("--trace-len")?,
            f if a.folder.is_empty() => a.folder = f.to_string(),
            other => return Err(format!("unknown argument {other}")),
        }
    }
    if a.folder.is_empty() {
        return Err("usage: spec_keys <workspace folder> [--demo] [--warm N] [--sample N] [--bodies B] [--top K]".into());
    }
    Ok(a)
}

#[rustfmt::skip]
const DEMO: &[u8] = &[
    // C000 reset
    0x78, 0xD8, 0xA2, 0xFF, 0x9A,             // SEI CLD LDX #$FF TXS
    0x2C, 0x02, 0x20, 0x10, 0xFB,             // C005 BIT $2002 BPL C005
    0x2C, 0x02, 0x20, 0x10, 0xFB,             // C00A BIT $2002 BPL C00A
    0xA2, 0x00,                               // C00F LDX #0
    0x8A, 0x4A,                               // C011 TXA LSR
    0x9D, 0x00, 0x02, 0x9D, 0x01, 0x02,       // STA $0200,X STA $0201,X
    0x29, 0x03, 0x9D, 0x02, 0x02,             // AND #3 STA $0202,X
    0x8A, 0x9D, 0x03, 0x02,                   // TXA STA $0203,X
    0xE8, 0xE8, 0xE8, 0xE8, 0xD0, 0xE9,       // INX x4 BNE C011
    0xA9, 0x1E, 0x8D, 0x01, 0x20,             // C028 LDA #$1E STA $2001
    0xA9, 0x80, 0x8D, 0x00, 0x20,             // C02D LDA #$80 STA $2000
    0xA2, 0x00,                               // C032 LDX #0
    0xFE, 0x03, 0x02,                         // C034 INC $0203,X
    0xE8, 0xE8, 0xE8, 0xE8, 0xD0, 0xF7,       // INX x4 BNE C034
    0x4C, 0x32, 0xC0,                         // C03D JMP C032
    // C040 NMI
    0x48, 0xA9, 0x00, 0x8D, 0x03, 0x20,       // PHA LDA #0 STA $2003
    0xA9, 0x02, 0x8D, 0x14, 0x40,             // LDA #2 STA $4014
    0x8D, 0x05, 0x20, 0x8D, 0x05, 0x20,       // STA $2005 STA $2005
    0x68, 0x40,                               // C051 PLA C052 RTI
];

/// `p` with the demo program in its 32 KiB PRG ROM (at $C000, vectors
/// at $FFFA), writing `ppumask` to $2001.
fn demo(p: &Program, ppumask: u8) -> Program {
    let mut p = p.clone();
    let m = p
        .mems
        .iter_mut()
        .find(|m| m.addr_bits == 15 && !m.writable && m.width == 8)
        .expect("a 32 KiB PRG ROM");
    let mut v = m.init.to_vec();
    v.resize(1 << 15, 0);
    for (k, &b) in DEMO.iter().enumerate() {
        v[0x4000 + k] = u64::from(b);
    }
    v[0x4000 + 0x29] = u64::from(ppumask);
    for (k, b) in [0x40u8, 0xC0, 0x00, 0xC0, 0x52, 0xC0].iter().enumerate() {
        v[0x7FFA + k] = u64::from(*b);
    }
    m.init = v.into();
    p
}

/// Names of the nets each real slot holds (`def/def/net`, from the root's
/// children down).
fn slot_names(p: &Program, w: &workspace::Loaded) -> HashMap<u32, Vec<String>> {
    let mut out: HashMap<u32, Vec<String>> = HashMap::new();
    let dm = &p.debug;
    let path_of = |inst: u32| {
        let mut parts = Vec::new();
        let mut i = inst;
        while let Some((parent, _)) = dm.instances[i as usize].parent {
            parts.push(
                w.def_names
                    .get(&dm.instances[i as usize].def)
                    .cloned()
                    .unwrap_or_default(),
            );
            i = parent;
        }
        parts.reverse();
        parts.join("/")
    };
    for (inst, map) in dm.net_slots.iter().enumerate() {
        let def = dm.instances[inst].def;
        let mut path = None;
        for (net, &slot) in map {
            let Some(name) = w.net_names.get(&(def, net)) else {
                continue;
            };
            let Some((real, _, _)) = p.resolve(slot) else {
                continue;
            };
            if real >= p.slot_count {
                continue;
            }
            let path = path.get_or_insert_with(|| path_of(inst as u32));
            let full = if path.is_empty() {
                name.clone()
            } else {
                format!("{path}/{name}")
            };
            out.entry(real).or_default().push(full);
        }
    }
    for v in out.values_mut() {
        v.sort_by_key(|n| (n.len(), n.clone()));
        v.dedup();
    }
    out
}

fn name(names: &HashMap<u32, Vec<String>>, slot: u32) -> String {
    match names.get(&slot) {
        Some(v) if v.len() > 1 => format!("{} (+{})", v[0], v.len() - 1),
        Some(v) => v[0].clone(),
        None => format!("slot {slot}"),
    }
}

/// Executed operations of a guarded body on settled slots: its commits,
/// and every instruction outside a guard or in one whose conditions hold.
fn executed(body: &Body, slots: &[u64]) -> usize {
    let mut n = body.commits.len();
    let mut at = 0usize;
    let len =
        |r: std::ops::Range<usize>| body.blocks[r].iter().map(|b| b.len as usize).sum::<usize>();
    for g in &body.guards {
        n += len(at..g.start as usize);
        if g.holds(slots) {
            n += len(g.start as usize..g.end as usize);
        }
        at = g.end as usize;
    }
    n + len(at..body.blocks.len())
}

/// A body as the runtime would run it: guarded.
fn guarded(p: &Program, b: Body) -> Body {
    guard_with(p.slot_count as usize, &b, None, MIN_REGION).unwrap_or(b)
}

/// One phase's fused body, its context, and sampled settled states.
struct Phase {
    k: usize,
    cx: SpecContext,
    general: Body,
    cands: Vec<Candidate>,
    rows: Vec<Vec<u64>>,
}

/// Sampled settled slots of `n` ticks per phase, spread over `sample`
/// ticks after `warm`.
fn profile(p: &Arc<Program>, warm: u64, sample: u64, n: usize) -> Vec<Vec<Vec<u64>>> {
    let period = p.phases.as_ref().map_or(1, |x| u64::from(x.period));
    let mut sim = Sim::new(Arc::clone(p));
    sim.run(warm);
    let mut rows = vec![Vec::new(); period as usize];
    let stride = (sample / n as u64 / period).max(1);
    let mut t = 0;
    while t < sample {
        // `stride` ticks, then one of each phase.
        sim.run(stride);
        for _ in 0..period {
            sim.tick();
            let k = (sim.tick_count() % period) as usize;
            if rows[k].len() < n {
                rows[k].push((0..p.slot_count).map(|s| sim.read(s)).collect());
            }
        }
        t += stride + period;
    }
    rows
}

/// What keying a phase on `layout` gives on `rows`, with bodies for
/// `keys` (the most frequent keys of the rows when `None`).
struct Outcome {
    /// Executed ops saved per tick of this phase (misses run the prefix
    /// and the general body).
    executed: f64,
    /// The same with every instruction counted.
    stat: f64,
    distinct: usize,
    hit: f64,
    keys: Vec<u64>,
}

fn evaluate(
    p: &Program,
    ph: &Phase,
    layout: &KeyLayout,
    rows: &[Vec<u64>],
    bodies: usize,
    keys: Option<&[u64]>,
) -> Outcome {
    let mut by_key: HashMap<u64, Vec<usize>> = HashMap::new();
    for (r, row) in rows.iter().enumerate() {
        by_key.entry(layout.pack(row)).or_default().push(r);
    }
    let keys: Vec<u64> = match keys {
        Some(k) => k.to_vec(),
        None => {
            let mut top: Vec<(u64, usize)> = by_key.iter().map(|(&k, v)| (k, v.len())).collect();
            top.sort_by_key(|&(k, n)| (std::cmp::Reverse(n), k));
            top.into_iter().take(bodies).map(|x| x.0).collect()
        }
    };
    let slots: Vec<u32> = layout.fields.iter().map(|f| f.slot).collect();
    let (prefix, pre) = ph.cx.prefix(&slots);
    let prefix_ops = prefix.ops() as f64;
    let total = rows.len().max(1) as f64;
    // A miss runs the general body without the prefix's instructions.
    let fallback = ph.cx.fallback(p, &pre);
    let fallback_ops = fallback.ops() as f64;
    let fallback = guarded(p, fallback);
    let general_ops = ph.cx.body().ops() as f64;
    let (mut ex, mut st, mut hit) = (0.0, 0.0, 0usize);
    let mut seen = vec![false; rows.len()];
    for &key in &keys {
        let Some(rs) = by_key.get(&key) else {
            continue;
        };
        let spec = ph.cx.specialize(p, &layout.assume(key), &pre);
        let sops = spec.ops() as f64;
        let spec = guarded(p, spec);
        for &r in rs {
            let row = &rows[r];
            seen[r] = true;
            ex += executed(&ph.general, row) as f64 - prefix_ops - executed(&spec, row) as f64;
            st += general_ops - prefix_ops - sops;
        }
        hit += rs.len();
    }
    for (r, row) in rows.iter().enumerate() {
        if !seen[r] {
            ex += executed(&ph.general, row) as f64 - prefix_ops - executed(&fallback, row) as f64;
            st += general_ops - prefix_ops - fallback_ops;
        }
    }
    Outcome {
        executed: ex / total,
        stat: st / total,
        distinct: by_key.len(),
        hit: hit as f64 / total,
        keys,
    }
}

fn main() -> Result<(), String> {
    let a = args()?;
    let t0 = Instant::now();
    let w = workspace::load(Path::new(&a.folder))?;
    let compiled = compile(&w.design, &Options::default()).map_err(|e| format!("{e:?}"))?;
    let program = Arc::new(if a.demo {
        demo(&compiled, 0x1E)
    } else {
        compiled
    });
    let p = &*program;
    let ph = p
        .phases
        .as_ref()
        .ok_or("the program has no clock-phase variants")?;
    println!(
        "{}{}: {} slots, {} instructions, {} commits; {} phases; loaded and compiled in {:.1} s",
        a.folder,
        if a.demo {
            " (demo program, PPUMASK $1E)"
        } else {
            ""
        },
        p.slot_count,
        p.instrs.len(),
        p.commits.len(),
        ph.period,
        t0.elapsed().as_secs_f64()
    );
    let names = slot_names(p, &w);
    let possible = possible_bits(p);
    let t1 = Instant::now();
    let n_rows = 3000;
    let rows = profile(&program, a.warm, a.sample, n_rows);
    println!(
        "profile: {:.1} s ({} states per phase)",
        t1.elapsed().as_secs_f64(),
        n_rows
    );
    let mut phases_: Vec<Phase> = Vec::new();
    let t2 = Instant::now();
    for ((k, v), rows) in ph.variants.iter().enumerate().zip(rows) {
        let fused = phases::prune(p, v).unwrap_or_else(|| v.clone());
        let cx = SpecContext::new(p, &fused, &possible, Some(k as u64)).ok_or("a packed body")?;
        let cands = cx.candidates(p);
        phases_.push(Phase {
            k,
            general: guarded(p, fused),
            cx,
            cands,
            rows,
        });
    }
    let analysis = t2.elapsed().as_secs_f64();
    // One specialization (and guarding) of the largest body, timed.
    let big = phases_
        .iter()
        .max_by_key(|x| x.cx.body().ops())
        .ok_or("no phases")?;
    let t = Instant::now();
    for _ in 0..50 {
        std::hint::black_box(guarded(p, big.cx.fallback(p, &[])));
    }
    println!(
        "analysis: {:.1} ms for every phase; specializing and guarding a {}-op body: {:.2} ms",
        analysis * 1e3,
        big.cx.body().ops(),
        t.elapsed().as_secs_f64() * 1e3 / 50.0
    );
    for phase in &phases_ {
        let ex: f64 = phase
            .rows
            .iter()
            .map(|r| executed(&phase.general, r) as f64)
            .sum::<f64>()
            / phase.rows.len() as f64;
        // No key at all: what the known-bits analysis folds by itself
        // (possible bits, ROM reads with partly known addresses).
        let alone = phase.cx.fallback(p, &[]);
        let alone_ops = alone.ops();
        let alone = guarded(p, alone);
        let ax: f64 = phase
            .rows
            .iter()
            .map(|r| executed(&alone, r) as f64)
            .sum::<f64>()
            / phase.rows.len() as f64;
        println!(
            "  phase {}: fused body {} ops, {:.1} executed with guards; with no key, {} ops, {:.1} executed",
            phase.k,
            phase.cx.body().ops(),
            ex,
            alone_ops,
            ax
        );
    }

    let t3 = Instant::now();
    let ctxs: Vec<Option<&SpecContext>> = phases_.iter().map(|x| Some(&x.cx)).collect();
    let samples: Vec<&[Vec<u64>]> = phases_.iter().map(|x| x.rows.as_slice()).collect();
    let plan = choose_plan(
        p,
        &ctxs,
        &samples,
        &PlanOptions {
            max_bodies: a.bodies,
            min_loss: a.min_loss,
            ..PlanOptions::default()
        },
    );
    println!("plan chosen in {:.1} s", t3.elapsed().as_secs_f64());
    let mut saved = 0.0;
    let mut layouts: Vec<KeyLayout> = Vec::new();
    let mut plan_keys: Vec<Vec<u64>> = Vec::new();
    for phase in &phases_ {
        println!(
            "\n== phase {}: {} candidates ({} ROM words, {} flags, {} registers)",
            phase.k,
            phase.cands.len(),
            phase
                .cands
                .iter()
                .filter(|c| c.class == KeyClass::RomWord)
                .count(),
            phase
                .cands
                .iter()
                .filter(|c| c.class == KeyClass::Flag)
                .count(),
            phase
                .cands
                .iter()
                .filter(|c| c.class == KeyClass::Register)
                .count(),
        );
        let mut scored: Vec<(usize, Outcome)> = phase
            .cands
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let layout = KeyLayout::new(vec![KeyField {
                    slot: c.slot,
                    mask: c.support,
                }]);
                (i, evaluate(p, phase, &layout, &phase.rows, a.bodies, None))
            })
            .collect();
        scored.sort_by(|x, y| y.1.executed.total_cmp(&x.1.executed));
        println!(
            "  {:<40} {:>5} {:>5} {:>5} {:>4} {:>6} {:>6} {:>6} {:>8} {:>7}",
            "candidate",
            "class",
            "whole",
            "exact",
            "fold",
            "prefix",
            "values",
            "hits",
            "executed",
            "static"
        );
        for (i, o) in scored.iter().take(a.top) {
            let c = &phase.cands[*i];
            println!(
                "  {:<40} {:>5} {:>5} {:>5} {:>4} {:>6} {:>6} {:>5.1}% {:>8.1} {:>7.1}",
                name(&names, c.slot),
                match c.class {
                    KeyClass::RomWord => "rom",
                    KeyClass::Flag => "flag",
                    KeyClass::Register => "reg",
                },
                64 - possible[c.slot as usize].leading_zeros(),
                phase.cx.exact_support(c.slot).count_ones(),
                c.support.count_ones(),
                c.prefix_ops,
                o.distinct,
                o.hit * 100.0,
                o.executed,
                o.stat
            );
        }
        // The plan `choose_plan` made (candidates scored alone, a greedy
        // combination, pruned bits), costed here with the same samples.
        let layout = plan.phases[phase.k]
            .as_ref()
            .map_or_else(KeyLayout::default, |pp| pp.layout.clone());
        let mut keys = Vec::new();
        if !layout.fields.is_empty() {
            let b = evaluate(p, phase, &layout, &phase.rows, a.bodies, None);
            keys = b.keys.clone();
            println!(
                "  plan key ({} fields, {} bits): {:.1} executed ops/tick saved ({:.1} static), {} distinct keys, {:.1}% of ticks hit the {} bodies",
                layout.fields.len(),
                layout.width(),
                b.executed,
                b.stat,
                b.distinct,
                b.hit * 100.0,
                b.keys.len()
            );
            saved += b.executed;
        } else {
            println!("  no plan: no key pays");
        }
        for f in &layout.fields {
            println!(
                "    {} mask {:#x} ({} of {} support bits)",
                name(&names, f.slot),
                f.mask,
                f.mask.count_ones(),
                phase.cx.support(f.slot).count_ones()
            );
        }
        plan_keys.push(keys);
        layouts.push(layout);
    }
    let period = phases_.len() as f64;
    let ex: f64 = phases_
        .iter()
        .map(|ph| {
            ph.rows
                .iter()
                .map(|r| executed(&ph.general, r) as f64)
                .sum::<f64>()
                / ph.rows.len() as f64
        })
        .sum::<f64>()
        / period;
    println!(
        "\nplan keys: {:.1} of {ex:.1} executed ops per tick saved ({:.0}%); selection {:.1} s",
        saved / period,
        100.0 * saved / period / ex,
        t3.elapsed().as_secs_f64()
    );

    if a.period_keys > 0 {
        period_keys(&program, &layouts, a.warm + a.sample, a.period_keys);
    }
    if a.cross_tick > 0 {
        cross_tick(&program, &phases_, &layouts, &possible, &a);
    }
    if a.adaptive > 0 {
        return adaptive_bench(&program, &phases_, &layouts, &a);
    }

    if a.check > 0 {
        let t4 = Instant::now();
        let n = check_exact(&program, &phases_, &layouts, a.warm, a.check, false)?;
        // The check catches a wrong body.
        match check_exact(&program, &phases_, &layouts, a.warm, a.check, true) {
            Err(e) => println!("mutation (bodies for the wrong key) caught: {e}"),
            Ok(_) => return Err("a wrong key's bodies passed the exactness check".into()),
        }
        println!(
            "exactness: {n} fused ticks of the run, the body with no key, the plan's bodies and their fallbacks against the general body: the same state and memories ({:.1} s)",
            t4.elapsed().as_secs_f64()
        );
    }

    if a.bench > 0 {
        let plan = SpecPlan {
            phases: layouts
                .iter()
                .zip(&plan_keys)
                .map(|(layout, keys)| {
                    (!layout.fields.is_empty()).then(|| PhasePlan {
                        layout: layout.clone(),
                        keys: keys.clone(),
                    })
                })
                .collect(),
        };
        bench(&program, plan, &a)?;
    }

    hand_picked(p, &w, &names, &phases_, &possible, &a);
    Ok(())
}

#[cfg(feature = "jit")]
fn code_size(on: &Sim, off: &Sim) -> String {
    let kib = |s: &Sim| s.jit_code().map_or(0, |j| j.code_bytes / 1024);
    format!(
        " ({} KiB of native code, {} KiB without)",
        kib(on),
        kib(off)
    )
}

#[cfg(not(feature = "jit"))]
fn code_size(_: &Sim, _: &Sim) -> String {
    String::new()
}

/// The runtime with the greedy keys (bodies for the keys `--bodies` most
/// frequent in the profile) against the runtime without, both JIT-compiled
/// when the feature is on: after `--warm` ticks each, `--rounds` rounds of
/// `--bench` ticks in `run` batches of 30000, the two alternating (the
/// machine may be shared: every round is printed). Then every state element
/// and memory must be the same. With the feature `counters`, also the
/// executed operations per tick.
fn bench(program: &Arc<Program>, plan: SpecPlan, a: &Args) -> Result<(), String> {
    println!("\n== runtime: greedy plan, spec on against off");
    let base = SimOptions {
        jit: cfg!(feature = "jit"),
        ..SimOptions::default()
    };
    let t = Instant::now();
    let on_options = SimOptions {
        spec: Some(SpecOptions::fixed(plan)),
        ..base.clone()
    };
    let mut on = Sim::with_options(Arc::clone(program), on_options);
    let build = t.elapsed().as_secs_f64();
    let mut off = Sim::with_options(Arc::clone(program), base);
    let st = on.spec_stats();
    println!(
        "  jit {}: {} keyed phases, {} bodies, built in {:.2} s{}",
        on.jit_active(),
        st.keyed_phases,
        st.bodies,
        build,
        code_size(&on, &off)
    );
    let batch = 30_000u64;
    let go = |s: &mut Sim, n: u64| -> f64 {
        let t = Instant::now();
        let mut left = n;
        while left > 0 {
            let k = left.min(batch);
            s.run(k);
            left -= k;
        }
        n as f64 / t.elapsed().as_secs_f64() / 1e3
    };
    go(&mut on, a.warm);
    go(&mut off, a.warm);
    let (mut r_on, mut r_off) = (Vec::new(), Vec::new());
    let (h0, m0) = (on.spec_stats().hits, on.spec_stats().misses);
    #[cfg(feature = "counters")]
    let mut ops = (0u64, 0u64, 0u64);
    for round in 0..a.rounds {
        #[cfg(feature = "counters")]
        {
            on.reset_counters();
            off.reset_counters();
        }
        // Alternate which goes first.
        let (x, y) = if round % 2 == 0 {
            let x = go(&mut on, a.bench);
            (x, go(&mut off, a.bench))
        } else {
            let y = go(&mut off, a.bench);
            (go(&mut on, a.bench), y)
        };
        #[cfg(feature = "counters")]
        {
            ops.0 += on.counters().ops();
            ops.1 += off.counters().ops();
            ops.2 += on.counters().ticks;
        }
        println!(
            "  round {round}: on {x:.0} k ticks/s, off {y:.0} k ticks/s ({:+.1}%)",
            (x / y - 1.0) * 100.0
        );
        r_on.push(x);
        r_off.push(y);
    }
    let med = |v: &mut Vec<f64>| {
        v.sort_by(f64::total_cmp);
        v[v.len() / 2]
    };
    let (lo_on, hi_on) = (
        r_on.iter().cloned().fold(f64::MAX, f64::min),
        r_on.iter().cloned().fold(0.0, f64::max),
    );
    let (lo_off, hi_off) = (
        r_off.iter().cloned().fold(f64::MAX, f64::min),
        r_off.iter().cloned().fold(0.0, f64::max),
    );
    let (mo, mf) = (med(&mut r_on), med(&mut r_off));
    let st = on.spec_stats();
    let (h, m) = (st.hits - h0, st.misses - m0);
    println!(
        "  median: on {mo:.0} ({lo_on:.0}..{hi_on:.0}), off {mf:.0} ({lo_off:.0}..{hi_off:.0}) k ticks/s: {:+.1}%; {:.1}% of keyed ticks hit",
        (mo / mf - 1.0) * 100.0,
        100.0 * h as f64 / (h + m).max(1) as f64
    );
    #[cfg(feature = "counters")]
    println!(
        "  executed ops per tick: on {:.1}, off {:.1}",
        ops.0 as f64 / ops.2.max(1) as f64,
        ops.1 as f64 / ops.2.max(1) as f64
    );
    let ticks = a.warm + a.rounds * a.bench;
    if on.export_state() != off.export_state() {
        return Err(format!("state differs after {ticks} ticks"));
    }
    let p = &**program;
    for m in 0..p.mems.len() as u32 {
        if on.mem(m) != off.mem(m) {
            return Err(format!("memory {m} differs after {ticks} ticks"));
        }
    }
    println!("  state and memories equal after {ticks} ticks");
    Ok(())
}

/// The prototype's hand-picked keys, whole and masked; with `--demo`,
/// also with the bodies of the `$1E` run on a `$18` run.
fn hand_picked(
    p: &Program,
    w: &workspace::Loaded,
    names: &HashMap<u32, Vec<String>>,
    phases_: &[Phase],
    possible: &[u64],
    a: &Args,
) {
    let render_outputs: Vec<String> = w
        .design
        .defs
        .values()
        .find(|d| d.name == "ppu_render_ctl")
        .map(|d| {
            d.outputs
                .iter()
                .filter_map(|&n| d.names.get(n).cloned())
                .collect()
        })
        .unwrap_or_default();
    let is_cw = |n: &str| n.ends_with("cpu6502/ref_decode/cw");
    let is_ppu = |n: &str| {
        render_outputs
            .iter()
            .any(|o| n.ends_with(&format!("ppu_render_ctl/{o}")))
            || ["fetching", "clr_oam_addr", "set_ovf"]
                .iter()
                .any(|o| n.ends_with(&format!("ppu_sprites/{o}")))
            || ["mask", "ctrl"]
                .iter()
                .any(|o| n.ends_with(&format!("ppu_regs/{o}")))
    };
    let slots_of = |f: &dyn Fn(&str) -> bool| -> Vec<u32> {
        let mut v: Vec<u32> = names
            .iter()
            .filter(|(_, ns)| ns.iter().any(|n| f(n)))
            .map(|(&s, _)| s)
            .collect();
        v.sort_unstable();
        v
    };
    let cw = slots_of(&is_cw);
    let ppu = slots_of(&is_ppu);
    // The CPU phase: where the microcode word's support is widest.
    let cpu_phase = phases_
        .iter()
        .max_by_key(|ph| {
            cw.iter()
                .map(|&s| ph.cx.support(s).count_ones())
                .sum::<u32>()
        })
        .map_or(0, |ph| ph.k);
    let other = (a.demo).then(|| {
        let p18 = Arc::new(demo(p, 0x18));
        profile(&p18, a.warm, a.sample, 3000)
    });
    println!("\n== hand-picked keys (prototype)");
    const WHAT: [&str; 3] = ["whole", "masked", "pruned"];
    let mut total = [[0.0f64; 3]; 2];
    for phase in phases_ {
        let slots = if phase.k == cpu_phase { &cw } else { &ppu };
        let slots: Vec<u32> = slots
            .iter()
            .copied()
            .filter(|&s| phase.cx.exact_support(s) != 0)
            .collect();
        println!(
            "  phase {} ({}): {}",
            phase.k,
            if phase.k == cpu_phase {
                "CPU key"
            } else {
                "PPU keys"
            },
            slots
                .iter()
                .map(|&s| name(names, s))
                .collect::<Vec<_>>()
                .join(", ")
        );
        let layout_of = |m: &dyn Fn(u32) -> u64| {
            KeyLayout::new(
                slots
                    .iter()
                    .map(|&s| KeyField {
                        slot: s,
                        mask: m(s),
                    })
                    .filter(|f| f.mask != 0)
                    .collect(),
            )
        };
        let whole = layout_of(&|s| possible[s as usize]);
        let masked = layout_of(&|s| phase.cx.support(s));
        // Bits worth less than `--min-loss` ops per settle on this run's
        // keys go.
        let mut freq: HashMap<u64, u64> = HashMap::new();
        for row in &phase.rows {
            *freq.entry(masked.pack(row)).or_default() += 1;
        }
        let samples: Vec<(u64, u64)> = freq.into_iter().collect();
        let pruned = phase.cx.prune_bits(p, &masked, &samples, a.min_loss);
        for (wi, layout) in [whole, masked, pruned].iter().enumerate() {
            let o = evaluate(p, phase, layout, &phase.rows, a.bodies, None);
            total[0][wi] += o.executed;
            println!(
                "    {:6}: {:>3} bits, {:>5} distinct keys, {:>5.1}% hit with {} bodies, {:>6.1} executed ops/tick saved ({:.1} static)",
                WHAT[wi],
                layout.width(),
                o.distinct,
                o.hit * 100.0,
                o.keys.len(),
                o.executed,
                o.stat
            );
            if let Some(rows) = &other {
                let t = evaluate(p, phase, layout, &rows[phase.k], a.bodies, Some(&o.keys));
                total[1][wi] += t.executed;
                println!(
                    "            on the $18 run with these bodies: {:>5.1}% hit, {:>6.1} executed ops/tick saved",
                    t.hit * 100.0,
                    t.executed
                );
            }
            if wi == 2 {
                println!(
                    "      kept: {}",
                    layout
                        .fields
                        .iter()
                        .map(|f| format!("{} {:#x}", name(names, f.slot), f.mask))
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            if wi > 0 {
                for f in &layout.fields {
                    let before = if wi == 1 {
                        possible[f.slot as usize]
                    } else {
                        phase.cx.support(f.slot)
                    };
                    if f.mask != before {
                        println!(
                            "      {}: {} -> {} bits ({:#x})",
                            name(names, f.slot),
                            before.count_ones(),
                            f.mask.count_ones(),
                            f.mask
                        );
                    }
                }
            }
        }
    }
    let period = phases_.len() as f64;
    let row = |t: &[f64; 3]| {
        WHAT.iter()
            .zip(t)
            .map(|(w, x)| format!("{w} {:.1}", x / period))
            .collect::<Vec<_>>()
            .join(", ")
    };
    println!("  per tick, executed ops saved: {}", row(&total[0]));
    if other.is_some() {
        println!(
            "  per tick on the $18 run with the $1E bodies: {}",
            row(&total[1])
        );
    }
}

/// One operation on settled slots (the runtime's semantics).
fn eval(op: Op, i: &Instr, imm: u64, s: &[u64], mems: &[Vec<u64>], tick: u64) -> u64 {
    let a = s[i.a as usize];
    let b = || s[i.b as usize];
    let c = || s[i.c as usize];
    let mask = |w: u32| if w >= 64 { u64::MAX } else { (1u64 << w) - 1 };
    match op {
        Op::Copy => a,
        Op::Not => !a & imm,
        Op::And => a & b(),
        Op::Or => a | b(),
        Op::Xor => a ^ b(),
        Op::Nand => !(a & b()) & imm,
        Op::Nor => !(a | b()) & imm,
        Op::Xnor => !(a ^ b()) & imm,
        Op::Mux => {
            if a != 0 {
                c()
            } else {
                b()
            }
        }
        Op::Switch => {
            if a != 0 {
                b()
            } else {
                0
            }
        }
        Op::AddSum => a.wrapping_add(b()).wrapping_add(c()) & imm,
        Op::AddCarry => ((a as u128 + b() as u128 + c() as u128) >> imm) as u64 & 1,
        Op::Eq => (a == b()) as u64,
        Op::LtU => (a < b()) as u64,
        Op::ReduceOr => (a != 0) as u64,
        Op::ReduceAnd => (a == imm) as u64,
        Op::ReduceXor => u64::from(a.count_ones() & 1),
        Op::Shr => a.checked_shr(i.b).unwrap_or(0) & imm,
        Op::Concat => a | b().checked_shl(i.c).unwrap_or(0),
        Op::Sext => {
            if (a >> (i.b - 1)) & 1 == 1 {
                (a | !mask(i.b)) & imm
            } else {
                a
            }
        }
        Op::Clock => u64::from((tick + u64::from(i.c)) % u64::from(i.a) < u64::from(i.b)),
        Op::MemRead => mems[i.b as usize].get(a as usize).copied().unwrap_or(0),
        Op::SrNext => {
            if b() != 0 {
                0
            } else if a != 0 {
                1
            } else {
                c()
            }
        }
        Op::Shl => ((a >> i.c) << i.b) & imm,
        Op::ShlV => (a << (b() & 63)) & imm,
    }
}

fn settle(body: &Body, s: &mut [u64], mems: &[Vec<u64>], tick: u64) {
    for b in &body.blocks {
        for k in b.start..b.start + b.len {
            let i = &body.instrs[k as usize];
            s[i.dst as usize] = eval(b.op, i, body.imms[k as usize], s, mems, tick);
        }
    }
}

fn commit(p: &Program, commits: &[Commit], s: &mut [u64], mems: &mut [Vec<u64>]) {
    let at = |m: u32, a: u64| (a & ((1u64 << p.mems[m as usize].addr_bits) - 1)) as usize;
    let mut copies = Vec::new();
    for c in commits {
        match *c {
            Commit::Copy { state, next } => copies.push((state, s[next as usize])),
            Commit::MemClear { mem, en } => {
                if s[en as usize] != 0 {
                    mems[mem as usize].fill(0);
                }
            }
            Commit::RamWrite {
                mem,
                we,
                addr,
                data,
            } => {
                if s[we as usize] != 0 {
                    let a = at(mem, s[addr as usize]);
                    mems[mem as usize][a] = s[data as usize];
                }
            }
            Commit::RamWriteMasked {
                mem,
                mask,
                addr,
                data,
            } => {
                let a = at(mem, s[addr as usize]);
                let (m, d) = (s[mask as usize], s[data as usize]);
                let w = &mut mems[mem as usize][a];
                *w = (*w & !m) | (d & m);
            }
        }
    }
    for (st, v) in copies {
        s[st as usize] = v;
    }
}

/// Exactness on the real design: from `n` states of a run per phase,
/// one fused tick of the general body against the body with no key and
/// against the prefix and specialized body of `layouts` (per phase) for
/// the key the state gives. Returns the ticks checked.
///
/// With `mutate`, each body is built for the key with its lowest bit
/// flipped (a wrong body): the check must then fail.
fn check_exact(
    p: &Arc<Program>,
    phases_: &[Phase],
    layouts: &[KeyLayout],
    warm: u64,
    n: usize,
    mutate: bool,
) -> Result<usize, String> {
    let period = phases_.len() as u64;
    let mut sim = Sim::new(Arc::clone(p));
    sim.run(warm);
    let ph = p.phases.as_ref().ok_or("no phases")?;
    let states: Vec<u32> = {
        let mut v: Vec<u32> = p
            .commits
            .iter()
            .filter_map(|c| match *c {
                Commit::Copy { state, .. } => Some(state),
                _ => None,
            })
            .collect();
        v.sort_unstable();
        v.dedup();
        v
    };
    let mut checked = 0;
    for round in 0..n * period as usize {
        sim.run(97 + (round as u64 % 13));
        sim.tick();
        let t = sim.tick_count();
        let k = (t % period) as usize;
        let mut slots: Vec<u64> = (0..p.slot_count).map(|s| sim.read(s)).collect();
        let mut mems: Vec<Vec<u64>> = (0..p.mems.len() as u32)
            .map(|m| sim.mem(m).to_vec())
            .collect();
        commit(p, &ph.variants[k].commits, &mut slots, &mut mems);
        let k1 = ((t + 1) % period) as usize;
        let next = &phases_[k1];
        let run = |bodies: &[&Body]| {
            let (mut s, mut m) = (slots.clone(), mems.clone());
            for b in bodies {
                settle(b, &mut s, &m, t + 1);
            }
            commit(p, &bodies.last().unwrap().commits, &mut s, &mut m);
            (
                states.iter().map(|&x| s[x as usize]).collect::<Vec<u64>>(),
                m,
            )
        };
        let want = run(&[next.cx.body()]);
        let alone = next.cx.fallback(p, &[]);
        if run(&[&alone]) != want {
            return Err(format!("tick {}: the body with no key differs", t + 1));
        }
        let layout = &layouts[k1];
        let keys: Vec<u32> = layout.fields.iter().map(|f| f.slot).collect();
        let (prefix, pre) = next.cx.prefix(&keys);
        let mut s = slots.clone();
        settle(&prefix, &mut s, &mems, t + 1);
        let key = layout.pack(&s);
        let spec = next
            .cx
            .specialize(p, &layout.assume(key ^ u64::from(mutate)), &pre);
        if run(&[&prefix, &spec]) != want {
            return Err(format!("tick {}: the body for key {key:#x} differs", t + 1));
        }
        let fb = next.cx.fallback(p, &pre);
        if run(&[&prefix, &fb]) != want {
            return Err(format!("tick {}: the fallback differs", t + 1));
        }
        checked += 1;
    }
    Ok(checked)
}

/// One runtime of `--adaptive`.
struct Runner {
    name: String,
    sim: Sim,
    /// Builds come back here (adaptive tables).
    builds: Vec<logi_sim::sim::SpecBuilt>,
    adaptive: bool,
    /// Seconds spent in this runner's `run` calls and installs.
    busy: f64,
    /// Ticks and busy seconds from construction to the first hit.
    first_hit: Option<(u64, f64)>,
    rates: [Vec<f64>; 2],
    hit: [(u64, u64); 2],
    /// Executed operations per tick (feature `counters`).
    ops: [f64; 2],
}

/// `--adaptive`: see the module documentation.
fn adaptive_bench(
    program: &Arc<Program>,
    phases_: &[Phase],
    layouts: &[KeyLayout],
    a: &Args,
) -> Result<(), String> {
    use logi_sim::sim::{SpecBuilt, SpecRequest, build_spec};
    use std::sync::mpsc;
    use std::time::Duration;

    let jit = cfg!(feature = "jit");
    let base = SimOptions {
        jit,
        ..SimOptions::default()
    };
    // The profile's most frequent keys per phase (`evaluate`'s order).
    let top = |n: usize| -> SpecPlan {
        SpecPlan {
            phases: phases_
                .iter()
                .zip(layouts)
                .map(|(ph, layout)| {
                    (!layout.fields.is_empty()).then(|| {
                        let mut count: HashMap<u64, u64> = HashMap::new();
                        for r in &ph.rows {
                            *count.entry(layout.pack(r)).or_default() += 1;
                        }
                        let mut v: Vec<(u64, u64)> = count.into_iter().collect();
                        v.sort_by_key(|&(k, c)| (std::cmp::Reverse(c), k));
                        PhasePlan {
                            layout: layout.clone(),
                            keys: v.into_iter().take(n).map(|x| x.0).collect(),
                        }
                    })
                })
                .collect(),
        }
    };
    let adaptive = |plan: Option<SpecPlan>, cap: usize| SpecOptions {
        plan: plan.map(Arc::new),
        max_per_phase: cap,
        max_bodies: 3 * cap,
        code_budget: usize::MAX,
        inline_build: false,
        min_loss: a.min_loss as u32,
        ..SpecOptions::default()
    };
    let mut configs: Vec<(String, Option<SpecOptions>)> = vec![
        ("off".into(), None),
        ("fixed 16".into(), Some(SpecOptions::fixed(top(16)))),
        ("fixed 64".into(), Some(SpecOptions::fixed(top(64)))),
    ];
    for cap in [8, 16, 32] {
        configs.push((format!("adaptive {cap}"), Some(adaptive(Some(top(0)), cap))));
    }
    configs.push((
        "automatic 16".into(),
        Some(SpecOptions {
            plan_window: a.auto_window,
            samples: a.auto_samples,
            ..adaptive(None, 16)
        }),
    ));

    // One build thread for every runner, as an engine's compile thread.
    let (to_builder, requests) = mpsc::channel::<(usize, SpecRequest)>();
    let (to_main, built) = mpsc::channel::<(usize, SpecBuilt, Duration)>();
    let prog = Arc::clone(program);
    let builder = std::thread::spawn(move || {
        for (i, r) in requests {
            let t = Instant::now();
            let b = build_spec(&prog, &r);
            if to_main.send((i, b, t.elapsed())).is_err() {
                break;
            }
        }
    });
    let mut body_ms: Vec<f64> = Vec::new();
    let mut plan_ms: Vec<f64> = Vec::new();
    let mut runners: Vec<Runner> = configs
        .into_iter()
        .map(|(name, spec)| {
            let t = Instant::now();
            let adaptive = spec.as_ref().is_some_and(|s| s.adaptive);
            let sim = Sim::with_options(
                Arc::clone(program),
                SimOptions {
                    spec,
                    ..base.clone()
                },
            );
            println!(
                "  {name}: built in {:.2} s, jit {}",
                t.elapsed().as_secs_f64(),
                sim.jit_active()
            );
            Runner {
                name,
                sim,
                builds: Vec::new(),
                adaptive,
                busy: 0.0,
                first_hit: None,
                rates: [Vec::new(), Vec::new()],
                hit: [(0, 0); 2],
                ops: [0.0; 2],
            }
        })
        .collect();

    // Run `n` ticks of runner `i` in batches, handing requests to the
    // builder and installing what came back between batches. Seconds.
    let mut step = |rs: &mut Vec<Runner>, i: usize, n: u64, batch: u64| -> f64 {
        let t = Instant::now();
        let mut left = n;
        while left > 0 {
            let k = left.min(batch);
            let r = &mut rs[i];
            r.sim.run(k);
            left -= k;
            if r.adaptive {
                for q in r.sim.spec_requests() {
                    to_builder.send((i, q)).expect("builder alive");
                }
            }
            while let Ok((j, b, d)) = built.try_recv() {
                match b.plan_phases() {
                    Some(_) => plan_ms.push(d.as_secs_f64() * 1e3),
                    None => body_ms.push(d.as_secs_f64() * 1e3),
                }
                rs[j].builds.push(b);
            }
            let r = &mut rs[i];
            for b in std::mem::take(&mut r.builds) {
                r.sim.install_spec(b);
            }
            let secs = t.elapsed().as_secs_f64();
            if r.first_hit.is_none() && r.sim.spec_stats().hits > 0 {
                r.first_hit = Some((r.sim.tick_count(), r.busy + secs));
            }
        }
        let secs = t.elapsed().as_secs_f64();
        rs[i].busy += secs;
        secs
    };
    let start = a.warm + a.sample;
    let t = Instant::now();
    for i in 0..runners.len() {
        // Small batches while warming: first hits are timed finely.
        step(&mut runners, i, start, 10_000);
    }
    println!(
        "  warmed {} runtimes to tick {start} in {:.1} s",
        runners.len(),
        t.elapsed().as_secs_f64()
    );
    let rounds = a.rounds.max(1);
    let per = a.adaptive / rounds;
    for (seg, from) in [start, a.later.max(start + a.adaptive)]
        .into_iter()
        .enumerate()
    {
        for i in 0..runners.len() {
            let now = runners[i].sim.tick_count();
            if from > now {
                step(&mut runners, i, from - now, 30_000);
            }
            let st = runners[i].sim.spec_stats();
            runners[i].hit[seg] = (st.hits, st.misses);
            #[cfg(feature = "counters")]
            runners[i].sim.reset_counters();
        }
        for round in 0..rounds {
            let n = runners.len();
            for j in 0..n {
                // Rotate the order every round.
                let i = (j + round as usize) % n;
                let secs = step(&mut runners, i, per, 30_000);
                runners[i].rates[seg].push(per as f64 / secs / 1e3);
            }
        }
        for r in &mut runners {
            let st = r.sim.spec_stats();
            r.hit[seg] = (st.hits - r.hit[seg].0, st.misses - r.hit[seg].1);
            #[cfg(feature = "counters")]
            {
                let c = r.sim.counters();
                r.ops[seg] = c.ops() as f64 / c.ticks.max(1) as f64;
            }
        }
    }
    drop(to_builder);
    builder.join().map_err(|_| "builder panicked")?;

    let stat = |v: &[f64]| {
        let mut v = v.to_vec();
        v.sort_by(f64::total_cmp);
        (v[v.len() / 2], v[0], *v.last().expect("a round"))
    };
    let off = [stat(&runners[0].rates[0]).0, stat(&runners[0].rates[1]).0];
    println!(
        "\n  k ticks/s (JIT {}), median (range) of {rounds} rounds of {per} ticks; hits of keyed ticks",
        jit
    );
    println!(
        "  {:<14} {:>28} {:>28} {:>8} {:>10} {:>9} {:>8}",
        "runtime", "from start", "from later", "bodies", "code KiB", "evicted", "1st hit"
    );
    for r in &runners {
        let st = r.sim.spec_stats();
        let cell = |seg: usize| {
            let (m, lo, hi) = stat(&r.rates[seg]);
            let (h, x) = r.hit[seg];
            let hit = if h + x > 0 {
                format!(" {:>3.0}%", 100.0 * h as f64 / (h + x) as f64)
            } else {
                "     ".into()
            };
            let ops = if cfg!(feature = "counters") {
                format!(" {:>5.0} ops", r.ops[seg])
            } else {
                String::new()
            };
            format!(
                "{m:>5.0} ({lo:>4.0}..{hi:>4.0}) {:+4.0}%{hit}{ops}",
                (m / off[seg] - 1.0) * 100.0
            )
        };
        #[cfg(feature = "jit")]
        let kib = (r.sim.jit_code().map_or(0, |j| j.code_bytes) + st.code_bytes) / 1024;
        #[cfg(not(feature = "jit"))]
        let kib = st.code_bytes / 1024;
        let first = r.first_hit.map_or("-".into(), |(t, s)| {
            format!("{:.2}M {:.1}s", t as f64 / 1e6, s)
        });
        println!(
            "  {:<14} {:>28} {:>28} {:>8} {:>10} {:>9} {:>8}",
            r.name,
            cell(0),
            cell(1),
            st.bodies,
            kib,
            st.evicted,
            first
        );
    }
    let ms = |v: &mut Vec<f64>| {
        if v.is_empty() {
            return "none".to_string();
        }
        v.sort_by(f64::total_cmp);
        format!(
            "{} builds, median {:.1} ms, 90% {:.1} ms, max {:.1} ms",
            v.len(),
            v[v.len() / 2],
            v[v.len() * 9 / 10],
            v[v.len() - 1]
        )
    };
    println!("  build latency per body: {}", ms(&mut body_ms));
    println!("  automatic plan build: {}", ms(&mut plan_ms));

    let want = &runners[0].sim;
    for r in &runners[1..] {
        if r.sim.export_state() != want.export_state()
            || (0..program.mems.len() as u32).any(|m| r.sim.mem(m) != want.mem(m))
        {
            return Err(format!("{}: state differs from spec off", r.name));
        }
    }
    println!(
        "  state and memories equal after {} ticks",
        want.tick_count()
    );
    Ok(())
}

/// `--period-keys N`: the keys of whole clock periods (the tuple of every
/// phase's key over one period, as a superstep would cover it: the
/// settles of phases 1, 2, ..., 0) on `N` ticks of the run after the
/// profile. How many distinct tuples, the share of periods the most
/// frequent ones cover (an upper bound on what supersteps per period key
/// could run under a cap), how often the tuple a period would be
/// predicted to have (the previous one; the one that followed the
/// previous one last time) is right, and how often each phase's key
/// changes from one period to the next.
fn period_keys(program: &Arc<Program>, layouts: &[KeyLayout], from: u64, n: u64) {
    println!("\n== period keys ({n} ticks from tick {from})");
    let p = &**program;
    let period = layouts.len() as u64;
    let mut sim = Sim::new(Arc::clone(program));
    sim.run(from);
    while !sim.tick_count().is_multiple_of(period) {
        sim.tick();
    }
    let mut tuples: Vec<Vec<u64>> = Vec::new();
    let mut cur = Vec::new();
    let mut slots = vec![0u64; p.slot_count as usize];
    for _ in 0..n {
        sim.tick();
        let k = (sim.tick_count() % period) as usize;
        for f in &layouts[k].fields {
            slots[f.slot as usize] = sim.read(f.slot);
        }
        cur.push(layouts[k].pack(&slots));
        if k == 0 {
            tuples.push(std::mem::take(&mut cur));
        }
    }
    let mut count: HashMap<&[u64], u64> = HashMap::new();
    for t in &tuples {
        *count.entry(t).or_default() += 1;
    }
    let mut v: Vec<u64> = count.values().copied().collect();
    v.sort_unstable_by(|a, b| b.cmp(a));
    let total = tuples.len().max(1) as f64;
    let cover = |m: usize| v.iter().take(m).sum::<u64>() as f64 / total * 100.0;
    println!(
        "  {} periods, {} distinct period keys; the most frequent 16 cover {:.1}% of periods, 64 {:.1}%, 256 {:.1}%",
        tuples.len(),
        v.len(),
        cover(16),
        cover(64),
        cover(256)
    );
    let (mut same, mut next, mut ticks) = (0u64, 0u64, 0u64);
    let mut succ: HashMap<&[u64], &[u64]> = HashMap::new();
    for w in tuples.windows(2) {
        same += u64::from(w[0] == w[1]);
        if let Some(&s) = succ.get(&w[0][..]) {
            // Ticks a superstep for the predicted key would run before
            // its first side exit.
            let ok = s.iter().zip(&w[1]).take_while(|(a, b)| a == b).count();
            next += u64::from(ok == w[1].len());
            ticks += ok as u64;
        }
        succ.insert(&w[0], &w[1]);
    }
    println!(
        "  predicted right: the previous period's key {:.1}%, the key that followed it last time {:.1}% ({:.1}% of ticks before the first wrong phase)",
        same as f64 / total * 100.0,
        next as f64 / total * 100.0,
        ticks as f64 / total / period as f64 * 100.0
    );
    for (i, k) in (1..=period).map(|i| (i % period) as usize).enumerate() {
        let distinct: std::collections::HashSet<u64> = tuples.iter().map(|t| t[i]).collect();
        let changes = tuples.windows(2).filter(|w| w[0][i] != w[1][i]).count();
        println!(
            "  phase {k}: {} distinct keys, changes in {:.1}% of periods",
            distinct.len(),
            changes as f64 / total * 100.0
        );
    }
}

/// Known bits of every slot after the settle of `body` (straight-line,
/// unpacked) under `assume`, as `SpecContext::specialize` derives them.
fn known_after(
    p: &Program,
    body: &Body,
    possible: &[u64],
    assume: &HashMap<u32, Known>,
    phase: u64,
) -> Vec<Known> {
    let mut kn: Vec<Known> = possible.iter().map(|&m| Known::UNKNOWN.within(m)).collect();
    for &(s, v) in &p.consts {
        kn[s as usize] = Known::constant(v);
    }
    kn[ZERO_SLOT as usize] = Known::constant(0);
    for (&s, &k) in assume {
        kn[s as usize] = kn[s as usize].union(k);
    }
    for b in &body.blocks {
        for idx in b.start..b.start + b.len {
            let i = &body.instrs[idx as usize];
            let mut k = transfer(
                b.op,
                i,
                body.imms[idx as usize],
                &|s| kn[s as usize],
                &p.mems,
                Some(phase),
            )
            .within(possible[i.dst as usize]);
            if let Some(a) = assume.get(&i.dst) {
                k = k.union(*a);
            }
            kn[i.dst as usize] = k;
        }
    }
    kn
}

/// See the module documentation (`--cross-tick`).
fn cross_tick(
    program: &Arc<Program>,
    phases_: &[Phase],
    layouts: &[KeyLayout],
    possible: &[u64],
    a: &Args,
) {
    let p = &**program;
    let period = phases_.len() as u64;
    let len = a.trace_len.max(2);
    let n = a.cross_tick;
    let t0 = Instant::now();
    let mut sim = Sim::new(Arc::clone(program));
    sim.run(a.warm + a.sample);
    // State slots (written by a commit copy of some phase).
    let mut is_state = vec![false; p.slot_count as usize];
    for ph in phases_ {
        for c in &ph.cx.body().commits {
            if let Commit::Copy { state, .. } = *c {
                is_state[state as usize] = true;
            }
        }
    }
    let prefixes: Vec<(Body, Vec<bool>)> = phases_
        .iter()
        .zip(layouts)
        .map(|(ph, l)| {
            ph.cx
                .prefix(&l.fields.iter().map(|f| f.slot).collect::<Vec<_>>())
        })
        .collect();
    let mut cache: HashMap<(usize, u64, Vec<(u32, u64, u64)>), Body> = HashMap::new();
    let mut spec_ex = |k: usize, key: u64, extra: &[(u32, Known)], row: &[u64]| -> usize {
        let id: Vec<(u32, u64, u64)> = extra.iter().map(|(s, k)| (*s, k.zeros, k.ones)).collect();
        let b = cache.entry((k, key, id)).or_insert_with(|| {
            let mut assume = layouts[k].assume(key);
            assume.extend_from_slice(extra);
            guarded(p, phases_[k].cx.specialize(p, &assume, &prefixes[k].1))
        });
        executed(b, row)
    };
    // Per offset in the trace: executed ops (key only, key + carry, carry
    // alone without the key's prefix), ticks whose key the carry implies,
    // carried state bits.
    let l = len as usize;
    let mut key_only = vec![0f64; l];
    let mut with_carry = vec![0f64; l];
    let mut carry_only = vec![0f64; l];
    let mut implied = vec![0u64; l];
    let mut carried_bits = vec![0f64; l];
    let mut general = vec![0f64; l];
    let mut wrong = 0u64;
    let stride = 997u64; // co-prime with the period: every start phase
    for _ in 0..n {
        sim.run(stride);
        let mut rows = Vec::with_capacity(l);
        for _ in 0..l {
            sim.tick();
            let k = (sim.tick_count() % period) as usize;
            rows.push((
                k,
                (0..p.slot_count).map(|s| sim.read(s)).collect::<Vec<u64>>(),
            ));
        }
        let mut carry: HashMap<u32, Known> = HashMap::new();
        for (off, (k, row)) in rows.iter().enumerate() {
            let k = *k;
            let layout = &layouts[k];
            let key = layout.pack(row);
            let pre_ops = prefixes[k].0.ops() as f64;
            general[off] += executed(&phases_[k].general, row) as f64;
            let extra: Vec<(u32, Known)> = {
                let mut v: Vec<(u32, Known)> = carry
                    .iter()
                    .filter(|(s, kn)| kn.mask() & possible[**s as usize] != 0)
                    .map(|(s, kn)| (*s, *kn))
                    .collect();
                v.sort_by_key(|x| x.0);
                v
            };
            for (s, kn) in &extra {
                if row[*s as usize] & kn.mask() != kn.ones {
                    wrong += 1;
                }
            }
            carried_bits[off] += extra
                .iter()
                .map(|(s, kn)| (kn.mask() & possible[*s as usize]).count_ones() as f64)
                .sum::<f64>();
            let pre = if layout.fields.is_empty() {
                0.0
            } else {
                pre_ops
            };
            key_only[off] += pre + spec_ex(k, key, &[], row) as f64;
            with_carry[off] += pre + spec_ex(k, key, &extra, row) as f64;
            // Does the carry imply the key? Then the trace needs no check
            // (and no prefix) here.
            let assume: HashMap<u32, Known> = extra.iter().copied().collect();
            let implies = layout
                .fields
                .iter()
                .all(|f| assume.get(&f.slot).is_some_and(|kn| kn.knows(f.mask)));
            if implies {
                implied[off] += 1;
            }
            // Carry alone (a trace that checks only its entry key).
            carry_only[off] += {
                let fb = guarded(
                    p,
                    phases_[k]
                        .cx
                        .specialize(p, &extra, &vec![false; p.slot_count as usize]),
                );
                executed(&fb, row) as f64
            };
            // The next tick's carry: state after this settle and commit.
            let mut assume_now: HashMap<u32, Known> = carry.clone();
            for (s, kn) in layout.assume(key) {
                let e = assume_now.entry(s).or_insert(Known::UNKNOWN);
                *e = e.union(kn);
            }
            let body = phases_[k].cx.body();
            let kn = known_after(p, body, possible, &assume_now, k as u64);
            let mut next: HashMap<u32, Known> = HashMap::new();
            for (s, &st) in is_state.iter().enumerate() {
                if st {
                    next.insert(s as u32, kn[s]);
                }
            }
            for c in &body.commits {
                if let Commit::Copy { state, next: nx } = *c {
                    next.insert(state, kn[nx as usize]);
                }
            }
            carry = next;
        }
    }
    let nf = n as f64;
    println!(
        "\ncross-tick known bits: {n} traces of {len} fused ticks (starts at every phase), {:.1} s; carried facts the real state contradicts: {wrong}",
        t0.elapsed().as_secs_f64()
    );
    println!(
        "  {:>6} {:>9} {:>9} {:>11} {:>11} {:>9} {:>12}",
        "offset", "general", "key only", "key+carry", "carry only", "implied", "carried bits"
    );
    let (mut g, mut ko, mut kc) = (0.0, 0.0, 0.0);
    for off in 0..l {
        println!(
            "  {:>6} {:>9.1} {:>9.1} {:>11.1} {:>11.1} {:>8.1}% {:>12.1}",
            off,
            general[off] / nf,
            key_only[off] / nf,
            with_carry[off] / nf,
            carry_only[off] / nf,
            100.0 * implied[off] as f64 / nf,
            carried_bits[off] / nf
        );
        g += general[off];
        ko += key_only[off];
        kc += with_carry[off];
    }
    println!(
        "  per tick over the trace: general {:.1}, plan keys {:.1}, keys + cross-tick known bits {:.1} executed ops ({:+.1}% vs keys)",
        g / nf / len as f64,
        ko / nf / len as f64,
        kc / nf / len as f64,
        100.0 * (kc / ko - 1.0)
    );
}
