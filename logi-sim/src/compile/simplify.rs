//! Pass 3: simplification (design doc section 5, issue #26).
//!
//! Runs on the lowered instruction list, before levelizing. One pass in
//! topological order applies, per instruction and as enabled in
//! [`Options`]:
//!
//! - **Constant folding** (`const_fold`): evaluate instructions whose operands
//!   are all constant (including ROM reads at a constant address), and
//!   identities involving constants: `x & 0`, `x | 0`, `x ^ 0`, `x & mask`,
//!   `x ^ x`, `x == x`, mux/switch with a constant select, adds of zero.
//! - **Alias elimination** (`alias`): copies, `x & x`, `x | x`, a mux whose
//!   two inputs are the same, and full-width slices. The result's readers
//!   (and its authored nets) use the source slot directly.
//! - **Common-subexpression merging** (`cse`): instructions with the same op,
//!   operands (commutative operands sorted) and immediate share one slot.
//!
//! Then, unless `debug` is set, **dead-code elimination** (`dce`) keeps only
//! instructions feeding root outputs, state (commits) and `probes`. Nets that
//! no longer have a value are dropped from the debug map.
//!
//! One invariant matters for state: a slot read by a commit must not become
//! an alias of a slot written by a commit, because commits run in sequence.
//! Such an instruction is kept as a copy instead.

use std::collections::HashMap;

use super::Options;
use super::emit::Emitter;
use super::program::*;
use crate::netlist::mask;

pub(super) fn simplify(
    em: &mut Emitter,
    order: &[u32],
    ports: &mut PortMap,
    debug: &mut DebugMap,
    probe_slots: &[u32],
    opts: &Options,
) {
    let n_slots = em.widths.len();
    let mut repl: Vec<u32> = (0..n_slots as u32).collect();
    let mut cval: Vec<Option<u64>> = vec![None; n_slots];
    cval[ZERO_SLOT as usize] = Some(0);
    for &(s, v) in &em.consts {
        cval[s as usize] = Some(v);
    }
    let mut commit_written = vec![false; n_slots];
    let mut commit_read = vec![false; n_slots];
    for c in &em.commits {
        match *c {
            Commit::Copy { state, next } => {
                commit_written[state as usize] = true;
                commit_read[next as usize] = true;
            }
            Commit::RamWrite { we, addr, data, .. } => {
                for s in [we, addr, data] {
                    commit_read[s as usize] = true;
                }
            }
        }
    }

    let resolve = |repl: &mut Vec<u32>, mut s: u32| {
        while repl[s as usize] != s {
            let p = repl[s as usize];
            repl[s as usize] = repl[p as usize];
            s = p;
        }
        s
    };

    let mut keep = vec![true; em.instrs.len()];
    let mut seen: HashMap<(Op, u32, u32, u32, u64), u32> = HashMap::new();

    for &k in order {
        let k = k as usize;
        let op = em.ops[k];
        let (sa, sb, sc) = op.slot_operands();
        let mut ins = em.instrs[k];
        if sa {
            ins.a = resolve(&mut repl, ins.a);
        }
        if sb {
            ins.b = resolve(&mut repl, ins.b);
        }
        if sc {
            ins.c = resolve(&mut repl, ins.c);
        }
        em.instrs[k] = ins;
        let imm = em.imms[k];
        let dst = ins.dst;
        let w = em.widths[dst as usize];
        let c = |s: u32| cval[s as usize];

        let mut outcome = Outcome::Keep;

        if opts.const_fold {
            let all_const = (!sa || c(ins.a).is_some())
                && (!sb || c(ins.b).is_some())
                && (!sc || c(ins.c).is_some());
            if all_const && let Some(v) = fold(op, &ins, imm, &c, &em.mems) {
                outcome = Outcome::Const(v);
            } else {
                outcome = const_identity(op, &ins, w, &c);
            }
        }
        if matches!(outcome, Outcome::Keep) && opts.alias {
            outcome = alias_identity(op, &ins, imm, &em.widths);
        }
        if matches!(outcome, Outcome::Keep) && opts.cse && op != Op::Clock {
            let (mut a, mut b) = (ins.a, ins.b);
            let commutative = matches!(
                op,
                Op::And
                    | Op::Or
                    | Op::Xor
                    | Op::Nand
                    | Op::Nor
                    | Op::Xnor
                    | Op::Eq
                    | Op::AddSum
                    | Op::AddCarry
            );
            if commutative && a > b {
                std::mem::swap(&mut a, &mut b);
            }
            let key = (op, a, b, ins.c, imm);
            match seen.get(&key) {
                Some(&prev) => outcome = Outcome::Alias(prev),
                None => {
                    seen.insert(key, dst);
                }
            }
        }

        match outcome {
            Outcome::Keep => {}
            Outcome::Const(v) => {
                let v = v & mask(w);
                cval[dst as usize] = Some(v);
                em.consts.push((dst, v));
                keep[k] = false;
            }
            Outcome::Alias(t) => {
                if commit_read[dst as usize] && commit_written[t as usize] {
                    // Keep the value in its own slot (see module docs).
                    em.ops[k] = Op::Copy;
                    em.instrs[k] = Instr {
                        dst,
                        a: t,
                        b: ZERO_SLOT,
                        c: ZERO_SLOT,
                    };
                    em.imms[k] = 0;
                } else {
                    repl[dst as usize] = t;
                    cval[dst as usize] = cval[t as usize];
                    keep[k] = false;
                }
            }
        }
    }

    // Apply replacements everywhere else.
    for c in &mut em.commits {
        *c = match *c {
            Commit::Copy { state, next } => Commit::Copy {
                state,
                next: resolve(&mut repl, next),
            },
            Commit::RamWrite {
                mem,
                we,
                addr,
                data,
            } => Commit::RamWrite {
                mem,
                we: resolve(&mut repl, we),
                addr: resolve(&mut repl, addr),
                data: resolve(&mut repl, data),
            },
        };
    }
    for p in &mut ports.outputs {
        p.slot = resolve(&mut repl, p.slot);
    }
    for map in &mut debug.net_slots {
        for (_, s) in map.iter_mut() {
            *s = resolve(&mut repl, *s);
        }
    }
    let probes: Vec<u32> = probe_slots.iter().map(|&s| resolve(&mut repl, s)).collect();

    // Dead-code elimination.
    if opts.dce && !opts.debug {
        let mut producer = vec![u32::MAX; n_slots];
        for (k, ins) in em.instrs.iter().enumerate() {
            if keep[k] {
                producer[ins.dst as usize] = k as u32;
            }
        }
        let mut live_slot = vec![false; n_slots];
        let mut stack: Vec<u32> = Vec::new();
        let mark = |s: u32, live_slot: &mut Vec<bool>, stack: &mut Vec<u32>| {
            if !live_slot[s as usize] {
                live_slot[s as usize] = true;
                stack.push(s);
            }
        };
        for p in &ports.outputs {
            mark(p.slot, &mut live_slot, &mut stack);
        }
        for c in &em.commits {
            match *c {
                Commit::Copy { next, .. } => mark(next, &mut live_slot, &mut stack),
                Commit::RamWrite { we, addr, data, .. } => {
                    for s in [we, addr, data] {
                        mark(s, &mut live_slot, &mut stack);
                    }
                }
            }
        }
        for &s in &probes {
            mark(s, &mut live_slot, &mut stack);
        }
        while let Some(s) = stack.pop() {
            let p = producer[s as usize];
            if p == u32::MAX {
                continue;
            }
            let ins = em.instrs[p as usize];
            let (sa, sb, sc) = em.ops[p as usize].slot_operands();
            for (is, x) in [(sa, ins.a), (sb, ins.b), (sc, ins.c)] {
                if is {
                    mark(x, &mut live_slot, &mut stack);
                }
            }
        }
        for (k, ins) in em.instrs.iter().enumerate() {
            if keep[k] && !live_slot[ins.dst as usize] {
                keep[k] = false;
            }
        }
        em.consts.retain(|&(s, _)| live_slot[s as usize]);

        // Authored nets keep a debug entry only if their slot still has a
        // value: zero, an input, a live constant, state, or a live result.
        let mut has_value = vec![false; n_slots];
        has_value[ZERO_SLOT as usize] = true;
        for p in &ports.inputs {
            has_value[p.slot as usize] = true;
        }
        for &(s, _) in &em.consts {
            has_value[s as usize] = true;
        }
        for (s, &w) in commit_written.iter().enumerate() {
            has_value[s] |= w;
        }
        for (k, ins) in em.instrs.iter().enumerate() {
            if keep[k] {
                has_value[ins.dst as usize] = true;
            }
        }
        for map in &mut debug.net_slots {
            map.retain(|_, s| has_value[*s as usize]);
        }
    }

    // Compact the instruction list.
    let mut w = 0;
    for k in 0..em.instrs.len() {
        if keep[k] {
            em.instrs[w] = em.instrs[k];
            em.imms[w] = em.imms[k];
            em.ops[w] = em.ops[k];
            w += 1;
        }
    }
    em.instrs.truncate(w);
    em.imms.truncate(w);
    em.ops.truncate(w);
}

/// Evaluate an instruction with constant operands. `None` for ops that are
/// not constant even then (clocks, RAM reads).
fn fold(
    op: Op,
    i: &Instr,
    imm: u64,
    c: &dyn Fn(u32) -> Option<u64>,
    mems: &[MemDesc],
) -> Option<u64> {
    let a = || c(i.a).unwrap_or(0);
    let b = || c(i.b).unwrap_or(0);
    let cc = || c(i.c).unwrap_or(0);
    Some(match op {
        Op::Copy => a(),
        Op::Not => !a() & imm,
        Op::And => a() & b(),
        Op::Or => a() | b(),
        Op::Xor => a() ^ b(),
        Op::Nand => !(a() & b()) & imm,
        Op::Nor => !(a() | b()) & imm,
        Op::Xnor => !(a() ^ b()) & imm,
        Op::Mux => {
            if a() != 0 {
                cc()
            } else {
                b()
            }
        }
        Op::Switch => {
            if a() != 0 {
                b()
            } else {
                0
            }
        }
        Op::AddSum => a().wrapping_add(b()).wrapping_add(cc()) & imm,
        Op::AddCarry => ((a() as u128 + b() as u128 + cc() as u128) >> imm) as u64 & 1,
        Op::Eq => (a() == b()) as u64,
        Op::LtU => (a() < b()) as u64,
        Op::ReduceOr => (a() != 0) as u64,
        Op::ReduceAnd => (a() == imm) as u64,
        Op::ReduceXor => (a().count_ones() & 1) as u64,
        Op::Shr => (a() >> i.b) & imm,
        Op::Concat => a() | (b() << i.c),
        Op::Sext => {
            let x = a();
            if (x >> (i.b - 1)) & 1 == 1 {
                (x | !mask(i.b as u8)) & imm
            } else {
                x
            }
        }
        Op::MemRead => {
            let m = &mems[i.b as usize];
            if m.writable {
                return None;
            }
            let addr = a() as usize & ((1usize << m.addr_bits) - 1);
            m.init.get(addr).copied().unwrap_or(0)
        }
        Op::SrNext => {
            if b() != 0 {
                0
            } else if a() != 0 {
                1
            } else {
                cc()
            }
        }
        Op::Clock => return None,
    })
}

/// What an instruction becomes.
enum Outcome {
    /// A constant value.
    Const(u64),
    /// The same value as another slot.
    Alias(u32),
    /// Itself.
    Keep,
}

/// Identities with a constant operand, or with a constant result.
fn const_identity(op: Op, i: &Instr, w: u8, c: &dyn Fn(u32) -> Option<u64>) -> Outcome {
    let m = mask(w);
    let (ca, cb, cc) = (c(i.a), c(i.b), c(i.c));
    match op {
        Op::And => match (ca, cb) {
            (Some(0), _) | (_, Some(0)) => Outcome::Const(0),
            (Some(x), _) if x == m => Outcome::Alias(i.b),
            (_, Some(x)) if x == m => Outcome::Alias(i.a),
            _ => Outcome::Keep,
        },
        Op::Or => match (ca, cb) {
            (Some(x), _) | (_, Some(x)) if x == m => Outcome::Const(m),
            (Some(0), _) => Outcome::Alias(i.b),
            (_, Some(0)) => Outcome::Alias(i.a),
            _ => Outcome::Keep,
        },
        Op::Xor => match (ca, cb) {
            (Some(0), _) => Outcome::Alias(i.b),
            (_, Some(0)) => Outcome::Alias(i.a),
            _ if i.a == i.b => Outcome::Const(0),
            _ => Outcome::Keep,
        },
        Op::Xnor if i.a == i.b => Outcome::Const(m),
        Op::Eq if i.a == i.b => Outcome::Const(1),
        Op::LtU if i.a == i.b => Outcome::Const(0),
        Op::Mux => match ca {
            Some(0) => Outcome::Alias(i.b),
            Some(_) => Outcome::Alias(i.c),
            None => Outcome::Keep,
        },
        Op::Switch => match ca {
            Some(0) => Outcome::Const(0),
            Some(_) => Outcome::Alias(i.b),
            None => Outcome::Keep,
        },
        Op::AddSum => match (ca, cb, cc) {
            (_, Some(0), Some(0)) => Outcome::Alias(i.a),
            (Some(0), _, Some(0)) => Outcome::Alias(i.b),
            _ => Outcome::Keep,
        },
        Op::AddCarry => match (ca, cb, cc) {
            (_, Some(0), Some(0)) | (Some(0), _, Some(0)) => Outcome::Const(0),
            _ => Outcome::Keep,
        },
        _ => Outcome::Keep,
    }
}

/// Copies and other identities that need no constant.
fn alias_identity(op: Op, i: &Instr, imm: u64, widths: &[u8]) -> Outcome {
    match op {
        Op::Copy => Outcome::Alias(i.a),
        Op::And | Op::Or if i.a == i.b => Outcome::Alias(i.a),
        Op::Mux if i.b == i.c => Outcome::Alias(i.b),
        Op::Shr if i.b == 0 && imm == mask(widths[i.a as usize]) => Outcome::Alias(i.a),
        _ => Outcome::Keep,
    }
}
