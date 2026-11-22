//! Hand-built v2 fixtures, ported from the legacy `crate::fixtures` and
//! extended. Word-level fixtures come with bit-blasted variants (same
//! function and port names, built from 1-bit gates) for benchmarks and
//! cross-checks. Ports are named; use `Reference::set`/`get`.

use crate::netlist::{Builder, DefId, Design, NetId, NodeId, NodeKind};

// ─────────────────────────────────────────────────────────────────────────────
// Adders
// ─────────────────────────────────────────────────────────────────────────────

/// 1-bit full adder definition: `a, b, cin -> sum, cout`, from XOR/AND/OR.
pub fn full_adder_def(d: &mut Design) -> DefId {
    let def = d.add_definition("full_adder");
    let mut b = Builder::new(d, def);
    let a = b.input("a", 1);
    let x = b.input("b", 1);
    let cin = b.input("cin", 1);
    let p = b.xor(a, x);
    let sum = b.xor(p, cin);
    let g = b.and(a, x);
    let t = b.and(p, cin);
    let cout = b.or(g, t);
    b.output("sum", sum);
    b.output("cout", cout);
    def
}

/// `w`-bit adder using the `Add` primitive. Ports `a, b, cin -> sum, cout`.
pub fn adder(w: u8) -> Design {
    let mut d = Design::new(format!("adder{w}"));
    let r = d.root;
    let mut b = d.builder(r);
    let a = b.input("a", w);
    let x = b.input("b", w);
    let cin = b.input("cin", 1);
    let (sum, cout) = b.add(a, x, Some(cin));
    b.output("sum", sum);
    b.output("cout", cout);
    d
}

/// Bit-blasted ripple-carry adder: `w` instances of one shared full-adder
/// definition. Same ports as [`adder`].
pub fn ripple_adder(w: u8) -> Design {
    let mut d = Design::new(format!("ripple_adder{w}"));
    let fa = full_adder_def(&mut d);
    let r = d.root;
    let mut b = d.builder(r);
    let a = b.input("a", w);
    let x = b.input("b", w);
    let mut carry = b.input("cin", 1);
    let ab = b.bus_expand(a);
    let bb = b.bus_expand(x);
    let mut sums = Vec::with_capacity(w as usize);
    for i in 0..w as usize {
        let o = b.instance(fa, &[Some(ab[i]), Some(bb[i]), Some(carry)]);
        sums.push(o[0]);
        carry = o[1];
    }
    let sum = b.bus_collect(&sums);
    b.output("sum", sum);
    b.output("cout", carry);
    d
}

// ─────────────────────────────────────────────────────────────────────────────
// Mux trees
// ─────────────────────────────────────────────────────────────────────────────

fn mux_ports(b: &mut Builder<'_>, sel_bits: u8, w: u8) -> (NetId, Vec<NetId>) {
    let sel = b.input("sel", sel_bits);
    let ds = (0..1u32 << sel_bits).map(|i| b.input(format!("d{i}"), w)).collect();
    (sel, ds)
}

/// `2^sel_bits`-way, `w`-bit mux tree from `Mux` primitives.
/// Ports `sel, d0, d1, .. -> y`.
pub fn mux_tree(sel_bits: u8, w: u8) -> Design {
    let mut d = Design::new(format!("mux_tree{sel_bits}x{w}"));
    let r = d.root;
    let mut b = d.builder(r);
    let (sel, ds) = mux_ports(&mut b, sel_bits, w);
    let y = b.mux_n(sel, &ds);
    b.output("y", y);
    d
}

/// Bit-blasted [`mux_tree`]: a gate-level decoder and an AND-OR plane per
/// bit. Same ports.
pub fn mux_tree_bitblasted(sel_bits: u8, w: u8) -> Design {
    let mut d = Design::new(format!("mux_tree_bb{sel_bits}x{w}"));
    let r = d.root;
    let mut b = d.builder(r);
    let (sel, ds) = mux_ports(&mut b, sel_bits, w);
    let sel_bits_v = b.bus_expand(sel);
    let sel_not: Vec<NetId> = sel_bits_v.iter().map(|&s| b.not(s)).collect();
    let decoded: Vec<NetId> = (0..ds.len())
        .map(|i| {
            let terms: Vec<NetId> = (0..sel_bits as usize)
                .map(|k| if i >> k & 1 == 1 { sel_bits_v[k] } else { sel_not[k] })
                .collect();
            b.and_n(&terms)
        })
        .collect();
    let d_bits: Vec<Vec<NetId>> = ds.iter().map(|&x| b.bus_expand(x)).collect();
    let y_bits: Vec<NetId> = (0..w as usize)
        .map(|bit| {
            let terms: Vec<NetId> = (0..ds.len()).map(|i| b.and(d_bits[i][bit], decoded[i])).collect();
            b.or_n(&terms)
        })
        .collect();
    let y = b.bus_collect(&y_bits);
    b.output("y", y);
    d
}

// ─────────────────────────────────────────────────────────────────────────────
// Counters
// ─────────────────────────────────────────────────────────────────────────────

/// `w`-bit counter (`Add` + `Register`). Ports `en, reset -> q`. Each tick,
/// reset clears it, else en increments it.
pub fn counter(w: u8) -> Design {
    let mut d = Design::new(format!("counter{w}"));
    let r = d.root;
    let mut b = d.builder(r);
    let en = b.input("en", 1);
    let reset = b.input("reset", 1);
    let q = b.counter(w, en, reset);
    b.output("q", q);
    d
}

/// Bit-blasted [`counter`]: 1-bit registers and a half-adder carry chain.
pub fn counter_bitblasted(w: u8) -> Design {
    let mut d = Design::new(format!("counter_bb{w}"));
    let r = d.root;
    let mut b = d.builder(r);
    let en = b.input("en", 1);
    let reset = b.input("reset", 1);
    let load = b.or(en, reset);
    let keep = b.not(reset);
    let mut carry = b.one();
    let mut qs = Vec::with_capacity(w as usize);
    for _ in 0..w {
        let (reg, q) = b.state_placeholder(NodeKind::Register { w: 1 });
        let sum = b.xor(q, carry);
        carry = b.and(q, carry);
        let next = b.and(sum, keep);
        b.connect(load, (reg, 0));
        b.connect(next, (reg, 1));
        qs.push(q);
    }
    let q = b.bus_collect(&qs);
    b.output("q", q);
    d
}

// ─────────────────────────────────────────────────────────────────────────────
// Register file
// ─────────────────────────────────────────────────────────────────────────────

/// `2^addr_bits` registers of `w` bits, one write port and two read ports.
/// Ports `we, waddr, wdata, ra, rb -> qa, qb`. Reads are combinational from
/// the committed registers; the write lands at the next commit.
pub fn register_file(addr_bits: u8, w: u8) -> Design {
    let mut d = Design::new(format!("regfile{addr_bits}x{w}"));
    let r = d.root;
    let mut b = d.builder(r);
    let we = b.input("we", 1);
    let waddr = b.input("waddr", addr_bits);
    let wdata = b.input("wdata", w);
    let ra = b.input("ra", addr_bits);
    let rb = b.input("rb", addr_bits);
    let regs: Vec<NetId> = b
        .decoder(waddr)
        .into_iter()
        .map(|sel| {
            let en = b.and(we, sel);
            b.register(en, wdata)
        })
        .collect();
    let qa = b.mux_n(ra, &regs);
    let qb = b.mux_n(rb, &regs);
    b.output("qa", qa);
    b.output("qb", qb);
    d
}

// ─────────────────────────────────────────────────────────────────────────────
// A small CPU
// ─────────────────────────────────────────────────────────────────────────────

/// Accumulator machine for [`cpu`]. An instruction is 12 bits:
/// `op << 8 | operand`. `pc`, `acc` and RAM words are 8 bits; RAM has 256
/// words addressed by the operand.
pub mod isa {
    pub const NOP: u64 = 0;
    /// `acc = imm`
    pub const LDI: u64 = 1;
    /// `acc = ram[a]`
    pub const LD: u64 = 2;
    /// `ram[a] = acc`
    pub const ST: u64 = 3;
    /// `acc += ram[a]`
    pub const ADD: u64 = 4;
    /// `acc -= ram[a]`
    pub const SUB: u64 = 5;
    /// `pc = a`
    pub const JMP: u64 = 6;
    /// `if acc == 0 { pc = a }`
    pub const JZ: u64 = 7;
    /// Stop: `pc` stays put; output `halted` is 1.
    pub const HLT: u64 = 8;
    /// `acc += imm`
    pub const ADDI: u64 = 9;

    pub const fn op(code: u64, operand: u64) -> u64 {
        code << 8 | (operand & 0xFF)
    }
}

/// Multiplies 5 by 7 by repeated addition; halts with `acc == 35` and
/// `ram[1] == 35`.
pub const MULTIPLY_PROGRAM: [u64; 19] = {
    use isa::*;
    [
        op(LDI, 5),
        op(ST, 0), // counter = 5
        op(LDI, 1),
        op(ST, 2), // one = 1
        op(LDI, 7),
        op(ST, 3), // addend = 7
        op(LDI, 0),
        op(ST, 1), // result = 0
        op(LD, 0), // 8: loop
        op(JZ, 17),
        op(SUB, 2),
        op(ST, 0), // counter -= 1
        op(LD, 1),
        op(ADD, 3),
        op(ST, 1), // result += addend
        op(JMP, 8),
        op(NOP, 0),
        op(LD, 1), // 17: done
        op(HLT, 0),
    ]
};

/// Result of [`MULTIPLY_PROGRAM`].
pub const MULTIPLY_RESULT: u64 = 35;

/// Single-cycle accumulator CPU running `program` from ROM, with 256 bytes
/// of RAM. Ports: no inputs; outputs `acc, pc, halted`. The RAM node is
/// named `"ram"` in the root definition.
pub fn cpu(program: &[u64]) -> Design {
    use isa::*;
    let mut d = Design::new("cpu");
    let r = d.root;
    let mut b = d.builder(r);
    let one = b.one();
    let (pc_reg, pc) = b.state_placeholder(NodeKind::Register { w: 8 });
    let (acc_reg, acc) = b.state_placeholder(NodeKind::Register { w: 8 });

    let instr = b.rom(pc, 12, program);
    let operand = b.split(instr, 0, 8);
    let opcode = b.split(instr, 8, 4);
    let is = |b: &mut Builder<'_>, code: u64| b.eq_const(opcode, code);
    let (is_ldi, is_ld, is_st, is_add, is_sub) = (
        is(&mut b, LDI),
        is(&mut b, LD),
        is(&mut b, ST),
        is(&mut b, ADD),
        is(&mut b, SUB),
    );
    let (is_jmp, is_jz, is_hlt, is_addi) = (is(&mut b, JMP), is(&mut b, JZ), is(&mut b, HLT), is(&mut b, ADDI));

    let rdata = b.ram(operand, is_st, acc, &[]);
    let ram_node = d.root_def().nets[rdata].driver.0;
    let mut b = d.builder(r);
    b.name(ram_node, "ram");

    let (sum, _) = b.add(acc, rdata, None);
    let diff = b.sub(acc, rdata);
    let (sum_imm, _) = b.add(acc, operand, None);
    let mut acc_next = acc;
    for (cond, value) in [
        (is_ldi, operand),
        (is_ld, rdata),
        (is_add, sum),
        (is_sub, diff),
        (is_addi, sum_imm),
    ] {
        acc_next = b.mux(cond, acc_next, value);
    }

    let acc_zero = b.eq_const(acc, 0);
    let take_jz = b.and(is_jz, acc_zero);
    let jump = b.or(is_jmp, take_jz);
    let pc_inc = b.inc(pc);
    let pc_next = b.mux(jump, pc_inc, operand);
    let pc_next = b.mux(is_hlt, pc_next, pc);

    b.connect(one, (pc_reg, 0));
    b.connect(pc_next, (pc_reg, 1));
    b.connect(one, (acc_reg, 0));
    b.connect(acc_next, (acc_reg, 1));
    b.output("acc", acc);
    b.output("pc", pc);
    b.output("halted", is_hlt);
    d
}

// ─────────────────────────────────────────────────────────────────────────────
// Gate-loop latches (expected to fail) and their legal counterparts
// ─────────────────────────────────────────────────────────────────────────────

/// A design that must be rejected with a combinational loop through
/// `loop_nodes` (in `def`).
pub struct LoopFixture {
    pub design: Design,
    pub def: DefId,
    pub loop_nodes: Vec<NodeId>,
}

fn nor_pair(b: &mut Builder<'_>, s: NetId, r: NetId) -> (NodeId, NodeId, NetId, NetId) {
    // NOR1: R, Qn -> Q.  NOR2: S, Q -> Qn.
    let n1 = b.node(NodeKind::Nor { w: 1 }, &[Some(r)]);
    let q = b.out(n1, 0);
    let n2 = b.node(NodeKind::Nor { w: 1 }, &[Some(s), Some(q)]);
    let qn = b.out(n2, 0);
    b.connect(qn, (n1, 1));
    (n1, n2, q, qn)
}

/// Legacy `sr_latch`: cross-coupled NORs. Ports `R, S -> Q, Qn`.
pub fn sr_latch_nor() -> LoopFixture {
    let mut d = Design::new("sr_latch_nor");
    let def = d.root;
    let mut b = d.builder(def);
    let r = b.input("R", 1);
    let s = b.input("S", 1);
    let (n1, n2, q, qn) = nor_pair(&mut b, s, r);
    b.output("Q", q);
    b.output("Qn", qn);
    LoopFixture { design: d, def, loop_nodes: vec![n1, n2] }
}

/// Cross-coupled NANDs (active-low SR latch). Ports `Sn, Rn -> Q, Qn`.
pub fn sr_latch_nand() -> LoopFixture {
    let mut d = Design::new("sr_latch_nand");
    let def = d.root;
    let mut b = d.builder(def);
    let sn = b.input("Sn", 1);
    let rn = b.input("Rn", 1);
    let n1 = b.node(NodeKind::Nand { w: 1 }, &[Some(sn)]);
    let q = b.out(n1, 0);
    let n2 = b.node(NodeKind::Nand { w: 1 }, &[Some(rn), Some(q)]);
    let qn = b.out(n2, 0);
    b.connect(qn, (n1, 1));
    b.output("Q", q);
    b.output("Qn", qn);
    LoopFixture { design: d, def, loop_nodes: vec![n1, n2] }
}

/// Legacy `d_flip_flop` (really a gated D latch): gating logic into a NOR
/// SR latch. Ports `D, E -> Q, Qn`.
pub fn d_latch_gates() -> LoopFixture {
    let mut d = Design::new("d_latch_gates");
    let def = d.root;
    let mut b = d.builder(def);
    let din = b.input("D", 1);
    let e = b.input("E", 1);
    let nd = b.not(din);
    let r = b.and(nd, e);
    let s = b.and(din, e);
    let (n1, n2, q, qn) = nor_pair(&mut b, s, r);
    b.output("Q", q);
    b.output("Qn", qn);
    LoopFixture { design: d, def, loop_nodes: vec![n1, n2] }
}

/// Ring oscillator: `n` inverters in a ring (`n >= 1`). Output `y`.
pub fn ring_oscillator(n: usize) -> LoopFixture {
    let mut d = Design::new("ring_oscillator");
    let def = d.root;
    let mut b = d.builder(def);
    let first = b.node(NodeKind::Not { w: 1 }, &[]);
    let mut nodes = vec![first];
    let mut prev = b.out(first, 0);
    for _ in 1..n {
        let id = b.node(NodeKind::Not { w: 1 }, &[Some(prev)]);
        prev = b.out(id, 0);
        nodes.push(id);
    }
    b.connect(prev, (first, 0));
    b.output("y", prev);
    LoopFixture { design: d, def, loop_nodes: nodes }
}

/// The legal version of [`sr_latch_nor`]: the Qn→NOR1 feedback passes
/// through a `Delay`. Ports `R, S -> Q, Qn`.
pub fn sr_latch_nor_delayed() -> Design {
    let mut d = Design::new("sr_latch_nor_delayed");
    let r = d.root;
    let mut b = d.builder(r);
    let rr = b.input("R", 1);
    let s = b.input("S", 1);
    let n1 = b.node(NodeKind::Nor { w: 1 }, &[Some(rr)]);
    let q = b.out(n1, 0);
    let qn = b.nor(s, q);
    let qn_d = b.delay(qn);
    b.connect(qn_d, (n1, 1));
    b.output("Q", q);
    b.output("Qn", qn);
    d
}

/// A program that never halts (for throughput benchmarks): increments
/// `ram[0]` forever and doubles it into `acc` each pass.
pub const LOOP_PROGRAM: [u64; 6] = {
    use isa::*;
    [op(LDI, 0), op(ADDI, 1), op(ST, 0), op(LD, 0), op(ADD, 0), op(JMP, 1)]
};
