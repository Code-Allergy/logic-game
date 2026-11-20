use std::time::Instant;

use super::exec::Exec;
use super::*;
use crate::netlist::{Builder, DefId, Design, NodeId, NodeKind, mask};
use crate::reference::Reference;
use crate::testkit::{GenConfig, Rng, fixtures, generate};

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
    NodePath { instances: instances.to_vec(), node }
}

/// Compare every root output and every authored net between the compiled
/// program and the reference interpreter, at tick 0 and after each of
/// `ticks` ticks with random inputs.
fn differential(d: &Design, seed: u64, ticks: u64) {
    let p = compile_ok(d);
    check_program_invariants(&p);
    let mut x = Exec::new(&p);
    let mut r = Reference::new(d).unwrap();
    let mut rng = Rng::new(seed);
    let widths = d.root_def().signature().0;
    for t in 0..=ticks {
        if t > 0 {
            for (port, &w) in widths.iter().enumerate() {
                let v = rng.next_u64() & mask(w);
                x.set_input(port, v);
                r.set_input(port, v);
            }
            x.tick();
            r.tick().unwrap();
        }
        for port in 0..p.ports.outputs.len() {
            assert_eq!(x.output(port), r.output(port), "seed {seed} tick {t} output {port}");
        }
        for inst in 0..p.debug.instances.len() as u32 {
            let path = p.debug.path(inst);
            for (net, &slot) in &p.debug.net_slots[inst as usize] {
                assert_eq!(
                    x.slots[slot as usize],
                    r.net(&path, net).unwrap(),
                    "seed {seed} tick {t} net {path:?}/{net:?}"
                );
            }
        }
    }
}

/// Blocks are sorted by (rank, op), cover the instruction list exactly, and
/// no instruction reads a slot written in its own or a later rank.
fn check_program_invariants(p: &Program) {
    let mut written_rank = vec![u32::MAX; p.slot_count as usize];
    let mut next = 0;
    for w in p.blocks.windows(2) {
        assert!((w[0].rank, w[0].op) < (w[1].rank, w[1].op), "blocks out of order");
    }
    for b in &p.blocks {
        assert_eq!(b.start, next);
        next += b.len;
        for ins in &p.instrs[b.start as usize..(b.start + b.len) as usize] {
            assert_eq!(written_rank[ins.dst as usize], u32::MAX, "slot written twice");
            written_rank[ins.dst as usize] = b.rank;
        }
    }
    assert_eq!(next as usize, p.instrs.len());
    for b in &p.blocks {
        let (sa, sb, sc) = b.op.slot_operands();
        for ins in &p.instrs[b.start as usize..(b.start + b.len) as usize] {
            for (is_slot, s) in [(sa, ins.a), (sb, ins.b), (sc, ins.c)] {
                if is_slot {
                    let r = written_rank[s as usize];
                    assert!(r == u32::MAX || r < b.rank, "operand from same/later rank");
                }
            }
        }
    }
    assert_eq!(written_rank[ZERO_SLOT as usize], u32::MAX, "zero slot written");
    for &(s, _) in &p.consts {
        assert_eq!(written_rank[s as usize], u32::MAX, "const slot written");
    }
}

// ── Differential against the reference ───────────────────────────────────

#[test]
fn generated_designs_match_the_reference() {
    let configs = [
        GenConfig::default(),
        GenConfig { max_width: 64, ..Default::default() },
        GenConfig { max_width: 1, ..Default::default() },
        GenConfig { max_depth: 5, child_defs: 6, instance_percent: 20, ..Default::default() },
        GenConfig { state_percent: 50, unconnected_percent: 20, ..Default::default() },
        GenConfig::dag(0, 200, 32),
    ];
    for (ci, cfg) in configs.iter().enumerate() {
        for seed in 0..40 {
            let g = generate(&GenConfig { seed: seed * 13 + ci as u64, ..cfg.clone() });
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
    let mut x = Exec::new(&p);
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
    assert_eq!(x.mems[ram.unwrap() as usize][1], fixtures::MULTIPLY_RESULT);
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
        assert_eq!(loops_of(&f.design), vec![want], "{}", f.design.root_def().name);
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
    let mut x = Exec::new(&p);
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
    let names: Vec<_> = p.ports.inputs.iter().map(|p| p.name.clone().unwrap()).collect();
    assert_eq!(names, ["a", "unused"]);
    assert_eq!(p.ports.inputs[1].width, 7);
    let outs_names: Vec<_> = p.ports.outputs.iter().map(|p| p.name.clone().unwrap()).collect();
    assert_eq!(outs_names, ["y", "z", "open", "q"]);
    assert_eq!(p.ports.outputs[2].slot, ZERO_SLOT, "unconnected output reads zero");

    // Port unification: no copy for the passthrough, one slot shared by
    // the root input, the child's input port net and the child's output.
    let a_slot = p.ports.inputs[0].slot;
    assert_eq!(p.ports.outputs[0].slot, a_slot);
    let cdef = &d.defs[c];
    let child_a = cdef.nodes[cdef.inputs[0]].outputs[0];
    assert_eq!(p.debug.slot(&[inst], child_a), Some(a_slot));
    assert_eq!(p.debug.nets_in_slot(a_slot).len(), 3, "root a, child a, root y");
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
    let mut x = Exec::new(&p);
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
    let mut x = Exec::new(&p);
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
    time("random dag 100k", &generate(&GenConfig::dag(1, 100_000, 32)).design);
    time("random dag 1M", &generate(&GenConfig::dag(1, 1_000_000, 32)).design);
    time("nested 20k", &nested(20_000));
}

/// Larger differential sweep: `cargo test -p logi-sim --release --lib -- --ignored differential_many`.
#[test]
#[ignore]
fn differential_many() {
    let configs = [
        GenConfig::default(),
        GenConfig { max_width: 64, nodes: 150, ..Default::default() },
        GenConfig { max_width: 1, ..Default::default() },
        GenConfig { max_depth: 5, child_defs: 6, instance_percent: 20, ..Default::default() },
        GenConfig { state_percent: 50, unconnected_percent: 20, ..Default::default() },
    ];
    for (ci, cfg) in configs.iter().enumerate() {
        for seed in 0..2000 {
            let g = generate(&GenConfig { seed: seed * 31 + ci as u64, ..cfg.clone() });
            differential(&g.design, seed, 20);
        }
    }
}
