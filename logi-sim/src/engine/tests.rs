use std::cell::Cell;
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::*;
use crate::netlist::{Design, NodeKind};

/// A clock that advances by `tick` on every read, plus manual jumps. Reads
/// must advance enough that a chunk of ticks looks costly; otherwise the
/// adaptive chunk size grows without bound while the budget never elapses.
struct FakeClock {
    now: Cell<Duration>,
    tick: Duration,
}

impl FakeClock {
    fn new() -> Self {
        Self {
            now: Cell::new(Duration::ZERO),
            tick: Duration::from_micros(40),
        }
    }
    fn read(&self) -> Duration {
        let t = self.now.get() + self.tick;
        self.now.set(t);
        t
    }
    fn advance(&self, d: Duration) {
        self.now.set(self.now.get() + d);
    }
}

/// Counter register `q` (8-bit) incremented by input `en`, a RAM recording
/// the count at address `q[3:0]`, and output `mem` reading it back.
/// `extra` adds unrelated logic (an edit).
fn counter_design(extra: bool) -> (Design, NodeId, NetId, NetId) {
    let mut d = Design::new("top");
    let r = d.root;
    let mut b = d.builder(r);
    let en = b.input("en", 1);
    let (reg, q) = b.state_placeholder(NodeKind::Register { w: 8 });
    let next = b.inc(q);
    b.connect(en, (reg, 0));
    b.connect(next, (reg, 1));
    let one = b.one();
    let addr = b.split(q, 0, 4);
    let mem = b.ram(addr, one, q, &[]);
    b.output("q", q);
    b.output("mem", mem);
    if extra {
        let x = b.input("x", 1);
        let nx = b.not(x);
        b.output("nx", nx);
    }
    let en_node = d.root_def().nets[en].driver.0;
    (d, en_node, q, mem)
}

fn probes(q: NetId, mem: NetId) -> Vec<Probe> {
    vec![
        Probe {
            path: vec![],
            net: q,
        },
        Probe {
            path: vec![],
            net: mem,
        },
    ]
}

// ── Core / LocalEngine (deterministic clock) ─────────────────────────────

#[test]
fn paced_rate_matches_wall_time() {
    let (d, en, q, mem) = counter_design(false);
    let clock = FakeClock::new();
    let c = || clock.read();
    let mut e = LocalEngine::new(Options::debug());
    e.load(&d, 1);
    e.core.set_input(en, 1);
    e.core.set_probes(probes(q, mem));
    e.core.set_rate(Rate::Hz(100.0));
    let mut ticks = 0;
    for _ in 0..100 {
        clock.advance(Duration::from_millis(10));
        // Resending the same rate every frame must not reset pacing.
        e.core.set_rate(Rate::Hz(100.0));
        ticks += e.run_frame(Duration::from_millis(5), &c);
    }
    // Clock reads add a little time of their own.
    assert!(
        (99..=103).contains(&ticks),
        "{ticks} ticks in 1 s at 100 Hz"
    );
    assert_eq!(e.frame().tick, ticks);
}

#[test]
fn paced_rate_drops_debt_it_cannot_pay() {
    let (d, ..) = counter_design(false);
    let clock = FakeClock::new();
    let c = || clock.read();
    let mut e = LocalEngine::new(Options::debug());
    e.load(&d, 1);
    e.core.set_rate(Rate::Hz(1000.0));
    e.run_frame(Duration::from_millis(5), &c);
    clock.advance(Duration::from_secs(10)); // a 10 s stall
    let n = e.run_frame(Duration::from_secs(1), &c);
    assert!(
        n <= 252,
        "caught up {n} ticks after a stall; at most ~0.25 s worth"
    );
    clock.advance(Duration::from_millis(100));
    let n = e.run_frame(Duration::from_secs(1), &c);
    assert!((99..=103).contains(&n), "back to normal pacing: {n}");
}

#[test]
fn steps_run_while_paused_and_probes_read_back() {
    let (d, en, q, mem) = counter_design(false);
    let clock = FakeClock::new();
    let c = || clock.read();
    let mut e = LocalEngine::new(Options::debug());
    e.load(&d, 7);
    e.core.set_input(en, 1);
    let mut ps = probes(q, mem);
    ps.push(Probe {
        path: vec![NodeId::default()],
        net: q,
    }); // no such instance
    e.core.set_probes(ps);
    e.core.step(5);
    assert_eq!(e.run_frame(Duration::from_secs(1), &c), 5);
    assert_eq!(e.run_frame(Duration::from_secs(1), &c), 0, "paused");
    let f = e.frame();
    assert_eq!((f.revision, f.tick), (7, 5));
    // en reaches the register at tick 1's settle; 4 increments committed.
    assert_eq!(f.values[0], 4);
    assert_eq!(f.present, vec![true, true, false]);
    assert_eq!(f.values.len(), 3, "readback is exactly the probes");
}

fn ram_node(d: &Design) -> NodeId {
    d.root_def()
        .nodes
        .iter()
        .find(|(_, n)| matches!(n.kind, NodeKind::Ram { .. }))
        .unwrap()
        .0
}

/// Memory probes copy a whole memory into the frame once per frame, and
/// only when it changed (the `Arc` stays the same otherwise).
#[test]
fn mem_probes_copy_changed_memories_once_per_frame() {
    let (d, en, ..) = counter_design(false);
    let ram = ram_node(&d);
    let clock = FakeClock::new();
    let c = || clock.read();
    let mut e = LocalEngine::new(Options::debug());
    e.load(&d, 1);
    e.core.set_input(en, 1);
    e.core.set_mem_probes(vec![
        MemProbe {
            path: Vec::new(),
            node: ram,
        },
        MemProbe {
            path: vec![ram],
            node: ram,
        }, // no such instance
    ]);
    e.core.step(6);
    e.run_frame(Duration::from_secs(1), &c);
    let first = e.frame().mems[0].clone().expect("resolved");
    assert_eq!(e.frame().mems[1], None);
    // The count is written at address count: 0..=4 by now (en is staged
    // for a tick).
    assert_eq!(&first[..6], &[0, 1, 2, 3, 4, 0]);
    assert_eq!(first.len(), 16);
    // Paused: nothing changes, the frame keeps the same copy.
    e.run_frame(Duration::from_secs(1), &c);
    assert!(Arc::ptr_eq(e.frame().mems[0].as_ref().unwrap(), &first));
    e.core.step(1);
    e.run_frame(Duration::from_secs(1), &c);
    let next = e.frame().mems[0].clone().unwrap();
    assert!(!Arc::ptr_eq(&next, &first));
    assert_eq!(next[5], 5);
    // An edit keeps the probe (and the memory).
    let (d2, ..) = counter_design(true);
    e.load(&d2, 2);
    e.run_frame(Duration::from_secs(1), &c);
    assert_eq!(e.frame().mems[0].as_deref().map(|m| m[5]), Some(5));
}

/// Probes on nets that live only in a packed lane (lane slots) and a
/// threaded `Sim` read the same values as an unpacked, single-threaded
/// engine (#41).
#[test]
fn probes_read_packed_lanes_and_threads() {
    use crate::compile::PackMode;
    use crate::testkit::{GenConfig, generate};
    let d = generate(&GenConfig::gates(4, 20_000, 1)).design;
    let probes: Vec<Probe> = d
        .root_def()
        .nets
        .keys()
        .map(|net| Probe { path: vec![], net })
        .collect();
    let clock = FakeClock::new();
    let c = || clock.read();
    let mut frames = Vec::new();
    for (opts, threads) in [
        (Options::debug(), 1),
        (
            Options {
                pack: PackMode::All,
                pack_min: 1,
                ..Options::debug()
            },
            4,
        ),
    ] {
        let mut e = LocalEngine::new(opts);
        e.core.set_sim_options(crate::sim::SimOptions {
            threads,
            par_min: 64,
            ..Default::default()
        });
        e.load(&d, 1);
        let sim = e.core.sim().unwrap();
        if threads > 1 {
            assert!(sim.threads() > 1);
            let p = sim.program();
            assert!(p.debug.net_slots[0].values().any(|&s| s >= p.slot_count));
        }
        for (k, &port) in d.root_def().inputs.iter().enumerate() {
            e.core.set_input(port, k as u64 & 1);
        }
        e.core.set_probes(probes.clone());
        e.core.step(3);
        e.run_frame(Duration::from_secs(1), &c);
        frames.push(e.frame().values.clone());
    }
    assert_eq!(frames[0].len(), probes.len());
    assert_eq!(frames[0], frames[1]);
}

/// The engine runs its `Sim` on one thread unless configured (#41).
#[test]
fn engine_defaults_to_one_thread() {
    let (d, _, _, _) = counter_design(false);
    let mut e = LocalEngine::new(Options::debug());
    assert_eq!(e.core.sim_options().threads, 1);
    e.load(&d, 1);
    let sim = e.core.sim().unwrap();
    assert_eq!((sim.threads(), sim.phases()), (1, (0, 0)));
}

#[test]
fn unlimited_runs_until_the_budget() {
    let (d, ..) = counter_design(false);
    let clock = FakeClock::new();
    let c = || clock.read();
    let mut e = LocalEngine::new(Options::debug());
    e.load(&d, 1);
    e.core.set_rate(Rate::Unlimited);
    let n = e.run_frame(Duration::from_millis(1), &c);
    assert!(n > 0);
    assert_eq!(e.core.wait_hint(&c), Some(Duration::ZERO));
}

#[test]
fn state_and_inputs_survive_an_edit() {
    let (d1, en, q, mem) = counter_design(false);
    let (d2, en2, q2, mem2) = counter_design(true);
    assert_eq!((en, q, mem), (en2, q2, mem2), "same ids for existing nodes");
    let clock = FakeClock::new();
    let c = || clock.read();
    let mut e = LocalEngine::new(Options::debug());
    e.load(&d1, 1);
    e.core.set_input(en, 1);
    e.core.set_probes(probes(q, mem));
    e.core.step(20);
    e.run_frame(Duration::from_secs(1), &c);
    assert_eq!(e.frame().values[0], 19);

    e.load(&d2, 2);
    assert!(matches!(
        e.drain_events()[..],
        [
            EngineEvent::Loaded { revision: 1, .. },
            EngineEvent::Loaded { revision: 2, .. }
        ]
    ));
    e.run_frame(Duration::from_secs(1), &c);
    let f = e.frame().clone();
    assert_eq!(
        (f.revision, f.tick, f.values[0]),
        (2, 20, 19),
        "register and tick kept"
    );
    // Committed writes so far are counts 0..=18 (count 19's write is still
    // pending), so address 2 holds 18 and address 3 holds 3.
    let ram = e
        .core
        .sim()
        .unwrap()
        .program()
        .state_keys
        .iter()
        .find_map(|k| match k.loc {
            crate::compile::StateLoc::Mem(m) => Some(m),
            _ => None,
        });
    let ram = ram.unwrap();
    assert_eq!(&e.core.sim().unwrap().mem(ram)[2..4], &[18, 3]);
    // en is still 1 after the swap, and the pending write (recomputed by the
    // settle after import) lands at the next commit.
    e.core.step(3);
    e.run_frame(Duration::from_secs(1), &c);
    assert_eq!(e.frame().values[0], 22);
    assert_eq!(e.core.sim().unwrap().mem(ram)[3], 19);
}

#[test]
fn compile_errors_keep_the_old_program() {
    let (d, en, q, mem) = counter_design(false);
    let clock = FakeClock::new();
    let c = || clock.read();
    let mut e = LocalEngine::new(Options::debug());
    e.load(&d, 1);
    e.core.set_input(en, 1);
    e.core.set_probes(probes(q, mem));

    let mut bad = d.clone();
    let r = bad.root;
    let mut b = bad.builder(r);
    let (n, y) = {
        let n = b.node(NodeKind::Not { w: 1 }, &[]);
        (n, b.out(n, 0))
    };
    b.connect(y, (n, 0));
    e.load(&bad, 2);
    let events = e.drain_events();
    assert!(
        matches!(&events[1], EngineEvent::CompileFailed { revision: 2, errors } if !errors.is_empty())
    );
    e.core.step(4);
    e.run_frame(Duration::from_secs(1), &c);
    assert_eq!((e.frame().revision, e.frame().values[0]), (1, 3));
}

// ── Threaded engine ──────────────────────────────────────────────────────

#[cfg(not(target_arch = "wasm32"))]
mod threaded {
    use super::*;

    fn loaded(design: &Design) -> Engine {
        let e = Engine::spawn(Options::debug());
        e.load(Arc::new(design.clone()), 1);
        match e.wait_event(Duration::from_secs(5)) {
            Some(EngineEvent::Loaded { revision: 1, .. }) => {}
            other => panic!("expected Loaded, got {other:?}"),
        }
        e
    }

    #[test]
    fn steps_probes_and_sync() {
        let (d, en, q, mem) = counter_design(false);
        let e = loaded(&d);
        e.set_input(en, 1);
        e.set_probes(probes(q, mem));
        e.step(10);
        e.sync();
        let f = e.frame();
        assert_eq!((f.tick, f.values[0], f.revision), (10, 9, 1));
        assert_eq!(e.tick_count(), 10);
    }

    #[test]
    fn mem_probes_on_the_worker() {
        let (d, en, ..) = counter_design(false);
        let e = loaded(&d);
        e.set_input(en, 1);
        e.set_mem_probes(vec![MemProbe {
            path: Vec::new(),
            node: ram_node(&d),
        }]);
        e.step(4);
        e.sync();
        let m = e.frame().mems[0].clone().unwrap();
        assert_eq!(&m[..4], &[0, 1, 2, 0]);
        // A request while paused still gets an answer.
        e.step(1);
        e.sync();
        e.request_mems();
        e.sync();
        assert_eq!(e.frame().mems[0].as_ref().unwrap()[3], 3);
    }

    #[test]
    fn paced_rate_on_a_real_clock() {
        let (d, ..) = counter_design(false);
        let e = loaded(&d);
        e.set_rate(Rate::Hz(200.0));
        let start = Instant::now();
        std::thread::sleep(Duration::from_millis(500));
        // Resend the same rate a lot, as the old app did every frame.
        for _ in 0..200 {
            e.set_rate(Rate::Hz(200.0));
        }
        e.sync();
        let secs = start.elapsed().as_secs_f64();
        let ticks = e.tick_count() as f64;
        let expected = secs * 200.0;
        assert!(
            (ticks - expected).abs() <= expected * 0.15 + 3.0,
            "{ticks} ticks in {secs:.3} s at 200 Hz"
        );
    }

    #[test]
    fn edits_compile_off_thread_and_keep_state() {
        let (d1, en, q, mem) = counter_design(false);
        let (d2, ..) = counter_design(true);
        let e = loaded(&d1);
        e.set_input(en, 1);
        e.set_probes(probes(q, mem));
        e.step(12);
        e.sync();
        e.load(Arc::new(d2), 2);
        assert!(matches!(
            e.wait_event(Duration::from_secs(5)),
            Some(EngineEvent::Loaded { revision: 2, .. })
        ));
        e.step(1);
        e.sync();
        let f = e.frame();
        assert_eq!((f.revision, f.tick, f.values[0]), (2, 13, 12));
    }

    #[test]
    fn compile_failure_is_an_event_and_ticking_continues() {
        let (d, en, q, mem) = counter_design(false);
        let e = loaded(&d);
        e.set_input(en, 1);
        e.set_probes(probes(q, mem));
        let mut bad = d.clone();
        let r = bad.root;
        let mut b = bad.builder(r);
        let n = b.node(NodeKind::Not { w: 1 }, &[]);
        let y = b.out(n, 0);
        b.connect(y, (n, 0));
        e.load(Arc::new(bad), 2);
        assert!(matches!(
            e.wait_event(Duration::from_secs(5)),
            Some(EngineEvent::CompileFailed { revision: 2, .. })
        ));
        e.step(5);
        e.sync();
        assert_eq!((e.revision(), e.frame().values[0]), (1, 4));
    }

    /// Unlimited-rate throughput vs calling `Sim::run` directly (acceptance:
    /// within 5%). Timing-sensitive, so ignored by default:
    /// `cargo test -p logi-sim --release --lib -- --ignored engine_throughput --nocapture`.
    #[test]
    #[ignore]
    fn engine_throughput() {
        use crate::testkit::{GenConfig, generate};
        let d = generate(&GenConfig::gates(1, 20_000, 1)).design;
        let p = Arc::new(crate::compile::compile(&d, &Options::debug()).unwrap());
        let mut sim = crate::sim::Sim::new(Arc::clone(&p));
        let t = Instant::now();
        let mut direct = 0u64;
        while t.elapsed() < Duration::from_secs(2) {
            sim.run(256);
            direct += 256;
        }
        let direct_rate = direct as f64 / t.elapsed().as_secs_f64();

        let e = loaded(&d);
        e.set_rate(Rate::Unlimited);
        e.sync();
        let (t0, n0) = (Instant::now(), e.tick_count());
        std::thread::sleep(Duration::from_secs(2));
        e.sync();
        let engine_rate = (e.tick_count() - n0) as f64 / t0.elapsed().as_secs_f64();
        let ratio = engine_rate / direct_rate;
        eprintln!(
            "direct {direct_rate:.0} ticks/s, engine {engine_rate:.0} ticks/s, ratio {ratio:.3}"
        );
        assert!(ratio > 0.95, "engine overhead too high: {ratio:.3}");
    }
}
