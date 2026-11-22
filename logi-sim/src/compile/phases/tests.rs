//! Clock-phase specialization and tick fusion: every case is compared
//! with the pass on and off every tick (outputs, state with carry-over
//! between the two programs, every net) under the editor's options and
//! the defaults, and against the reference; fused batches against tick by
//! tick at every boundary a caller can observe.

use std::sync::Arc;

use crate::compile::func::tests::on_off_with;
use crate::compile::tests::check_program_invariants;
use crate::compile::{Options, Program, compile};
use crate::netlist::{Builder, DefId, Design, NetId};
use crate::sim::SimOptions;
use crate::testkit::diff::{check_batches, check_design_with};
use crate::testkit::{GenConfig, fixtures, generate};

/// Phases on against off, under the editor's options and the defaults,
/// and both against the reference (also with every runtime switch off);
/// then fused batches against single ticks. Returns the default options'
/// program with phases.
fn check(name: &str, d: &Design, ticks: u64) -> Program {
    for debug in [false, true] {
        on_off_with(name, d, debug, ticks, |o| Options {
            clock_phases: false,
            ..o
        });
    }
    for o in [Options::default(), Options::debug()] {
        for so in [SimOptions::default(), SimOptions::baseline()] {
            check_design_with(d, 5, ticks.min(200), &o, &so)
                .unwrap_or_else(|e| panic!("{name}: {e}"));
        }
        let p = compile(d, &o).unwrap();
        check_program_invariants(&p);
        check_batches(&Arc::new(p), 3).unwrap_or_else(|e| panic!("{name}: {e}"));
    }
    compile(d, &Options::default()).unwrap()
}

/// A word register built as a master/slave pair of latches on a
/// two-phase clock (as the gate CPU's flip-flops): `q` counts up by
/// `step`, and RAM is written on clock high.
fn master_slave(period: u32, duty: u32, phase: u32) -> Design {
    let mut d = Design::new("top");
    let r = d.root;
    let mut b = d.builder(r);
    let step = b.input("step", 8);
    let we = b.input("we", 1);
    let clk = b.clock(period, duty, phase);
    let nclk = b.not(clk);
    let (slave_node, q) = b.state_placeholder(crate::netlist::NodeKind::Register { w: 8 });
    let (sum, _) = b.add(q, step, None);
    let master = b.register(nclk, sum);
    b.connect(clk, (slave_node, 0));
    b.connect(master, (slave_node, 1));
    let wen = b.and(we, clk);
    let addr = b.split(q, 0, 3);
    let m = b.ram(addr, wen, master, &[]);
    b.output("q", q);
    b.output("m", m);
    b.output("clk", clk);
    d
}

#[test]
fn master_slave_registers_specialize() {
    let d = master_slave(2, 1, 1);
    let p = check("master/slave", &d, 120);
    let ph = p.phases.as_ref().expect("phases");
    assert_eq!(ph.period, 2);
    let base = p.instrs.len() + p.commits.len();
    // Each phase drops one latch's mux and commit.
    for v in &ph.variants {
        assert!(v.ops() < base, "{} -> {}", base, v.ops());
        assert!(v.commits.len() < p.commits.len());
    }
}

#[test]
fn every_clock_phase_and_duty_specializes_exactly() {
    for period in 1..=4 {
        for duty in 0..=period {
            for phase in 0..period + 1 {
                let d = master_slave(period, duty, phase);
                check(&format!("clock {period}/{duty}/{phase}"), &d, 40);
            }
        }
    }
}

/// Several clocks of different periods and phases, with a common period
/// of 8, plus one design whose common period (12) is too long.
#[test]
fn several_clocks_specialize_over_their_common_period() {
    let build = |clocks: &[(u32, u32, u32)]| {
        let mut d = Design::new("top");
        let r = d.root;
        let mut b = d.builder(r);
        let x = b.input("x", 4);
        let mut acc = x;
        for (k, &(p, du, ph)) in clocks.iter().enumerate() {
            let c = b.clock(p, du, ph);
            let held = b.dlatch(c, acc);
            let c4 = b.zext(c, 4);
            acc = b.xor(held, c4);
            b.output(format!("c{k}"), c);
        }
        b.output("y", acc);
        d
    };
    let d = build(&[(2, 1, 0), (4, 3, 1), (8, 2, 5)]);
    let p = check("periods 2/4/8", &d, 100);
    assert_eq!(p.phases.as_ref().map(|ph| ph.period), Some(8));
    let d = build(&[(2, 1, 0), (3, 1, 2), (4, 1, 1)]);
    let p = check("periods 2/3/4", &d, 100);
    assert!(
        p.phases.is_none(),
        "common period 12 > {}",
        super::MAX_PHASES
    );
}

/// A clock used as data: XORed into a word, added, compared and used as a
/// mux select, alongside latches on it.
#[test]
fn a_clock_used_as_data_specializes_exactly() {
    let mut d = Design::new("top");
    let r = d.root;
    let mut b = d.builder(r);
    let x = b.input("x", 8);
    let clk = b.clock(2, 1, 0);
    let c8 = b.zext(clk, 8);
    let y = b.xor(x, c8);
    let (s, carry) = b.add(x, c8, Some(clk));
    let e = b.eq(y, s);
    let m = b.mux(clk, x, s);
    let q = b.register(clk, m);
    let nq = b.not(q);
    let held = b.dlatch(e, nq);
    b.output("y", y);
    b.output("s", s);
    b.output("carry", carry);
    b.output("held", held);
    b.output("q", q);
    check("clock as data", &d, 100);
}

/// A gate-built D flip-flop (master/slave NAND latches) on a clock, used
/// as a 4-bit register: function matching, lifting and the cleanup run
/// first, then the phases.
#[test]
fn gate_flip_flops_specialize() {
    fn dff(d: &mut Design) -> DefId {
        let def = d.add_definition("dff");
        let mut b = Builder::new(d, def);
        let din = b.input("D", 1);
        let clk = b.input("Clk", 1);
        let nclk = b.not(clk);
        let m = b.dlatch(nclk, din);
        let q = b.dlatch(clk, m);
        b.output("Q", q);
        def
    }
    let mut d = Design::new("top");
    let ff = dff(&mut d);
    let r = d.root;
    let mut b = d.builder(r);
    let x = b.input("x", 4);
    let clk = b.clock(2, 1, 1);
    let bits: Vec<NetId> = (0..4).map(|k| b.bit(x, k)).collect();
    let mut qs = Vec::new();
    for bit in bits {
        let q = b.instance(ff, &[Some(bit), Some(clk)])[0];
        qs.push(q);
    }
    let q = b.merge(&qs);
    b.output("q", q);
    let p = check("gate dffs", &d, 120);
    assert!(p.phases.is_some());
}

#[test]
fn fixtures_without_clocks_are_unchanged() {
    for d in [fixtures::cpu(&fixtures::LOOP_PROGRAM), fixtures::counter(8)] {
        let p = compile(&d, &Options::default()).unwrap();
        assert!(p.phases.is_none());
        check_batches(&Arc::new(p), 9).unwrap();
    }
}

/// Random designs (which contain clocks of periods 1 to 7): phases on and
/// off, against each other and the reference, and fused batches.
#[test]
fn random_designs_specialize_exactly() {
    let mut with_phases = 0;
    for seed in 0..60 {
        let g = generate(&GenConfig {
            seed: seed * 7 + 3,
            state_percent: 30,
            ..GenConfig::default()
        });
        let p = check(&format!("random {seed}"), &g.design, 30);
        with_phases += p.phases.is_some() as u32;
    }
    assert!(with_phases >= 5, "only {with_phases} designs had phases");
}
