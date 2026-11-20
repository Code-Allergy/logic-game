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
