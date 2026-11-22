use super::fixtures::*;
use super::*;
use crate::netlist::{Design, NodeKind, mask};
use crate::reference::{Reference, ReferenceError};

/// Drive random root inputs for `ticks` ticks.
fn run_random(r: &mut Reference, rng: &mut Rng, ticks: u64) {
    let widths = r.design().root_def().signature().0;
    for _ in 0..ticks {
        for (port, &w) in widths.iter().enumerate() {
            r.set_input(port, rng.next_u64() & mask(w));
        }
        r.tick().unwrap();
    }
}

/// Run two designs with the same port names side by side on the same random
/// inputs and require identical outputs every tick.
fn assert_equivalent(a: &Design, b: &Design, seed: u64, ticks: u64) {
    let mut ra = Reference::new(a).unwrap();
    let mut rb = Reference::new(b).unwrap();
    let (ins, _) = a.root_def().signature();
    let names: Vec<String> = a
        .root_def()
        .inputs
        .iter()
        .map(|&n| a.root_def().name_of(n).unwrap().to_string())
        .collect();
    let outs: Vec<String> = a
        .root_def()
        .outputs
        .iter()
        .map(|&n| a.root_def().name_of(n).unwrap().to_string())
        .collect();
    let mut rng = Rng::new(seed);
    for t in 0..ticks {
        for (name, &w) in names.iter().zip(&ins) {
            let v = rng.next_u64() & mask(w);
            ra.set(name, v);
            rb.set(name, v);
        }
        ra.tick().unwrap();
        rb.tick().unwrap();
        for o in &outs {
            assert_eq!(ra.get(o), rb.get(o), "output {o} differs at tick {t}");
        }
    }
}

fn configs() -> Vec<GenConfig> {
    vec![
        GenConfig::default(),
        GenConfig {
            max_width: 64,
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
    ]
}

#[test]
fn rng_is_deterministic() {
    let a: Vec<u64> = {
        let mut r = Rng::new(42);
        (0..8).map(|_| r.next_u64()).collect()
    };
    let b: Vec<u64> = {
        let mut r = Rng::new(42);
        (0..8).map(|_| r.next_u64()).collect()
    };
    assert_eq!(a, b);
    assert_ne!(a, {
        let mut r = Rng::new(43);
        (0..8).map(|_| r.next_u64()).collect::<Vec<_>>()
    });
    let mut r = Rng::new(7);
    for _ in 0..1000 {
        assert!(r.below(5) < 5);
        let x = r.range(3, 9);
        assert!((3..=9).contains(&x));
    }
}

#[test]
fn generator_is_deterministic() {
    for cfg in configs() {
        let a = generate(&GenConfig {
            seed: 99,
            ..cfg.clone()
        });
        let b = generate(&GenConfig {
            seed: 99,
            ..cfg.clone()
        });
        assert_eq!(format!("{:?}", a.design), format!("{:?}", b.design));
        let c = generate(&GenConfig { seed: 100, ..cfg });
        assert_ne!(format!("{:?}", a.design), format!("{:?}", c.design));
    }
}

#[test]
fn generated_designs_validate_and_run() {
    let mut stats = (0usize, 0usize, 0usize, 0usize, 0usize); // nodes, state, instances, unconnected, defs
    for (ci, cfg) in configs().into_iter().enumerate() {
        for seed in 0..60 {
            let cfg = GenConfig {
                seed: seed * 7 + ci as u64,
                ..cfg.clone()
            };
            let g = generate(&cfg);
            assert!(g.injected_loop.is_none());
            g.design
                .validate()
                .unwrap_or_else(|e| panic!("cfg {ci} seed {seed}: {e:?}"));
            let mut r =
                Reference::new(&g.design).unwrap_or_else(|e| panic!("cfg {ci} seed {seed}: {e:?}"));
            let mut rng = Rng::new(seed);
            run_random(&mut r, &mut rng, 20);

            stats.4 += g.design.defs_bottom_up().len();
            for def in g.design.defs.values() {
                for node in def.nodes.values() {
                    stats.0 += 1;
                    stats.1 += node.kind.is_state() as usize;
                    stats.2 += matches!(node.kind, NodeKind::Instance(_)) as usize;
                    stats.3 += node.inputs.iter().filter(|n| n.is_none()).count();
                }
            }
        }
    }
    let (nodes, state, instances, unconnected, defs) = stats;
    // The generator really exercises what it claims to.
    assert!(state > nodes / 20, "{stats:?}");
    assert!(instances > 100, "{stats:?}");
    assert!(unconnected > 100, "{stats:?}");
    assert!(defs > 360, "nested definitions are reachable: {stats:?}");
}

#[test]
fn generated_designs_respect_max_depth() {
    for max_depth in 1..=4 {
        for seed in 0..20 {
            let g = generate(&GenConfig {
                seed,
                max_depth,
                child_defs: 6,
                instance_percent: 30,
                ..Default::default()
            });
            // Depth of the instance tree under the root.
            let d = &g.design;
            let mut depth = std::collections::HashMap::new();
            for def in d.defs_bottom_up() {
                let below = d.defs[def]
                    .nodes
                    .values()
                    .filter_map(|n| match n.kind {
                        NodeKind::Instance(c) => Some(depth[&c]),
                        _ => None,
                    })
                    .max()
                    .unwrap_or(0);
                depth.insert(def, below + 1);
            }
            assert!(
                depth[&d.root] <= max_depth,
                "depth {} > {max_depth}",
                depth[&d.root]
            );
        }
    }
}

#[test]
fn injected_loops_are_reported_with_their_nodes() {
    for (ci, cfg) in configs().into_iter().enumerate() {
        for seed in 0..30 {
            let cfg = GenConfig {
                seed,
                inject_loop: true,
                ..cfg.clone()
            };
            let g = generate(&cfg);
            let (def, mut want) = g.injected_loop.clone().unwrap();
            g.design.validate().unwrap();
            match Reference::new(&g.design) {
                Err(ReferenceError::CombinationalLoop {
                    def: got_def,
                    mut nodes,
                }) => {
                    assert_eq!(got_def, def, "cfg {ci} seed {seed}");
                    nodes.sort();
                    want.sort();
                    assert_eq!(nodes, want, "cfg {ci} seed {seed}");
                }
                other => panic!("cfg {ci} seed {seed}: expected loop, got {:?}", other.err()),
            }
        }
    }
}

// ── Fixtures ─────────────────────────────────────────────────────────────

#[test]
fn adders_compute_sums() {
    for w in [1u8, 4, 8, 64] {
        for d in [adder(w), ripple_adder(w)] {
            let mut r = Reference::new(&d).unwrap();
            let m = mask(w);
            for (a, b, c) in [
                (0, 0, 0),
                (m, 1, 0),
                (m, m, 1),
                (m >> 1, 1, 1),
                (5 & m, 3 & m, 0),
            ] {
                r.set("a", a);
                r.set("b", b);
                r.set("cin", c);
                r.tick().unwrap();
                let t = a as u128 + b as u128 + c as u128;
                assert_eq!(
                    r.get("sum"),
                    (t as u64) & m,
                    "{} {a}+{b}+{c}",
                    d.root_def().name
                );
                assert_eq!(r.get("cout"), (t >> w) as u64 & 1);
            }
        }
    }
}

#[test]
fn word_and_bitblasted_fixtures_agree() {
    // Exhaustive for a 4-bit adder.
    let (wa, ba) = (adder(4), ripple_adder(4));
    let mut rw = Reference::new(&wa).unwrap();
    let mut rb = Reference::new(&ba).unwrap();
    for x in 0..512u64 {
        for r in [&mut rw, &mut rb] {
            r.set("a", x & 15);
            r.set("b", x >> 4 & 15);
            r.set("cin", x >> 8);
            r.tick().unwrap();
        }
        assert_eq!(rw.outputs(), rb.outputs(), "input {x:#x}");
    }
    assert_equivalent(&adder(64), &ripple_adder(64), 1, 200);
    assert_equivalent(&adder(13), &ripple_adder(13), 2, 200);
    assert_equivalent(&mux_tree(3, 8), &mux_tree_bitblasted(3, 8), 3, 200);
    assert_equivalent(&mux_tree(1, 64), &mux_tree_bitblasted(1, 64), 4, 100);
    assert_equivalent(&counter(8), &counter_bitblasted(8), 5, 600);
    assert_equivalent(&counter(64), &counter_bitblasted(64), 6, 200);
}

/// Port of the legacy `test_recursive_16bit_adder_all_runners`: four levels
/// of nested definitions add like the `Add` primitive.
#[test]
fn nested_adder_matches_the_add_primitive() {
    let d = nested_adder16();
    assert_eq!(d.defs.len(), 4);
    let mut r = Reference::new(&d).unwrap();
    for (a, b, c) in [
        (1, 1, 0),
        (0xFFFF, 1, 0),
        (0xFFFF, 0xFFFF, 1),
        (0x1234, 0x4321, 1),
    ] {
        r.set("a", a);
        r.set("b", b);
        r.set("cin", c);
        r.tick().unwrap();
        let t = a + b + c;
        assert_eq!(
            (r.get("sum"), r.get("cout")),
            (t & 0xFFFF, t >> 16),
            "{a}+{b}+{c}"
        );
    }
    assert_equivalent(&adder(16), &d, 7, 300);
}

#[test]
fn mux_tree_selects() {
    let d = mux_tree(2, 8);
    let mut r = Reference::new(&d).unwrap();
    for i in 0..4 {
        r.set(&format!("d{i}"), 10 + i);
    }
    for s in 0..4 {
        r.set("sel", s);
        r.tick().unwrap();
        assert_eq!(r.get("y"), 10 + s);
    }
}

#[test]
fn counter_counts_and_resets() {
    for d in [counter(8), counter_bitblasted(8)] {
        let mut r = Reference::new(&d).unwrap();
        r.set("en", 1);
        r.run(301).unwrap();
        assert_eq!(r.get("q"), 300 % 256);
        r.set("en", 0);
        r.run(5).unwrap();
        assert_eq!(r.get("q"), 301 % 256, "one increment was pending");
        r.set("reset", 1);
        r.run(2).unwrap();
        assert_eq!(r.get("q"), 0);
    }
}

#[test]
fn register_file_writes_and_reads() {
    let d = register_file(3, 16);
    let mut r = Reference::new(&d).unwrap();
    r.set("we", 1);
    for i in 0..8 {
        r.set("waddr", i);
        r.set("wdata", 1000 + i);
        r.tick().unwrap();
    }
    r.set("we", 0);
    r.tick().unwrap();
    for i in 0..8 {
        r.set("ra", i);
        r.set("rb", 7 - i);
        r.tick().unwrap();
        assert_eq!((r.get("qa"), r.get("qb")), (1000 + i, 1000 + 7 - i));
    }
    // A write with we=0 is ignored.
    r.set("waddr", 2);
    r.set("wdata", 1);
    r.set("ra", 2);
    r.run(2).unwrap();
    assert_eq!(r.get("qa"), 1002);
}

#[test]
fn cpu_runs_the_multiply_program() {
    let d = cpu(&MULTIPLY_PROGRAM);
    let ram = d.root_def().find("ram").unwrap();
    let mut r = Reference::new(&d).unwrap();
    let mut ticks = 0;
    while r.get("halted") == 0 {
        r.tick().unwrap();
        ticks += 1;
        assert!(ticks < 500, "CPU did not halt");
    }
    assert_eq!(r.get("acc"), MULTIPLY_RESULT);
    assert_eq!(r.mem(&[], ram).unwrap()[1], MULTIPLY_RESULT);
    assert_eq!(r.get("pc"), 18);
    // Halted stays halted.
    r.run(10).unwrap();
    assert_eq!(
        (r.get("pc"), r.get("acc"), r.get("halted")),
        (18, MULTIPLY_RESULT, 1)
    );
}

#[test]
fn cpu_jumps_and_wraps() {
    use isa::*;
    // Count down from 3 with SUB/JZ, then ADDI past 255.
    let program = [
        op(LDI, 1),
        op(ST, 9),
        op(LDI, 3),
        op(JZ, 6), // 3
        op(SUB, 9),
        op(JMP, 3),
        op(LDI, 250), // 6
        op(ADDI, 10),
        op(HLT, 0),
    ];
    let d = cpu(&program);
    let mut r = Reference::new(&d).unwrap();
    r.run(100).unwrap();
    assert_eq!(r.get("halted"), 1);
    assert_eq!(r.get("acc"), 4, "250 + 10 wraps to 4");
}

#[test]
fn gate_loop_latches_are_loop_errors() {
    for f in [
        sr_latch_nor(),
        sr_latch_nand(),
        d_latch_gates(),
        ring_oscillator(1),
        ring_oscillator(5),
    ] {
        f.design.validate().unwrap();
        match Reference::new(&f.design) {
            Err(ReferenceError::CombinationalLoop { def, mut nodes }) => {
                let mut want = f.loop_nodes.clone();
                nodes.sort();
                want.sort();
                assert_eq!((def, nodes), (f.def, want), "{}", f.design.root_def().name);
            }
            other => panic!(
                "{}: expected loop, got {:?}",
                f.design.root_def().name,
                other.err()
            ),
        }
    }
}

#[test]
fn delayed_sr_latch_holds_state() {
    let d = sr_latch_nor_delayed();
    let mut r = Reference::new(&d).unwrap();
    let mut step = |s, rr, ticks| {
        r.set("S", s);
        r.set("R", rr);
        r.run(ticks).unwrap();
        (r.get("Q"), r.get("Qn"))
    };
    assert_eq!(step(1, 0, 3), (1, 0), "set");
    assert_eq!(step(0, 0, 5), (1, 0), "hold set");
    assert_eq!(step(0, 1, 3), (0, 1), "reset");
    assert_eq!(step(0, 0, 5), (0, 1), "hold reset");
}

/// Larger sweep: `cargo test -p logi-sim --release -- --ignored many`.
#[test]
#[ignore]
fn many_generated_designs_validate_and_run() {
    for (ci, cfg) in configs().into_iter().enumerate() {
        for seed in 0..2000u64 {
            let g = generate(&GenConfig {
                seed,
                ..cfg.clone()
            });
            let mut r =
                Reference::new(&g.design).unwrap_or_else(|e| panic!("cfg {ci} seed {seed}: {e:?}"));
            run_random(&mut r, &mut Rng::new(seed), 10);
            let g = generate(&GenConfig {
                seed,
                inject_loop: true,
                ..cfg.clone()
            });
            assert!(
                matches!(
                    Reference::new(&g.design),
                    Err(ReferenceError::CombinationalLoop { .. })
                ),
                "cfg {ci} seed {seed}"
            );
        }
    }
}

// ── Bit-blasting ─────────────────────────────────────────────────────────

/// Same random inputs by port index; outputs must agree every tick.
fn assert_equivalent_by_index(a: &Design, b: &Design, seed: u64, ticks: u64) {
    let mut ra = Reference::new(a).unwrap();
    let mut rb = Reference::new(b).unwrap();
    let (ins, _) = a.root_def().signature();
    assert_eq!(ins, b.root_def().signature().0);
    let mut rng = Rng::new(seed);
    assert_eq!(ra.outputs(), rb.outputs(), "tick 0");
    for t in 1..=ticks {
        for (port, &w) in ins.iter().enumerate() {
            let v = rng.next_u64() & mask(w);
            ra.set_input(port, v);
            rb.set_input(port, v);
        }
        ra.tick().unwrap();
        rb.tick().unwrap();
        assert_eq!(ra.outputs(), rb.outputs(), "seed {seed} tick {t}");
    }
}

#[test]
fn bitblasted_designs_are_equivalent() {
    for seed in 0..40 {
        for max_width in [1, 8, 64] {
            let cfg = GenConfig {
                seed,
                child_defs: 0,
                max_depth: 1,
                max_width,
                state_percent: 20,
                ..Default::default()
            };
            let d = generate(&cfg).design;
            let bb = super::bitblast::bitblast(&d);
            bb.validate().unwrap();
            // Only 1-bit logic, apart from memories, clocks and wiring.
            for n in bb.root_def().nodes.values() {
                if let Some((ins, outs)) = n.kind.pins()
                    && !matches!(
                        n.kind,
                        NodeKind::Split { .. }
                            | NodeKind::Merge { .. }
                            | NodeKind::Ram { .. }
                            | NodeKind::Ram2 { .. }
                            | NodeKind::Rom { .. }
                            | NodeKind::Input { .. }
                            | NodeKind::Output { .. }
                    )
                {
                    assert!(ins.iter().chain(&outs).all(|&w| w == 1), "{:?}", n.kind);
                }
            }
            assert_equivalent_by_index(&d, &bb, seed, 20);
        }
    }
}

#[test]
fn ram2_register_file_matches_the_register_one() {
    for (addr_bits, w) in [(1, 1), (3, 8), (5, 64)] {
        assert_equivalent(
            &register_file(addr_bits, w),
            &register_file_ram2(addr_bits, w, &[]),
            addr_bits as u64,
            300,
        );
    }
}

#[test]
fn fifo_matches_a_queue() {
    let d = fifo(3, 8);
    let mut r = Reference::new(&d).unwrap();
    let mut model = std::collections::VecDeque::new();
    let mut rng = Rng::new(9);
    for t in 0..2000 {
        let (push, pop, data) = (rng.coin(), rng.coin(), rng.next_u64() & 0xFF);
        r.set("push", push as u64);
        r.set("pop", pop as u64);
        r.set("data", data);
        r.tick().unwrap();
        // The inputs reach state at the next commit; the outputs show the
        // queue before them.
        assert_eq!(r.get("count"), model.len() as u64, "tick {t}");
        if let Some(&head) = model.front() {
            assert_eq!(r.get("head"), head, "tick {t}");
        }
        let (was_full, was_empty) = (model.len() == 8, model.is_empty());
        if pop && !was_empty {
            model.pop_front();
        }
        if push && !was_full {
            model.push_back(data);
        }
    }
}

#[test]
fn bitblasted_cpu_runs_the_program() {
    let d = cpu(&MULTIPLY_PROGRAM);
    let bb = super::bitblast::bitblast(&d);
    let mut r = Reference::new(&bb).unwrap();
    let mut ticks = 0;
    while r.get("halted") == 0 {
        r.tick().unwrap();
        ticks += 1;
        assert!(ticks < 500);
    }
    assert_eq!(r.get("acc"), MULTIPLY_RESULT);
    assert!(bb.root_def().nodes.len() > 5 * d.root_def().nodes.len());
}

/// With `screens`, the generator draws screens of every mode, and the
/// differential check sees their memories: the sweep compares them with
/// the reference (`diff::standard_configs` has such a config).
#[test]
fn generated_screens_are_covered() {
    let mut modes = std::collections::HashSet::new();
    let mut screens = 0;
    for seed in 0..40 {
        let cfg = GenConfig {
            seed,
            state_percent: 40,
            screens: true,
            ..Default::default()
        };
        let d = generate(&cfg).design;
        for def in d.defs.values() {
            for n in def.nodes.values() {
                if let NodeKind::Screen { mode, .. } = n.kind {
                    modes.insert(format!("{mode:?}"));
                    screens += 1;
                }
            }
        }
        diff::check_generated(&cfg, 25, &crate::compile::Options::default()).unwrap();
    }
    assert!(screens > 40, "{screens} screens");
    assert_eq!(modes.len(), 3, "{modes:?}");
    // Without `screens`, none.
    let d = generate(&GenConfig::default()).design;
    assert!(d.defs.values().all(|def| {
        def.nodes
            .values()
            .all(|n| !matches!(n.kind, NodeKind::Screen { .. }))
    }));
}

/// A flat design's screens survive bit-blasting with the same memory.
#[test]
fn bitblasted_screens_match() {
    for seed in 0..10 {
        let cfg = GenConfig {
            seed,
            max_depth: 1,
            child_defs: 0,
            state_percent: 40,
            screens: true,
            ..Default::default()
        };
        let d = generate(&cfg).design;
        let bb = super::bitblast::bitblast(&d);
        bb.validate().unwrap();
        let count = |d: &Design| {
            d.root_def()
                .nodes
                .values()
                .filter(|n| matches!(n.kind, NodeKind::Screen { .. }))
                .count()
        };
        assert_eq!(count(&d), count(&bb));
        assert_equivalent_by_index(&d, &bb, seed, 20);
    }
}
