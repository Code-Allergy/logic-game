//! Measurements behind `docs/bold-ideas.md`, ideas 3 and 4: how often the
//! CPU and the PPU of the gate-level NES interact (speculative partitioned
//! execution), and how much CPU time is idle (skipping idle stretches).
//!
//! ```sh
//! cargo run -p logi-sim --profile profiling --features jit --example bold_trace -- \
//!     <workspace> [--frames 300] [--skip-frames 0] [--changes 4] [--windows 3] \
//!     [--press p1_start:100:4,...]
//! ```
//!
//! One run ticks the workspace's console (`Sim::tick`, so every net is
//! readable) for `--frames` PPU frames (a frame is 262 lines of 341 ticks)
//! after `--skip-frames` and prints markdown tables:
//!
//! 1. **Interactions** (idea 3). Bus events seen at the PPU's register
//!    port (`ppu_ref/cs`, `ra`, `rw`), the CPU's bus (`cpu6502/*`), the
//!    DMA unit (`oam_dma_ref/own`), the MMC3 (`cart_mmc3/irq_n`, CHR
//!    address bit 12) and the PPU's `nmi_n`: events per frame, and the
//!    distribution of ticks between consecutive events of a class. Class
//!    A is what the CPU side sends to the PPU side or changes there
//!    (register writes, DMA start, mapper writes, and the reads of $2002 and
//!    $2007, which clear the vblank flag and the write toggle or advance the
//!    address); class B is what comes back (register reads, NMI, IRQ). The horizon table gives the share of ticks whose
//!    distance to the next event of the class is at least H: how long the
//!    other side could have run ahead from there.
//! 2. **Idle loops** (idea 4a). Instructions are found by `cpu6502/sync`
//!    and their PCs; a repeating lap of at most 16 instructions (every
//!    instruction equal to the one a lap earlier) is an idle-loop
//!    candidate, classified by what it touches (data reads and writes,
//!    operand fetches excluded). Ticks per class, and per episode length.
//! 3. **State changes** (idea 4b), `--changes N` frames in each of
//!    `--windows` places: per tick, which state words changed, grouped by
//!    the root's child they belong to, and how many changed by exactly
//!    +1 (counters); and whether the CPU-side state alone repeats
//!    exactly with a period of at most 96 ticks (state-level loop
//!    detection, with and without RAM writes).
//!
//! `--press input:frame:frames` holds a root input (a `p1_*` button) at 1
//! from that frame (counted from reset) for that many frames, to get past a
//! title screen.
//!
//! The root circuit must have the layout of `nes/` (names above). The
//! numbers are measurements of the committed ROM workspaces; nothing here
//! changes the simulator.

mod bold_common;

use std::collections::BTreeMap;
use std::sync::Arc;

use bold_common::Loaded;
use logi_sim::compile::{Program, StateLoc};
use logi_sim::sim::{Sim, SimOptions};

const KINDS: [&str; 22] = [
    "W $2000",
    "W $2001",
    "W $2002",
    "W $2003",
    "W $2004",
    "W $2005",
    "W $2006",
    "W $2007",
    "R $2000",
    "R $2001",
    "R $2002",
    "R $2003",
    "R $2004",
    "R $2005",
    "R $2006",
    "R $2007",
    "DMA start ($4014 write)",
    "mapper write ($8000+)",
    "NMI edge",
    "IRQ edge",
    "DMA data (to $2004)",
    "CHR A12 rising",
];
const W0: usize = 0;
const R0: usize = 8;
const DMA_START: usize = 16;
const MAP_W: usize = 17;
const NMI: usize = 18;
const IRQ: usize = 19;
const DMA_DATA: usize = 20;
const A12: usize = 21;

struct Args {
    folder: String,
    frames: u64,
    skip: u64,
    changes: u64,
    windows: u64,
    /// (input, first frame, frames held): a root input held at 1.
    presses: Vec<(String, u64, u64)>,
}

fn args() -> Result<Args, String> {
    let mut it = std::env::args().skip(1);
    let mut a = Args {
        folder: String::new(),
        frames: 300,
        skip: 0,
        changes: 4,
        windows: 3,
        presses: Vec::new(),
    };
    while let Some(x) = it.next() {
        let mut num = |what: &str| -> Result<u64, String> {
            it.next()
                .and_then(|v| v.parse().ok())
                .ok_or_else(|| format!("{what} needs a number"))
        };
        match x.as_str() {
            "--frames" => a.frames = num("--frames")?,
            "--skip-frames" => a.skip = num("--skip-frames")?,
            "--changes" => a.changes = num("--changes")?,
            "--windows" => a.windows = num("--windows")?,
            "--press" => {
                let v = it.next().ok_or("--press needs input:frame:frames[,...]")?;
                for item in v.split(',') {
                    let f: Vec<&str> = item.split(':').collect();
                    let [name, from, len] = f[..] else {
                        return Err(format!("bad --press item {item}"));
                    };
                    a.presses.push((
                        name.to_string(),
                        from.parse().map_err(|e| format!("{e}"))?,
                        len.parse().map_err(|e| format!("{e}"))?,
                    ));
                }
            }
            f if a.folder.is_empty() => a.folder = f.to_string(),
            o => return Err(format!("unknown argument {o}")),
        }
    }
    if a.folder.is_empty() {
        return Err("usage: bold_trace <workspace> [--frames N] [--skip-frames N] [--changes N] [--windows N] [--press input:frame:frames,...]".into());
    }
    Ok(a)
}

fn new_sim(p: &Arc<Program>) -> Sim {
    #[cfg(feature = "jit")]
    let o = SimOptions::jit();
    #[cfg(not(feature = "jit"))]
    let o = SimOptions::default();
    Sim::with_options(Arc::clone(p), o)
}

/// Set the held inputs for `frame` (the first tick of a frame).
fn apply_presses(sim: &mut Sim, presses: &[(String, u64, u64)], frame: u64) {
    for (name, from, len) in presses {
        if frame == *from {
            sim.set(name, 1);
        } else if frame == from + len {
            sim.set(name, 0);
        }
    }
}

struct Sigs {
    ce: u32,
    own: u32,
    cpu_rw: u32,
    addr: u32,
    sync: u32,
    cs: u32,
    ra: u32,
    ppu_rw: u32,
    nmi_n: u32,
    irq_n: Option<u32>,
    pa: u32,
    line: u32,
    mask: u32,
}

impl Sigs {
    fn find(l: &Loaded) -> Result<Sigs, String> {
        Ok(Sigs {
            ce: l.slot("cpu6502/ce")?,
            own: l.slot("oam_dma_ref/own")?,
            cpu_rw: l.slot("cpu6502/rw")?,
            addr: l.slot("cpu6502/addr")?,
            sync: l.slot("cpu6502/sync")?,
            cs: l.slot("ppu_ref/cs")?,
            ra: l.slot("ppu_ref/ra")?,
            ppu_rw: l.slot("ppu_ref/rw")?,
            nmi_n: l.slot("ppu_ref/nmi_n")?,
            irq_n: l.slot("cart_mmc3/irq_n").ok(),
            pa: l.slot("ppu_ref/pa")?,
            line: l.slot("ppu_ref/line")?,
            mask: l.slot("ppu_ref/ppu_regs/mask")?,
        })
    }
}

/// Address categories of data accesses (bit set per instruction).
fn cat(a: u16) -> u16 {
    match a {
        0x2002 => 1,
        0x2000..=0x3FFF => 2,
        0x4000..=0x401F => 4,
        0x0100..=0x01FF => 16,
        0x0000..=0x1FFF => 8,
        0x6000..=0x7FFF => 32,
        0x8000..=0xFFFF => 64,
        _ => 128,
    }
}

#[derive(Clone, Copy)]
struct Instr {
    tick: u64,
    pc: u16,
    /// Data read categories (operand fetches excluded).
    reads: u16,
    writes: u16,
    nwrites: u16,
    /// PPU was rendering when the instruction began.
    rendering: bool,
}

struct Trace {
    /// (tick, kind, line)
    events: Vec<(u64, u8, u16)>,
    instrs: Vec<Instr>,
    dma_ticks: Vec<u64>,
    ticks: u64,
    frames: u64,
    rendering_ticks: u64,
    cpu_cycles: u64,
}

fn collect(l: &Loaded, a: &Args) -> Result<Trace, String> {
    let s = Sigs::find(l)?;
    let mut sim = new_sim(&l.program);
    let mut t = Trace {
        events: Vec::new(),
        instrs: Vec::new(),
        dma_ticks: Vec::new(),
        ticks: 0,
        frames: 0,
        rendering_ticks: 0,
        cpu_cycles: 0,
    };
    let mut prev_line = u64::MAX;
    let (mut prev_nmi, mut prev_irq, mut prev_a12, mut prev_own) = (1, 1, 0, 0);
    let mut dma_start = 0;
    let mut frame = 0u64;
    let mut origin = 0u64;
    let total = a.skip + a.frames;
    while frame < total {
        sim.tick();
        let now = sim.tick_count();
        let line = sim.read(s.line);
        if line == 0 && prev_line != 0 {
            frame += 1;
            apply_presses(&mut sim, &a.presses, frame);
            if frame == a.skip {
                origin = now;
            }
        }
        prev_line = line;
        if frame < a.skip {
            continue;
        }
        t.ticks = now - origin;
        let ce = sim.read(s.ce) != 0;
        let own = sim.read(s.own) as u8;
        let rendering = sim.read(s.mask) & 0x18 != 0;
        t.rendering_ticks += u64::from(rendering);
        let line16 = line as u16;
        let t_rel = now - origin;
        let mut ev = |k: usize| t.events.push((t_rel, k as u8, line16));
        if sim.read(s.cs) != 0 {
            let ra = sim.read(s.ra) as usize;
            let read = sim.read(s.ppu_rw) != 0;
            if own != 0 {
                if !read {
                    ev(DMA_DATA);
                }
            } else {
                ev(if read { R0 } else { W0 } + ra);
            }
        }
        let nmi = sim.read(s.nmi_n);
        if prev_nmi == 1 && nmi == 0 {
            ev(NMI);
        }
        prev_nmi = nmi;
        let irq = s.irq_n.map_or(1, |x| sim.read(x));
        if prev_irq == 1 && irq == 0 {
            ev(IRQ);
        }
        prev_irq = irq;
        let a12 = (sim.read(s.pa) >> 12) & 1;
        if prev_a12 == 0 && a12 == 1 {
            ev(A12);
        }
        prev_a12 = a12;
        if own != 0 && prev_own == 0 {
            dma_start = t_rel;
        }
        if own == 0 && prev_own != 0 {
            t.dma_ticks.push(t_rel - dma_start);
        }
        prev_own = own;
        if ce && own == 0 {
            t.cpu_cycles += 1;
            let addr = sim.read(s.addr) as u16;
            let read = sim.read(s.cpu_rw) != 0;
            if sim.read(s.sync) != 0 {
                t.instrs.push(Instr {
                    tick: t_rel,
                    pc: addr,
                    reads: 0,
                    writes: 0,
                    nwrites: 0,
                    rendering,
                });
            } else if let Some(i) = t.instrs.last_mut() {
                if read {
                    // Operand fetches and dummy reads are within the
                    // instruction's own bytes.
                    if !(i.pc..=i.pc.wrapping_add(2)).contains(&addr) {
                        i.reads |= cat(addr);
                    }
                } else {
                    i.writes |= cat(addr);
                    i.nwrites += 1;
                    if addr == 0x4014 {
                        ev(DMA_START);
                    } else if addr >= 0x8000 && s.irq_n.is_some() {
                        ev(MAP_W);
                    }
                }
            }
        }
    }
    t.frames = a.frames;
    Ok(t)
}

fn pct(v: &[u64], p: f64) -> u64 {
    if v.is_empty() {
        return 0;
    }
    v[((v.len() - 1) as f64 * p) as usize]
}

fn gaps(times: &[u64]) -> Vec<u64> {
    let mut g: Vec<u64> = times.windows(2).map(|w| w[1] - w[0]).collect();
    g.sort_unstable();
    g
}

fn interactions(t: &Trace) {
    let f = t.frames as f64;
    println!(
        "### Events per frame ({} frames, {} ticks, {} CPU cycles)\n",
        t.frames, t.ticks, t.cpu_cycles
    );
    println!(
        "PPU mask has background or sprites on in {:.1}% of ticks; {} OAM DMAs ({} ticks each, median).\n",
        100.0 * t.rendering_ticks as f64 / t.ticks as f64,
        t.dma_ticks.len(),
        {
            let mut d = t.dma_ticks.clone();
            d.sort_unstable();
            pct(&d, 0.5)
        }
    );
    println!("| event | per frame | in vblank | median gap | p10 gap | min gap |");
    println!("|---|---:|---:|---:|---:|---:|");
    for (k, name) in KINDS.iter().enumerate() {
        let times: Vec<u64> = t
            .events
            .iter()
            .filter(|e| e.1 as usize == k)
            .map(|e| e.0)
            .collect();
        if times.is_empty() {
            continue;
        }
        let vb = t
            .events
            .iter()
            .filter(|e| e.1 as usize == k && (241..=260).contains(&e.2))
            .count();
        let g = gaps(&times);
        println!(
            "| {} | {:.2} | {:.0}% | {} | {} | {} |",
            name,
            times.len() as f64 / f,
            100.0 * vb as f64 / times.len() as f64,
            pct(&g, 0.5),
            pct(&g, 0.1),
            g.first().copied().unwrap_or(0)
        );
    }
    // Classes. A DMA's data writes are folded into one event at its start.
    let classes: [(&str, Vec<usize>); 5] = [
        (
            "A: CPU->PPU (writes, DMA start, mapper writes, state-changing reads $2002/$2007)",
            (W0..W0 + 8)
                .chain([DMA_START, MAP_W, R0 + 2, R0 + 7])
                .collect(),
        ),
        (
            "A': PPU register accesses that change PPU state (writes, $2002/$2007 reads)",
            (W0..W0 + 8).chain([R0 + 2, R0 + 7]).collect(),
        ),
        (
            "B: PPU->CPU (reads, NMI, IRQ)",
            (R0..R0 + 8).chain([NMI, IRQ]).collect(),
        ),
        (
            "B': reads of $2002/$2004/$2007",
            vec![R0 + 2, R0 + 4, R0 + 7],
        ),
        ("B'': NMI and IRQ only", vec![NMI, IRQ]),
    ];
    let mut all = vec![(
        "A or B",
        (W0..W0 + 16)
            .chain([DMA_START, MAP_W, NMI, IRQ])
            .collect::<Vec<_>>(),
    )];
    let _ = A12;
    all.extend(classes.iter().map(|(n, k)| (*n, k.clone())));
    println!("\n### Distance to the next event of a class\n");
    println!(
        "Share of ticks whose distance to the next event of the class is at least H (the side that is not\nsending could run H ticks ahead from there), and events per frame.\n"
    );
    let hs = [100u64, 300, 1000, 3000, 10_000, 30_000];
    print!("| class | events/frame | median gap |");
    for h in hs {
        print!(" H={h} |");
    }
    println!();
    print!("|---|---:|---:|");
    for _ in hs {
        print!("---:|");
    }
    println!();
    for (name, kinds) in &all {
        let times: Vec<u64> = t
            .events
            .iter()
            .filter(|e| kinds.contains(&(e.1 as usize)))
            .map(|e| e.0)
            .collect();
        let g = gaps(&times);
        print!(
            "| {name} | {:.1} | {} |",
            times.len() as f64 / f,
            pct(&g, 0.5)
        );
        for h in hs {
            // Time (ticks) inside gaps where the remaining distance is >= h.
            let covered: u64 = g.iter().map(|&x| x.saturating_sub(h)).sum();
            print!(" {:.0}% |", 100.0 * covered as f64 / t.ticks as f64);
        }
        println!();
    }
    // Rollback waste bound for the PPU running ahead of the CPU by at most H.
    println!("\n### PPU ahead of the CPU, rolled back at every class-A event\n");
    println!(
        "Upper bound of the discarded PPU work: each event discards at most min(H, gap since the previous event)\nticks. Waste is that sum over all events, as a share of the ticks.\n"
    );
    println!("| lead H | waste, A | waste, A' |");
    println!("|---:|---:|---:|");
    let a_times: Vec<u64> = t
        .events
        .iter()
        .filter(|e| classes[0].1.contains(&(e.1 as usize)))
        .map(|e| e.0)
        .collect();
    let a2_times: Vec<u64> = t
        .events
        .iter()
        .filter(|e| classes[1].1.contains(&(e.1 as usize)))
        .map(|e| e.0)
        .collect();
    for h in [300u64, 1000, 3000, 10_000, 30_000] {
        let w = |ts: &[u64]| -> f64 {
            let mut prev = 0;
            let mut sum = 0;
            for &x in ts {
                sum += (x - prev).min(h);
                prev = x;
            }
            100.0 * sum as f64 / t.ticks as f64
        };
        println!("| {h} | {:.0}% | {:.0}% |", w(&a_times), w(&a2_times));
    }
    // Where class-A events are within the frame.
    let mut by_line: BTreeMap<&str, u64> = BTreeMap::new();
    for e in t
        .events
        .iter()
        .filter(|e| classes[0].1.contains(&(e.1 as usize)))
    {
        let r = match e.2 {
            0..=239 => "visible lines 0-239",
            240 => "line 240",
            241..=260 => "vblank 241-260",
            _ => "pre-render 261",
        };
        *by_line.entry(r).or_default() += 1;
    }
    println!(
        "\nClass-A events by place in the frame (per frame): {}\n",
        by_line
            .iter()
            .map(|(k, v)| format!("{k}: {:.1}", *v as f64 / f))
            .collect::<Vec<_>>()
            .join(", ")
    );
}

const CLASSES: [&str; 7] = [
    "register loop (no memory access)",
    "poll PPUSTATUS $2002",
    "poll joypad/APU $40xx",
    "poll RAM/stack (flag)",
    "ROM/PRG-RAM table loop",
    "loop with writes",
    "other",
];

fn class_of(reads: u16, writes: u16) -> usize {
    if writes != 0 {
        5
    } else if reads == 0 {
        0
    } else if reads & 1 != 0 {
        1
    } else if reads & 4 != 0 {
        2
    } else if reads & (8 | 16) != 0 && reads & !(8 | 16) == 0 {
        3
    } else if reads & (32 | 64) != 0 {
        4
    } else {
        6
    }
}

fn idle_loops(t: &Trace) {
    let n = t.instrs.len();
    println!("\n### Repeating instruction laps (idea 4a)\n");
    println!(
        "{n} instructions in {} ticks ({:.2} ticks per instruction).\n",
        t.ticks,
        t.ticks as f64 / n as f64
    );
    let mut run = [0u32; 17];
    // Prefix sums of reads/writes masks are not additive: keep per-lap
    // masks by scanning the lap when the lap is first confirmed (cheap:
    // at most 16 instructions).
    let mut cls_ticks = [0u64; 7];
    let mut cls_ticks_idle_ppu = [0u64; 7];
    let mut episode: Option<(usize, usize, u64)> = None; // (lag, class, ticks)
    let mut episodes: Vec<(usize, u64)> = Vec::new(); // (class, ticks)
    let dur = |i: usize| -> u64 {
        if i + 1 < n {
            t.instrs[i + 1].tick - t.instrs[i].tick
        } else {
            0
        }
    };
    for i in 0..n {
        let mut lag = 0;
        for (l, r) in run.iter_mut().enumerate().skip(1) {
            if i >= l && t.instrs[i].pc == t.instrs[i - l].pc {
                *r += 1;
            } else {
                *r = 0;
            }
            if lag == 0 && *r as usize >= l {
                lag = l;
            }
        }
        if lag == 0 {
            if let Some((_, c, d)) = episode.take() {
                episodes.push((c, d));
            }
            continue;
        }
        let (mut rd, mut wr) = (0, 0);
        for ins in &t.instrs[i + 1 - lag..=i] {
            rd |= ins.reads;
            wr |= ins.writes;
        }
        let c = class_of(rd, wr);
        let d = dur(i);
        cls_ticks[c] += d;
        if !t.instrs[i].rendering {
            cls_ticks_idle_ppu[c] += d;
        }
        match &mut episode {
            Some((l, ec, ed)) if *l == lag && *ec == c => *ed += d,
            _ => {
                if let Some((_, c0, d0)) = episode.take() {
                    episodes.push((c0, d0));
                }
                episode = Some((lag, c, d));
            }
        }
    }
    if let Some((_, c, d)) = episode {
        episodes.push((c, d));
    }
    println!(
        "| loop class | share of ticks | of which PPU not rendering | episodes | median episode (ticks) | p90 episode |"
    );
    println!("|---|---:|---:|---:|---:|---:|");
    let mut total = 0;
    for (c, name) in CLASSES.iter().enumerate() {
        let mut e: Vec<u64> = episodes.iter().filter(|x| x.0 == c).map(|x| x.1).collect();
        e.sort_unstable();
        total += cls_ticks[c];
        println!(
            "| {name} | {:.1}% | {:.1}% | {} | {} | {} |",
            100.0 * cls_ticks[c] as f64 / t.ticks as f64,
            100.0 * cls_ticks_idle_ppu[c] as f64 / t.ticks as f64,
            e.len(),
            pct(&e, 0.5),
            pct(&e, 0.9)
        );
    }
    let skippable = cls_ticks[0] + cls_ticks[1] + cls_ticks[2] + cls_ticks[3];
    println!(
        "| all repeating laps | {:.1}% | | | | |\n\nIdle by the strict rule (no writes, no ROM data reads: the first four classes): {:.1}% of ticks. The first lap of every loop is not counted (it is not yet known to repeat).",
        100.0 * total as f64 / t.ticks as f64,
        100.0 * skippable as f64 / t.ticks as f64
    );
}

/// Side of the NES a root-level state element belongs to, by the
/// definition name of its group.
fn side_of(group: &str) -> usize {
    match group {
        "cpu6502" | "oam_dma_ref" | "joypad" | "dffe" | "reg8" | "por" | "div3" | "Ram<-mux2x8" => {
            0
        }
        "cart_mmc3" | "cart_nrom" => 2,
        _ => 1,
    }
}

const SIDES: [&str; 3] = ["CPU side", "PPU side", "mapper"];
const REGIONS: [&str; 3] = [
    "visible (lines 0-239)",
    "vblank (241-260)",
    "line 240 and 261",
];

fn region(line: u64) -> usize {
    match line {
        0..=239 => 0,
        241..=260 => 1,
        _ => 2,
    }
}

#[derive(Default, Clone, Copy)]
struct Acc {
    ticks: u64,
    words: u64,
    zero: u64,
    counters_only: u64,
}

fn changes(l: &Loaded, a: &Args, total_frames: u64) -> Result<(), String> {
    let p = &l.program;
    let s = Sigs::find(l)?;
    let keys: Vec<usize> = (0..p.state_keys.len())
        .filter(|&k| matches!(p.state_keys[k].loc, StateLoc::Slot(_)))
        .collect();
    let mem_group = |m: u32| -> String {
        p.state_keys
            .iter()
            .find(|k| k.loc == StateLoc::Mem(m))
            .map(|k| l.group_of(&k.path))
            .unwrap_or_default()
    };
    // Memories the CPU side reads and writes (the CPU RAM and the
    // cartridge's PRG RAM): what a loop's state includes.
    let mems: Vec<u32> = (0..p.mems.len() as u32)
        .filter(|&m| p.mems[m as usize].writable)
        .filter(|&m| {
            let g = mem_group(m);
            g == "Ram<-mux2x8" || (g == "cart_mmc3" && p.mems[m as usize].addr_bits <= 13)
        })
        .collect();
    let groups: Vec<String> = keys
        .iter()
        .map(|&k| l.group_of(&p.state_keys[k].path))
        .collect();
    let side: Vec<usize> = groups.iter().map(|g| side_of(g)).collect();
    // Counter-like words: those of the PPU's dot/line counters and the
    // screen's scan address, and any word of 4 bits or more that changed
    // by exactly +1. (The compiler keeps the dot counter as 1-bit words,
    // so a width test alone would miss it.)
    let timing: Vec<bool> = groups
        .iter()
        .map(|g| g == "ppu_timing" || g == "Screen")
        .collect();
    let wide: Vec<bool> = keys.iter().map(|&k| p.state_keys[k].width >= 4).collect();
    let phase = p.phases.as_ref().map_or(1, |x| x.period as usize);
    let mut sim = new_sim(&l.program);
    println!("\n### State changes per tick (idea 4b)\n");
    println!(
        "{} state words; writable memories: {}.\nWindows of {} frames at {} places (counter-like: the PPU's timing and screen-address words, and any word of 4 bits or more that changed by exactly +1).\n",
        keys.len(),
        (0..p.mems.len() as u32)
            .filter(|&m| p.mems[m as usize].writable)
            .map(|m| format!(
                "{}[{}x{}b]",
                mem_group(m),
                1u64 << p.mems[m as usize].addr_bits,
                p.mems[m as usize].width
            ))
            .collect::<Vec<_>>()
            .join(", "),
        a.changes,
        a.windows
    );
    let ticks_per_frame = 341 * 262u64;
    let mut acc = [[Acc::default(); 3]; 3];
    let mut all_hist = [0u64; 8];
    let mut ticks = 0u64;
    // Whole-machine: ticks where only counters changed anywhere.
    let mut only_counters_all = 0u64;
    let mut nothing_all = 0u64;
    let mut cpu_period = [0u64; 3];
    let mut cpu_and_ppu_quiet = 0u64;
    let mut ppu_quiet_ticks = 0u64;
    let mut cpu_run = vec![0u32; 97];
    let mut cpu_hist: Vec<u64> = Vec::new();
    for w in 0..a.windows {
        let start = (total_frames / (a.windows + 1)) * (w + 1);
        let target = start * ticks_per_frame;
        while sim.tick_count() < target {
            let now = sim.tick_count();
            if now.is_multiple_of(ticks_per_frame) {
                apply_presses(&mut sim, &a.presses, now / ticks_per_frame);
            }
            let step = (ticks_per_frame - now % ticks_per_frame).min(target - now);
            sim.run(step);
        }
        let mut prev: Vec<u64> = keys.iter().map(|&k| sim.state(k)).collect();
        let mut mem_prev: Vec<u64> = mems.iter().map(|&m| sim.mem_version(m)).collect();
        cpu_hist.clear();
        cpu_run.iter_mut().for_each(|r| *r = 0);
        for _ in 0..a.changes * ticks_per_frame {
            let at = sim.tick_count();
            if at.is_multiple_of(ticks_per_frame) {
                apply_presses(&mut sim, &a.presses, at / ticks_per_frame);
            }
            sim.tick();
            let reg = region(sim.read(s.line));
            let mut n = [0u64; 3];
            let mut other = [false; 3];
            let mut cpu_h = 0xcbf2_9ce4_8422_2325u64;
            for (i, &k) in keys.iter().enumerate() {
                let v = sim.state(k);
                if side[i] == 0 {
                    cpu_h = (cpu_h ^ v).wrapping_mul(0x100_0000_01b3).rotate_left(5);
                }
                if v != prev[i] {
                    n[side[i]] += 1;
                    let wmask = if p.state_keys[k].width >= 64 {
                        u64::MAX
                    } else {
                        (1u64 << p.state_keys[k].width) - 1
                    };
                    if !(timing[i] || wide[i] && v == (prev[i].wrapping_add(1) & wmask)) {
                        other[side[i]] = true;
                    }
                    prev[i] = v;
                }
            }
            let mut mem_written = false;
            for (j, &m) in mems.iter().enumerate() {
                let v = sim.mem_version(m);
                if v != mem_prev[j] {
                    mem_written = true;
                    mem_prev[j] = v;
                }
            }
            ticks += 1;
            let total: u64 = n.iter().sum();
            all_hist[match total {
                0 => 0,
                1 => 1,
                2 => 2,
                3..=4 => 3,
                5..=8 => 4,
                9..=16 => 5,
                17..=32 => 6,
                _ => 7,
            }] += 1;
            if total == 0 {
                nothing_all += 1;
            } else if !other.iter().any(|&o| o) {
                only_counters_all += 1;
            }
            for sd in 0..3 {
                let c = &mut acc[reg][sd];
                c.ticks += 1;
                c.words += n[sd];
                if n[sd] == 0 {
                    c.zero += 1;
                } else if !other[sd] {
                    c.counters_only += 1;
                }
            }
            // The CPU side repeats exactly: its state (and its memories'
            // write count) equals that of `lag` ticks ago, for a full lap.
            cpu_hist.push(cpu_h ^ u64::from(mem_written).wrapping_mul(0x9e37_79b9_7f4a_7c15));
            let i = cpu_hist.len() - 1;
            let mut period = 0;
            for lag in (phase..=96usize).step_by(phase) {
                if i >= lag && cpu_hist[i] == cpu_hist[i - lag] {
                    cpu_run[lag] += 1;
                } else {
                    cpu_run[lag] = 0;
                }
                if period == 0 && cpu_run[lag] as usize >= lag {
                    period = lag;
                }
            }
            let ppu_quiet = n[1] == 0 || !other[1];
            ppu_quiet_ticks += u64::from(ppu_quiet);
            if period != 0 {
                cpu_period[0] += 1;
                if ppu_quiet {
                    cpu_and_ppu_quiet += 1;
                }
            }
        }
    }
    let f = |x: u64| 100.0 * x as f64 / ticks as f64;
    println!("Ticks sampled: {ticks}.\n");
    println!("| changed state words per tick (whole machine) | share of ticks |");
    println!("|---|---:|");
    for (k, label) in ["0", "1", "2", "3-4", "5-8", "9-16", "17-32", "33+"]
        .iter()
        .enumerate()
    {
        println!("| {label} | {:.1}% |", f(all_hist[k]));
    }
    println!(
        "\nNothing changed: {:.2}% of ticks; only counters changed (anywhere): {:.2}%.\n",
        f(nothing_all),
        f(only_counters_all)
    );
    println!(
        "| region | side | mean words changed/tick | nothing changed | only counters changed |"
    );
    println!("|---|---|---:|---:|---:|");
    for (r, name) in REGIONS.iter().enumerate() {
        for (sd, sname) in SIDES.iter().enumerate() {
            let c = acc[r][sd];
            if c.ticks == 0 {
                continue;
            }
            let g = |x: u64| 100.0 * x as f64 / c.ticks as f64;
            println!(
                "| {name} | {sname} | {:.2} | {:.1}% | {:.1}% |",
                c.words as f64 / c.ticks as f64,
                g(c.zero),
                g(c.counters_only)
            );
        }
    }
    println!(
        "\nThe CPU side's state (cpu6502, oam_dma_ref, pad logic, bus latch, RAM write count) repeats exactly with a period of at most 96 ticks that is a multiple of the clock-phase period (counted after one full lap): {:.1}% of ticks; in {:.1}% of ticks that also had the PPU side quiet (nothing or counters only changed in it) -- {:.1}% of ticks have the PPU side quiet in all.",
        f(cpu_period[0]),
        f(cpu_and_ppu_quiet),
        f(ppu_quiet_ticks)
    );
    Ok(())
}

fn main() -> Result<(), String> {
    let a = args()?;
    let l = bold_common::load(&a.folder)?;
    let p = &l.program;
    println!("## {}\n", a.folder);
    println!(
        "{} slots, {} state words, {} memories ({} writable).\n",
        p.slot_count,
        p.state_keys
            .iter()
            .filter(|k| matches!(k.loc, StateLoc::Slot(_)))
            .count(),
        p.mems.len(),
        p.mems.iter().filter(|m| m.writable).count()
    );
    let t = collect(&l, &a)?;
    interactions(&t);
    idle_loops(&t);
    if a.changes > 0 && a.windows > 0 {
        changes(&l, &a, a.skip + a.frames)?;
    }
    Ok(())
}
