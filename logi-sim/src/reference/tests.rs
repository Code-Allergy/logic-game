use std::sync::Arc;

use smallvec::smallvec;

use super::*;
use crate::netlist::{Builder, NodeKind};

/// Build a root containing one `kind` node whose inputs are root input ports
/// and whose outputs are root output ports.
fn single(kind: NodeKind) -> Reference {
    let mut d = Design::new("top");
    let r = d.root;
    let (ins, outs) = kind.pins().unwrap();
    let mut b = d.builder(r);
    let in_nets: Vec<_> = ins
        .iter()
        .enumerate()
        .map(|(i, &w)| b.input(format!("i{i}"), w))
        .collect();
    let id = b.node(kind, &in_nets.iter().map(|&n| Some(n)).collect::<Vec<_>>());
    for q in 0..outs.len() {
        let net = b.out(id, q as u8);
        b.output(format!("o{q}"), net);
    }
    Reference::new(&d).unwrap()
}

/// Apply inputs, settle (one tick), return outputs.
fn apply(r: &mut Reference, inputs: &[u64]) -> Vec<u64> {
    for (i, &v) in inputs.iter().enumerate() {
        r.set_input(i, v);
    }
    r.tick().unwrap();
    r.outputs()
}

fn comb(kind: NodeKind, inputs: &[u64]) -> Vec<u64> {
    apply(&mut single(kind), inputs)
}

const WIDTHS: [u8; 3] = [1, 8, 64];

type KindFn = fn(u8) -> NodeKind;
type BinOp = fn(u64, u64) -> u64;

/// Sample operand values for width `w`, including the edges.
fn samples(w: u8) -> Vec<u64> {
    let m = mask(w);
    let mut v = vec![0, 1, m, m >> 1, 0x5555_5555_5555_5555 & m, 0xA5C3_0F96_1234_8001 & m];
    v.dedup();
    v
}

#[test]
fn bitwise_truth_tables_width_1() {
    // Rows are (a, b) = 00, 01, 10, 11.
    let tables: [(KindFn, [u64; 4]); 6] = [
        (|w| NodeKind::And { w }, [0, 0, 0, 1]),
        (|w| NodeKind::Or { w }, [0, 1, 1, 1]),
        (|w| NodeKind::Xor { w }, [0, 1, 1, 0]),
        (|w| NodeKind::Nand { w }, [1, 1, 1, 0]),
        (|w| NodeKind::Nor { w }, [1, 0, 0, 0]),
        (|w| NodeKind::Xnor { w }, [1, 0, 0, 1]),
    ];
    for (kind, table) in tables {
        let mut r = single(kind(1));
        for (row, want) in table.iter().enumerate() {
            let (a, b) = ((row >> 1) as u64, (row & 1) as u64);
            assert_eq!(apply(&mut r, &[a, b]), vec![*want], "{:?} {a}{b}", kind(1));
        }
    }
    assert_eq!(comb(NodeKind::Not { w: 1 }, &[0]), vec![1]);
    assert_eq!(comb(NodeKind::Not { w: 1 }, &[1]), vec![0]);
}

#[test]
fn bitwise_ops_wide() {
    for w in WIDTHS {
        let m = mask(w);
        let ops: [(NodeKind, BinOp); 6] = [
            (NodeKind::And { w }, |a, b| a & b),
            (NodeKind::Or { w }, |a, b| a | b),
            (NodeKind::Xor { w }, |a, b| a ^ b),
            (NodeKind::Nand { w }, |a, b| !(a & b)),
            (NodeKind::Nor { w }, |a, b| !(a | b)),
            (NodeKind::Xnor { w }, |a, b| !(a ^ b)),
        ];
        for (kind, f) in ops {
            let mut r = single(kind.clone());
            for &a in &samples(w) {
                for &b in &samples(w) {
                    assert_eq!(apply(&mut r, &[a, b]), vec![f(a, b) & m], "{kind:?} {a:#x} {b:#x}");
                }
            }
        }
        let mut not = single(NodeKind::Not { w });
        for &a in &samples(w) {
            assert_eq!(apply(&mut not, &[a]), vec![!a & m]);
        }
    }
    // Hand-checked 8-bit values.
    assert_eq!(comb(NodeKind::And { w: 8 }, &[0b1100_1010, 0b1010_0110]), vec![0b1000_0010]);
    assert_eq!(comb(NodeKind::Nor { w: 8 }, &[0b1100_1010, 0b1010_0110]), vec![0b0001_0001]);
    assert_eq!(comb(NodeKind::Not { w: 64 }, &[0]), vec![u64::MAX]);
}

#[test]
fn mux_and_switch() {
    for w in WIDTHS {
        let (a, b) = (mask(w) & 0x1234_5678_9ABC_DEF0, mask(w));
        let mut mux = single(NodeKind::Mux { w });
        assert_eq!(apply(&mut mux, &[0, a, b]), vec![a]);
        assert_eq!(apply(&mut mux, &[1, a, b]), vec![b]);
        let mut sw = single(NodeKind::Switch { w });
        assert_eq!(apply(&mut sw, &[0, b]), vec![0]);
        assert_eq!(apply(&mut sw, &[1, b]), vec![b]);
    }
}

#[test]
fn add_truth_table() {
    // 1-bit full adder: (a, b, cin) -> (sum, cout).
    let mut r = single(NodeKind::Add { w: 1 });
    for row in 0..8u64 {
        let (a, b, c) = (row >> 2 & 1, row >> 1 & 1, row & 1);
        let t = a + b + c;
        assert_eq!(apply(&mut r, &[a, b, c]), vec![t & 1, t >> 1], "{a}{b}{c}");
    }
    let mut r = single(NodeKind::Add { w: 8 });
    assert_eq!(apply(&mut r, &[200, 55, 0]), vec![255, 0]);
    assert_eq!(apply(&mut r, &[200, 55, 1]), vec![0, 1]);
    assert_eq!(apply(&mut r, &[255, 255, 1]), vec![255, 1]);
    let mut r = single(NodeKind::Add { w: 64 });
    assert_eq!(apply(&mut r, &[u64::MAX, 0, 1]), vec![0, 1]);
    assert_eq!(apply(&mut r, &[u64::MAX, u64::MAX, 1]), vec![u64::MAX, 1]);
    assert_eq!(apply(&mut r, &[1 << 63, 1 << 62, 0]), vec![3 << 62, 0]);
}

#[test]
fn comparisons() {
    for w in WIDTHS {
        let mut eq = single(NodeKind::Eq { w });
        let mut lt = single(NodeKind::LtU { w });
        for &a in &samples(w) {
            for &b in &samples(w) {
                assert_eq!(apply(&mut eq, &[a, b]), vec![(a == b) as u64]);
                assert_eq!(apply(&mut lt, &[a, b]), vec![(a < b) as u64]);
            }
        }
    }
    // LtU is unsigned: 0x80 is not less than 1.
    assert_eq!(comb(NodeKind::LtU { w: 8 }, &[0x80, 1]), vec![0]);
}

#[test]
fn reductions() {
    for w in WIDTHS {
        let m = mask(w);
        let mut or = single(NodeKind::ReduceOr { w });
        let mut and = single(NodeKind::ReduceAnd { w });
        let mut xor = single(NodeKind::ReduceXor { w });
        for &a in &samples(w) {
            assert_eq!(apply(&mut or, &[a]), vec![(a != 0) as u64]);
            assert_eq!(apply(&mut and, &[a]), vec![(a == m) as u64]);
            assert_eq!(apply(&mut xor, &[a]), vec![(a.count_ones() % 2) as u64]);
        }
    }
    assert_eq!(comb(NodeKind::ReduceXor { w: 8 }, &[0b0110_1000]), vec![1]);
}

#[test]
fn split_merge_extend() {
    assert_eq!(comb(NodeKind::Split { w: 8, lo: 2, len: 3 }, &[0b1011_0100]), vec![0b101]);
    assert_eq!(comb(NodeKind::Split { w: 64, lo: 63, len: 1 }, &[1 << 63]), vec![1]);
    assert_eq!(comb(NodeKind::Split { w: 64, lo: 0, len: 64 }, &[u64::MAX]), vec![u64::MAX]);
    assert_eq!(comb(NodeKind::Split { w: 1, lo: 0, len: 1 }, &[1]), vec![1]);
    assert_eq!(
        comb(NodeKind::Merge { widths: smallvec![1, 8, 3] }, &[1, 0xAB, 0b101]),
        vec![0b1011_0101_0111]
    );
    assert_eq!(
        comb(NodeKind::Merge { widths: smallvec![32, 32] }, &[0xDEAD_BEEF, 0x0123_4567]),
        vec![0x0123_4567_DEAD_BEEF]
    );
    assert_eq!(comb(NodeKind::Extend { from: 8, to: 64, signed: false }, &[0x80]), vec![0x80]);
    assert_eq!(
        comb(NodeKind::Extend { from: 8, to: 64, signed: true }, &[0x80]),
        vec![0xFFFF_FFFF_FFFF_FF80]
    );
    assert_eq!(comb(NodeKind::Extend { from: 8, to: 16, signed: true }, &[0x7F]), vec![0x7F]);
    assert_eq!(comb(NodeKind::Extend { from: 1, to: 8, signed: true }, &[1]), vec![0xFF]);
    assert_eq!(comb(NodeKind::Extend { from: 64, to: 64, signed: true }, &[1 << 63]), vec![1 << 63]);
}

#[test]
fn const_is_masked() {
    for w in WIDTHS {
        assert_eq!(comb(NodeKind::Const { w, value: u64::MAX }, &[]), vec![mask(w)]);
    }
}

#[test]
fn clock_is_a_function_of_tick_count() {
    let mut r = single(NodeKind::Clock { period: 4, duty: 1, phase: 0 });
    let mut seen = vec![r.output(0)];
    for _ in 0..8 {
        r.tick().unwrap();
        seen.push(r.output(0));
    }
    assert_eq!(seen, vec![1, 0, 0, 0, 1, 0, 0, 0, 1]);

    let mut r = single(NodeKind::Clock { period: 2, duty: 1, phase: 1 });
    let mut seen = vec![r.output(0)];
    r.run(1).unwrap();
    seen.push(r.output(0));
    assert_eq!(seen, vec![0, 1]);
}

#[test]
fn rom_reads_combinationally() {
    for w in WIDTHS {
        let contents: Arc<[u64]> = Arc::from(vec![u64::MAX, 2, 3]);
        let mut r = single(NodeKind::Rom { addr_bits: 2, w, contents });
        assert_eq!(apply(&mut r, &[0]), vec![mask(w)]);
        assert_eq!(apply(&mut r, &[2]), vec![3 & mask(w)]);
        assert_eq!(apply(&mut r, &[3]), vec![0], "past the end reads 0");
    }
}

#[test]
fn unconnected_inputs_read_zero() {
    let mut d = Design::new("top");
    let r = d.root;
    let mut b = d.builder(r);
    let nor = b.node(NodeKind::Nor { w: 8 }, &[]);
    let y = b.out(nor, 0);
    b.output("y", y);
    assert_eq!(Reference::new(&d).unwrap().get("y"), 0xFF);
}

// ── State elements ───────────────────────────────────────────────────────

#[test]
fn delay_chain() {
    for w in WIDTHS {
        let mut d = Design::new("top");
        let r = d.root;
        let mut b = d.builder(r);
        let x = b.input("x", w);
        let d1 = b.delay(x);
        let d2 = b.delay(d1);
        let d3 = b.delay(d2);
        b.output("y", d3);
        let mut r = Reference::new(&d).unwrap();
        let seq = [mask(w), 0, 1 & mask(w), mask(w) >> 1, 0, 0, 0];
        let mut got = Vec::new();
        for &v in &seq {
            r.set("x", v);
            r.tick().unwrap();
            got.push(r.get("y"));
        }
        // Set before tick k, visible to the first delay's commit at tick k+1,
        // and at the output after two more commits.
        assert_eq!(&got[3..], &seq[..4], "width {w}");
        assert_eq!(&got[..3], &[0, 0, 0]);
    }
}

#[test]
fn register_with_enable() {
    for w in WIDTHS {
        let mut r = single(NodeKind::Register { w });
        let v = mask(w) & 0xF0F0_F0F0_F0F0_F0F5;
        // Tick 1 settles with en=1, d=v; the commit at tick 2 loads it.
        assert_eq!(apply(&mut r, &[1, v]), vec![0]);
        assert_eq!(apply(&mut r, &[0, 0]), vec![v]);
        // en=0: holds.
        assert_eq!(apply(&mut r, &[0, 0]), vec![v]);
        assert_eq!(apply(&mut r, &[1, 0]), vec![v]);
        assert_eq!(apply(&mut r, &[0, 1]), vec![0]);
    }
}

#[test]
fn dlatch_is_transparent_when_enabled() {
    for w in WIDTHS {
        let mut r = single(NodeKind::DLatch { w });
        let v = mask(w);
        // Transparent: output follows d in the same settle.
        assert_eq!(apply(&mut r, &[1, v]), vec![v]);
        // Disabled: holds the value captured at the last commit.
        assert_eq!(apply(&mut r, &[0, 0]), vec![v]);
        assert_eq!(apply(&mut r, &[0, 0]), vec![v]);
        assert_eq!(apply(&mut r, &[1, 0]), vec![0]);
        assert_eq!(apply(&mut r, &[0, v]), vec![0]);
    }
}

#[test]
fn sr_latch_truth_table() {
    let mut r = single(NodeKind::SrLatch);
    // (s, r) applied at a tick, q observed one tick later.
    let mut step = |s, rr| {
        apply(&mut r, &[s, rr]);
        apply(&mut r, &[0, 0])[0]
    };
    assert_eq!(step(1, 0), 1, "set");
    assert_eq!(step(0, 0), 1, "hold");
    assert_eq!(step(0, 1), 0, "reset");
    assert_eq!(step(0, 0), 0, "hold");
    assert_eq!(step(1, 1), 0, "reset wins");
}

#[test]
fn ram_write_then_read() {
    for w in WIDTHS {
        let mut d = Design::new("top");
        let r = d.root;
        let mut b = d.builder(r);
        let addr = b.input("addr", 3);
        let we = b.input("we", 1);
        let data = b.input("data", w);
        let q = b.ram(addr, we, data, &[7, 6]);
        b.output("q", q);
        let ram_node = d.root_def().nets[q].driver.0;
        let mut r = Reference::new(&d).unwrap();
        assert_eq!(r.get("q"), 7 & mask(w), "init contents");

        let v = mask(w) & 0x8000_0000_0000_00AA;
        r.set("addr", 5);
        r.set("we", 1);
        r.set("data", v);
        r.tick().unwrap();
        // Read is combinational from committed memory: the write is pending.
        assert_eq!(r.get("q"), 0);
        r.set("we", 0);
        r.tick().unwrap();
        assert_eq!(r.get("q"), v, "write applied at commit");
        assert_eq!(r.mem(&[], ram_node).unwrap()[5], v);
        r.set("addr", 1);
        r.tick().unwrap();
        assert_eq!(r.get("q"), 6 & mask(w));
        r.set("addr", 5);
        r.tick().unwrap();
        assert_eq!(r.get("q"), v, "persists");
    }
}

#[test]
fn counter_from_add_and_register() {
    let mut d = Design::new("top");
    let r = d.root;
    let mut b = d.builder(r);
    let (reg, q) = b.state_placeholder(NodeKind::Register { w: 8 });
    let one = b.konst(8, 1);
    let (next, _) = b.add(q, one, None);
    let en = b.one();
    b.connect(en, (reg, 0));
    b.connect(next, (reg, 1));
    b.output("q", q);
    let mut r = Reference::new(&d).unwrap();
    for t in 0..300u64 {
        assert_eq!(r.get("q"), t % 256);
        r.tick().unwrap();
    }
}

#[test]
fn builder_counter_with_reset() {
    let mut d = Design::new("top");
    let r = d.root;
    let mut b = d.builder(r);
    let en = b.input("en", 1);
    let rst = b.input("rst", 1);
    let q = b.counter(4, en, rst);
    b.output("q", q);
    let mut r = Reference::new(&d).unwrap();
    r.set("en", 1);
    r.run(20).unwrap();
    // Enabled at tick 1's settle, first increment committed at tick 2.
    assert_eq!(r.get("q"), 19 % 16);
    r.set("rst", 1);
    r.run(2).unwrap();
    assert_eq!(r.get("q"), 0);
    r.set("rst", 0);
    r.set("en", 0);
    r.run(5).unwrap();
    // The last settle before the change had rst=1, so the first commit after
    // it loads 0; then en=0 holds.
    assert_eq!(r.get("q"), 0);
}

#[test]
fn feedback_through_delay_is_legal() {
    // q = delay(!q): toggles every tick.
    let mut d = Design::new("top");
    let r = d.root;
    let mut b = d.builder(r);
    let (dl, q) = b.state_placeholder(NodeKind::Delay { w: 1 });
    let nq = b.not(q);
    b.connect(nq, (dl, 0));
    b.output("q", q);
    let mut r = Reference::new(&d).unwrap();
    let mut seen = vec![r.get("q")];
    for _ in 0..4 {
        r.tick().unwrap();
        seen.push(r.get("q"));
    }
    assert_eq!(seen, vec![0, 1, 0, 1, 0]);
}

#[test]
fn feedback_without_state_is_a_loop_error() {
    // Cross-coupled NORs: settles by value under Gauss-Seidel evaluation, so
    // only the structural check catches it.
    let mut d = Design::new("top");
    let r = d.root;
    let mut b = d.builder(r);
    let s = b.input("s", 1);
    let rr = b.input("r", 1);
    let n1 = b.node(NodeKind::Nor { w: 1 }, &[Some(rr)]);
    let q = b.out(n1, 0);
    let n2 = b.node(NodeKind::Nor { w: 1 }, &[Some(s), Some(q)]);
    let qn = b.out(n2, 0);
    b.connect(qn, (n1, 1));
    b.output("q", q);
    match Reference::new(&d) {
        Err(ReferenceError::CombinationalLoop { def, mut nodes }) => {
            assert_eq!(def, r);
            nodes.sort();
            let mut want = vec![n1, n2];
            want.sort();
            assert_eq!(nodes, want);
        }
        other => panic!("expected loop error, got {:?}", other.map(|_| ())),
    }

    // A DLatch is transparent, so a loop through it is still combinational.
    let mut d = Design::new("top");
    let r = d.root;
    let mut b = d.builder(r);
    let en = b.one();
    let (latch, q) = b.state_placeholder(NodeKind::DLatch { w: 1 });
    let nq = b.not(q);
    b.connect(en, (latch, 0));
    b.connect(nq, (latch, 1));
    assert!(matches!(
        Reference::new(&d),
        Err(ReferenceError::CombinationalLoop { .. })
    ));
}

#[test]
fn ram_address_is_combinational_but_write_data_is_not() {
    // rdata -> wdata feedback is legal.
    let mut d = Design::new("top");
    let r = d.root;
    let mut b = d.builder(r);
    let addr = b.input("addr", 2);
    let we = b.one();
    let ram = b.node(NodeKind::Ram { addr_bits: 2, w: 2, init: Arc::from(vec![]) }, &[Some(addr), Some(we)]);
    let q = b.out(ram, 0);
    let inc = b.inc(q);
    b.connect(inc, (ram, 2));
    b.output("q", q);
    let mut r = Reference::new(&d).unwrap();
    r.run(3).unwrap();
    assert_eq!(r.get("q"), 3);

    // rdata -> addr feedback is a loop.
    let mut d = Design::new("top");
    let r = d.root;
    let mut b = d.builder(r);
    let (ram, q) = b.state_placeholder(NodeKind::Ram { addr_bits: 2, w: 2, init: Arc::from(vec![]) });
    b.connect(q, (ram, 0));
    assert!(matches!(
        Reference::new(&d),
        Err(ReferenceError::CombinationalLoop { nodes, .. }) if nodes == vec![ram]
    ));
}

/// A child definition: `y = a` (combinational passthrough) and `q = reg(b)`.
fn child_def(d: &mut Design) -> DefId {
    let c = d.add_definition("child");
    let mut b = Builder::new(d, c);
    let a = b.input("a", 4);
    let x = b.input("b", 4);
    b.output("y", a);
    let en = b.one();
    let q = b.register(en, x);
    b.output("q", q);
    c
}

#[test]
fn loops_through_instances() {
    // Parent wires y back into a: combinational loop through the instance.
    let mut d = Design::new("top");
    let c = child_def(&mut d);
    let r = d.root;
    let mut b = d.builder(r);
    let inst = b.node(NodeKind::Instance(c), &[]);
    let y = b.out(inst, 0);
    b.connect(y, (inst, 0));
    assert!(matches!(
        Reference::new(&d),
        Err(ReferenceError::CombinationalLoop { nodes, .. }) if nodes == vec![inst]
    ));

    // Wiring q back into b passes through the child's register: legal.
    let mut d = Design::new("top");
    let c = child_def(&mut d);
    let r = d.root;
    let mut b = d.builder(r);
    let inst = b.node(NodeKind::Instance(c), &[]);
    let q = b.out(inst, 1);
    let next = b.inc(q);
    b.connect(next, (inst, 1));
    b.output("q", q);
    let mut r = Reference::new(&d).unwrap();
    r.run(5).unwrap();
    assert_eq!(r.get("q"), 5);
}

#[test]
fn nets_are_readable_by_instance_path() {
    let mut d = Design::new("top");
    let c = child_def(&mut d);
    let mid = d.add_definition("mid");
    let mut b = Builder::new(&mut d, mid);
    let a = b.input("a", 4);
    let outs = b.instance(c, &[Some(a), Some(a)]);
    b.output("y", outs[0]);
    b.output("q", outs[1]);
    let mid_inst_of_child = d.defs[mid]
        .nodes
        .iter()
        .find(|(_, n)| n.kind == NodeKind::Instance(c))
        .unwrap()
        .0;
    let r = d.root;
    let mut b = d.builder(r);
    let x = b.input("x", 4);
    let outs = b.instance(mid, &[Some(x)]);
    b.output("y", outs[0]);
    b.output("q", outs[1]);
    let top_inst = d.root_def().nets[outs[0]].driver.0;

    // The child's register output net, two levels down.
    let child_q_net = {
        let cd = &d.defs[c];
        cd.nodes[cd.outputs[1]].inputs[0].unwrap()
    };
    let mut r = Reference::new(&d).unwrap();
    r.set("x", 9);
    r.run(2).unwrap();
    assert_eq!(r.get("y"), 9);
    assert_eq!(r.get("q"), 9);
    assert_eq!(r.net(&[top_inst, mid_inst_of_child], child_q_net), Some(9));
    assert_eq!(r.net(&[], outs[1]), Some(9));
}

#[test]
fn shared_definitions_have_independent_state() {
    let mut d = Design::new("top");
    let c = child_def(&mut d);
    let r = d.root;
    let mut b = d.builder(r);
    let x = b.input("x", 4);
    let k = b.konst(4, 3);
    let o1 = b.instance(c, &[None, Some(x)]);
    let o2 = b.instance(c, &[None, Some(k)]);
    b.output("q1", o1[1]);
    b.output("q2", o2[1]);
    let mut r = Reference::new(&d).unwrap();
    r.set("x", 12);
    r.run(2).unwrap();
    assert_eq!((r.get("q1"), r.get("q2")), (12, 3));
}

#[test]
fn input_timing_matches_the_spec() {
    // An input changed between ticks reaches combinational logic at the next
    // settle and state at the commit after that.
    let mut d = Design::new("top");
    let r = d.root;
    let mut b = d.builder(r);
    let x = b.input("x", 8);
    let nx = b.not(x);
    let q = b.delay(x);
    b.output("nx", nx);
    b.output("q", q);
    let mut r = Reference::new(&d).unwrap();
    assert_eq!((r.get("nx"), r.get("q")), (0xFF, 0));
    r.set("x", 5);
    assert_eq!(r.get("nx"), 0xFF, "not visible before a settle");
    r.tick().unwrap();
    assert_eq!((r.get("nx"), r.get("q")), (0xFA, 0));
    r.tick().unwrap();
    assert_eq!(r.get("q"), 5);
}

#[test]
fn invalid_designs_are_rejected() {
    let mut d = Design::new("top");
    let r = d.root;
    let g = d.add_node(r, NodeKind::And { w: 1 }).unwrap();
    d.defs[r].nodes[g].inputs.pop();
    assert!(matches!(Reference::new(&d), Err(ReferenceError::Invalid(_))));
}
