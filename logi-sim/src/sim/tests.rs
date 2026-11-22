use std::sync::Arc;

use super::*;
use crate::compile::{Options, compile};
use crate::netlist::{Builder, Design, NodeKind};
use crate::reference::Reference;
use crate::testkit::{diff, fixtures};

fn sim_of(d: &Design) -> Sim {
    Sim::new(Arc::new(compile(d, &Options::default()).unwrap()))
}

// ── Differential ─────────────────────────────────────────────────────────

#[test]
fn differential_random_designs() {
    let n = diff::sweep(&diff::standard_configs(), 170, 20, &Options::default());
    assert_eq!(n, 1020);
}

/// Acceptance sweep (10,000+ designs):
/// `cargo test -p logi-sim --release --lib -- --ignored differential_10k`.
#[test]
#[ignore]
fn differential_10k() {
    let n = diff::sweep(&diff::standard_configs(), 1700, 30, &Options::default());
    assert!(n >= 10_000);
}

#[test]
fn fixtures_match_the_reference() {
    use fixtures::*;
    for (i, d) in [
        adder(8),
        ripple_adder(8),
        adder(64),
        ripple_adder(64),
        mux_tree(3, 8),
        mux_tree_bitblasted(3, 8),
        counter(8),
        counter_bitblasted(8),
        counter(64),
        counter_bitblasted(64),
        register_file(3, 16),
        sr_latch_nor_delayed(),
        cpu(&MULTIPLY_PROGRAM),
    ]
    .iter()
    .enumerate()
    {
        diff::check_design(d, i as u64, 150, &Options::default())
            .unwrap_or_else(|e| panic!("{}: {e}", d.root_def().name));
    }
}

#[test]
fn cpu_halts_with_the_expected_result() {
    let mut s = sim_of(&fixtures::cpu(&fixtures::MULTIPLY_PROGRAM));
    let mut ticks = 0;
    while s.get("halted") == 0 {
        s.tick();
        ticks += 1;
        assert!(ticks < 500);
    }
    assert_eq!(s.get("acc"), fixtures::MULTIPLY_RESULT);
    assert_eq!(s.tick_count(), ticks);
}

// ── Basic API ────────────────────────────────────────────────────────────

#[test]
fn inputs_are_staged_until_the_next_settle() {
    let mut d = Design::new("top");
    let r = d.root;
    let mut b = d.builder(r);
    let x = b.input("x", 8);
    b.output("y", x);
    let q = b.delay(x);
    b.output("q", q);
    let mut s = sim_of(&d);
    s.set("x", 0x1FF);
    assert_eq!(s.get("y"), 0, "not visible before a tick, even for a wire");
    s.tick();
    assert_eq!((s.get("y"), s.get("q")), (0xFF, 0), "masked to width");
    s.tick();
    assert_eq!(s.get("q"), 0xFF);
}

#[test]
fn read_net_by_path() {
    let mut d = Design::new("top");
    let c = d.add_definition("c");
    let mut b = Builder::new(&mut d, c);
    let a = b.input("a", 4);
    let n = b.not(a);
    b.output("y", n);
    let r = d.root;
    let mut b = d.builder(r);
    let x = b.input("x", 4);
    let o = b.instance(c, &[Some(x)]);
    b.output("y", o[0]);
    let inst = d.root_def().nets[o[0]].driver.0;
    let mut s = sim_of(&d);
    s.set("x", 3);
    s.tick();
    assert_eq!(s.read_net(&[inst], n), Some(12));
    assert_eq!(s.read_net(&[], x), Some(3));
    assert_eq!(s.read_net(&[x_node(&d, x)], n), None, "no such instance");
}

fn x_node(d: &Design, net: crate::netlist::NetId) -> crate::netlist::NodeId {
    d.root_def().nets[net].driver.0
}

#[test]
fn malformed_programs_are_rejected() {
    let d = fixtures::counter(8);
    let good = compile(&d, &Options::default()).unwrap();
    assert!(Sim::try_new(Arc::new(good.clone())).is_ok());

    let mut p = good.clone();
    p.instrs[0].a = p.slot_count;
    assert!(Sim::try_new(Arc::new(p)).is_err());

    let mut p = good.clone();
    p.instrs[0].dst = crate::compile::ZERO_SLOT;
    assert!(Sim::try_new(Arc::new(p)).is_err());

    let mut p = good.clone();
    p.blocks[0].len += 1000;
    assert!(Sim::try_new(Arc::new(p)).is_err());

    let mut p = good;
    p.slot_count -= 1;
    assert!(Sim::try_new(Arc::new(p)).is_err());
}

// ── Regression tests for the audit bugs ──────────────────────────────────

#[test]
fn unconnected_pin_does_not_shift_later_pins() {
    // Mux with `sel` unconnected: must pick `a` (pin 1), not treat `a` as sel.
    let mut d = Design::new("top");
    let r = d.root;
    let mut b = d.builder(r);
    let a = b.input("a", 8);
    let bb = b.input("b", 8);
    let mux = b.node(NodeKind::Mux { w: 8 }, &[None, Some(a), Some(bb)]);
    let y = b.out(mux, 0);
    b.output("y", y);
    let mut s = sim_of(&d);
    s.set("a", 0x11);
    s.set("b", 0x22);
    s.tick();
    assert_eq!(s.get("y"), 0x11);
}

#[test]
fn passthrough_subcircuit() {
    // A child whose output is wired straight to its input, plus one that
    // swaps its two ports.
    let mut d = Design::new("top");
    let pass = d.add_definition("pass");
    let mut b = Builder::new(&mut d, pass);
    let a = b.input("a", 8);
    b.output("y", a);
    let swap = d.add_definition("swap");
    let mut b = Builder::new(&mut d, swap);
    let a = b.input("a", 8);
    let bb = b.input("b", 8);
    b.output("x", bb);
    b.output("y", a);
    let r = d.root;
    let mut b = d.builder(r);
    let x = b.input("x", 8);
    let z = b.input("z", 8);
    let p = b.instance(pass, &[Some(x)]);
    let p2 = b.instance(pass, &p.iter().map(|&n| Some(n)).collect::<Vec<_>>());
    let s_out = b.instance(swap, &[Some(p2[0]), Some(z)]);
    b.output("p", p2[0]);
    b.output("sx", s_out[0]);
    b.output("sy", s_out[1]);
    let p = compile(&d, &Options::default()).unwrap();
    assert!(p.instrs.is_empty(), "passthroughs need no instructions");
    let mut s = Sim::new(Arc::new(p));
    s.set("x", 7);
    s.set("z", 9);
    s.tick();
    assert_eq!((s.get("p"), s.get("sx"), s.get("sy")), (7, 9, 7));
}

#[test]
fn port_order_after_deletion() {
    // Child with inputs a, b, c and outputs a', b', c' (identity). Delete b:
    // the parent's connections to a and c must still reach a' and c'.
    let mut d = Design::new("top");
    let c = d.add_definition("c");
    let mut b = Builder::new(&mut d, c);
    let ins: Vec<_> = ["a", "b", "c"].iter().map(|n| b.input(*n, 8)).collect();
    for (k, &net) in ins.iter().enumerate() {
        let n = b.not(net);
        b.output(format!("o{k}"), n);
    }
    let r = d.root;
    let mut b = d.builder(r);
    let xs: Vec<_> = ["x0", "x1", "x2"].iter().map(|n| b.input(*n, 8)).collect();
    let outs = b.instance(c, &xs.iter().map(|&n| Some(n)).collect::<Vec<_>>());
    for (k, &o) in outs.iter().enumerate() {
        b.output(format!("y{k}"), o);
    }
    let b_port = d.defs[c].inputs[1];
    let b_out = d.defs[c].outputs[1];
    d.remove_node(c, b_port).unwrap();
    d.remove_node(c, b_out).unwrap();
    d.validate().unwrap();

    let mut s = sim_of(&d);
    s.set("x0", 1);
    s.set("x1", 2);
    s.set("x2", 3);
    s.tick();
    assert_eq!(s.get("y0"), !1u64 & 0xFF);
    assert_eq!(s.get("y1"), 0, "the removed port's sink is unconnected");
    assert_eq!(s.get("y2"), !3u64 & 0xFF);

    let mut r = Reference::new(&d).unwrap();
    for (n, v) in [("x0", 1), ("x1", 2), ("x2", 3)] {
        r.set(n, v);
    }
    r.tick().unwrap();
    assert_eq!(r.outputs(), (0..3).map(|k| s.output(k)).collect::<Vec<_>>());
}

// ── State carry-over ─────────────────────────────────────────────────────

/// A counter register plus a RAM that records the count at address `count`.
fn stateful(extra_gate: bool) -> Design {
    let mut d = Design::new("top");
    let r = d.root;
    let mut b = d.builder(r);
    let one = b.one();
    let (reg, q) = b.state_placeholder(NodeKind::Register { w: 8 });
    let n = b.inc(q);
    b.connect(one, (reg, 0));
    b.connect(n, (reg, 1));
    let addr = b.split(q, 0, 4);
    let ram = b.ram(addr, one, q, &[]);
    b.output("q", q);
    b.output("ram", ram);
    b.name_net(q, "count");
    b.name_net(ram, "mem");
    if extra_gate {
        let x = b.input("x", 1);
        let nx = b.not(x);
        b.output("nx", nx);
    }
    d
}

#[test]
fn register_and_ram_survive_an_unrelated_edit() {
    let d1 = stateful(false);
    let mut s1 = sim_of(&d1);
    s1.run(10);
    assert_eq!(s1.get("q"), 10);
    let snap = s1.export_state();
    assert_eq!(snap.tick, 10);
    assert_eq!(snap.words.len(), 1);
    assert_eq!(snap.mems.len(), 1);

    // Same NodeIds for the existing nodes, plus new logic.
    let d2 = stateful(true);
    let mut s2 = sim_of(&d2);
    s2.import_state(&snap);
    assert_eq!(s2.tick_count(), 10);
    assert_eq!(s2.get("q"), 10, "re-settled after import");
    s2.run(5);
    assert_eq!(s2.get("q"), 15);
    // Addresses 0..=14 were written with their count.
    let ram = s2.program().state_keys.iter().find_map(|k| match k.loc {
        StateLoc::Mem(m) => Some(m),
        _ => None,
    });
    assert_eq!(
        &s2.mem(ram.unwrap())[..14],
        &(0..14).collect::<Vec<u64>>()[..]
    );
}

#[test]
fn unmatched_state_resets() {
    let d1 = stateful(false);
    let mut s1 = sim_of(&d1);
    s1.run(10);
    let mut snap = s1.export_state();
    // Pretend the register belonged to a different node.
    let (path, v) = snap.words.drain().next().unwrap();
    let mut other = path.clone();
    other.instances.push(path.node);
    snap.words.insert(other, v);
    // And change the RAM geometry.
    for (ab, _, _) in snap.mems.values_mut() {
        *ab += 1;
    }
    let mut s2 = sim_of(&d1);
    s2.run(3);
    s2.import_state(&snap);
    assert_eq!(s2.get("q"), 0, "unmatched register resets");
    let m = s2.program().mems.len() as u32 - 1;
    assert!(
        s2.mem(m).iter().all(|&w| w == 0),
        "geometry change resets RAM"
    );
}

#[test]
fn imported_words_are_masked_to_the_new_width() {
    let build = |w: u8| {
        let mut d = Design::new("top");
        let r = d.root;
        let mut b = d.builder(r);
        let (reg, q) = b.state_placeholder(NodeKind::Register { w });
        let _ = reg;
        b.output("q", q);
        d
    };
    let mut snap = sim_of(&build(16)).export_state();
    for v in snap.words.values_mut() {
        *v = 0xABCD;
    }
    let mut s = sim_of(&build(8));
    s.import_state(&snap);
    assert_eq!(s.get("q"), 0xCD);
}

#[test]
fn snapshot_round_trip_matches_continuous_run() {
    let d = fixtures::cpu(&fixtures::MULTIPLY_PROGRAM);
    let mut a = sim_of(&d);
    a.run(40);
    let snap = a.export_state();
    let mut b = sim_of(&d);
    b.import_state(&snap);
    a.run(40);
    b.run(40);
    assert_eq!(
        (a.get("acc"), a.get("pc"), a.tick_count()),
        (b.get("acc"), b.get("pc"), b.tick_count())
    );
}

// ── Performance switches (#41) ───────────────────────────────────────────

/// Every layout and runtime switch alone, then all together, each under the
/// optimizing and the unoptimized compile.
fn perf_variants() -> Vec<(String, Options, SimOptions)> {
    use crate::compile::{PackMode, SlotOrder};
    let mut out = Vec::new();
    for (base_name, base) in [
        ("default", Options::default()),
        ("unoptimized", Options::unoptimized()),
    ] {
        let off = Options {
            slot_order: SlotOrder::Production,
            pack: PackMode::Off,
            ..base.clone()
        };
        let mut push = |name: &str, o: Options, so: SimOptions| {
            out.push((format!("{base_name}/{name}"), o, so));
        };
        push("none", off.clone(), SimOptions::baseline());
        for order in [SlotOrder::FirstUse, SlotOrder::Dfs] {
            push(
                &format!("{order:?}"),
                Options {
                    slot_order: order,
                    ..off.clone()
                },
                SimOptions::baseline(),
            );
        }
        let mut stream = SimOptions::baseline();
        stream.stream = true;
        push("stream", off.clone(), stream);
        for (mode, min) in [
            (PackMode::All, 1),
            (PackMode::All, 8),
            (PackMode::Aligned, 1),
        ] {
            push(
                &format!("pack {mode:?}/{min}"),
                Options {
                    pack: mode,
                    pack_min: min,
                    ..off.clone()
                },
                SimOptions::baseline(),
            );
        }
        let all = Options {
            slot_order: SlotOrder::FirstUse,
            pack: PackMode::Aligned,
            ..base.clone()
        };
        push("defaults", base.clone(), SimOptions::default());
        for threads in [2, 3, 4] {
            push(
                &format!("threads{threads}"),
                off.clone(),
                SimOptions {
                    threads,
                    par_min: 1,
                    ..SimOptions::baseline()
                },
            );
        }
        push(
            "partitions3/threads3",
            off.clone().for_threads(3),
            SimOptions {
                threads: 3,
                par_min: 1,
                ..SimOptions::baseline()
            },
        );
        push(
            "partitions3/threads1",
            off.clone().for_threads(3),
            SimOptions::baseline(),
        );
        push("all", all.clone(), SimOptions::default());
        push(
            "all/threads4",
            all.clone().for_threads(4),
            SimOptions {
                threads: 4,
                par_min: 1,
                ..SimOptions::default()
            },
        );
        push(
            "all/pack-all-1/threads3",
            Options {
                pack: PackMode::All,
                pack_min: 1,
                ..all.clone()
            },
            SimOptions {
                threads: 3,
                par_min: 1,
                ..SimOptions::default()
            },
        );
    }
    out
}

fn perf_designs() -> Vec<Design> {
    use crate::testkit::{GenConfig, bitblast::bitblast, generate};
    vec![
        bitblast(&fixtures::cpu(&fixtures::MULTIPLY_PROGRAM)),
        bitblast(&fixtures::counter(16)),
        fixtures::ripple_adder(16),
        fixtures::register_file(2, 8),
        fixtures::cpu(&fixtures::MULTIPLY_PROGRAM),
        generate(&GenConfig::gates(7, 3_000, 1)).design,
        bitblast(
            &generate(&GenConfig {
                seed: 9,
                nodes: 300,
                child_defs: 0,
                max_depth: 1,
                max_width: 16,
                state_percent: 20,
                ..Default::default()
            })
            .design,
        ),
    ]
}

#[test]
fn perf_switches_match_the_reference() {
    let designs = perf_designs();
    for (name, opts, so) in perf_variants() {
        let n = diff::sweep_with(&diff::standard_configs(), 20, 15, &opts, &so);
        assert_eq!(n, 120, "{name}");
        for (i, d) in designs.iter().enumerate() {
            crate::compile::tests::check_program_invariants(&compile(d, &opts).unwrap());
            diff::check_design_with(d, i as u64, 40, &opts, &so)
                .unwrap_or_else(|e| panic!("{name}, design {i}: {e}"));
        }
    }
}

/// Acceptance sweep for every switch (10,000+ designs each):
/// `cargo test -p logi-sim --release --lib -- --ignored perf_switches_10k`.
#[test]
#[ignore]
fn perf_switches_10k() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    // Single-threaded variants run four at a time; threaded ones alone, so
    // they do not oversubscribe the cores. Each checks 10,200 designs.
    let (threaded, single): (Vec<_>, Vec<_>) = perf_variants()
        .into_iter()
        .partition(|(_, _, so)| so.threads > 1);
    let total = AtomicUsize::new(0);
    for (variants, workers) in [(single, 4), (threaded, 1)] {
        let next = AtomicUsize::new(0);
        std::thread::scope(|scope| {
            for _ in 0..workers {
                scope.spawn(|| {
                    while let Some((name, opts, so)) =
                        variants.get(next.fetch_add(1, Ordering::Relaxed))
                    {
                        let n = diff::sweep_with(&diff::standard_configs(), 1700, 30, opts, so);
                        assert!(n >= 10_000, "{name}");
                        total.fetch_add(n as usize, Ordering::Relaxed);
                        eprintln!("{name}: {n} designs match");
                    }
                });
            }
        });
    }
    eprintln!("{} designs in all", total.load(Ordering::Relaxed));
}

/// `PackMode::Auto` packs a large 1-bit design (past `AUTO_PACK_BYTES`),
/// lane slots read back through the debug map, and it matches the
/// reference.
#[test]
fn auto_packing_on_a_large_gate_design() {
    use crate::testkit::{GenConfig, generate};
    let d = generate(&GenConfig::gates(11, 600_000, 1)).design;
    let opts = Options::debug();
    let p = compile(&d, &opts).unwrap();
    assert!(p.packed_lanes() > 500_000, "{}", p.packed_lanes());
    let lane_nets = p
        .debug
        .net_slots
        .iter()
        .flat_map(|m| m.values())
        .filter(|&&s| s >= p.slot_count)
        .count();
    assert!(lane_nets > 500_000, "{lane_nets}");
    crate::compile::tests::check_program_invariants(&p);
    diff::check_design(&d, 5, 3, &opts).unwrap_or_else(|e| panic!("{e}"));
}

/// Threads start only when some rank is large enough, and a threaded `Sim`
/// keeps working across ticks, state import and drop.
#[test]
fn thread_pool_lifecycle() {
    use crate::testkit::{GenConfig, generate};
    let small = Arc::new(compile(&fixtures::counter(8), &Options::default()).unwrap());
    let s = Sim::with_options(Arc::clone(&small), SimOptions::threaded(4));
    assert_eq!(s.threads(), 1, "small circuits stay single-threaded");

    let d = generate(&GenConfig::gates(3, 40_000, 1)).design;
    let p = Arc::new(compile(&d, &Options::debug()).unwrap());
    let mut one = Sim::new(Arc::clone(&p));
    let mut four = Sim::with_options(
        Arc::clone(&p),
        SimOptions {
            threads: 4,
            par_min: 64,
            ..SimOptions::default()
        },
    );
    assert_eq!(four.threads(), 4);
    let (par, total) = four.phases();
    assert!(par > 0 && total >= par, "{par}/{total}");
    for t in 0..50u64 {
        for port in 0..p.ports.inputs.len() {
            let v = t.wrapping_mul(0x9E37_79B9) >> port & 1;
            one.set_input(port, v);
            four.set_input(port, v);
        }
        one.tick();
        four.tick();
        if t == 20 {
            // Idle long enough for the workers to fall asleep.
            std::thread::sleep(std::time::Duration::from_millis(30));
        }
        for port in 0..p.ports.outputs.len() {
            assert_eq!(one.output(port), four.output(port), "tick {t}");
        }
    }
    let snap = one.export_state();
    four.import_state(&snap);
    drop(four);
}

/// A program whose ranks read their own results (all blocks relabelled to
/// rank 0) is valid for the sequential runtime but not provably race-free,
/// so it must run single-threaded, with the same results.
#[test]
fn racy_programs_run_single_threaded() {
    let d = fixtures::cpu(&fixtures::MULTIPLY_PROGRAM);
    let mut p = compile(&d, &Options::unoptimized()).unwrap();
    for b in &mut p.blocks {
        b.rank = 0;
    }
    let p = Arc::new(p);
    let mut one = Sim::new(Arc::clone(&p));
    let mut many = Sim::with_options(
        Arc::clone(&p),
        SimOptions {
            threads: 4,
            par_min: 1,
            ..SimOptions::default()
        },
    );
    assert_eq!(many.threads(), 1);
    one.run(100);
    many.run(100);
    assert_eq!(one.get("acc"), many.get("acc"));
}

/// With one thread (the default) no worker starts and no schedule is built,
/// whatever `par_min` says; results match a 4-thread run of the same
/// programs on every design shape.
#[test]
fn one_thread_spawns_nothing_and_matches_threads() {
    use crate::testkit::{GenConfig, bitblast::bitblast, generate};
    let designs = [
        generate(&GenConfig::gates(3, 40_000, 1)).design,
        generate(&GenConfig::dag(5, 20_000, 32)).design,
        bitblast(&fixtures::cpu(&fixtures::MULTIPLY_PROGRAM)),
    ];
    for d in &designs {
        let p = Arc::new(compile(d, &Options::debug()).unwrap());
        let force = |threads| SimOptions {
            threads,
            par_min: 1,
            ..SimOptions::default()
        };
        assert_eq!(SimOptions::default().threads, 1);
        let mut one = Sim::new(Arc::clone(&p));
        let one_forced = Sim::with_options(Arc::clone(&p), force(1));
        for s in [&one, &one_forced] {
            assert_eq!(s.threads(), 1);
            assert_eq!(s.phases(), (0, 0), "no schedule with one thread");
            assert!(s.pool.is_none());
        }
        let mut four = Sim::with_options(Arc::clone(&p), force(4));
        if !cfg!(target_arch = "wasm32") {
            assert_eq!(four.threads(), 4);
        }
        for t in 0..40u64 {
            for (port, input) in p.ports.inputs.iter().enumerate() {
                let v = t.wrapping_mul(0x2545_F491_4F6C_DD1D) >> (port % 60);
                let v = v & crate::netlist::mask(input.width);
                one.set_input(port, v);
                four.set_input(port, v);
            }
            one.tick();
            four.tick();
            for (inst, map) in p.debug.net_slots.iter().enumerate() {
                for (net, &slot) in map {
                    assert_eq!(
                        one.read(slot),
                        four.read(slot),
                        "tick {t} inst {inst} {net:?}"
                    );
                }
            }
        }
    }
}
