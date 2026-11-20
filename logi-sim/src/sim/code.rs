//! The runtime's own instruction encoding (issue #41).
//!
//! [`Sim::try_new`](super::Sim::try_new) validates a [`Program`] and then
//! lowers it into a [`Code`]: the same blocks, re-encoded for streaming.
//!
//! - **Implicit destinations.** Slots are numbered in execution order, so in
//!   most blocks instruction `k` writes `dst0 + k`. Such a block streams only
//!   its slot operands (4 bytes each) instead of a 16-byte [`Instr`] plus an
//!   8-byte immediate, and its writes are sequential.
//! - **Uniform immediates.** A block whose immediates are all equal (a
//!   1-bit `Nand` block: every mask is 1) keeps one copy; ops that ignore
//!   the immediate never read one.
//!
//! Blocks that do not fit (ops with literal fields, or destinations out of
//! order) run from the program's own [`Instr`]s.
//!
//! Every kernel takes an instruction sub-range `lo..hi` of its block, so a
//! block can be split between threads (`super::pool`).

use crate::compile::{Gather, Instr, Op, Pack, PackOperand, Program};
use crate::netlist::mask;

/// One block of [`Code`]: `len` instructions sharing `op` and `rank`.
#[derive(Debug, Clone, Copy)]
pub(super) struct XBlock {
    pub op: Op,
    pub rank: u32,
    /// The program block's `start` (into `instrs` or `packs`).
    pub src: u32,
    pub len: u32,
    pub form: Form,
}

#[derive(Debug, Clone, Copy)]
pub(super) enum Form {
    /// Instruction `k` writes `dst0 + k` and reads `arity` slots from
    /// `operands[opnd + arity * k..]`.
    Seq { dst0: u32, opnd: u32, imm: Imm },
    /// Instructions `instrs[start..start + len]` and their `imms`.
    Plain { start: u32 },
    /// Packed words `packs[start..start + len]`.
    Packed { start: u32 },
}

#[derive(Debug, Clone, Copy)]
pub(super) enum Imm {
    /// Every instruction's immediate is this value (or the op ignores it).
    Uniform(u64),
    /// Immediate `k` is `imms[start + k]`.
    Each { start: u32 },
}

#[derive(Debug, Default)]
pub(super) struct Code {
    pub blocks: Vec<XBlock>,
    pub operands: Vec<u32>,
    pub imms: Vec<u64>,
}

/// Ops whose immediate matters.
fn uses_imm(op: Op) -> bool {
    use Op::*;
    matches!(
        op,
        Not | Nand | Nor | Xnor | AddSum | AddCarry | ReduceAnd | Shr | Sext
    )
}

/// Ops whose fields are all slots (no literals), so they can stream only
/// their operands.
fn streamable(op: Op) -> bool {
    use Op::*;
    !matches!(op, Shr | Concat | Sext | Clock | MemRead)
}

/// Number of slot operands of `op`.
pub(super) fn arity(op: Op) -> u32 {
    let (a, b, c) = op.slot_operands();
    a as u32 + b as u32 + c as u32
}

impl Code {
    /// Lower a program that passed `check`.
    pub fn new(p: &Program) -> Self {
        let mut code = Code::default();
        for b in &p.blocks {
            if b.packed {
                code.blocks.push(XBlock {
                    op: b.op,
                    rank: b.rank,
                    src: b.start,
                    len: b.len,
                    form: Form::Packed { start: b.start },
                });
                continue;
            }
            let range = b.start as usize..(b.start + b.len) as usize;
            let ins = &p.instrs[range.clone()];
            let imms = &p.imms[range];
            let dst0 = ins.first().map_or(0, |i| i.dst);
            let seq = streamable(b.op)
                && ins
                    .iter()
                    .enumerate()
                    .all(|(k, i)| i.dst as u64 == dst0 as u64 + k as u64);
            let form = if seq {
                let opnd = code.operands.len() as u32;
                let (sa, sb, sc) = b.op.slot_operands();
                for i in ins {
                    for (is, s) in [(sa, i.a), (sb, i.b), (sc, i.c)] {
                        if is {
                            code.operands.push(s);
                        }
                    }
                }
                let imm = if !uses_imm(b.op) {
                    Imm::Uniform(0)
                } else if imms.iter().all(|&m| m == imms[0]) {
                    Imm::Uniform(imms[0])
                } else {
                    let start = code.imms.len() as u32;
                    code.imms.extend_from_slice(imms);
                    Imm::Each { start }
                };
                Form::Seq { dst0, opnd, imm }
            } else {
                Form::Plain { start: b.start }
            };
            code.blocks.push(XBlock {
                op: b.op,
                rank: b.rank,
                src: b.start,
                len: b.len,
                form,
            });
        }
        code
    }

    /// Every block runs from the program's own instructions (the M3
    /// runtime).
    pub fn plain(p: &Program) -> Self {
        Code {
            blocks: p
                .blocks
                .iter()
                .map(|b| XBlock {
                    op: b.op,
                    rank: b.rank,
                    src: b.start,
                    len: b.len,
                    form: if b.packed {
                        Form::Packed { start: b.start }
                    } else {
                        Form::Plain { start: b.start }
                    },
                })
                .collect(),
            ..Code::default()
        }
    }
}

/// What a kernel needs besides the slots.
pub(super) struct Env<'a> {
    pub program: &'a Program,
    pub code: &'a Code,
    pub mems: &'a [Vec<u64>],
    pub tick: u64,
}

/// Run instructions `lo..hi` of block `b`.
///
/// # Safety
///
/// `s` points to `program.slot_count` slots, the program passed `check`
/// and `env.code` was built from it. No other thread may write a slot this
/// range reads or read a slot it writes, while it runs.
#[inline]
pub(super) unsafe fn exec(env: &Env<'_>, b: &XBlock, lo: u32, hi: u32, s: *mut u64) {
    let (lo, hi) = (lo as usize, hi as usize);
    match b.form {
        Form::Seq { dst0, opnd, imm } => {
            let n = arity(b.op) as usize;
            // SAFETY: `Code::new` pushed `n * len` operands at `opnd`, and
            // `Imm::Each` ranges have `len` entries; `dst0 + k` was a
            // checked destination slot.
            unsafe {
                let d = s.add(dst0 as usize);
                let o = env.code.operands.as_ptr().add(opnd as usize);
                match imm {
                    Imm::Uniform(m) => seq(b.op, s, d, o, n, lo, hi, |_| m),
                    Imm::Each { start } => {
                        let m = env.code.imms.as_ptr().add(start as usize);
                        seq(b.op, s, d, o, n, lo, hi, |k| *m.add(k))
                    }
                }
            }
        }
        Form::Plain { start } => {
            let range = start as usize + lo..start as usize + hi;
            // SAFETY: `check` validated the block range and every index.
            unsafe {
                let ins = env.program.instrs.get_unchecked(range.clone());
                let imms = env.program.imms.get_unchecked(range);
                plain(env, b.op, s, ins, imms)
            }
        }
        Form::Packed { start } => {
            let range = start as usize + lo..start as usize + hi;
            // SAFETY: `check` validated the block range, every slot, bit,
            // lane and gather/scatter range of these packs.
            unsafe {
                let packs = env.program.packs.get_unchecked(range);
                packed(env.program, b.op, s, packs)
            }
        }
    }
}

/// Assemble one packed operand word (see [`PackOperand`]).
#[inline(always)]
unsafe fn operand(s: *const u64, o: &PackOperand, g: *const Gather) -> u64 {
    unsafe {
        let mut v = *s.add(o.base as usize) & o.keep;
        let splat = (*s.add(o.splat as usize) >> o.splat_bit) & 1;
        v |= splat.wrapping_neg() & o.splat_mask;
        let g = g.add(o.gather_start as usize);
        for k in 0..o.gather_len as usize {
            let g = &*g.add(k);
            v |= ((*s.add(g.slot as usize) >> g.bit) & 1) << g.lane;
        }
        v
    }
}

/// Packed kernels: assemble operands, apply the op to all lanes, store the
/// word and scatter the lanes read as plain slots.
#[inline(always)]
unsafe fn packed(p: &Program, op: Op, s: *mut u64, packs: &[Pack]) {
    #[inline(always)]
    unsafe fn each(
        p: &Program,
        s: *mut u64,
        packs: &[Pack],
        n: u8,
        f: impl Fn(u64, u64, u64) -> u64,
    ) {
        let g = p.gathers.as_ptr();
        let sc = p.scatters.as_ptr();
        for pk in packs {
            unsafe {
                let a = operand(s, &pk.a, g);
                let b = if n >= 2 { operand(s, &pk.b, g) } else { 0 };
                let c = if n >= 3 { operand(s, &pk.c, g) } else { 0 };
                let r = f(a, b, c);
                *s.add(pk.dst as usize) = r;
                let sc = sc.add(pk.scatter_start as usize);
                for k in 0..pk.scatter_len as usize {
                    let x = &*sc.add(k);
                    *s.add(x.slot as usize) = (r >> x.lane) & 1;
                }
            }
        }
    }
    // SAFETY: forwarded from `exec`.
    unsafe {
        match op {
            Op::Copy => each(p, s, packs, 1, |a, _, _| a),
            Op::Not => each(p, s, packs, 1, |a, _, _| !a),
            Op::And => each(p, s, packs, 2, |a, b, _| a & b),
            Op::Or => each(p, s, packs, 2, |a, b, _| a | b),
            Op::Xor => each(p, s, packs, 2, |a, b, _| a ^ b),
            Op::Nand => each(p, s, packs, 2, |a, b, _| !(a & b)),
            Op::Nor => each(p, s, packs, 2, |a, b, _| !(a | b)),
            Op::Xnor => each(p, s, packs, 2, |a, b, _| !(a ^ b)),
            Op::Switch => each(p, s, packs, 2, |a, b, _| a & b),
            Op::Mux => each(p, s, packs, 3, |a, b, c| (a & c) | (!a & b)),
            _ => unreachable!("checked: not a packable op"),
        }
    }
}

/// Ops a packed block may have.
pub(super) fn packable_op(op: Op) -> bool {
    use Op::*;
    matches!(
        op,
        Copy | Not | And | Or | Xor | Nand | Nor | Xnor | Mux | Switch
    )
}

/// Streaming kernels: `n` operands per instruction, destination `d + k`.
#[inline(always)]
#[allow(clippy::too_many_arguments)]
unsafe fn seq(
    op: Op,
    s: *mut u64,
    d: *mut u64,
    o: *const u32,
    n: usize,
    lo: usize,
    hi: usize,
    imm: impl Fn(usize) -> u64,
) {
    #[inline(always)]
    unsafe fn s1(
        s: *mut u64,
        d: *mut u64,
        o: *const u32,
        lo: usize,
        hi: usize,
        imm: impl Fn(usize) -> u64,
        f: impl Fn(u64, u64) -> u64,
    ) {
        for k in lo..hi {
            unsafe { *d.add(k) = f(*s.add(*o.add(k) as usize), imm(k)) };
        }
    }
    #[inline(always)]
    unsafe fn s2(
        s: *mut u64,
        d: *mut u64,
        o: *const u32,
        lo: usize,
        hi: usize,
        imm: impl Fn(usize) -> u64,
        f: impl Fn(u64, u64, u64) -> u64,
    ) {
        for k in lo..hi {
            unsafe {
                let a = *s.add(*o.add(2 * k) as usize);
                let b = *s.add(*o.add(2 * k + 1) as usize);
                *d.add(k) = f(a, b, imm(k));
            }
        }
    }
    #[inline(always)]
    unsafe fn s3(
        s: *mut u64,
        d: *mut u64,
        o: *const u32,
        lo: usize,
        hi: usize,
        imm: impl Fn(usize) -> u64,
        f: impl Fn(u64, u64, u64, u64) -> u64,
    ) {
        for k in lo..hi {
            unsafe {
                let a = *s.add(*o.add(3 * k) as usize);
                let b = *s.add(*o.add(3 * k + 1) as usize);
                let c = *s.add(*o.add(3 * k + 2) as usize);
                *d.add(k) = f(a, b, c, imm(k));
            }
        }
    }
    debug_assert_eq!(n, arity(op) as usize);
    // SAFETY: forwarded from `exec`.
    unsafe {
        match op {
            Op::Copy => s1(s, d, o, lo, hi, imm, |a, _| a),
            Op::Not => s1(s, d, o, lo, hi, imm, |a, m| !a & m),
            Op::And => s2(s, d, o, lo, hi, imm, |a, b, _| a & b),
            Op::Or => s2(s, d, o, lo, hi, imm, |a, b, _| a | b),
            Op::Xor => s2(s, d, o, lo, hi, imm, |a, b, _| a ^ b),
            Op::Nand => s2(s, d, o, lo, hi, imm, |a, b, m| !(a & b) & m),
            Op::Nor => s2(s, d, o, lo, hi, imm, |a, b, m| !(a | b) & m),
            Op::Xnor => s2(s, d, o, lo, hi, imm, |a, b, m| !(a ^ b) & m),
            Op::Mux => s3(
                s,
                d,
                o,
                lo,
                hi,
                imm,
                |a, b, c, _| if a != 0 { c } else { b },
            ),
            Op::Switch => s2(s, d, o, lo, hi, imm, |a, b, _| if a != 0 { b } else { 0 }),
            Op::AddSum => s3(s, d, o, lo, hi, imm, |a, b, c, m| {
                a.wrapping_add(b).wrapping_add(c) & m
            }),
            Op::AddCarry => s3(s, d, o, lo, hi, imm, |a, b, c, w| {
                ((a as u128 + b as u128 + c as u128) >> w) as u64 & 1
            }),
            Op::Eq => s2(s, d, o, lo, hi, imm, |a, b, _| (a == b) as u64),
            Op::LtU => s2(s, d, o, lo, hi, imm, |a, b, _| (a < b) as u64),
            Op::ReduceOr => s1(s, d, o, lo, hi, imm, |a, _| (a != 0) as u64),
            Op::ReduceAnd => s1(s, d, o, lo, hi, imm, |a, m| (a == m) as u64),
            Op::ReduceXor => s1(s, d, o, lo, hi, imm, |a, _| (a.count_ones() & 1) as u64),
            Op::SrNext => s3(s, d, o, lo, hi, imm, |a, b, c, _| {
                if b != 0 {
                    0
                } else if a != 0 {
                    1
                } else {
                    c
                }
            }),
            Op::Shr | Op::Concat | Op::Sext | Op::Clock | Op::MemRead => {
                unreachable!("not streamable")
            }
        }
    }
}

/// Kernels over the program's own instructions.
#[inline(always)]
unsafe fn plain(env: &Env<'_>, op: Op, s: *mut u64, ins: &[Instr], imms: &[u64]) {
    let tick = env.tick;
    // SAFETY: forwarded from `exec`. Reads and writes go through the raw
    // pointer, so `dst` may equal an operand (it never does in a compiled
    // program, but it would be sound).
    unsafe {
        match op {
            Op::Copy => run1(s, ins, imms, |a, _, _| a),
            Op::Not => run1(s, ins, imms, |a, _, m| !a & m),
            Op::And => run2(s, ins, imms, |a, b, _, _| a & b),
            Op::Or => run2(s, ins, imms, |a, b, _, _| a | b),
            Op::Xor => run2(s, ins, imms, |a, b, _, _| a ^ b),
            Op::Nand => run2(s, ins, imms, |a, b, _, m| !(a & b) & m),
            Op::Nor => run2(s, ins, imms, |a, b, _, m| !(a | b) & m),
            Op::Xnor => run2(s, ins, imms, |a, b, _, m| !(a ^ b) & m),
            Op::Mux => run3(s, ins, imms, |a, b, c, _| if a != 0 { c } else { b }),
            Op::Switch => run2(s, ins, imms, |a, b, _, _| if a != 0 { b } else { 0 }),
            Op::AddSum => run3(s, ins, imms, |a, b, c, m| {
                a.wrapping_add(b).wrapping_add(c) & m
            }),
            Op::AddCarry => run3(s, ins, imms, |a, b, c, w| {
                ((a as u128 + b as u128 + c as u128) >> w) as u64 & 1
            }),
            Op::Eq => run2(s, ins, imms, |a, b, _, _| (a == b) as u64),
            Op::LtU => run2(s, ins, imms, |a, b, _, _| (a < b) as u64),
            Op::ReduceOr => run1(s, ins, imms, |a, _, _| (a != 0) as u64),
            Op::ReduceAnd => run1(s, ins, imms, |a, _, m| (a == m) as u64),
            Op::ReduceXor => run1(s, ins, imms, |a, _, _| (a.count_ones() & 1) as u64),
            Op::Shr => run1(s, ins, imms, |a, i, m| (a >> i.b) & m),
            Op::Concat => run2(s, ins, imms, |a, b, i, _| a | (b << i.c)),
            Op::Sext => run1(s, ins, imms, |a, i, m| {
                if (a >> (i.b - 1)) & 1 == 1 {
                    (a | !mask(i.b as u8)) & m
                } else {
                    a
                }
            }),
            Op::Clock => {
                for i in ins {
                    let v = ((tick + i.c as u64) % i.a as u64) < i.b as u64;
                    *s.add(i.dst as usize) = v as u64;
                }
            }
            Op::MemRead => {
                for i in ins {
                    let m = env.mems.get_unchecked(i.b as usize);
                    let a = *s.add(i.a as usize) as usize & (m.len() - 1);
                    *s.add(i.dst as usize) = *m.get_unchecked(a);
                }
            }
            Op::SrNext => run3(s, ins, imms, |a, b, c, _| {
                if b != 0 {
                    0
                } else if a != 0 {
                    1
                } else {
                    c
                }
            }),
        }
    }
}

#[inline(always)]
unsafe fn run1(s: *mut u64, ins: &[Instr], imms: &[u64], f: impl Fn(u64, &Instr, u64) -> u64) {
    for (k, i) in ins.iter().enumerate() {
        unsafe { *s.add(i.dst as usize) = f(*s.add(i.a as usize), i, *imms.get_unchecked(k)) };
    }
}

#[inline(always)]
unsafe fn run2(s: *mut u64, ins: &[Instr], imms: &[u64], f: impl Fn(u64, u64, &Instr, u64) -> u64) {
    for (k, i) in ins.iter().enumerate() {
        unsafe {
            *s.add(i.dst as usize) = f(
                *s.add(i.a as usize),
                *s.add(i.b as usize),
                i,
                *imms.get_unchecked(k),
            )
        };
    }
}

#[inline(always)]
unsafe fn run3(s: *mut u64, ins: &[Instr], imms: &[u64], f: impl Fn(u64, u64, u64, u64) -> u64) {
    for (k, i) in ins.iter().enumerate() {
        unsafe {
            *s.add(i.dst as usize) = f(
                *s.add(i.a as usize),
                *s.add(i.b as usize),
                *s.add(i.c as usize),
                *imms.get_unchecked(k),
            )
        };
    }
}
