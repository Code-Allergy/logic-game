//! Trace-compilation experiment bench (`claude/trace-experiments`): the
//! JIT `Sim::run` of a workspace with adaptive specialization (automatic
//! plan, the engine's defaults, bodies built on a thread of their own)
//! and/or with spec off (supersteps), in interleaved rounds; a hash of the
//! final state and memories, to compare builds for exactness.
//!
//! ```sh
//! cargo run -p logi-sim --profile profiling --features jit --example trace_bench -- \
//!     nes/workspace [--demo] [--warm 8000000] [--ticks 1000000] [--rounds 5] [--modes spec,off]
//! ```
//!
//! With `LOGI_JIT_COUNT=1` the generated code counts its slot loads,
//! stores and function entries; the bench prints them per tick (the
//! rates are then meaningless).

mod workspace;

use std::hash::{Hash, Hasher};
use std::path::Path;
use std::sync::Arc;
use std::sync::mpsc;
use std::time::Instant;

use logi_sim::compile::{Options, Program, compile};
use logi_sim::sim::{Sim, SimOptions, SpecBuilt, SpecOptions, SpecRequest, build_spec, jit};

/// The demo program of `spec_keys --demo` (renders like a game).
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

struct Runner {
    name: String,
    sim: Sim,
    builds: Vec<SpecBuilt>,
    rates: Vec<f64>,
}

fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let folder = args
        .first()
        .ok_or("usage: trace_bench <workspace> [flags]")?;
    let num = |flag: &str, d: u64| -> u64 {
        args.iter()
            .position(|a| a == flag)
            .and_then(|i| args.get(i + 1))
            .and_then(|v| v.parse().ok())
            .unwrap_or(d)
    };
    let demo = args.iter().any(|a| a == "--demo");
    let warm = num("--warm", 8_000_000);
    let ticks = num("--ticks", 1_000_000);
    let rounds = num("--rounds", 5);
    let modes = args
        .iter()
        .position(|a| a == "--modes")
        .and_then(|i| args.get(i + 1))
        .cloned()
        .unwrap_or_else(|| "spec,off".into());
    let w = workspace::load(Path::new(folder))?;
    let mut program = compile(&w.design, &Options::default()).map_err(|e| format!("{e:?}"))?;
    if demo {
        patch_demo(&mut program);
    }
    let program = Arc::new(program);

    let (to_builder, requests) = mpsc::channel::<(usize, SpecRequest)>();
    let (to_main, built) = mpsc::channel::<(usize, SpecBuilt)>();
    let prog = Arc::clone(&program);
    let builder = std::thread::spawn(move || {
        for (i, r) in requests {
            if to_main.send((i, build_spec(&prog, &r))).is_err() {
                break;
            }
        }
    });
    let mut runners: Vec<Runner> = Vec::new();
    for m in modes.split(',') {
        // `off:N`: supersteps of chunks of N items.
        let (m, chunk) = match m.split_once(':') {
            Some((m, n)) => (m, n.parse::<usize>().map_err(|e| e.to_string())?),
            None => (m, 0),
        };
        let (name, spec) = match m {
            "spec" => (
                "spec (automatic, cap 16)".to_string(),
                Some(SpecOptions {
                    inline_build: false,
                    ..SpecOptions::default()
                }),
            ),
            "ispec" => (
                "spec (automatic, inline builds)".to_string(),
                Some(SpecOptions::default()),
            ),
            "off" => (
                format!(
                    "spec off (supersteps, {})",
                    if chunk > 0 { chunk } else { 64 }
                ),
                None,
            ),
            _ => return Err(format!("unknown mode {m}")),
        };
        let built = Instant::now();
        let sim = Sim::with_options(
            Arc::clone(&program),
            SimOptions {
                spec,
                superstep_ticks: std::env::var("SUPERSTEP_TICKS")
                    .ok()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(SimOptions::default().superstep_ticks),
                superstep_chunk: if chunk > 0 {
                    chunk
                } else {
                    std::env::var("SUPERSTEP_CHUNK")
                        .ok()
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(SimOptions::default().superstep_chunk)
                },
                ..SimOptions::jit()
            },
        );
        eprintln!("  {name}: built in {:.2} s", built.elapsed().as_secs_f64());
        if !sim.jit_active() {
            return Err("JIT not active (build with --features jit)".into());
        }
        runners.push(Runner {
            name,
            sim,
            builds: Vec::new(),
            rates: Vec::new(),
        });
    }
    // On-CPU seconds of this thread (CLOCK_THREAD_CPUTIME_ID), less
    // sensitive than wall time to other processes on a shared machine.
    let cpu = || -> f64 {
        #[repr(C)]
        struct Timespec {
            sec: i64,
            nsec: i64,
        }
        unsafe extern "C" {
            fn clock_gettime(clock: i32, t: *mut Timespec) -> i32;
        }
        let mut t = Timespec { sec: 0, nsec: 0 };
        // SAFETY: `t` is a valid timespec; 3 is CLOCK_THREAD_CPUTIME_ID.
        unsafe { clock_gettime(3, &mut t) };
        t.sec as f64 + t.nsec as f64 / 1e9
    };
    let step = |rs: &mut Vec<Runner>, i: usize, n: u64| -> f64 {
        let t = Instant::now();
        let c0 = cpu();
        let mut left = n;
        while left > 0 {
            let k = left.min(30_000);
            rs[i].sim.run(k);
            left -= k;
            for q in rs[i].sim.spec_requests() {
                to_builder.send((i, q)).expect("builder alive");
            }
            while let Ok((j, b)) = built.try_recv() {
                rs[j].builds.push(b);
            }
            let r = &mut rs[i];
            for b in std::mem::take(&mut r.builds) {
                r.sim.install_spec(b);
            }
        }
        if std::env::var_os("WALL").is_some() {
            t.elapsed().as_secs_f64()
        } else {
            cpu() - c0
        }
    };
    for i in 0..runners.len() {
        step(&mut runners, i, warm);
    }
    // Let the last builds land.
    std::thread::sleep(std::time::Duration::from_millis(300));
    for i in 0..runners.len() {
        step(&mut runners, i, 30_000);
    }
    let _ = jit::probe_take();
    let mut counts = vec![[0u64; 5]; runners.len()];
    let hits0: Vec<(u64, u64)> = runners
        .iter()
        .map(|r| (r.sim.spec_stats().hits, r.sim.spec_stats().misses))
        .collect();
    for round in 0..rounds {
        let n = runners.len();
        for j in 0..n {
            let i = (j + round as usize) % n;
            let secs = step(&mut runners, i, ticks);
            runners[i].rates.push(ticks as f64 / secs / 1e3);
            let c = jit::probe_take();
            for k in 0..5 {
                counts[i][k] += c[k];
            }
        }
    }
    drop(to_builder);
    builder.join().map_err(|_| "builder panicked")?;
    println!(
        "{folder}{}: {rounds} rounds of {ticks} ticks after {warm}",
        if demo { " (demo)" } else { "" }
    );
    for ((r, c), h0) in runners.iter().zip(&counts).zip(&hits0) {
        let mut v = r.rates.clone();
        v.sort_by(f64::total_cmp);
        let st = r.sim.spec_stats();
        print!(
            "  {:<30} median {:>6.0} k ticks/s ({:.0}..{:.0}); keyed phases {}, bodies {}, hits {:.1}%",
            r.name,
            v[v.len() / 2],
            v[0],
            v[v.len() - 1],
            st.keyed_phases,
            st.bodies,
            100.0 * (st.hits - h0.0) as f64 / (st.hits - h0.0 + st.misses - h0.1).max(1) as f64
        );
        if jit::probe_counting() {
            let t = (rounds * ticks) as f64;
            print!(
                "; per tick: {:.1} slot loads, {:.1} slot stores, {:.2} settle + {:.2} commit + {:.2} superstep-chunk calls",
                c[0] as f64 / t,
                c[1] as f64 / t,
                c[2] as f64 / t,
                c[3] as f64 / t,
                c[4] as f64 / t
            );
        }
        if let Some(sup) = r.sim.jit_code().and_then(|j| j.superstep()) {
            let i = &sup.info;
            print!(
                "; superstep {} ticks, {} KiB in {} chunks, {} of {} writes stored",
                i.ticks,
                i.code_bytes / 1024,
                i.chunks,
                i.stores,
                i.writes
            );
        } else {
            print!(
                "; code {} KiB program + {} KiB spec",
                r.sim.jit_code().map_or(0, |j| j.code_bytes) / 1024,
                st.code_bytes / 1024
            );
        }
        println!("; state hash {:016x}", state_hash(&r.sim, &program));
    }
    Ok(())
}

fn state_hash(sim: &Sim, p: &Program) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    let s = sim.export_state();
    s.tick.hash(&mut h);
    let mut words: Vec<_> = s
        .words
        .iter()
        .map(|(k, v)| (format!("{k:?}"), *v))
        .collect();
    words.sort();
    words.hash(&mut h);
    for m in 0..p.mems.len() as u32 {
        sim.mem(m).hash(&mut h);
    }
    h.finish()
}

fn patch_demo(p: &mut Program) {
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
    v[0x4000 + 0x29] = 0x1E;
    for (k, b) in [0x40u8, 0xC0, 0x00, 0xC0, 0x52, 0xC0].iter().enumerate() {
        v[0x7FFA + k] = u64::from(*b);
    }
    m.init = v.into();
}
