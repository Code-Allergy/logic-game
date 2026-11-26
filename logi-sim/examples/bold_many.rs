//! Measurements behind `docs/bold-ideas.md`, idea 6 (many consoles at
//! once): what a batched or bit-sliced simulator of many independent
//! instances of one program would cost and gain.
//!
//! ```sh
//! cargo run -p logi-sim --profile profiling --features jit --example bold_many -- \
//!     <workspace> [--lanes 64] [--sample 4000] [--throughput]
//! ```
//!
//! 1. **Op census.** Every instruction of the (default-compiled) program's
//!    clock-phase variants by opcode and result width (from
//!    `possible_bits`), with the cost per instance of the same tick in
//!    three layouts, from a per-op table that is an *estimate* (the table
//!    is in the source): scalar words as today (1 per instruction),
//!    word-SIMD across instances (AVX2: 4 lanes, AVX-512: 8 lanes), and
//!    bit-sliced (one `u64` per bit, 64 instances per word).
//! 2. **Footprint** of one instance: slots, state, memories as `Sim`
//!    keeps them (a `u64` per memory word), the same with packed bytes,
//!    and what is read-only (ROMs) and could be shared. The resident set
//!    growth per `Sim` is measured too.
//! 3. **Divergence** (`--lanes N`): N instances of the program with
//!    different start times and button schedules run in lock step; per
//!    tick, the executed operations of the guarded body in each instance
//!    (what the scalar runtime executes) against the operations a batch
//!    would run (a guarded region runs when its condition holds in *any*
//!    lane), and the number of distinct values of the decode word
//!    (`cpu6502/ref_decode/cw`) across lanes, the key of the CPU phase's
//!    specialized bodies.
//! 4. **Throughput** (`--throughput`): N independent `Sim`s (one shared
//!    JIT code) run round-robin on one thread, and on 1, 2 and 4 threads,
//!    in ticks per second: the aggregate of the status quo with no
//!    batching.
//!
//! Nothing here changes the simulator.

mod bold_common;

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

use logi_sim::compile::guard::{MIN_REGION, guard_with};
use logi_sim::compile::spec::possible_bits;
use logi_sim::compile::{Body, Commit, Op, Program, phases};
use logi_sim::sim::Sim;

fn width_of(bits: u64) -> u32 {
    64 - bits.leading_zeros()
}

/// Plane operations of one instruction for 64 bit-sliced instances (width
/// `w` of the result, `wa` and `wb` of the operands).
fn bitsliced(op: Op, w: u32, wa: u32, wb: u32) -> f64 {
    let (w, wa, wb) = (
        f64::from(w.max(1)),
        f64::from(wa.max(1)),
        f64::from(wb.max(1)),
    );
    match op {
        Op::Copy | Op::Not => w,
        Op::And | Op::Or | Op::Xor | Op::Nand | Op::Nor | Op::Xnor => w,
        Op::Switch => w + wa - 1.0,
        Op::Mux => 3.0 * w + wa - 1.0,
        Op::AddSum => 5.0 * w,
        Op::AddCarry => 3.0 * wa.max(wb),
        Op::Eq => 2.0 * wa,
        Op::LtU => 3.0 * wa,
        Op::ReduceOr | Op::ReduceAnd | Op::ReduceXor => wa,
        // Moving bits between planes: renaming only.
        Op::Shr | Op::Concat | Op::Sext | Op::Shl => 0.0,
        Op::ShlV => 3.0 * w,
        Op::Clock => 0.0,
        // One scalar access per lane, plus moving the address and the
        // data between planes and words.
        Op::MemRead => 64.0 * 5.0,
        Op::SrNext => 3.0 * w,
    }
}

/// Vector operations of one instruction across `lanes` instances in word
/// layout (an estimate of instruction counts: AVX2 has no unsigned 64-bit
/// compare, no 64-bit multiply-high and a gather costs about a vector op
/// per lane).
fn word_simd(op: Op, avx512: bool) -> f64 {
    match op {
        Op::Mux | Op::Switch | Op::ReduceOr => {
            if avx512 {
                1.0
            } else {
                2.0
            }
        }
        Op::LtU => {
            if avx512 {
                1.0
            } else {
                3.0
            }
        }
        Op::AddCarry => 4.0,
        Op::MemRead => {
            if avx512 {
                8.0
            } else {
                4.0
            }
        }
        _ => 1.0,
    }
}

struct Args {
    folder: String,
    lanes: usize,
    sample: u64,
    throughput: bool,
}

fn args() -> Result<Args, String> {
    let mut a = Args {
        folder: String::new(),
        lanes: 64,
        sample: 4000,
        throughput: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(x) = it.next() {
        let mut num = |what: &str| -> Result<u64, String> {
            it.next()
                .and_then(|v| v.parse().ok())
                .ok_or_else(|| format!("{what} needs a number"))
        };
        match x.as_str() {
            "--lanes" => a.lanes = num("--lanes")? as usize,
            "--sample" => a.sample = num("--sample")?,
            "--throughput" => a.throughput = true,
            f if a.folder.is_empty() => a.folder = f.to_string(),
            o => return Err(format!("unknown argument {o}")),
        }
    }
    if a.folder.is_empty() {
        return Err("usage: bold_many <workspace> [--lanes N] [--sample N] [--throughput]".into());
    }
    Ok(a)
}

fn census(p: &Program) {
    let ph = p.phases.as_ref().expect("phases");
    let pos = possible_bits(p);
    let w_of = |slot: u32| width_of(pos.get(slot as usize).copied().unwrap_or(u64::MAX));
    let per = f64::from(ph.period);
    // op -> (count, sum of width, bitsliced plane ops)
    let mut rows: BTreeMap<String, (f64, f64, f64, f64, f64)> = BTreeMap::new();
    let mut widths = [0f64; 4];
    let (mut scalar, mut bs, mut v4, mut v8) = (0.0, 0.0, 0.0, 0.0);
    for v in &ph.variants {
        for blk in &v.blocks {
            let op = blk.op;
            let (sa, sb, _) = op.slot_operands();
            for i in &v.instrs[blk.start as usize..(blk.start + blk.len) as usize] {
                let w = w_of(i.dst);
                let wa = if sa { w_of(i.a) } else { 0 };
                let wb = if sb { w_of(i.b) } else { 0 };
                let b = bitsliced(op, w, wa, wb);
                let e = rows.entry(format!("{op:?}")).or_default();
                e.0 += 1.0;
                e.1 += f64::from(w);
                e.2 += b;
                e.3 += word_simd(op, false);
                e.4 += word_simd(op, true);
                scalar += 1.0;
                bs += b;
                v4 += word_simd(op, false);
                v8 += word_simd(op, true);
                widths[match w {
                    0..=1 => 0,
                    2..=8 => 1,
                    9..=16 => 2,
                    _ => 3,
                }] += 1.0;
            }
        }
        for c in &v.commits {
            let (cost_bs, cost_w) = match *c {
                Commit::Copy { state, .. } => (f64::from(w_of(state)), 1.0),
                _ => (64.0 * 5.0, 4.0),
            };
            let e = rows.entry("commit".into()).or_default();
            e.0 += 1.0;
            e.1 += 0.0;
            e.2 += cost_bs;
            e.3 += cost_w;
            e.4 += cost_w;
            scalar += 1.0;
            bs += cost_bs;
            v4 += cost_w;
            v8 += cost_w;
        }
    }
    println!(
        "### Operation census (per tick, mean over {} clock phases, unguarded)\n",
        ph.period
    );
    println!(
        "| op | count | mean width | bit-sliced plane ops (64 instances) | AVX2 vector ops (4) | AVX-512 vector ops (8) |"
    );
    println!("|---|---:|---:|---:|---:|---:|");
    let mut sorted: Vec<_> = rows.into_iter().collect();
    sorted.sort_by(|a, b| b.1.0.total_cmp(&a.1.0));
    for (name, (n, w, b, a4, a8)) in &sorted {
        println!(
            "| {name} | {:.0} | {:.1} | {:.0} | {:.0} | {:.0} |",
            n / per,
            if *n > 0.0 { w / n } else { 0.0 },
            b / per,
            a4 / per,
            a8 / per
        );
    }
    let t = widths.iter().sum::<f64>();
    println!(
        "\nInstructions by result width: 1 bit {:.0}%, 2-8 bits {:.0}%, 9-16 bits {:.0}%, wider {:.0}%.\n",
        100.0 * widths[0] / t,
        100.0 * widths[1] / t,
        100.0 * widths[2] / t,
        100.0 * widths[3] / t
    );
    println!("| layout | ops per tick per instance | relative to scalar |");
    println!("|---|---:|---:|");
    let rel = |x: f64| x / scalar;
    println!("| scalar words (today) | {:.0} | 1.00 |", scalar / per);
    println!(
        "| AVX2 word-SIMD, 4 lanes | {:.0} | {:.2} |",
        v4 / per / 4.0,
        rel(v4 / 4.0)
    );
    println!(
        "| AVX-512 word-SIMD, 8 lanes | {:.0} | {:.2} |",
        v8 / per / 8.0,
        rel(v8 / 8.0)
    );
    println!(
        "| bit-sliced, 64 lanes | {:.0} | {:.2} |",
        bs / per / 64.0,
        rel(bs / 64.0)
    );
    println!(
        "\nThese are instruction counts under the cost table in the source, not time: a vector op, a plane op and a scalar op cost about the same on one core, but gathers, scatters, transposes and cache misses are not modelled beyond the table.\n"
    );
}

fn rss_kib() -> u64 {
    std::fs::read_to_string("/proc/self/statm")
        .ok()
        .and_then(|s| {
            s.split_whitespace()
                .nth(1)
                .and_then(|x| x.parse::<u64>().ok())
        })
        .map_or(0, |pages| pages * 4)
}

fn new_sims(p: &Arc<Program>, n: usize) -> Vec<Sim> {
    #[cfg(feature = "jit")]
    {
        let o = logi_sim::sim::SimOptions::jit().tier_layout();
        let first = Sim::with_options(Arc::clone(p), o.clone());
        let code = first.compile_jit().ok().map(Arc::new);
        let mut v = vec![first];
        while v.len() < n {
            v.push(Sim::with_options(Arc::clone(p), o.clone()));
        }
        for s in &mut v {
            s.install_jit(code.clone());
        }
        v
    }
    #[cfg(not(feature = "jit"))]
    {
        (0..n).map(|_| Sim::new(Arc::clone(p))).collect()
    }
}

fn footprint(p: &Arc<Program>) {
    let mut writable = (0u64, 0u64);
    let mut rom = (0u64, 0u64);
    for m in &p.mems {
        let words = 1u64 << m.addr_bits;
        let bytes = words * u64::from(m.width).div_ceil(8).next_power_of_two().min(8);
        let t = if m.writable { &mut writable } else { &mut rom };
        t.0 += words * 8;
        t.1 += bytes;
    }
    let slots = u64::from(p.slot_count) * 8;
    let state = p.state_keys.len() as u64 * 8;
    let before = rss_kib();
    let mut sims = new_sims(p, 8);
    for s in &mut sims {
        s.run(100_000);
    }
    let after = rss_kib();
    println!("### Footprint of one instance\n");
    println!("| part | bytes as `Sim` keeps it | bytes if packed to the word width |");
    println!("|---|---:|---:|");
    println!("| slots ({} of them) | {slots} | {slots} |", p.slot_count);
    println!(
        "| state words ({}), a subset of the slots | {state} | |",
        p.state_keys.len()
    );
    println!("| writable memories | {} | {} |", writable.0, writable.1);
    println!(
        "| read-only memories (ROMs, shareable between instances) | {} | {} |",
        rom.0, rom.1
    );
    println!(
        "| total, ROMs copied per instance | {} | {} |",
        slots + writable.0 + rom.0,
        slots + writable.1 + rom.1
    );
    println!(
        "| total, ROMs shared | {} | {} |\n",
        slots + writable.0,
        slots + writable.1
    );
    println!(
        "Resident set growth per `Sim` when 8 are created and run (JIT code shared): {} KiB.\n",
        (after.saturating_sub(before)) / 8
    );
}

/// Executed operations of a guarded body on settled slots (commits,
/// unguarded instructions, and guarded ranges whose conditions hold),
/// and which guards held.
fn executed(body: &Body, slots: &[u64], held: &mut [bool]) -> usize {
    let len =
        |r: std::ops::Range<usize>| body.blocks[r].iter().map(|b| b.len as usize).sum::<usize>();
    let mut n = body.commits.len();
    let mut at = 0usize;
    for (k, g) in body.guards.iter().enumerate() {
        n += len(at..g.start as usize);
        if g.holds(slots) {
            held[k] = true;
            n += len(g.start as usize..g.end as usize);
        }
        at = g.end as usize;
    }
    n + len(at..body.blocks.len())
}

fn guard_len(body: &Body, k: usize) -> usize {
    let g = &body.guards[k];
    body.blocks[g.start as usize..g.end as usize]
        .iter()
        .map(|b| b.len as usize)
        .sum()
}

fn divergence(l: &bold_common::Loaded, a: &Args) -> Result<(), String> {
    let p = &l.program;
    let ph = p.phases.as_ref().ok_or("no phases")?;
    let bodies: Vec<Body> = ph
        .variants
        .iter()
        .map(|v| {
            let fused = phases::prune(p, v).unwrap_or_else(|| v.clone());
            guard_with(p.slot_count as usize, &fused, None, MIN_REGION).unwrap_or(fused)
        })
        .collect();
    let cw = l.slot("cpu6502/ref_decode/cw").ok();
    let mut sims = new_sims(p, a.lanes);
    // Different start times and button schedules: lane i has run
    // 1.2M + i * 53,001 ticks, pressing Start at different frames.
    let presses = ["p1_start", "p1_a", "p1_right", "p1_b"];
    for (i, s) in sims.iter_mut().enumerate() {
        let start = 1_200_000 + i as u64 * 53_001;
        let btn = presses[i % presses.len()];
        let mut at = 0;
        while at < start {
            let step = 89_342.min(start - at);
            let frame = at / 89_342;
            if frame == 20 + (i as u64 % 7) * 11 {
                s.set(btn, 1);
            } else if frame == 22 + (i as u64 % 7) * 11 {
                s.set(btn, 0);
            }
            s.run(step);
            at += step;
        }
    }
    let n_lanes = a.lanes;
    let mut slots: Vec<Vec<u64>> = vec![vec![0; p.slot_count as usize]; n_lanes];
    let mut scalar = 0f64;
    let mut distinct = BTreeMap::<usize, u64>::new();
    let mut distinct_sum = 0f64;
    let mut ticks = 0f64;
    let sizes = [1usize, 4, 8, 16, 64];
    let mut union_by_size = vec![0f64; sizes.len()];
    for _ in 0..a.sample {
        for s in sims.iter_mut() {
            s.tick();
        }
        let k = (sims[0].tick_count() % u64::from(ph.period)) as usize;
        let body = &bodies[k];
        let mut held_all = vec![vec![false; body.guards.len()]; n_lanes];
        let mut sum = 0f64;
        for (lane, s) in sims.iter().enumerate() {
            for (sl, v) in slots[lane].iter_mut().enumerate() {
                *v = s.read(sl as u32);
            }
            sum += executed(body, &slots[lane], &mut held_all[lane]) as f64;
        }
        scalar += sum / n_lanes as f64;
        // Union over lanes in groups of each size.
        let unguarded = executed(body, &slots[0], &mut vec![false; body.guards.len()]) as f64
            - (0..body.guards.len())
                .filter(|&g| held_all[0][g])
                .map(|g| guard_len(body, g) as f64)
                .sum::<f64>();
        for (si, &size) in sizes.iter().enumerate() {
            let size = size.min(n_lanes);
            let mut tot = 0f64;
            let mut groups = 0f64;
            for chunk in held_all.chunks(size) {
                let mut ops = unguarded;
                for g in 0..body.guards.len() {
                    if chunk.iter().any(|h| h[g]) {
                        ops += guard_len(body, g) as f64;
                    }
                }
                tot += ops;
                groups += 1.0;
            }
            union_by_size[si] += tot / groups;
        }
        if let Some(c) = cw {
            let d: HashSet<u64> = sims.iter().map(|s| s.read(c)).collect();
            *distinct.entry(d.len()).or_default() += 1;
            distinct_sum += d.len() as f64;
        }
        ticks += 1.0;
    }
    println!(
        "### Divergence over {n_lanes} instances ({} lock-step ticks)\n",
        a.sample
    );
    println!(
        "Executed operations per tick of the guarded fused body: {:.0} per instance on average (what the scalar runtime executes).\n",
        scalar / ticks
    );
    println!(
        "| lanes per batch | ops per tick per batch if a guarded region runs when any lane needs it | per-instance executed (scalar) | batch / scalar |"
    );
    println!("|---:|---:|---:|---:|");
    for (si, &size) in sizes.iter().enumerate() {
        println!(
            "| {} | {:.0} | {:.0} | {:.2}x |",
            size.min(n_lanes),
            union_by_size[si] / ticks,
            scalar / ticks,
            union_by_size[si] / scalar
        );
    }
    if cw.is_some() {
        println!(
            "\nDistinct values of the CPU decode word `cw` across the {n_lanes} lanes: mean {:.1} per tick (the number of specialized bodies a batch would need at once).\n",
            distinct_sum / ticks
        );
    }
    Ok(())
}

fn throughput(p: &Arc<Program>) {
    println!("### Aggregate throughput of independent `Sim`s (JIT, shared code)\n");
    println!(
        "| instances | threads | M ticks/s aggregate | M ticks/s per instance | real-time consoles (5.37 M ticks/s each) |"
    );
    println!("|---:|---:|---:|---:|---:|");
    let ticks = 400_000u64;
    // One warmed-up state (past the boot, steady state of the game), copied
    // into every instance: same work per tick in each, separate memory.
    let mut base = new_sims(p, 1).remove(0);
    base.run(9_000_000);
    let snap = base.export_state();
    for (n, threads) in [
        (1usize, 1usize),
        (4, 1),
        (16, 1),
        (64, 1),
        (4, 4),
        (16, 4),
        (64, 4),
    ] {
        let mut sims = new_sims(p, n);
        for s in &mut sims {
            s.import_state(&snap);
        }
        let t = Instant::now();
        let chunk = n.div_ceil(threads);
        std::thread::scope(|sc| {
            for part in sims.chunks_mut(chunk) {
                sc.spawn(move || {
                    // Round-robin in slices of 20,000 ticks.
                    let mut done = 0;
                    while done < ticks {
                        for s in part.iter_mut() {
                            s.run(20_000);
                        }
                        done += 20_000;
                    }
                });
            }
        });
        let secs = t.elapsed().as_secs_f64();
        let agg = (n as u64 * ticks) as f64 / secs / 1e6;
        println!(
            "| {n} | {threads} | {agg:.2} | {:.2} | {:.2} |",
            agg / n as f64,
            agg / 5.37
        );
    }
    println!();
}

fn main() -> Result<(), String> {
    let a = args()?;
    let l = bold_common::load_with(&a.folder, false)?;
    println!("## {}\n", a.folder);
    census(&l.program);
    footprint(&l.program);
    divergence(&l, &a)?;
    if a.throughput {
        throughput(&l.program);
    }
    Ok(())
}
