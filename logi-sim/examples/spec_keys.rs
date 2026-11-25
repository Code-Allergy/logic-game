//! State-keyed specialization report (issue #117, `docs/adaptive-spec.md`):
//! the key candidates the compile-side analysis finds in each fused-run
//! body of a workspace's design, their widths (whole slot, exact support,
//! fold support), and the operations each would save per tick, from the
//! states a run gives them. Then a greedy combined key per phase, and the
//! prototype's hand-picked keys (`cpu6502/ref_decode/cw` on the CPU
//! phase; `ppu_render_ctl/*`, `ppu_sprites/{fetching,clr_oam_addr,set_ovf}`
//! and `ppu_regs/{mask,ctrl}` on the others), whole and masked, including
//! whether bodies built in one scene still hit in another.
//!
//! ```sh
//! cargo run -p logi-sim --release --example spec_keys -- nes/workspace \
//!     [--demo] [--warm 1500000] [--sample 600000] [--bodies 64] [--top 12] [--min-loss 2] [--check 300]
//! ```
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
    Candidate, KeyClass, KeyField, KeyLayout, SpecContext, possible_bits,
};
use logi_sim::compile::{Body, Commit, Instr, Op, Options, Program, compile, phases};
use logi_sim::sim::Sim;

struct Args {
    folder: String,
    demo: bool,
    warm: u64,
    sample: u64,
    bodies: usize,
    top: usize,
    min_loss: f64,
    check: usize,
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
    let mut saved = 0.0;
    let mut greedy: Vec<KeyLayout> = Vec::new();
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
        // Greedy combination of the best candidates.
        let pool: Vec<usize> = scored
            .iter()
            .take(20)
            .filter(|s| s.1.executed > 0.0)
            .map(|s| s.0)
            .collect();
        let mut fields: Vec<KeyField> = Vec::new();
        let mut best: Option<Outcome> = None;
        loop {
            let mut pick: Option<(usize, Outcome)> = None;
            for &i in &pool {
                let c = &phase.cands[i];
                if fields.iter().any(|f| f.slot == c.slot) {
                    continue;
                }
                let mut f = fields.clone();
                f.push(KeyField {
                    slot: c.slot,
                    mask: c.support,
                });
                let layout = KeyLayout::new(f);
                if layout.width() > 32 {
                    continue;
                }
                let r = evaluate(p, phase, &layout, &phase.rows, a.bodies, None);
                if pick.as_ref().is_none_or(|x| r.executed > x.1.executed) {
                    pick = Some((i, r));
                }
            }
            match pick {
                Some((i, r)) if r.executed > best.as_ref().map_or(0.0, |b| b.executed) + 2.0 => {
                    let c = &phase.cands[i];
                    fields.push(KeyField {
                        slot: c.slot,
                        mask: c.support,
                    });
                    best = Some(r);
                }
                _ => break,
            }
        }
        let layout = KeyLayout::new(fields);
        if let Some(b) = &best {
            println!(
                "  greedy key ({} fields, {} bits): {:.1} executed ops/tick saved ({:.1} static), {} distinct keys, {:.1}% of ticks hit the {} bodies",
                layout.fields.len(),
                layout.width(),
                b.executed,
                b.stat,
                b.distinct,
                b.hit * 100.0,
                b.keys.len()
            );
            saved += b.executed;
        }
        for f in &layout.fields {
            println!("    {} mask {:#x}", name(&names, f.slot), f.mask);
        }
        greedy.push(layout);
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
        "\ngreedy keys: {:.1} of {ex:.1} executed ops per tick saved ({:.0}%); selection {:.1} s",
        saved / period,
        100.0 * saved / period / ex,
        t3.elapsed().as_secs_f64()
    );

    if a.check > 0 {
        let t4 = Instant::now();
        let n = check_exact(&program, &phases_, &greedy, a.warm, a.check, false)?;
        // The check catches a wrong body.
        match check_exact(&program, &phases_, &greedy, a.warm, a.check, true) {
            Err(e) => println!("mutation (bodies for the wrong key) caught: {e}"),
            Ok(_) => return Err("a wrong key's bodies passed the exactness check".into()),
        }
        println!(
            "exactness: {n} fused ticks of the run, the body with no key, the greedy keys' bodies and their fallbacks against the general body: the same state and memories ({:.1} s)",
            t4.elapsed().as_secs_f64()
        );
    }

    hand_picked(p, &w, &names, &phases_, &possible, &a);
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
        Op::Shl => ((a >> i.c) << i.b) & imm,
        Op::ShlV => (a << (b() & 63)) & imm,
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
