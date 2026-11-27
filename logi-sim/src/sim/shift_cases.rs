//! Shift-op cases (issue #117) shared by the interpreter's tests
//! (`sim/tests.rs`) and the JIT's (`jit/tests.rs`, which also run on wasm
//! with the feature `selftest`).

use std::sync::Arc;

use super::{Sim, SimOptions};
use crate::compile::{Op, Options, Program, compile};
use crate::netlist::Design;

/// A program of one instruction `y = op(a, b)` over two 64-bit inputs: a
/// compiled `a & b` with its op, literal fields and immediate replaced.
/// For `Op::Shl`, `b` and `c` are the literal shifts (the `b` input is
/// unused); for `Op::ShlV`, `b` is the amount input.
pub(crate) fn shift_program(op: Op, b: u32, c: u32, imm: u64) -> Arc<Program> {
    static BASE: std::sync::OnceLock<Program> = std::sync::OnceLock::new();
    let mut p = BASE
        .get_or_init(|| {
            let mut d = Design::new("top");
            let r = d.root;
            let mut bb = d.builder(r);
            let x = bb.input("a", 64);
            let y = bb.input("b", 64);
            let z = bb.and(x, y);
            bb.output("y", z);
            compile(&d, &Options::default()).unwrap()
        })
        .clone();
    assert_eq!(p.instrs.len(), 1);
    assert_eq!(p.blocks.len(), 1);
    p.blocks[0].op = op;
    p.imms[0] = imm;
    if op == Op::Shl {
        p.instrs[0].b = b;
        p.instrs[0].c = c;
    }
    Arc::new(p)
}

/// `Op::Shl`: `((a >> c) << b) & imm`.
pub(crate) fn shl_model(a: u64, b: u32, c: u32, imm: u64) -> u64 {
    ((a >> c) << b) & imm
}

/// `Op::ShlV`: `(a << (b & 63)) & imm`, written without the mask: a shift
/// by `b mod 64`.
pub(crate) fn shlv_model(a: u64, b: u64, imm: u64) -> u64 {
    a.checked_shl((b % 64) as u32).unwrap_or(0) & imm
}

/// The runtimes a shift op runs under: streamed and plain blocks, and (in
/// the JIT tests) native code.
pub(crate) fn shift_sims(p: &Arc<Program>, jit: bool) -> Vec<Sim> {
    let mut out = vec![
        Sim::with_options(Arc::clone(p), SimOptions::default()),
        Sim::with_options(Arc::clone(p), SimOptions::baseline()),
    ];
    if jit {
        let s = Sim::with_options(
            Arc::clone(p),
            SimOptions {
                jit: true,
                ..SimOptions::default()
            },
        );
        assert!(s.jit_active(), "the JIT compiled the shift");
        out = vec![s];
    }
    out
}

/// Every runtime of `p` on inputs `(a, b)` gives `want`.
pub(crate) fn check_shift(sims: &mut [Sim], a: u64, b: u64, want: u64, what: &str) {
    for (k, s) in sims.iter_mut().enumerate() {
        s.set_input(0, a);
        s.set_input(1, b);
        s.tick();
        assert_eq!(s.output(0), want, "{what}: a {a:#x} b {b:#x} (runtime {k})");
    }
}

/// Literal shifts and masks for the shift tests: every small value, the
/// top of the range, and run masks at every offset.
pub(crate) fn shift_cases(rng: &mut crate::testkit::rng::Rng, all: bool) -> Vec<(u32, u32, u64)> {
    let shifts: Vec<u32> = if all {
        (0..64).collect()
    } else {
        vec![0, 1, 2, 3, 7, 8, 15, 31, 32, 33, 62, 63]
    };
    let mut out = Vec::new();
    for &b in &shifts {
        for &c in &shifts {
            let w = 1 + rng.below(64) as u8;
            for imm in [
                u64::MAX,
                crate::netlist::mask(w),
                crate::netlist::mask(w) & (u64::MAX << b),
                rng.next_u64(),
            ] {
                out.push((b, c, imm));
            }
        }
    }
    out
}

pub(crate) fn shl_semantics(jit: bool) {
    let mut rng = crate::testkit::rng::Rng::new(117);
    for (b, c, imm) in shift_cases(&mut rng, !jit) {
        let p = shift_program(Op::Shl, b, c, imm);
        let mut sims = shift_sims(&p, jit);
        let what = format!("Shl b {b} c {c} imm {imm:#x}");
        // Every 8-bit operand (small widths exhaustively), then random
        // 64-bit words.
        let small: Vec<u64> = if jit || (b < 9 && c < 9) {
            (0..256).collect()
        } else {
            vec![0, 1, 0x80, 0xff]
        };
        for a in small {
            check_shift(&mut sims, a, 0, shl_model(a, b, c, imm), &what);
        }
        for _ in 0..8 {
            let a = rng.next_u64();
            check_shift(&mut sims, a, rng.next_u64(), shl_model(a, b, c, imm), &what);
        }
    }
}

pub(crate) fn shlv_semantics(jit: bool) {
    let mut rng = crate::testkit::rng::Rng::new(118);
    for imm in [u64::MAX, 0xff, 0xffff, 0xff00, 1, rng.next_u64()] {
        let p = shift_program(Op::ShlV, 0, 0, imm);
        let mut sims = shift_sims(&p, jit);
        let what = format!("ShlV imm {imm:#x}");
        // Small operands and every amount up to past 128 (the amount is
        // taken modulo 64), exhaustively.
        for a in 0..64u64 {
            for b in 0..140u64 {
                check_shift(&mut sims, a, b, shlv_model(a, b, imm), &what);
            }
        }
        for _ in 0..2000 {
            let (a, b) = (rng.next_u64(), rng.next_u64());
            let b = if rng.coin() { b % 70 } else { b };
            check_shift(&mut sims, a, b, shlv_model(a, b, imm), &what);
        }
    }
}
