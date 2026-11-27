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
use crate::sim::{PhasePlan, Sim, SimOptions, SpecOptions, SpecPlan, spec_contexts};

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

/// The JIT backend (`SimOptions::jit`) is compiled in: logi-sim feature
/// `jit` (Cranelift natively, the wasm backend on wasm32). Without it, the JIT checks below compare the
/// interpreter with itself.
pub const JIT: bool = cfg!(feature = "jit");

/// Tick, outputs, state, every net, memories and memory versions.
pub(crate) type Observed = (
    u64,
    Vec<u64>,
    Vec<(String, u64)>,
    Vec<Option<u64>>,
    Vec<Vec<u64>>,
    Vec<u64>,
);

/// Everything a caller can observe of `sim`: tick, outputs, state, every
/// net in the debug map, memories and their versions.
pub(crate) fn observe(sim: &Sim) -> Observed {
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
/// Runs the program as is, with phase variants off, with fusion off and
/// with guards off; the reference runs none of them.
pub fn check_batches(p: &Arc<Program>, seed: u64) -> Result<(), String> {
    check_batches_with(p, seed, None)
}

/// [`check_batches`] with state-keyed specialization
/// ([`SimOptions::spec`]) on every runtime variant (the reference runs
/// none).
pub fn check_batches_with(
    p: &Arc<Program>,
    seed: u64,
    spec: Option<&SpecOptions>,
) -> Result<(), String> {
    let plain = SimOptions {
        fuse: false,
        phases: false,
        guard: false,
        ..SimOptions::default()
    };
    let mut want = Sim::with_options(Arc::clone(p), plain);
    // Guards in the interpreter too (`SimOptions::guard_interpreter`).
    let on = SimOptions {
        guard_interpreter: true,
        ..SimOptions::default()
    };
    let variants = [
        SimOptions::default(),
        on.clone(),
        SimOptions {
            phases: false,
            ..on.clone()
        },
        SimOptions {
            fuse: false,
            ..on.clone()
        },
        SimOptions {
            guard: false,
            ..on.clone()
        },
        // Guards down to single-instruction regions.
        SimOptions {
            guard_min: 1,
            ..on.clone()
        },
        // Supersteps (JIT only) off, cut into small chunks (most values
        // cross a chunk), and over several clock periods.
        SimOptions {
            superstep: false,
            ..SimOptions::default()
        },
        SimOptions {
            superstep_chunk: 5,
            guard_min: 1,
            ..SimOptions::default()
        },
        SimOptions {
            superstep_ticks: 7,
            superstep_chunk: 100_000,
            ..SimOptions::default()
        },
    ];
    // With the JIT compiled in, every variant also runs JIT-compiled.
    let jit = variants.iter().filter(|_| JIT).map(|o| SimOptions {
        jit: true,
        ..o.clone()
    });
    let mut sims: Vec<Sim> = variants
        .iter()
        .cloned()
        .chain(jit)
        .map(|o| SimOptions {
            spec: spec.cloned(),
            ..o
        })
        .map(|o| Sim::with_options(Arc::clone(p), o))
        .collect();
    for s in &sims {
        if s.options().jit && !s.jit_active() {
            return Err("the JIT did not compile the program".into());
        }
    }
    let mut rng = Rng::new(seed);
    let lens = [
        1u64, 2, 3, 5, 8, 7, 1, 1, 16, 9, 4, 6, 11, 2, 3, 25, 17, 40, 13, 2,
    ];
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

/// The JIT backend against the interpreter, tick by tick: random inputs
/// every tick, and everything observable (tick, outputs, state, every net
/// in the debug map, memories, memory versions) compared after every tick,
/// with `run` batches of random lengths, state and memory pokes followed
/// by `resettle`, a reset and a state import mixed in. Both run the
/// runtime's defaults (phase variants, fusion); `options` adds the
/// runtime switches to compare under (`jit` is forced on for one side and
/// off for the other).
///
/// Errors if the JIT is compiled in but did not compile the program.
pub fn check_jit(
    p: &Arc<Program>,
    ticks: u64,
    seed: u64,
    options: &SimOptions,
) -> Result<(), String> {
    let want = SimOptions {
        jit: false,
        ..options.clone()
    };
    let got = SimOptions {
        jit: true,
        ..options.clone()
    };
    if JIT && options.threads <= 1 && !Sim::with_options(Arc::clone(p), got.clone()).jit_active() {
        return Err("the JIT did not compile the program".into());
    }
    check_pair(p, ticks, seed, &want, &got).map_err(|e| {
        e.replace(
            "the runtime options differ",
            "JIT differs from the interpreter",
        )
    })
}

/// Two runtimes of one program, `want` and `got`, side by side as
/// [`check_jit`] runs them: everything observable compared after every
/// tick, `run` batch, poke plus `resettle`, reset and import.
pub fn check_pair(
    p: &Arc<Program>,
    ticks: u64,
    seed: u64,
    want: &SimOptions,
    got: &SimOptions,
) -> Result<(), String> {
    let mut want = Sim::with_options(Arc::clone(p), want.clone());
    let mut got = Sim::with_options(Arc::clone(p), got.clone());
    let mut rng = Rng::new(seed);
    let same = |want: &Sim, got: &Sim, at: &str| {
        if observe(want) == observe(got) {
            Ok(())
        } else {
            Err(format!(
                "the runtime options differ {at} (tick {})",
                want.tick_count()
            ))
        }
    };
    same(&want, &got, "after construction")?;
    let mut saved = None;
    let mut t = 0;
    while t < ticks {
        for (port, q) in p.ports.inputs.iter().enumerate() {
            let v = rng.next_u64() & mask(q.width);
            want.set_input(port, v);
            got.set_input(port, v);
        }
        match rng.below(40) {
            0..=4 => {
                // Mostly short runs, some spanning several supersteps.
                let most = if rng.below(4) == 0 { 60 } else { 20 };
                let n = 1 + rng.below(most) as u64;
                want.run(n);
                got.run(n);
                t += n;
                same(&want, &got, &format!("after run({n})"))?;
                continue;
            }
            5 if !p.state_keys.is_empty() => {
                let k = rng.below(p.state_keys.len());
                let v = rng.next_u64();
                match p.state_keys[k].loc {
                    StateLoc::Slot(_) => {
                        want.set_state(k, v);
                        got.set_state(k, v);
                    }
                    StateLoc::Mem(m) => {
                        let a = rng.below(want.mem(m).len());
                        want.set_mem_word(m, a, v);
                        got.set_mem_word(m, a, v);
                    }
                }
                want.resettle();
                got.resettle();
                same(&want, &got, "after a state poke and resettle")?;
            }
            6 if t % 3 == 0 => {
                saved = Some(want.export_state());
            }
            7 => {
                if let Some(snap) = &saved {
                    want.import_state(snap);
                    got.import_state(snap);
                    same(&want, &got, "after import_state")?;
                }
            }
            8 if rng.below(4) == 0 => {
                want.reset();
                got.reset();
                same(&want, &got, "after reset")?;
            }
            _ => {}
        }
        want.tick();
        got.tick();
        t += 1;
        same(&want, &got, "after a tick")?;
    }
    Ok(())
}

/// [`check_jit`] for a design, under the default and `Options::debug`
/// compile options, plus [`check_batches`] (which runs the JIT too).
pub fn check_jit_design(d: &Design, ticks: u64, seed: u64) -> Result<(), String> {
    for options in [Options::default(), Options::debug()] {
        let p = Arc::new(compile(d, &options).map_err(|e| format!("{e:?}"))?);
        check_jit(&p, ticks, seed, &SimOptions::default())?;
        check_batches(&p, seed)?;
    }
    Ok(())
}

// ── Specialization plans (`SimOptions::spec`) ─────────────────────────────

/// A plan for testing specialization on `p`: per phase of the default
/// runtime layout, up to three key candidates
/// ([`crate::compile::spec::SpecContext::candidates`]) drawn by `seed`,
/// each on its support or a random sub-mask of it (any sub-mask is exact),
/// at most 12 key bits; bodies for every value of a key of up to 4 bits,
/// and for the 8 most frequent keys of a sample run of `ticks` ticks with
/// random inputs. A phase without candidates is not keyed.
pub fn spec_plan(p: &Arc<Program>, seed: u64, ticks: u64) -> SpecPlan {
    use crate::compile::spec::{KeyField, KeyLayout};
    let contexts = spec_contexts(p, &SimOptions::default());
    let mut rng = Rng::new(seed ^ 0x5eed_5bec);
    let mut layouts: Vec<Option<KeyLayout>> = Vec::new();
    for cx in &contexts {
        let Some(cx) = cx else {
            layouts.push(None);
            continue;
        };
        let cands = cx.candidates(p);
        let mut fields: Vec<KeyField> = Vec::new();
        let mut width = 0;
        for _ in 0..3 {
            if cands.is_empty() {
                break;
            }
            let c = rng.pick(&cands);
            if fields.iter().any(|f| f.slot == c.slot) {
                continue;
            }
            let mut m = c.support;
            if rng.coin() {
                m &= rng.next_u64() | m.isolate_lowest_one();
            }
            while m.count_ones() + width > 12 {
                m &= m - 1;
            }
            if m != 0 {
                width += m.count_ones();
                fields.push(KeyField {
                    slot: c.slot,
                    mask: m,
                });
            }
        }
        layouts.push((!fields.is_empty()).then(|| KeyLayout::new(fields)));
    }
    // Key frequencies of a sample run (full settles compute every slot).
    let period = layouts.len().max(1) as u64;
    let mut freq: Vec<std::collections::HashMap<u64, u64>> =
        vec![Default::default(); layouts.len()];
    let mut sim = Sim::with_options(Arc::clone(p), SimOptions::baseline());
    let slots: Vec<u32> = (0..p.slot_count).collect();
    for _ in 0..ticks {
        for (port, q) in p.ports.inputs.iter().enumerate() {
            sim.set_input(port, rng.next_u64() & mask(q.width));
        }
        sim.tick();
        let k = (sim.tick_count() % period) as usize;
        if let Some(Some(l)) = layouts.get(k) {
            let row: Vec<u64> = slots.iter().map(|&s| sim.read(s)).collect();
            *freq[k].entry(l.pack(&row)).or_default() += 1;
        }
    }
    let phases = layouts
        .into_iter()
        .zip(freq)
        .map(|(l, f)| {
            let layout = l?;
            let w = layout.width();
            let mut keys: Vec<u64> = if w <= 4 {
                (0..1u64 << w).collect()
            } else {
                Vec::new()
            };
            let mut hot: Vec<(u64, u64)> = f.into_iter().collect();
            hot.sort_by_key(|&(k, n)| (std::cmp::Reverse(n), k));
            keys.extend(hot.into_iter().take(8).map(|x| x.0));
            Some(PhasePlan { layout, keys })
        })
        .collect();
    SpecPlan { phases }
}

/// [`SpecOptions`] with `plan`, fixed (P4: every body built with the
/// `Sim`).
pub fn spec_options(plan: SpecPlan) -> SpecOptions {
    SpecOptions::fixed(plan)
}
