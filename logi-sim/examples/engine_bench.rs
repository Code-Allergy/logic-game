//! The engine against the raw runtime on one workspace, apples to apples:
//! the same program and `SimOptions`, in interleaved rounds in one
//! process, wall and CPU time, medians.
//!
//! ```sh
//! cargo run -p logi-sim --release --features jit --example engine_bench -- \
//!     nes/workspace [--secs 2] [--rounds 5] [--warm 4] [--spec] \
//!     [--debug] [--modes raw,chunk:4096,local,engine]
//! ```
//!
//! Modes:
//! - `raw`: `Sim::run(1M)` in a loop (the JIT compiled inline).
//! - `chunk:N`: `Sim::run(N)` in a loop.
//! - `local`: a `LocalEngine`, unlimited rate, `run_frame` with a 16 ms
//!   budget, a screen requested and an input poked once per frame.
//! - `hz:R`: `engine` paced at R ticks/s (`Rate::Hz`); real time on the
//!   NES console is 5369318.
//! - `engine`: the threaded `Engine` (tiered JIT, spec built on its compile
//!   thread when `--spec`), unlimited rate, the caller asking for the
//!   screen, reading the frame and poking an input 60 times a second; it is
//!   paused while the other modes run. Its CPU time is the worker thread's.
//!
//! `--spec` turns on adaptive specialization (`SpecOptions::default`) in
//! every mode; the raw modes then install their builds between runs.

mod workspace;

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use logi_sim::compile::{Options, compile};
use logi_sim::engine::{Engine, LocalEngine, MemProbe, Rate};
use logi_sim::netlist::{NodeId, NodeKind};
use logi_sim::sim::{Sim, SimOptions, SpecOptions, build_spec};

fn cpu(clock: i32) -> f64 {
    #[repr(C)]
    struct Timespec {
        sec: i64,
        nsec: i64,
    }
    unsafe extern "C" {
        fn clock_gettime(clock: i32, t: *mut Timespec) -> i32;
    }
    let mut t = Timespec { sec: 0, nsec: 0 };
    // SAFETY: `t` is a valid timespec.
    unsafe { clock_gettime(clock, &mut t) };
    t.sec as f64 + t.nsec as f64 / 1e9
}
const THREAD: i32 = 3;

/// Ids of this process's threads named `name` (Linux).
fn threads_named(name: &str) -> Vec<String> {
    let Ok(dir) = std::fs::read_dir("/proc/self/task") else {
        return Vec::new();
    };
    dir.flatten()
        .filter(|t| std::fs::read_to_string(t.path().join("comm")).is_ok_and(|c| c.trim() == name))
        .map(|t| t.file_name().to_string_lossy().into_owned())
        .collect()
}

/// On-CPU seconds of thread `tid` of this process (Linux schedstat).
fn thread_cpu(tid: &str) -> f64 {
    let s = std::fs::read_to_string(format!("/proc/self/task/{tid}/schedstat")).unwrap_or_default();
    let ns: f64 = s
        .split_whitespace()
        .next()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0.0);
    ns / 1e9
}

enum Runner {
    Raw {
        sim: Box<Sim>,
        chunk: u64,
        /// Spec builds: their times and install times (ms).
        builds: Vec<f64>,
        installs: Vec<f64>,
    },
    Local {
        e: Box<LocalEngine>,
        input: Option<NodeId>,
    },
    Threaded {
        e: Engine,
        input: Option<NodeId>,
        rate: Rate,
        /// The worker thread's id.
        tid: String,
    },
}

struct Mode {
    name: String,
    r: Runner,
    wall: Vec<f64>,
    cpu: Vec<f64>,
}

impl Mode {
    /// Run for about `secs`: (ticks, wall s, cpu s).
    fn round(&mut self, secs: f64) -> (u64, f64, f64) {
        let t0 = Instant::now();
        let origin = t0;
        let clock = move || origin.elapsed();
        match &mut self.r {
            Runner::Raw {
                sim,
                chunk,
                builds,
                installs,
            } => {
                let c0 = cpu(THREAD);
                let n0 = sim.tick_count();
                while t0.elapsed().as_secs_f64() < secs {
                    let mut left = 1_000_000u64;
                    while left > 0 {
                        let k = left.min(*chunk);
                        sim.run(k);
                        left -= k;
                    }
                    for q in sim.spec_requests() {
                        let p = Arc::clone(sim.program());
                        let t = Instant::now();
                        let b = build_spec(&p, &q);
                        builds.push(t.elapsed().as_secs_f64() * 1e3);
                        let t = Instant::now();
                        sim.install_spec(b);
                        installs.push(t.elapsed().as_secs_f64() * 1e3);
                    }
                }
                (
                    sim.tick_count() - n0,
                    t0.elapsed().as_secs_f64(),
                    cpu(THREAD) - c0,
                )
            }
            Runner::Local { e, input } => {
                let c0 = cpu(THREAD);
                let n0 = e.core.tick_count();
                let mut k = 0u64;
                while t0.elapsed().as_secs_f64() < secs {
                    if let Some(i) = input {
                        k += 1;
                        e.core.set_input(*i, (k / 30) & 1);
                    }
                    e.run_frame(Duration::from_millis(16), &clock);
                    std::hint::black_box(e.frame());
                }
                (
                    e.core.tick_count() - n0,
                    t0.elapsed().as_secs_f64(),
                    cpu(THREAD) - c0,
                )
            }
            Runner::Threaded {
                e,
                input,
                rate,
                tid,
            } => {
                e.set_rate(*rate);
                e.sync();
                let c0 = thread_cpu(tid);
                let t0 = Instant::now();
                let n0 = e.tick_count();
                let mut k = 0u64;
                while t0.elapsed().as_secs_f64() < secs {
                    std::thread::sleep(Duration::from_micros(16_667));
                    if let Some(i) = input {
                        k += 1;
                        e.set_input(*i, (k / 30) & 1);
                    }
                    e.request_mems();
                    std::hint::black_box(e.frame());
                }
                let n = e.tick_count() - n0;
                let (w, c) = (t0.elapsed().as_secs_f64(), thread_cpu(tid) - c0);
                e.set_rate(Rate::Paused);
                e.sync();
                (n, w, c)
            }
        }
    }
}

fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let folder = args
        .first()
        .ok_or("usage: engine_bench <workspace> [flags]")?;
    let num = |flag: &str, d: f64| -> f64 {
        args.iter()
            .position(|a| a == flag)
            .and_then(|i| args.get(i + 1))
            .and_then(|v| v.parse().ok())
            .unwrap_or(d)
    };
    let secs = num("--secs", 2.0);
    let rounds = num("--rounds", 5.0) as usize;
    let warm = num("--warm", 4.0);
    let spec = args.iter().any(|a| a == "--spec");
    // `--debug`: compile with `Options::debug` (every net probeable), as
    // the game does.
    let options = if args.iter().any(|a| a == "--debug") {
        Options::debug()
    } else {
        Options::default()
    };
    let modes = args
        .iter()
        .position(|a| a == "--modes")
        .and_then(|i| args.get(i + 1))
        .cloned()
        .unwrap_or_else(|| "raw,chunk:4096,local,engine".into());
    let w = workspace::load(Path::new(folder))?;
    let design = Arc::new(w.design);
    let program = Arc::new(compile(&design, &options).map_err(|e| format!("{e:?}"))?);
    let sim_options = SimOptions {
        jit: true,
        spec: spec.then(SpecOptions::default),
        // `SUPERSTEP_CHUNK`: items per superstep chunk function.
        superstep_chunk: std::env::var("SUPERSTEP_CHUNK")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(SimOptions::default().superstep_chunk),
        ..SimOptions::default()
    };
    let root = design.root_def();
    let screens: Vec<MemProbe> = root
        .nodes
        .iter()
        .filter(|(_, n)| matches!(n.kind, NodeKind::Screen { .. }))
        .map(|(id, _)| MemProbe {
            path: Vec::new(),
            node: id,
        })
        .collect();
    // An input to poke: a button if there is one (`p1_*`), else the first.
    let ports = &program.ports.inputs;
    let input = ports
        .iter()
        .find(|p| p.name.as_deref().is_some_and(|n| n.starts_with("p1_")))
        .or(ports.first())
        .map(|p| p.node);
    if std::env::var_os("NO_INPUT").is_some() {
        println!("(no input pokes)");
    }
    let input = input.filter(|_| std::env::var_os("NO_INPUT").is_none());
    println!(
        "{folder}: spec {}, {:?} compile options, {} screens, input {:?}",
        if spec { "on" } else { "off" },
        if options == Options::debug() {
            "debug"
        } else {
            "default"
        },
        screens.len(),
        input
    );

    let mut list = Vec::new();
    for m in modes.split(',') {
        let (m, arg) = m.split_once(':').unwrap_or((m, ""));
        let r = match m {
            "raw" | "chunk" => {
                let chunk = arg.parse().unwrap_or(1_000_000);
                let t = Instant::now();
                let mut sim =
                    Box::new(Sim::with_options(Arc::clone(&program), sim_options.clone()));
                if !sim.jit_active() {
                    return Err("no JIT (build with --features jit)".into());
                }
                eprintln!(
                    "  {m}: built in {:.0} ms ({} bytes of code)",
                    t.elapsed().as_secs_f64() * 1e3,
                    sim.jit_code().map_or(0, |c| c.code_bytes)
                );
                if let Some(i) = input {
                    let p = sim.program().ports.input_by_node(i).expect("a port");
                    sim.set_input(p, 0);
                }
                Runner::Raw {
                    sim,
                    chunk,
                    builds: Vec::new(),
                    installs: Vec::new(),
                }
            }
            "local" => {
                let mut e = Box::new(LocalEngine::with_sim_options(
                    options.clone(),
                    sim_options.clone(),
                ));
                e.load(&design, 1);
                e.core.set_mem_probes(screens.clone());
                e.core.set_rate(Rate::Unlimited);
                Runner::Local { e, input }
            }
            "engine" | "hz" => {
                // `hz:R`: the threaded engine paced at R ticks/s.
                let rate = match m {
                    "hz" => Rate::Hz(arg.parse().map_err(|_| "hz:<ticks per second>")?),
                    _ => Rate::Unlimited,
                };
                let before = threads_named("sim-worker");
                let e = Engine::spawn_with(options.clone(), sim_options.clone());
                e.load(Arc::clone(&design), 1);
                e.wait_event(Duration::from_secs(600)).ok_or("no load")?;
                let tid = threads_named("sim-worker")
                    .into_iter()
                    .find(|t| !before.contains(t))
                    .ok_or("no worker thread")?;
                e.set_mem_probes(screens.clone());
                e.sync();
                Runner::Threaded {
                    e,
                    input,
                    rate,
                    tid,
                }
            }
            _ => return Err(format!("unknown mode {m}")),
        };
        list.push(Mode {
            name: if arg.is_empty() {
                m.to_string()
            } else {
                format!("{m}:{arg}")
            },
            r,
            wall: Vec::new(),
            cpu: Vec::new(),
        });
    }
    for m in &mut list {
        let (n, w, _) = m.round(warm);
        eprintln!("  warm {}: {:.0} k/s", m.name, n as f64 / w / 1e3);
    }
    for round in 0..rounds {
        let k = list.len();
        for j in 0..k {
            let m = &mut list[(j + round) % k];
            let (n, w, c) = m.round(secs);
            m.wall.push(n as f64 / w / 1e3);
            m.cpu.push(n as f64 / c / 1e3);
        }
    }
    let med = |v: &[f64]| {
        let mut v = v.to_vec();
        v.sort_by(f64::total_cmp);
        v[v.len() / 2]
    };
    for m in &list {
        println!(
            "  {:<12} k ticks/s: wall median {:>6.0} ({:.0}..{:.0}), per cpu-s median {:>6.0}",
            m.name,
            med(&m.wall),
            m.wall.iter().copied().fold(f64::MAX, f64::min),
            m.wall.iter().copied().fold(0.0, f64::max),
            med(&m.cpu),
        );
        if let Runner::Raw {
            builds, installs, ..
        } = &m.r
            && !builds.is_empty()
        {
            let max = |v: &[f64]| v.iter().copied().fold(0.0, f64::max);
            println!(
                "  {:<12} spec builds: {}, {:.1} ms in all, longest {:.2} ms; installs: longest {:.3} ms",
                "",
                builds.len(),
                builds.iter().sum::<f64>(),
                max(builds),
                max(installs),
            );
        }
    }
    Ok(())
}
