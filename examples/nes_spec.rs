//! Issue #117 experiment: state-keyed specialization of the fused-run
//! bodies (`SimOptions::spec`). Profiles the values of the chosen key
//! slots, builds bodies for the most frequent keys, checks the state
//! against the unspecialized JIT, then benchmarks both, interleaved.
//!
//! ```sh
//! cargo run --release --features jit --example nes_spec -- nes/workspace \
//!     --slot 1234:8 --slot 99:1 [--ticks 3000000] [--keys 24] [--rounds 3] [--check 1000000]
//! ```
//!
//! `--slot S:W` names a slot (number from `nes_audit`) and its key width;
//! `--net chip/path:net-name:W` is not supported: use the audit's dump.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

#[path = "nes_demo/mod.rs"]
mod nes_demo;

use gate_quest::mcp::suite;
use gate_quest::sim::netlist::design_of;
use logi_sim::compile::{Options, compile};
use logi_sim::sim::{Sim, SimOptions, SpecPlan};

/// This thread's CPU time in seconds (`/proc/thread-self/schedstat`):
/// with other jobs on the machine, CPU time is a steadier measure than
/// wall time.
fn cpu_time() -> f64 {
    let s = std::fs::read_to_string("/proc/thread-self/schedstat").unwrap_or_default();
    s.split_whitespace().next().and_then(|x| x.parse::<f64>().ok()).unwrap_or(0.0) / 1e9
}

fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let folder = args.first().ok_or("usage: nes_spec <workspace> --slot S:W ...")?;
    let mut slots: Vec<(u32, u8)> = Vec::new();
    let mut ports: Vec<String> = Vec::new();
    let mut ticks = 3_000_000u64;
    let mut nkeys = 24usize;
    let mut rounds = 3usize;
    let mut check = 1_000_000u64;
    let mut profile = 300_000u64;
    let mut warm = 1_500_000u64;
    let mut phases: Vec<u8> = Vec::new();
    let mut ports2: Vec<String> = Vec::new();
    let mut slots2: Vec<(u32, u8)> = Vec::new();
    let mut nkeys2 = 24usize;
    let mut it = args.iter().skip(1);
    while let Some(a) = it.next() {
        let mut v = || it.next().cloned().unwrap_or_default();
        match a.as_str() {
            "--slot" => {
                let s = v();
                let (x, w) = s.split_once(':').ok_or("--slot S:W")?;
                slots.push((x.parse().map_err(|_| "slot")?, w.parse().map_err(|_| "width")?));
            }
            "--port" => ports.push(v()),
            "--port2" => ports2.push(v()),
            "--keys2" => nkeys2 = v().parse().map_err(|_| "keys2")?,
            "--ticks" => ticks = v().parse().map_err(|_| "ticks")?,
            "--keys" => nkeys = v().parse().map_err(|_| "keys")?,
            "--rounds" => rounds = v().parse().map_err(|_| "rounds")?,
            "--check" => check = v().parse().map_err(|_| "check")?,
            "--phases" => phases = v().split(',').map(|x| x.parse().unwrap()).collect(),
            "--warm" => warm = v().parse().map_err(|_| "warm")?,
            "--profile" => profile = v().parse().map_err(|_| "profile")?,
            other => return Err(format!("unknown argument {other}")),
        }
    }
    let ws = suite::read_workspace(Path::new(folder))?;
    let design = design_of(&ws)?;
    let program = Arc::new(nes_demo::maybe(compile(&design, &Options::default()).map_err(|e| format!("{e:?}"))?));
    for (which, spec) in ports.iter().map(|p| (1, p)).chain(ports2.iter().map(|p| (2, p))) {
        // chip/sub/name[:W]: a named node's output net (any node kind, an
        // Output port reads its input net) in the instance at chip/sub.
        let (path_name, w) = match spec.rsplit_once(':') {
            Some((a, b)) => (a, Some(b.parse::<u8>().map_err(|_| "width")?)),
            None => (spec.as_str(), None),
        };
        let mut parts: Vec<&str> = path_name.split('/').collect();
        let name = parts.pop().unwrap();
        let mut def = design.root;
        let mut path = Vec::new();
        for label in parts {
            let d = &design.defs[def];
            let (node, child) = d
                .nodes
                .iter()
                .find_map(|(id, n)| match &n.kind {
                    logi_sim::netlist::NodeKind::Instance(c)
                        if d.name_of(id) == Some(label) || design.defs[*c].name == label => Some((id, *c)),
                    _ => None,
                })
                .ok_or(format!("no instance {label}"))?;
            path.push(node);
            def = child;
        }
        let d = &design.defs[def];
        let (id, n) = d
            .nodes
            .iter()
            .find(|(id, _)| d.name_of(*id) == Some(name))
            .ok_or(format!("no node {name} in {}", d.name))?;
        let net = if matches!(n.kind, logi_sim::netlist::NodeKind::Output { .. }) {
            n.inputs[0].ok_or("unconnected output")?
        } else {
            n.outputs[0]
        };
        let _ = id;
        let s = program.debug.slot(&path, net).ok_or(format!("{spec}: no slot (optimized away?)"))?;
        let (real, shift, m) = program.resolve(s).ok_or("resolve")?;
        let width = w.unwrap_or(d.nets[net].width);
        if shift != 0 || (m != u64::MAX && m.count_ones() != u32::from(width)) {
            println!("{spec}: slot {s} is bits {shift}.. of word s{real}: skipped (a view)");
            continue;
        }
        println!("{spec}: s{real} w{width}");
        if which == 1 {
            slots.push((real, width));
        } else {
            slots2.push((real, width));
        }
    }
    slots.dedup();

    // Profile the keys (tick by tick, every slot fresh).
    let profile_keys = |slots: &[(u32, u8)], phases: &dyn Fn(u8) -> bool, nkeys: usize| -> Vec<u64> {
        let mut sim = Sim::with_options(
            Arc::clone(&program),
            SimOptions {
                guard: false,
                ..SimOptions::jit()
            },
        );
        let pack = |sim: &Sim| {
            let mut key = 0u64;
            let mut shift = 0;
            for &(s, w) in slots {
                key |= (sim.read(s) & logi_sim::netlist::mask(w)) << shift;
                shift += u32::from(w);
            }
            key
        };
        sim.run(warm);
        let mut hist: HashMap<u64, u64> = HashMap::new();
        let mut changes = 0u64;
        let mut prev = u64::MAX;
        let mut seen = 0u64;
        for _ in 0..profile {
            sim.tick();
            let ph = (sim.tick_count() % 3) as u8;
            if !phases(ph) {
                continue;
            }
            seen += 1;
            let k = pack(&sim);
            *hist.entry(k).or_default() += 1;
            if k != prev {
                changes += 1;
                prev = k;
            }
        }
        let mut h: Vec<(u64, u64)> = hist.into_iter().collect();
        h.sort_by(|a, b| b.1.cmp(&a.1));
        let keys: Vec<u64> = h.iter().take(nkeys).map(|x| x.0).collect();
        let covered: u64 = h.iter().take(nkeys).map(|x| x.1).sum();
        println!(
            "profile {seen} ticks: {} distinct keys, key changes {changes} ({:.4}/tick); top {} cover {:.3}%",
            h.len(),
            changes as f64 / seen.max(1) as f64,
            keys.len(),
            covered as f64 / seen.max(1) as f64 * 100.0
        );
        for (k, n) in h.iter().take(4) {
            println!("  key {k:#x}: {:.2}%", *n as f64 / seen.max(1) as f64 * 100.0);
        }
        keys
    };
    let in1 = |ph: u8| phases.is_empty() || phases.contains(&ph);
    let keys = profile_keys(&slots, &in1, nkeys);
    let plan2 = (!slots2.is_empty()).then(|| {
        let keys2 = profile_keys(&slots2, &|ph| !in1(ph), nkeys2);
        Arc::new(SpecPlan { slots: slots2.clone(), keys: keys2, phases: Vec::new() })
    });
    let plan = Arc::new(SpecPlan { slots: slots.clone(), keys, phases: phases.clone() });
    let spec_opts = SimOptions {
        spec: Some(Arc::clone(&plan)),
        spec2: plan2.clone(),
        ..SimOptions::jit()
    };

    // Correctness: the same state at every batch boundary.
    let t0 = Instant::now();
    let mut a = Sim::with_options(Arc::clone(&program), SimOptions::jit());
    let mut b = Sim::with_options(Arc::clone(&program), spec_opts.clone());
    println!("spec Sim built + JIT in {:.2} s", t0.elapsed().as_secs_f64());
    if !b.jit_active() {
        return Err("JIT not active".into());
    }
    let mut t = 0u64;
    let mut n = 1u64;
    let mut boundaries = 0;
    while t < check {
        n = (n * 7919 + 13) % 20_011 + 1;
        a.run(n);
        b.run(n);
        t += n;
        boundaries += 1;
        if a.export_state() != b.export_state() {
            return Err(format!("state differs at tick {t}"));
        }
    }
    println!(
        "check: {t} ticks, {boundaries} batch boundaries, same state; hits {} misses {} ({:.3}% specialized)",
        b.spec_hits,
        b.spec_misses,
        b.spec_hits as f64 / (b.spec_hits + b.spec_misses).max(1) as f64 * 100.0
    );

    #[cfg(feature = "counters")]
    {
        for (name, o) in [("general", SimOptions::jit()), ("spec", spec_opts.clone())] {
            let mut s = Sim::with_options(Arc::clone(&program), o);
            s.run(warm);
            s.reset_counters();
            s.run(200_000);
            let c = s.counters();
            println!(
                "{name:8} executed {:.1} ops/tick ({:.1} instrs, {:.1} commits)",
                c.ops() as f64 / c.ticks as f64,
                c.instrs as f64 / c.ticks as f64,
                c.commits as f64 / c.ticks as f64
            );
        }
    }

    // Throughput, interleaved.
    let mut rates: [Vec<f64>; 2] = [Vec::new(), Vec::new()];
    let mut cpu_rates: [Vec<f64>; 2] = [Vec::new(), Vec::new()];
    let mut sims = [
        Sim::with_options(Arc::clone(&program), SimOptions::jit()),
        Sim::with_options(Arc::clone(&program), spec_opts),
    ];
    for s in &mut sims {
        s.run(warm);
    }
    for _ in 0..rounds {
        for (k, s) in sims.iter_mut().enumerate() {
            let st = Instant::now();
            let c0 = cpu_time();
            s.run(ticks);
            rates[k].push(ticks as f64 / st.elapsed().as_secs_f64() / 1e3);
            cpu_rates[k].push(ticks as f64 / (cpu_time() - c0) / 1e3);
        }
    }
    for (k, name) in ["general", "spec", "general (cpu time)", "spec (cpu time)"].iter().enumerate() {
        let r = if k < 2 { &rates[k] } else { &cpu_rates[k - 2] };
        let med = {
            let mut x = r.clone();
            x.sort_by(f64::total_cmp);
            x[x.len() / 2]
        };
        println!(
            "{name:18} k ticks/s: median {med:.0}, runs {:?}",
            r.iter().map(|x| x.round() as i64).collect::<Vec<_>>()
        );
    }
    Ok(())
}
