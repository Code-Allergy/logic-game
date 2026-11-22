//! Differential testing: the compiled [`Sim`] against the reference
//! interpreter (design doc section 8).
//!
//! Both are driven with the same random root inputs and must agree on every
//! root output and every debug-observable net, at tick 0 and after every
//! tick. A mismatch reports the generator config and seed, which reproduce it
//! exactly via [`check_generated`].

use std::fmt;
use std::sync::Arc;

use super::random::{GenConfig, generate};
use super::rng::Rng;
use crate::compile::{Options, Program, StateLoc, compile};
use crate::netlist::{Design, mask};
use crate::reference::Reference;
use crate::sim::{Sim, SimOptions};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mismatch {
    pub tick: u64,
    /// What differed, e.g. `output 3 ("sum")` or `net [n1, n2]/NetId(7v1)`.
    pub what: String,
    pub sim: u64,
    pub reference: u64,
}

impl fmt::Display for Mismatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "tick {}: {}: sim {:#x} != reference {:#x}",
            self.tick, self.what, self.sim, self.reference
        )
    }
}

/// Why a differential check failed.
#[derive(Debug, Clone)]
pub enum DiffError {
    Compile(String),
    Reference(String),
    Mismatch(Mismatch),
}

impl fmt::Display for DiffError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DiffError::Compile(e) => write!(f, "compile failed: {e}"),
            DiffError::Reference(e) => write!(f, "reference failed: {e}"),
            DiffError::Mismatch(m) => write!(f, "{m}"),
        }
    }
}

/// Run `design` for `ticks` ticks with random inputs from `input_seed` under
/// both simulators and compare everything observable.
pub fn check_design(
    design: &Design,
    input_seed: u64,
    ticks: u64,
    options: &Options,
) -> Result<(), DiffError> {
    check_design_with(design, input_seed, ticks, options, &SimOptions::default())
}

/// [`check_design`] with runtime options for the compiled [`Sim`].
pub fn check_design_with(
    design: &Design,
    input_seed: u64,
    ticks: u64,
    options: &Options,
    sim_options: &SimOptions,
) -> Result<(), DiffError> {
    let program = compile(design, options).map_err(|e| DiffError::Compile(format!("{e:?}")))?;
    let program = Arc::new(program);
    let mut sim = Sim::with_options(Arc::clone(&program), sim_options.clone());
    let mut reference =
        Reference::new(design).map_err(|e| DiffError::Reference(format!("{e:?}")))?;
    let widths: Vec<u8> = program.ports.inputs.iter().map(|p| p.width).collect();
    let mut rng = Rng::new(input_seed);

    for t in 0..=ticks {
        if t > 0 {
            for (port, &w) in widths.iter().enumerate() {
                let v = rng.next_u64() & mask(w);
                sim.set_input(port, v);
                reference.set_input(port, v);
            }
            sim.tick();
            reference
                .tick()
                .map_err(|e| DiffError::Reference(format!("{e:?}")))?;
        }
        for (port, p) in program.ports.outputs.iter().enumerate() {
            let (s, r) = (sim.output(port), reference.output(port));
            if s != r {
                return Err(DiffError::Mismatch(Mismatch {
                    tick: t,
                    what: format!("output {port} ({:?})", p.name),
                    sim: s,
                    reference: r,
                }));
            }
        }
        // State: every register's word and every memory (RAMs and
        // screens, which have no outputs to compare).
        for key in &program.state_keys {
            let path = &key.path;
            let (s, r, what) = match key.loc {
                StateLoc::Slot(slot) => (
                    sim.read(slot),
                    reference
                        .state(&path.instances, path.node)
                        .unwrap_or(u64::MAX),
                    format!("state {path:?}"),
                ),
                StateLoc::Mem(m) => {
                    let mem = sim.mem(m);
                    let theirs = reference.mem(&path.instances, path.node).unwrap_or(&[]);
                    let at = (0..mem.len().max(theirs.len()))
                        .find(|&a| mem.get(a) != theirs.get(a))
                        .unwrap_or(usize::MAX);
                    if at == usize::MAX {
                        continue;
                    }
                    (
                        mem.get(at).copied().unwrap_or(u64::MAX),
                        theirs.get(at).copied().unwrap_or(u64::MAX),
                        format!("memory {path:?}[{at}]"),
                    )
                }
            };
            if s != r {
                return Err(DiffError::Mismatch(Mismatch {
                    tick: t,
                    what,
                    sim: s,
                    reference: r,
                }));
            }
        }
        for inst in 0..program.debug.instances.len() as u32 {
            let path = program.debug.path(inst);
            for (net, &slot) in &program.debug.net_slots[inst as usize] {
                let (s, r) = (
                    sim.read(slot),
                    reference.net(&path, net).unwrap_or(u64::MAX),
                );
                if s != r {
                    return Err(DiffError::Mismatch(Mismatch {
                        tick: t,
                        what: format!("net {path:?}/{net:?}"),
                        sim: s,
                        reference: r,
                    }));
                }
            }
        }
    }
    Ok(())
}

/// Generate the design for `cfg` and check it, using `cfg.seed` for inputs.
pub fn check_generated(cfg: &GenConfig, ticks: u64, options: &Options) -> Result<(), DiffError> {
    check_generated_with(cfg, ticks, options, &SimOptions::default())
}

/// [`check_generated`] with runtime options.
pub fn check_generated_with(
    cfg: &GenConfig,
    ticks: u64,
    options: &Options,
    sim_options: &SimOptions,
) -> Result<(), DiffError> {
    let g = generate(cfg);
    check_design_with(&g.design, cfg.seed, ticks, options, sim_options)
}

/// Check `count` designs per config (seeds `0..count`, mixed with the config
/// index). Panics on the first failure with a message that reproduces it.
/// Returns the number of designs checked.
#[track_caller]
pub fn sweep(configs: &[GenConfig], count: u64, ticks: u64, options: &Options) -> u64 {
    sweep_with(configs, count, ticks, options, &SimOptions::default())
}

/// [`sweep`] with runtime options.
#[track_caller]
pub fn sweep_with(
    configs: &[GenConfig],
    count: u64,
    ticks: u64,
    options: &Options,
    sim_options: &SimOptions,
) -> u64 {
    let mut n = 0;
    for (ci, base) in configs.iter().enumerate() {
        for k in 0..count {
            let cfg = GenConfig {
                seed: k.wrapping_mul(0x9E37_79B9) ^ (ci as u64) << 48,
                ..base.clone()
            };
            if let Err(e) = check_generated_with(&cfg, ticks, options, sim_options) {
                panic!(
                    "differential failure: {e}\n  reproduce with: \
                     testkit::diff::check_generated_with(&{cfg:?}, {ticks}, &{options:?}, \
                     &{sim_options:?})"
                );
            }
            n += 1;
        }
    }
    n
}

/// Generator configs covering the shapes the differential tests care about.
pub fn standard_configs() -> Vec<GenConfig> {
    vec![
        GenConfig::default(),
        GenConfig {
            max_width: 64,
            nodes: 120,
            ..Default::default()
        },
        GenConfig {
            max_width: 1,
            ..Default::default()
        },
        GenConfig {
            max_depth: 5,
            child_defs: 6,
            instance_percent: 20,
            ..Default::default()
        },
        GenConfig {
            state_percent: 50,
            unconnected_percent: 20,
            ..Default::default()
        },
        GenConfig::dag(0, 200, 32),
        GenConfig {
            state_percent: 40,
            screens: true,
            ..Default::default()
        },
    ]
}

// ── Batches (tick fusion, clock-phase variants) ──────────────────────────

/// Tick, outputs, state, every net, memories and memory versions.
type Observed = (
    u64,
    Vec<u64>,
    Vec<(String, u64)>,
    Vec<Option<u64>>,
    Vec<Vec<u64>>,
    Vec<u64>,
);

/// Everything a caller can observe of `sim`: tick, outputs, state, every
/// net in the debug map, memories and their versions.
fn observe(sim: &Sim) -> Observed {
    let p = sim.program();
    let outs = (0..p.ports.outputs.len()).map(|k| sim.output(k)).collect();
    let mut state: Vec<(String, u64)> = sim
        .export_state()
        .words
        .into_iter()
        .map(|(k, v)| (format!("{k:?}"), v))
        .collect();
    state.sort();
    let mut nets = Vec::new();
    for inst in 0..p.debug.instances.len() {
        let path = p.debug.path(inst as u32);
        for (net, _) in &p.debug.net_slots[inst] {
            nets.push(sim.read_net(&path, net));
        }
    }
    let mems = (0..p.mems.len() as u32)
        .map(|m| sim.mem(m).to_vec())
        .collect();
    let versions = (0..p.mems.len() as u32)
        .map(|m| sim.mem_version(m))
        .collect();
    (sim.tick_count(), outs, state, nets, mems, versions)
}

/// Batches of odd and even lengths from misaligned starts, inputs changed
/// between batches, a reset and a state import in the middle: a fused
/// `run(n)` (with and without phase variants) equals `n` single ticks of
/// the unspecialized program at every batch boundary.
///
/// Runs the program as is, with phase variants off, and with fusion off.
pub fn check_batches(p: &Arc<Program>, seed: u64) -> Result<(), String> {
    let plain = SimOptions {
        fuse: false,
        phases: false,
        ..SimOptions::default()
    };
    let mut want = Sim::with_options(Arc::clone(p), plain);
    let mut sims = [
        Sim::new(Arc::clone(p)),
        Sim::with_options(
            Arc::clone(p),
            SimOptions {
                phases: false,
                ..SimOptions::default()
            },
        ),
        Sim::with_options(
            Arc::clone(p),
            SimOptions {
                fuse: false,
                ..SimOptions::default()
            },
        ),
    ];
    let mut rng = Rng::new(seed);
    let lens = [1u64, 2, 3, 5, 8, 7, 1, 1, 16, 9, 4, 6, 11, 2, 3];
    let mut saved = None;
    for (round, &n) in lens.iter().enumerate() {
        for (port, q) in p.ports.inputs.iter().enumerate() {
            let v = rng.next_u64() & mask(q.width);
            want.set_input(port, v);
            for s in &mut sims {
                s.set_input(port, v);
            }
        }
        for _ in 0..n {
            want.tick();
        }
        for s in &mut sims {
            s.run(n);
        }
        let w = observe(&want);
        for (k, s) in sims.iter().enumerate() {
            if observe(s) != w {
                return Err(format!(
                    "batch {round} (+{n} ticks, now at tick {}): runtime variant {k} differs",
                    w.0
                ));
            }
        }
        if round == 3 {
            saved = Some(want.export_state());
        }
        if round == 6 {
            want.reset();
            for s in &mut sims {
                s.reset();
            }
        }
        if round == 10 {
            // Carry state in (as a recompile does) from an earlier tick
            // of the other parity.
            want.tick();
            let mut snap = saved.take().expect("saved at round 3");
            snap.tick += 1;
            for s in &mut sims {
                s.tick();
                s.import_state(&snap);
            }
            want.import_state(&snap);
        }
    }
    Ok(())
}
