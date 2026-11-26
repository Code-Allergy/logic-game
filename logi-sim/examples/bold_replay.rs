//! Soundness and coverage test of the idle-loop replay rule of
//! `docs/bold-ideas.md` (idea 4): the CPU side is a deterministic machine
//! with a small input and output interface, so once its state, outputs and
//! inputs have repeated exactly over one lap of P ticks, it will repeat
//! again for as long as its inputs do, and its state and outputs need not
//! be computed: they are those of a lap ago.
//!
//! ```sh
//! cargo run -p logi-sim --profile profiling --features jit --example bold_replay -- \
//!     <workspace> [--frames 300] [--skip-frames 30] [--press p1_start:100:3,...]
//! ```
//!
//! The example runs the whole machine tick by tick (nothing is skipped: it
//! is a test of the rule, not an implementation) and, per tick, hashes
//!
//! * the CPU side's state words (cpu6502, DMA, pads, bus latch, clock
//!   divider, the RAM write count),
//! * its inputs: the pins of the CPU and of the DMA unit that the bus
//!   glue drives (`cpu6502/din`, `nmi_n`, `irq_n`, `rdy`,
//!   `oam_dma_ref/din`), so the PPU register data counts only when the
//!   glue passes it to the CPU, and
//! * its outputs: the pins that drive the glue (addresses, data, `rw`,
//!   `sync`, the DMA's bus request).
//!
//! A lap of P <= 96 ticks (a multiple of the clock-phase period, 3) is found when state, inputs and outputs of the
//! last P ticks equal those of the P before. From then on, at each tick
//! whose inputs equal those of one lap earlier the rule *predicts* that
//! state and outputs equal those of one lap earlier, and the prediction is
//! checked against the real value. Two input rules are run side by side:
//! `strict` compares every input at every tick, `relaxed` only at the
//! ticks where the CPU's clock enable is high (the CPU samples its inputs
//! only then). A wrong prediction is a counterexample to the rule; the
//! count must be 0. A tick whose inputs differ ends the regime (the CPU
//! side would be woken: its state at that tick is the replayed one).
//! Coverage is the share of ticks predicted and the length of regimes.

mod bold_common;

use bold_common::Loaded;
use logi_sim::compile::StateLoc;
use logi_sim::sim::{Sim, SimOptions};
use std::sync::Arc;

const RING: usize = 256;
const MAX_LAP: usize = 96;

#[derive(Clone, Copy, Default)]
struct Entry {
    state: u64,
    inputs: u64,
    outputs: u64,
    ce: bool,
}

#[derive(Default)]
struct Tracker {
    relaxed: bool,
    /// The clock-phase period: laps are multiples of it (the circuit is a
    /// function of the state, the inputs and the tick modulo it).
    phase: usize,
    /// Consecutive ticks matching the tick `lag` earlier, per lag.
    run: Vec<u32>,
    lap: usize,
    predicted: u64,
    wrong: u64,
    exits: u64,
    regimes: Vec<u64>,
    in_regime: u64,
    laps: [u64; 4],
}

impl Tracker {
    fn new(relaxed: bool, phase: usize) -> Self {
        Tracker {
            relaxed,
            phase,
            run: vec![0; MAX_LAP + 1],
            ..Default::default()
        }
    }

    fn inputs_equal(&self, now: &Entry, then: &Entry) -> bool {
        now.ce == then.ce && (self.relaxed && !now.ce || now.inputs == then.inputs)
    }

    /// `ring[t % RING]` holds tick `t`; `t >= MAX_LAP`.
    fn step(&mut self, ring: &[Entry; RING], t: usize) {
        let now = ring[t % RING];
        if self.lap != 0 {
            let then = ring[(t - self.lap) % RING];
            if self.inputs_equal(&now, &then) {
                self.predicted += 1;
                self.in_regime += 1;
                if now.state != then.state || now.outputs != then.outputs {
                    self.wrong += 1;
                }
                return;
            }
            // Inputs differ: the regime ends here.
            self.exits += 1;
            self.regimes.push(self.in_regime);
            self.in_regime = 0;
            self.lap = 0;
            self.run.iter_mut().for_each(|r| *r = 0);
            return;
        }
        let mut found = 0;
        for lag in (self.phase..=MAX_LAP).step_by(self.phase) {
            let then = ring[(t - lag) % RING];
            if now.state == then.state
                && now.outputs == then.outputs
                && self.inputs_equal(&now, &then)
            {
                self.run[lag] += 1;
            } else {
                self.run[lag] = 0;
            }
            if found == 0 && self.run[lag] as usize >= lag {
                found = lag;
            }
        }
        if found != 0 {
            self.lap = found;
            self.laps[match found {
                0..=3 => 0,
                4..=12 => 1,
                13..=48 => 2,
                _ => 3,
            }] += 1;
        }
    }
}

fn mix(h: u64, v: u64) -> u64 {
    (h ^ v).wrapping_mul(0x100_0000_01b3).rotate_left(13) ^ (h >> 7)
}

fn main() -> Result<(), String> {
    let mut folder = String::new();
    let (mut frames, mut skip) = (300u64, 30u64);
    let mut presses: Vec<(String, u64, u64)> = Vec::new();
    let mut it = std::env::args().skip(1);
    while let Some(x) = it.next() {
        match x.as_str() {
            "--frames" => frames = it.next().and_then(|v| v.parse().ok()).ok_or("--frames N")?,
            "--skip-frames" => {
                skip = it
                    .next()
                    .and_then(|v| v.parse().ok())
                    .ok_or("--skip-frames N")?
            }
            "--press" => {
                for item in it.next().ok_or("--press list")?.split(',') {
                    let f: Vec<&str> = item.split(':').collect();
                    let [n, a, b] = f[..] else {
                        return Err(format!("bad --press item {item}"));
                    };
                    presses.push((
                        n.into(),
                        a.parse().map_err(|e| format!("{e}"))?,
                        b.parse().map_err(|e| format!("{e}"))?,
                    ));
                }
            }
            f if folder.is_empty() => folder = f.into(),
            o => return Err(format!("unknown argument {o}")),
        }
    }
    if folder.is_empty() {
        return Err(
            "usage: bold_replay <workspace> [--frames N] [--skip-frames N] [--press ...]".into(),
        );
    }
    let l: Loaded = bold_common::load(&folder)?;
    let p = &l.program;
    // The replayed part is the CPU chip, the DMA unit, the pad logic and
    // the bus latch; the bus glue (the data-bus multiplexer, the address
    // decoders) stays live. Inputs: the pins of the CPU and the DMA unit
    // that the glue and the other chips drive; outputs: the pins that
    // drive the glue.
    let ins = [
        l.slot("cpu6502/din")?,
        l.slot("cpu6502/nmi_n")?,
        l.slot("cpu6502/irq_n")?,
        l.slot("cpu6502/rdy")?,
        l.slot("oam_dma_ref/din")?,
    ];
    let outs = [
        l.slot("cpu6502/addr")?,
        l.slot("cpu6502/rw")?,
        l.slot("cpu6502/dout")?,
        l.slot("cpu6502/sync")?,
        l.slot("oam_dma_ref/addr")?,
        l.slot("oam_dma_ref/rw")?,
        l.slot("oam_dma_ref/own")?,
        l.slot("oam_dma_ref/dout")?,
        l.slot("oam_dma_ref/rdy")?,
    ];
    let ce = l.slot("cpu6502/ce")?;
    let line = l.slot("ppu_ref/line")?;
    let cpu_keys: Vec<usize> = (0..p.state_keys.len())
        .filter(|&k| {
            matches!(p.state_keys[k].loc, StateLoc::Slot(_))
                && matches!(
                    l.group_of(&p.state_keys[k].path).as_str(),
                    "cpu6502" | "oam_dma_ref" | "joypad" | "dffe" | "reg8" | "por" | "div3"
                )
        })
        .collect();
    let rams: Vec<u32> = (0..p.mems.len() as u32)
        .filter(|&m| {
            p.mems[m as usize].writable
                && p.state_keys
                    .iter()
                    .any(|k| k.loc == StateLoc::Mem(m) && l.group_of(&k.path) == "Ram<-mux2x8")
        })
        .collect();
    #[cfg(feature = "jit")]
    let o = SimOptions::jit();
    #[cfg(not(feature = "jit"))]
    let o = SimOptions::default();
    let mut sim = Sim::with_options(Arc::clone(p), o);
    let phase = p.phases.as_ref().map_or(1, |x| x.period as usize);
    let mut ring = [Entry::default(); RING];
    let mut strict = Tracker::new(false, phase);
    let mut relaxed = Tracker::new(true, phase);
    let mut prev_line = u64::MAX;
    let mut frame = 0u64;
    let (mut t, mut total) = (0usize, 0u64);
    let mut vblank_pred = [0u64; 2];
    while frame < skip + frames {
        sim.tick();
        let ln = sim.read(line);
        if ln == 0 && prev_line != 0 {
            frame += 1;
            for (n, from, len) in &presses {
                if frame == *from {
                    sim.set(n, 1);
                } else if frame == from + len {
                    sim.set(n, 0);
                }
            }
        }
        prev_line = ln;
        let mut s = 0xcbf2_9ce4_8422_2325u64;
        for &k in &cpu_keys {
            s = mix(s, sim.state(k));
        }
        for &m in &rams {
            s = mix(s, sim.mem_version(m));
        }
        let i = ins.iter().fold(7u64, |h, &x| mix(h, sim.read(x)));
        let out = outs.iter().fold(11u64, |h, &x| mix(h, sim.read(x)));
        ring[t % RING] = Entry {
            state: s,
            inputs: i,
            outputs: out,
            ce: sim.read(ce) != 0,
        };
        if frame >= skip && t >= MAX_LAP {
            total += 1;
            let before = [strict.predicted, relaxed.predicted];
            strict.step(&ring, t);
            relaxed.step(&ring, t);
            if (241..=260).contains(&ln) {
                vblank_pred[0] += strict.predicted - before[0];
                vblank_pred[1] += relaxed.predicted - before[1];
            }
        }
        t += 1;
    }
    println!("## {folder}\n");
    println!(
        "{} CPU-side state words, {} RAMs, {} input signals, {} output signals; {total} ticks tested ({frames} frames after {skip}).\n",
        cpu_keys.len(),
        rams.len(),
        ins.len(),
        outs.len()
    );
    println!(
        "| input rule | ticks predicted | wrong predictions | regimes ended by an input change | median regime (ticks) | p90 regime | laps of 1-3 / 4-12 / 13-48 / 49-96 ticks |"
    );
    println!("|---|---:|---:|---:|---:|---:|---|");
    for (name, tr) in [
        ("strict (every tick)", &mut strict),
        ("relaxed (CPU clock-enable ticks only)", &mut relaxed),
    ] {
        if tr.in_regime > 0 {
            tr.regimes.push(tr.in_regime);
        }
        let mut r = tr.regimes.clone();
        r.sort_unstable();
        let pct = |q: f64| {
            r.get(((r.len().max(1) - 1) as f64 * q) as usize)
                .copied()
                .unwrap_or(0)
        };
        println!(
            "| {name} | {} ({:.1}%) | {} | {} | {} | {} | {} / {} / {} / {} |",
            tr.predicted,
            100.0 * tr.predicted as f64 / total as f64,
            tr.wrong,
            tr.exits,
            pct(0.5),
            pct(0.9),
            tr.laps[0],
            tr.laps[1],
            tr.laps[2],
            tr.laps[3]
        );
    }
    println!(
        "\nOf the predicted ticks, in vblank: strict {:.1}% of all ticks, relaxed {:.1}%.",
        100.0 * vblank_pred[0] as f64 / total as f64,
        100.0 * vblank_pred[1] as f64 / total as f64
    );
    Ok(())
}
