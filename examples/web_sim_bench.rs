//! Simulator throughput in a browser (wasm32): the interpreter against the
//! wasm JIT (logi-sim feature `jit`) on the bundled NES workspace (nestest)
//! and on gallery designs, as the web build runs them (the JIT tiered:
//! generated, then instantiated asynchronously and installed).
//!
//! Build it and open `examples/web_sim_bench/index.html` through a local
//! server; `examples/web_sim_bench/run.js` does both, in headless Chromium:
//!
//! ```sh
//! cargo build --release --target wasm32-unknown-unknown --example web_sim_bench
//! wasm-bindgen --target web --out-dir target/web_sim_bench \
//!     target/wasm32-unknown-unknown/release/examples/web_sim_bench.wasm
//! node examples/web_sim_bench/run.js target/web_sim_bench [ticks...]
//! ```
//!
//! Natively it is the reference: `cargo run --release --features jit
//! --example web_sim_bench -- nes:200000 cpu16:200000` times the
//! interpreter against the Cranelift JIT in the same way.
//!
//! The page calls `load(design, jit, debug)`, polls `ready()` between
//! animation frames until the code is installed, then times `run(ticks)`;
//! `digest()` hashes everything observable (state and memories) so that
//! the two runtimes can be compared after the same ticks.

use std::hash::{Hash, Hasher};
use std::sync::Arc;

use gate_quest::sim::demos::DemoCatalog;
use gate_quest::sim::netlist::design_of;
use logi_sim::compile::{Options, Program, compile};
use logi_sim::sim::Sim;

fn workspace(name: &str) -> Result<gate_quest::sim::WorkspaceDocument, String> {
    if name == "nes" {
        let text = include_str!("../nes/workspace/manifest.json");
        return gate_quest::editor::workspace::parse_workspace_text("nes", text)
            .map_err(|e| format!("{e:?}"));
    }
    let demos = DemoCatalog::embedded()?;
    let demo = demos.get(name).ok_or(format!("no demo {name}"))?;
    Ok((*demo.workspace).clone())
}

/// The program of `name` (`nes`, the bundled NES workspace, or a gallery
/// id), compiled with `Options::debug` (the editor's) when `debug`.
fn program(name: &str, debug: bool) -> Result<Arc<Program>, String> {
    let design = design_of(&workspace(name)?)?;
    let options = if debug {
        Options::debug()
    } else {
        Options::default()
    };
    Ok(Arc::new(
        compile(&design, &options).map_err(|e| format!("{e:?}"))?,
    ))
}

/// A hash of the tick count, the state and every memory.
fn digest(sim: &Sim) -> String {
    let mut h = std::hash::DefaultHasher::new();
    sim.tick_count().hash(&mut h);
    // (In key order: the snapshot is a hash map.)
    let mut words: Vec<(String, u64)> = sim
        .export_state()
        .words
        .into_iter()
        .map(|(k, v)| (format!("{k:?}"), v))
        .collect();
    words.sort();
    words.hash(&mut h);
    for m in 0..sim.program().mems.len() as u32 {
        sim.mem(m).hash(&mut h);
    }
    format!("{:016x}", h.finish())
}

#[cfg(target_arch = "wasm32")]
mod bench {
    use std::cell::RefCell;
    use std::sync::Arc;

    use logi_sim::sim::{Sim, SimOptions};
    use wasm_bindgen::prelude::*;

    type Pending = Option<logi_sim::sim::jit::PendingJit>;

    thread_local! {
        static SIM: RefCell<Option<(Sim, Pending)>> = const { RefCell::new(None) };
    }

    /// Build `name` (`nes` or a gallery id) on the interpreter (`jit`
    /// false: the web build's defaults) or tiered for the JIT; compile
    /// with `Options::debug` (the editor's) when `debug`.
    #[wasm_bindgen]
    pub fn load(name: &str, jit: bool, debug: bool) -> Result<String, String> {
        std::panic::set_hook(Box::new(|info| {
            web_sys_log(&format!("panicked: {info}"));
        }));
        let p = super::program(name, debug)?;
        let info = format!("{} instrs, {} slots", p.instrs.len(), p.slot_count);
        let sim_options = if jit {
            SimOptions::jit().tier_layout()
        } else {
            SimOptions::default()
        };
        let sim = Sim::with_options(p, sim_options);
        let pending = if jit {
            let t = js_sys::Date::now();
            let pending = sim.compile_jit_async().map_err(|e| e.0)?;
            web_sys_log(&format!(
                "{name}: code generated in {:.0} ms",
                js_sys::Date::now() - t
            ));
            Some(pending)
        } else {
            None
        };
        SIM.set(Some((sim, pending)));
        Ok(info)
    }

    /// Whether the `Sim` is ready to time: its code installed (JIT) or
    /// at once (interpreter). Errors if the code failed.
    #[wasm_bindgen]
    pub fn ready() -> Result<bool, String> {
        SIM.with_borrow_mut(|s| {
            let (sim, pending) = s.as_mut().ok_or("nothing loaded")?;
            if let Some(p) = pending {
                match p.poll() {
                    None => return Ok(false),
                    Some(Ok(code)) => {
                        if !sim.install_jit(Some(Arc::new(code))) {
                            return Err("code refused".to_string());
                        }
                        *pending = None;
                    }
                    Some(Err(e)) => return Err(e.0),
                }
            }
            Ok(true)
        })
    }

    /// Whether wasm code runs.
    #[wasm_bindgen]
    pub fn jit_active() -> bool {
        SIM.with_borrow(|s| s.as_ref().is_some_and(|(sim, _)| sim.jit_active()))
    }

    /// Run `ticks` ticks in one `run` call.
    #[wasm_bindgen]
    pub fn run(ticks: u32) {
        SIM.with_borrow_mut(|s| s.as_mut().expect("loaded").0.run(u64::from(ticks)));
    }

    /// A hash of the tick count, the state and every memory.
    #[wasm_bindgen]
    pub fn digest() -> String {
        SIM.with_borrow(|s| super::digest(&s.as_ref().expect("loaded").0))
    }

    #[wasm_bindgen]
    extern "C" {
        #[wasm_bindgen(js_namespace = console, js_name = log)]
        fn web_sys_log(s: &str);
    }
}

/// The native reference (feature `jit`: Cranelift): the same designs,
/// chunks and timing, interpreter against JIT.
#[cfg(not(target_arch = "wasm32"))]
fn main() -> Result<(), String> {
    use logi_sim::sim::SimOptions;
    use std::time::{Duration, Instant};
    let args: Vec<String> = std::env::args().skip(1).collect();
    let names = if args.is_empty() {
        vec!["nes:200000".to_string(), "cpu16:200000".to_string()]
    } else {
        args
    };
    let env = |k: &str, d: f64| {
        std::env::var(k)
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(d)
    };
    let (reps, secs, warm, chunk) = (
        env("REPS", 5.0) as usize,
        env("SECONDS", 2.0),
        env("WARM", 1.0),
        env("CHUNK", 4096.0) as u64,
    );
    let debug = std::env::var("DEBUG").as_deref() == Ok("1");
    let options = |jit: bool| {
        if jit {
            SimOptions::jit()
        } else {
            SimOptions::default()
        }
    };
    for n in &names {
        let (name, ticks) = n.split_once(':').unwrap_or((n, "200000"));
        let ticks: u64 = ticks.parse().map_err(|_| "ticks")?;
        let p = program(name, debug)?;
        let mut d = Vec::new();
        for jit in [false, true] {
            let mut sim = Sim::with_options(Arc::clone(&p), options(jit));
            assert_eq!(sim.jit_active(), jit && cfg!(feature = "jit"));
            sim.run(ticks);
            d.push(digest(&sim));
        }
        println!(
            "{name} ({} instrs): after {ticks} ticks interpreter {}, JIT {}: {}",
            p.instrs.len(),
            d[0],
            d[1],
            if d[0] == d[1] { "same" } else { "DIFFERENT" }
        );
        let mut rates = [Vec::new(), Vec::new()];
        for r in 0..reps {
            for jit in if r % 2 == 1 {
                [true, false]
            } else {
                [false, true]
            } {
                let mut sim = Sim::with_options(Arc::clone(&p), options(jit));
                let t = Instant::now();
                while t.elapsed() < Duration::from_secs_f64(warm) {
                    sim.run(chunk);
                }
                let (t, mut n) = (Instant::now(), 0);
                while t.elapsed() < Duration::from_secs_f64(secs) {
                    sim.run(chunk);
                    n += chunk;
                }
                rates[jit as usize].push(n as f64 / t.elapsed().as_secs_f64());
            }
        }
        let median = |v: &mut Vec<f64>| {
            v.sort_by(f64::total_cmp);
            v[v.len() / 2]
        };
        let (i, j) = (median(&mut rates[0]), median(&mut rates[1]));
        println!(
            "{name}: interpreter {:.1} k ticks/s, JIT {:.1} k ticks/s, {:.2}x (median of {reps})",
            i / 1e3,
            j / 1e3,
            j / i
        );
    }
    Ok(())
}

#[cfg(target_arch = "wasm32")]
fn main() {}
