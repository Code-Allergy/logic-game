use std::time::Instant;

use std::sync::Arc;

use super::*;
use crate::netlist::{Builder, DefId, Design, NodeId, NodeKind};
use crate::sim::Sim;
use crate::testkit::{GenConfig, fixtures, generate};

fn compile_ok(d: &Design) -> Program {
    compile(d, &Options::default()).unwrap_or_else(|e| panic!("compile failed: {e:?}"))
}

fn loops_of(d: &Design) -> Vec<Vec<NodePath>> {
    match compile(d, &Options::default()) {
        Err(errs) => errs
            .into_iter()
            .map(|e| match e {
                CompileError::CombinationalLoop { nodes } => nodes,
                other => panic!("expected only loop errors, got {other:?}"),
            })
            .collect(),
        Ok(_) => panic!("expected a loop error"),
    }
}

fn at(instances: &[NodeId], node: NodeId) -> NodePath {
    NodePath {
        instances: instances.to_vec(),
        node,
    }
}

/// Check program invariants, then run the differential harness.
fn differential(d: &Design, seed: u64, ticks: u64) {
    check_program_invariants(&compile_ok(d));
    if let Err(e) = crate::testkit::diff::check_design(d, seed, ticks, &Options::default()) {
        panic!("seed {seed}: {e}");
    }
}

fn sim(p: Program) -> Sim {
    Sim::new(Arc::new(p))
}

/// Blocks are sorted by (rank, op, packed first, partition), cover the
/// instruction list (and the
/// packed words) exactly, every slot is written once, and nothing reads a
/// slot written in its own or a later rank.
pub(crate) fn check_program_invariants(p: &Program) {
    let mut written_rank = vec![u32::MAX; p.slot_count as usize];
    let (mut next, mut next_pack) = (0, 0);
    let part = |i: usize| p.block_parts.get(i).copied().unwrap_or(0);
    assert!(p.block_parts.is_empty() || p.block_parts.len() == p.blocks.len());
    for (i, w) in p.blocks.windows(2).enumerate() {
        assert!(
            (w[0].rank, w[0].op, !w[0].packed, part(i))
                < (w[1].rank, w[1].op, !w[1].packed, part(i + 1)),
            "blocks out of order"
        );
    }
    // (writes, reads) of block `b`.
    let slots = |b: &Block| -> (Vec<u32>, Vec<u32>) {
        let range = b.start as usize..(b.start + b.len) as usize;
        let (mut w, mut r) = (Vec::new(), Vec::new());
        if b.packed {
            for pk in &p.packs[range] {
                w.push(pk.dst);
                w.extend(
                    p.scatters[pk.scatter_start as usize..][..pk.scatter_len as usize]
                        .iter()
                        .map(|x| x.slot),
                );
                for o in [&pk.a, &pk.b, &pk.c] {
                    r.extend([o.base, o.splat]);
                    r.extend(
                        p.gathers[o.gather_start as usize..][..o.gather_len as usize]
                            .iter()
                            .map(|g| g.slot),
                    );
                }
            }
        } else {
            let (sa, sb, sc) = b.op.slot_operands();
            for ins in &p.instrs[range] {
                w.push(ins.dst);
                for (is_slot, s) in [(sa, ins.a), (sb, ins.b), (sc, ins.c)] {
                    if is_slot {
                        r.push(s);
                    }
                }
            }
        }
        (w, r)
    };
    for b in &p.blocks {
        if b.packed {
            assert_eq!(b.start, next_pack);
            next_pack += b.len;
        } else {
            assert_eq!(b.start, next);
            next += b.len;
        }
        for d in slots(b).0 {
            assert_eq!(written_rank[d as usize], u32::MAX, "slot written twice");
            written_rank[d as usize] = b.rank;
        }
    }
    assert_eq!(next as usize, p.instrs.len());
    assert_eq!(next_pack as usize, p.packs.len());
    for b in &p.blocks {
        for s in slots(b).1 {
            let r = written_rank[s as usize];
            assert!(r == u32::MAX || r < b.rank, "operand from same/later rank");
        }
    }
    assert_eq!(
        written_rank[ZERO_SLOT as usize],
        u32::MAX,
        "zero slot written"
    );
    for &(s, _) in &p.consts {
        assert_eq!(written_rank[s as usize], u32::MAX, "const slot written");
    }
    // Lane words are packed destinations; lane slots resolve.
    let dsts: std::collections::HashSet<u32> = p.packs.iter().map(|x| x.dst).collect();
    assert!(p.lane_words.iter().all(|w| dsts.contains(w)));
    for map in &p.debug.net_slots {
        for (_, &s) in map {
            assert!(p.locate(s).is_some(), "debug slot {s} out of range");
        }
    }
}

// ── Differential against the reference ───────────────────────────────────

#[test]
fn generated_designs_match_the_reference() {
    let configs = [
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
    ];
    for (ci, cfg) in configs.iter().enumerate() {
        for seed in 0..40 {
            let g = generate(&GenConfig {
                seed: seed * 13 + ci as u64,
                ..cfg.clone()
            });
            differential(&g.design, seed, 15);
        }
    }
}

#[test]
fn fixtures_match_the_reference() {
    use fixtures::*;
    for d in [
        adder(8),
        ripple_adder(8),
        adder(64),
        ripple_adder(64),
        mux_tree(3, 8),
        mux_tree_bitblasted(3, 8),
        counter(8),
        counter_bitblasted(8),
        register_file(3, 16),
        sr_latch_nor_delayed(),
    ] {
        differential(&d, 7, 100);
    }
}

#[test]
fn compiled_cpu_runs_the_multiply_program() {
    let d = fixtures::cpu(&fixtures::MULTIPLY_PROGRAM);
    let p = compile_ok(&d);
    let mut x = sim(p.clone());
    let halted = p.ports.output("halted").unwrap();
    let acc = p.ports.output("acc").unwrap();
    let mut ticks = 0;
    while x.output(halted) == 0 {
        x.tick();
        ticks += 1;
        assert!(ticks < 500);
    }
    assert_eq!(x.output(acc), fixtures::MULTIPLY_RESULT);
    let ram = p.state_keys.iter().find_map(|k| match k.loc {
        StateLoc::Mem(m) => Some(m),
        _ => None,
    });
    assert_eq!(x.mem(ram.unwrap())[1], fixtures::MULTIPLY_RESULT);
    differential(&d, 0, 120);
}

// ── Loop diagnostics ─────────────────────────────────────────────────────

#[test]
fn latch_fixtures_report_their_loop() {
    for f in [
        fixtures::sr_latch_nor(),
        fixtures::sr_latch_nand(),
        fixtures::d_latch_gates(),
        fixtures::ring_oscillator(1),
        fixtures::ring_oscillator(7),
    ] {
        let mut want: Vec<_> = f.loop_nodes.iter().map(|&n| at(&[], n)).collect();
        want.sort();
        assert_eq!(
            loops_of(&f.design),
            vec![want],
            "{}",
            f.design.root_def().name
        );
    }
}

/// Every instance path of `def` in the design, from the root.
fn instance_paths(d: &Design, def: DefId) -> Vec<Vec<NodeId>> {
    let mut out = Vec::new();
    let mut stack = vec![(d.root, Vec::new())];
    while let Some((cur, path)) = stack.pop() {
        if cur == def {
            out.push(path.clone());
        }
        for (id, n) in &d.defs[cur].nodes {
            if let NodeKind::Instance(c) = n.kind {
                let mut p = path.clone();
                p.push(id);
                stack.push((c, p));
            }
        }
    }
    out
}

#[test]
fn injected_loops_are_reported_in_every_instance() {
    for seed in 0..60 {
        let cfg = GenConfig {
            seed,
            inject_loop: true,
            child_defs: 4,
            instance_percent: 25,
            ..Default::default()
        };
        let g = generate(&cfg);
        let (def, chain) = g.injected_loop.unwrap();
        let mut want: Vec<Vec<NodePath>> = instance_paths(&g.design, def)
            .into_iter()
            .map(|path| {
                let mut v: Vec<_> = chain.iter().map(|&n| at(&path, n)).collect();
                v.sort();
                v
            })
            .collect();
        want.sort();
        let mut got = loops_of(&g.design);
        got.sort();
        assert_eq!(got, want, "seed {seed}");
    }
}

/// `child`: `y = a` (wire passthrough), `z = !b` (one gate).
fn passthrough_child(d: &mut Design) -> (DefId, NodeId) {
    let c = d.add_definition("child");
    let mut b = Builder::new(d, c);
    let a = b.input("a", 1);
    let x = b.input("b", 1);
    b.output("y", a);
    let z = b.not(x);
    b.output("z", z);
    let not = d.defs[c].nets[z].driver.0;
    (c, not)
}

#[test]
fn loops_across_instance_boundaries() {
    // Wire-only loop through a passthrough: reported as the instance node.
    let mut d = Design::new("top");
    let (c, _) = passthrough_child(&mut d);
    let r = d.root;
    let mut b = d.builder(r);
    let inst = b.node(NodeKind::Instance(c), &[]);
    let y = b.out(inst, 0);
    b.connect(y, (inst, 0));
    assert_eq!(loops_of(&d), vec![vec![at(&[], inst)]]);

    // Loop through a gate inside the child and a gate in the parent.
    let mut d = Design::new("top");
    let (c, not) = passthrough_child(&mut d);
    let r = d.root;
    let mut b = d.builder(r);
    let inst = b.node(NodeKind::Instance(c), &[]);
    let z = b.out(inst, 1);
    let buf = b.node(NodeKind::Xor { w: 1 }, &[Some(z)]);
    let y = b.out(buf, 0);
    b.connect(y, (inst, 1));
    let mut want = vec![at(&[inst], not), at(&[], buf)];
    want.sort();
    assert_eq!(loops_of(&d), vec![want]);

    // Two levels deep, through two sibling instances of a shared definition.
    let mut d = Design::new("top");
    let (c, not) = passthrough_child(&mut d);
    let mid = d.add_definition("mid");
    let mut b = Builder::new(&mut d, mid);
    let a = b.input("a", 1);
    let o = b.instance(c, &[None, Some(a)]);
    b.output("y", o[1]);
    let mid_child = d.defs[mid].nets[o[1]].driver.0;
    let r = d.root;
    let mut b = d.builder(r);
    let m1 = b.node(NodeKind::Instance(mid), &[]);
    let m2 = b.node(NodeKind::Instance(mid), &[]);
    let y1 = b.out(m1, 0);
    let y2 = b.out(m2, 0);
    b.connect(y1, (m2, 0));
    b.connect(y2, (m1, 0));
    let mut want = vec![at(&[m1, mid_child], not), at(&[m2, mid_child], not)];
    want.sort();
    assert_eq!(loops_of(&d), vec![want]);
}

#[test]
fn feedback_through_state_inside_an_instance_compiles() {
    let mut d = Design::new("top");
    let c = d.add_definition("reg");
    let mut b = Builder::new(&mut d, c);
    let x = b.input("d", 8);
    let one = b.one();
    let q = b.register(one, x);
    b.output("q", q);
    let r = d.root;
    let mut b = d.builder(r);
    let inst = b.node(NodeKind::Instance(c), &[]);
    let q = b.out(inst, 0);
    let next = b.inc(q);
    b.connect(next, (inst, 0));
    b.output("q", q);
    differential(&d, 0, 20);
    let p = compile_ok(&d);
    let mut x = sim(p.clone());
    for _ in 0..300 {
        x.tick();
    }
    assert_eq!(x.output(0), 300 % 256);
}

#[test]
fn several_loops_are_all_reported() {
    let mut d = Design::new("top");
    let r = d.root;
    let mut b = d.builder(r);
    let n1 = b.node(NodeKind::Not { w: 1 }, &[]);
    let o1 = b.out(n1, 0);
    b.connect(o1, (n1, 0));
    let n2 = b.node(NodeKind::Not { w: 4 }, &[]);
    let o2 = b.out(n2, 0);
    b.connect(o2, (n2, 0));
    let mut got = loops_of(&d);
    got.sort();
    let mut want = vec![vec![at(&[], n1)], vec![at(&[], n2)]];
    want.sort();
    assert_eq!(got, want);
}

#[test]
fn invalid_designs_return_validation_errors() {
    let mut d = Design::new("top");
    let r = d.root;
    let g = d.add_node(r, NodeKind::And { w: 1 }).unwrap();
    d.defs[r].nodes[g].inputs.pop();
    let errs = compile(&d, &Options::default()).unwrap_err();
    assert!(matches!(errs[0], CompileError::Invalid(_)));
}

// ── Program structure ────────────────────────────────────────────────────

#[test]
fn ports_debug_map_and_state_keys() {
    let mut d = Design::new("top");
    let (c, not) = passthrough_child(&mut d);
    let r = d.root;
    let mut b = d.builder(r);
    let a = b.input("a", 1);
    let unused = b.input("unused", 7);
    let outs = b.instance(c, &[Some(a), None]);
    let inst = d.root_def().nets[outs[0]].driver.0;
    let mut b = d.builder(r);
    b.output("y", outs[0]);
    b.output("z", outs[1]);
    let open = b.node(NodeKind::Output { w: 3 }, &[]);
    b.name(open, "open");
    let q = b.delay(a);
    b.output("q", q);
    let delay = d.root_def().nets[q].driver.0;
    let p = compile_ok(&d);

    // Ports in order, with names.
    let names: Vec<_> = p
        .ports
        .inputs
        .iter()
        .map(|p| p.name.clone().unwrap())
        .collect();
    assert_eq!(names, ["a", "unused"]);
    assert_eq!(p.ports.inputs[1].width, 7);
    let outs_names: Vec<_> = p
        .ports
        .outputs
        .iter()
        .map(|p| p.name.clone().unwrap())
        .collect();
    assert_eq!(outs_names, ["y", "z", "open", "q"]);
    assert_eq!(
        p.ports.outputs[2].slot, ZERO_SLOT,
        "unconnected output reads zero"
    );

    // Port unification: no copy for the passthrough, one slot shared by
    // the root input, the child's input port net and the child's output.
    let a_slot = p.ports.inputs[0].slot;
    assert_eq!(p.ports.outputs[0].slot, a_slot);
    let cdef = &d.defs[c];
    let child_a = cdef.nodes[cdef.inputs[0]].outputs[0];
    assert_eq!(p.debug.slot(&[inst], child_a), Some(a_slot));
    assert_eq!(
        p.debug.nets_in_slot(a_slot).len(),
        3,
        "root a, child a, root y"
    );
    // Unconnected child input reads zero.
    let child_b = cdef.nodes[cdef.inputs[1]].outputs[0];
    assert_eq!(p.debug.slot(&[inst], child_b), Some(ZERO_SLOT));
    let not_out = cdef.nodes[not].outputs[0];
    assert_ne!(p.debug.slot(&[inst], not_out), Some(ZERO_SLOT));
    let _ = unused;

    // One state key, for the delay at the root.
    assert_eq!(p.state_keys.len(), 1);
    assert_eq!(p.state_keys[0].path, at(&[], delay));
    assert_eq!(p.state_keys[0].loc, StateLoc::Slot(p.ports.outputs[3].slot));
    assert_eq!(p.commits.len(), 1);
}

#[test]
fn chain_depth_is_its_rank_count() {
    let d = chain(1000);
    let p = compile_ok(&d);
    assert_eq!(p.instrs.len(), 1000);
    assert_eq!(p.depth(), 1000);
    check_program_invariants(&p);
    let mut x = sim(p.clone());
    x.set_input(0, 1);
    x.tick();
    assert_eq!(x.output(0), 1, "even number of inverters");
}

// ── Scale ────────────────────────────────────────────────────────────────

/// `n` inverters in series.
fn chain(n: usize) -> Design {
    let mut d = Design::new("chain");
    let r = d.root;
    let mut b = d.builder(r);
    let mut x = b.input("x", 1);
    for _ in 0..n {
        x = b.not(x);
    }
    b.output("y", x);
    d
}

/// `depth` definitions, each instancing the next, with one gate per level.
fn nested(depth: usize) -> Design {
    let mut d = Design::new("top");
    let mut inner: Option<DefId> = None;
    for i in 0..depth {
        let def = d.add_definition(format!("level{i}"));
        let mut b = Builder::new(&mut d, def);
        let x = b.input("x", 1);
        let y = match inner {
            Some(c) => b.instance(c, &[Some(x)])[0],
            None => x,
        };
        let z = b.not(y);
        b.output("y", z);
        inner = Some(def);
    }
    let r = d.root;
    let mut b = d.builder(r);
    let x = b.input("x", 1);
    let y = b.instance(inner.unwrap(), &[Some(x)])[0];
    b.output("y", y);
    d
}

#[test]
fn long_chain_compiles_without_recursion() {
    let p = compile_ok(&chain(100_000));
    assert_eq!(p.depth(), 100_000);
}

#[test]
fn deep_nesting_compiles_without_recursion() {
    let p = compile_ok(&nested(5_000));
    assert_eq!(p.debug.instances.len(), 5_001);
    let mut x = sim(p.clone());
    x.set_input(0, 1);
    x.tick();
    assert_eq!(x.output(0), 1, "5000 inverters");
}

/// Compile-time report: `cargo test -p logi-sim --release --lib -- --ignored compile_time --nocapture`.
#[test]
#[ignore]
fn compile_time_report() {
    let time = |label: &str, d: &Design| {
        let nodes: usize = d.defs.values().map(|def| def.nodes.len()).sum();
        let t = Instant::now();
        let p = compile_ok(d);
        let dt = t.elapsed();
        eprintln!(
            "{label:>16}: {nodes:>8} nodes -> {:>8} instrs, {:>8} slots, depth {:>7}, {:>4} blocks: {:>8.1} ms",
            p.instrs.len(),
            p.slot_count,
            p.depth(),
            p.blocks.len(),
            dt.as_secs_f64() * 1e3
        );
    };
    time("chain 100k", &chain(100_000));
    time("chain 1M", &chain(1_000_000));
    time(
        "random dag 100k",
        &generate(&GenConfig::dag(1, 100_000, 32)).design,
    );
    time(
        "random dag 1M",
        &generate(&GenConfig::dag(1, 1_000_000, 32)).design,
    );
    time("nested 20k", &nested(20_000));
}

/// Larger differential sweep: `cargo test -p logi-sim --release --lib -- --ignored differential_many`.
#[test]
#[ignore]
fn differential_many() {
    let configs = [
        GenConfig::default(),
        GenConfig {
            max_width: 64,
            nodes: 150,
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
    ];
    for (ci, cfg) in configs.iter().enumerate() {
        for seed in 0..2000 {
            let g = generate(&GenConfig {
                seed: seed * 31 + ci as u64,
                ..cfg.clone()
            });
            differential(&g.design, seed, 20);
        }
    }
}

// ── Simplify passes ──────────────────────────────────────────────────────

fn pass_variants() -> Vec<(&'static str, Options)> {
    let none = Options {
        debug: false,
        ..Options::unoptimized()
    };
    vec![
        ("unoptimized", Options::unoptimized()),
        (
            "const_fold",
            Options {
                const_fold: true,
                ..none.clone()
            },
        ),
        (
            "alias",
            Options {
                alias: true,
                ..none.clone()
            },
        ),
        (
            "cse",
            Options {
                cse: true,
                ..none.clone()
            },
        ),
        (
            "dce",
            Options {
                dce: true,
                ..none.clone()
            },
        ),
        ("all", Options::default()),
        ("debug", Options::debug()),
    ]
}

#[test]
fn each_pass_alone_and_together_matches_the_reference() {
    use crate::testkit::diff;
    for (name, opts) in pass_variants() {
        let n = diff::sweep(&diff::standard_configs(), 25, 15, &opts);
        assert_eq!(n, 150, "{name}");
        // Bit-blasted designs are full of constants and duplicate logic.
        for d in [
            crate::testkit::bitblast::bitblast(&fixtures::cpu(&fixtures::MULTIPLY_PROGRAM)),
            crate::testkit::bitblast::bitblast(&fixtures::counter(16)),
            fixtures::ripple_adder(16),
            fixtures::register_file(2, 8),
        ] {
            diff::check_design(&d, 3, 80, &opts).unwrap_or_else(|e| panic!("{name}: {e}"));
        }
    }
}

/// Larger sweep per pass: `cargo test -p logi-sim --release --lib -- --ignored simplify_many`.
#[test]
#[ignore]
fn simplify_many() {
    use crate::testkit::diff;
    for (_, opts) in pass_variants() {
        diff::sweep(&diff::standard_configs(), 400, 20, &opts);
    }
}

fn count(d: &Design, opts: &Options) -> usize {
    compile(d, opts).unwrap().instrs.len()
}

#[test]
fn constant_folding_and_identities() {
    let mut d = Design::new("top");
    let r = d.root;
    let mut b = d.builder(r);
    let x = b.input("x", 8);
    let k3 = b.konst(8, 3);
    let k4 = b.konst(8, 4);
    let (sum, _) = b.add(k3, k4, None); // 7, folded
    let zero = b.zero(8);
    let ones = b.konst(8, 0xFF);
    let a0 = b.and(x, zero); // 0
    let a1 = b.and(x, ones); // x
    let o0 = b.or(x, zero); // x
    let x0 = b.xor(x, x); // 0
    let one = b.one();
    let m = b.mux(one, a0, sum); // sum = 7
    let rom = b.rom(k3, 8, &[10, 11, 12, 13]); // ROM[3] = 13, folded
    let all = b.merge(&[m, rom]); // 16 bits, constant 13 << 8 | 7
    b.output("all", all);
    b.output("a1", a1);
    b.output("o0", o0);
    b.output("x0", x0);
    let p = compile_ok(&d);
    assert!(
        p.instrs.is_empty(),
        "everything folds or aliases: {:?}",
        p.instrs
    );
    let mut s = sim(p.clone());
    s.set("x", 0x5A);
    s.tick();
    assert_eq!(s.get("all"), 13 << 8 | 7);
    assert_eq!((s.get("a1"), s.get("o0"), s.get("x0")), (0x5A, 0x5A, 0));
    assert_eq!(
        p.ports.outputs[1].slot, p.ports.inputs[0].slot,
        "x & 0xFF aliases x"
    );
    assert!(count(&d, &Options::unoptimized()) >= 8);
}

#[test]
fn cse_merges_identical_and_commuted_instructions() {
    let mut d = Design::new("top");
    let r = d.root;
    let mut b = d.builder(r);
    let x = b.input("x", 8);
    let y = b.input("y", 8);
    let a = b.and(x, y);
    let a2 = b.and(y, x);
    let s1 = b.xor(a, x);
    let s2 = b.xor(a2, x);
    let out = b.or(s1, s2); // == s1 after CSE, then alias x|x
    b.output("out", out);
    let only_cse = Options {
        cse: true,
        ..Options::unoptimized()
    };
    // The second xor merges too once the ands merge, leaving or(s1, s1).
    assert_eq!(count(&d, &only_cse), 3, "and, xor, or");
    assert_eq!(count(&d, &Options::default()), 2, "and, xor");
    differential(&d, 1, 20);
}

/// Port of the legacy `common_cells_are_merged_without_losing_wire_observability`
/// and `common_cell_pass_recurses_into_subcircuits`: CSE merges commuted
/// instructions inside an instance and across two instances, and every
/// merged net stays readable at its own instance path.
#[test]
fn cse_inside_and_across_instances_keeps_merged_nets_readable() {
    let mut d = Design::new("top");
    let c = d.add_definition("c");
    let mut b = Builder::new(&mut d, c);
    let x = b.input("x", 8);
    let y = b.input("y", 8);
    let p = b.and(x, y);
    let q = b.and(y, x);
    b.output("p", p);
    b.output("q", q);
    let r = d.root;
    let mut b = d.builder(r);
    let rx = b.input("x", 8);
    let ry = b.input("y", 8);
    let o1 = b.instance(c, &[Some(rx), Some(ry)]);
    let o2 = b.instance(c, &[Some(ry), Some(rx)]);
    for (i, &o) in o1.iter().chain(&o2).enumerate() {
        b.output(format!("o{i}"), o);
    }
    let i1 = d.root_def().nets[o1[0]].driver.0;
    let i2 = d.root_def().nets[o2[0]].driver.0;
    assert_eq!(count(&d, &Options::unoptimized()), 4);
    assert_eq!(
        count(&d, &Options::default()),
        1,
        "four ands merge into one"
    );
    differential(&d, 2, 20);

    let mut s = sim(compile_ok(&d));
    s.set("x", 0b1100_1010);
    s.set("y", 0b1010_0110);
    s.tick();
    for inst in [i1, i2] {
        for net in [p, q] {
            assert_eq!(s.read_net(&[inst], net), Some(0b1000_0010));
        }
    }
}

#[test]
fn dce_keeps_outputs_state_and_probes() {
    let mut d = Design::new("top");
    let r = d.root;
    let mut b = d.builder(r);
    let x = b.input("x", 8);
    let used = b.not(x);
    let dead = b.inc(x);
    let dead2 = b.xor(dead, x);
    let st = b.delay(dead2); // state keeps dead2 live
    let probe_only = b.reduce_xor(x);
    let gone = b.reduce_or(x);
    b.output("y", used);
    let _ = (st, gone);
    let all = Options::default();
    let p = compile(&d, &all).unwrap();
    // not, inc's sum, xor. inc's carry and the two reductions are dead, and
    // the delay's next-state copy aliases the xor.
    assert_eq!(p.instrs.len(), 3);
    assert_eq!(
        p.debug.slot(&[], gone),
        None,
        "dead nets leave the debug map"
    );
    assert!(p.debug.slot(&[], dead2).is_some());

    let with_probe = Options {
        probes: vec![(vec![], probe_only)],
        ..all
    };
    let p = compile(&d, &with_probe).unwrap();
    assert_eq!(p.instrs.len(), 4);
    let mut s = sim(p);
    s.set("x", 0b0111);
    s.tick();
    assert_eq!(s.read_net(&[], probe_only), Some(1));

    // Debug keeps everything observable.
    let p = compile(&d, &Options::debug()).unwrap();
    assert!(p.debug.slot(&[], gone).is_some());
    assert_eq!(p.instrs.len(), 6);
}

#[test]
fn aliasing_never_lets_a_commit_read_state_written_by_another_commit() {
    // Shift register from enabled registers (en = 1 makes next == d), a
    // delay chain, and a DLatch with en = 1 fed by a register. Folding turns
    // each next-state into an alias of another state slot, which commits
    // would then read after it had already been overwritten.
    let mut d = Design::new("top");
    let r = d.root;
    let mut b = d.builder(r);
    let x = b.input("x", 8);
    let one = b.one();
    let q1 = b.register(one, x);
    let q2 = b.register(one, q1);
    let q3 = b.register(one, q2);
    let d1 = b.delay(q3);
    let d2 = b.delay(d1);
    let l = b.dlatch(one, d2);
    let l2 = b.delay(l);
    b.output("q3", q3);
    b.output("d2", d2);
    b.output("l2", l2);
    for (_, opts) in pass_variants() {
        let mut s = Sim::new(Arc::new(compile(&d, &opts).unwrap()));
        let mut seen = Vec::new();
        for t in 1..=12u64 {
            s.set("x", t);
            s.tick();
            seen.push((s.get("q3"), s.get("d2"), s.get("l2")));
        }
        // x set before tick t is in q1 after tick t+1 (one commit later), so
        // after tick t each stage holds t minus its depth.
        assert_eq!(seen[11], (12 - 3, 12 - 5, 12 - 6), "{opts:?}");
        differential(&d, 0, 20);
    }
}

#[test]
fn bitblasted_fixtures_shrink() {
    use crate::testkit::bitblast::bitblast;
    for d in [
        bitblast(&fixtures::cpu(&fixtures::MULTIPLY_PROGRAM)),
        bitblast(&fixtures::counter(32)),
        fixtures::counter_bitblasted(32),
    ] {
        let (before, after) = (
            count(&d, &Options::unoptimized()),
            count(&d, &Options::default()),
        );
        assert!(after < before, "{}: {before} -> {after}", d.root_def().name);
    }
}
