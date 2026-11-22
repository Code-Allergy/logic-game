//! Runtime for compiled programs (design doc section 6).
//!
//! [`Sim`] implements the execution model documented in
//! [`crate::compile::program`]: commit, advance the tick, apply staged
//! inputs, settle by running the blocks in order. Dispatch happens once per
//! block; each block's inner loop is a pass over a contiguous slice of
//! instructions with one opcode.
//!
//! `tick()` allocates nothing and uses no locks or atomics, and the code is
//! the same on native and wasm. Every index in the program is checked once in
//! [`Sim::new`], so the inner loops can skip bounds checks.

#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::sync::Arc;

use crate::compile::{Commit, Instr, NodePath, Op, Program, StateLoc, ZERO_SLOT};
use crate::netlist::{NetId, NodeId, mask};

/// Index of a root port, in port order.
pub type PortId = usize;

/// A program index is out of range. Programs from [`crate::compile::compile`]
/// never produce this.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BadProgram(pub String);

/// State of every state element, keyed by its node path, plus the tick count.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StateSnapshot {
    pub tick: u64,
    pub words: HashMap<NodePath, u64>,
    /// RAM contents with their geometry `(addr_bits, width)`.
    pub mems: HashMap<NodePath, (u8, u8, Vec<u64>)>,
}

pub struct Sim {
    program: Arc<Program>,
    slots: Vec<u64>,
    mems: Vec<Vec<u64>>,
    tick: u64,
    /// Root input values, applied to their slots at the start of each settle.
    inputs: Vec<u64>,
}

impl Sim {
    /// Load the program and perform the tick-0 settle. Panics if the program
    /// is malformed; see [`Sim::try_new`].
    #[track_caller]
    pub fn new(program: Arc<Program>) -> Self {
        Self::try_new(program).unwrap_or_else(|e| panic!("bad program: {}", e.0))
    }

    pub fn try_new(program: Arc<Program>) -> Result<Self, BadProgram> {
        check(&program)?;
        let mems = program
            .mems
            .iter()
            .map(|m| {
                let mut v = vec![0u64; 1usize << m.addr_bits];
                v[..m.init.len()].copy_from_slice(&m.init);
                v
            })
            .collect();
        let mut sim = Sim {
            slots: vec![0; program.slot_count as usize],
            mems,
            tick: 0,
            inputs: vec![0; program.ports.inputs.len()],
            program,
        };
        for &(s, v) in &sim.program.consts {
            sim.slots[s as usize] = v;
        }
        sim.settle();
        Ok(sim)
    }

    pub fn program(&self) -> &Arc<Program> {
        &self.program
    }

    /// Set a root input. It is visible to logic at the next settle (the next
    /// [`Self::tick`]) and to state at the commit after that.
    pub fn set_input(&mut self, port: PortId, value: u64) {
        self.inputs[port] = value & mask(self.program.ports.inputs[port].width);
    }

    /// Set a root input by name. Panics if there is no such port.
    #[track_caller]
    pub fn set(&mut self, name: &str, value: u64) {
        let port = self
            .program
            .ports
            .input(name)
            .unwrap_or_else(|| panic!("no input port {name:?}"));
        self.set_input(port, value);
    }

    /// Value of root output `port` after the last settle.
    pub fn output(&self, port: PortId) -> u64 {
        self.slots[self.program.ports.outputs[port].slot as usize]
    }

    /// Root output by name. Panics if there is no such port.
    #[track_caller]
    pub fn get(&self, name: &str) -> u64 {
        let port = self
            .program
            .ports
            .output(name)
            .unwrap_or_else(|| panic!("no output port {name:?}"));
        self.output(port)
    }

    /// Raw slot value (see [`crate::compile::DebugMap`]).
    pub fn read(&self, slot: u32) -> u64 {
        self.slots[slot as usize]
    }

    /// Value of an authored net, by instance path and `NetId`.
    pub fn read_net(&self, path: &[NodeId], net: NetId) -> Option<u64> {
        self.program.debug.slot(path, net).map(|s| self.read(s))
    }

    /// Contents of memory `index` (see [`crate::compile::MemDesc`]).
    pub fn mem(&self, index: u32) -> &[u64] {
        &self.mems[index as usize]
    }

    pub fn tick_count(&self) -> u64 {
        self.tick
    }

    pub fn tick(&mut self) {
        self.commit();
        self.tick += 1;
        self.settle();
    }

    pub fn run(&mut self, ticks: u64) {
        for _ in 0..ticks {
            self.tick();
        }
    }

    fn commit(&mut self) {
        let p = &*self.program;
        for c in &p.commits {
            match *c {
                Commit::Copy { state, next } => {
                    // SAFETY: indices checked in `check`.
                    unsafe {
                        let v = *self.slots.get_unchecked(next as usize);
                        *self.slots.get_unchecked_mut(state as usize) = v;
                    }
                }
                Commit::RamWrite { mem, we, addr, data } => unsafe {
                    if *self.slots.get_unchecked(we as usize) != 0 {
                        let m = self.mems.get_unchecked_mut(mem as usize);
                        let a = *self.slots.get_unchecked(addr as usize) as usize & (m.len() - 1);
                        *m.get_unchecked_mut(a) = *self.slots.get_unchecked(data as usize);
                    }
                },
            }
        }
    }

    fn settle(&mut self) {
        let p = &*self.program;
        for (port, &v) in p.ports.inputs.iter().zip(&self.inputs) {
            self.slots[port.slot as usize] = v;
        }
        let s = self.slots.as_mut_ptr();
        let tick = self.tick;
        for b in &p.blocks {
            let ins = &p.instrs[b.start as usize..(b.start + b.len) as usize];
            // SAFETY: every slot index in `ins` was checked against
            // `slot_count` in `check`, and `slots` has that length. Reads and
            // writes go through the raw pointer so `dst` may equal no operand
            // (it never does in a compiled program, but it would be sound).
            unsafe {
                match b.op {
                    Op::Copy => run1(s, ins, |a, _| a),
                    Op::Not => run1(s, ins, |a, i| !a & i.imm),
                    Op::And => run2(s, ins, |a, b, _| a & b),
                    Op::Or => run2(s, ins, |a, b, _| a | b),
                    Op::Xor => run2(s, ins, |a, b, _| a ^ b),
                    Op::Nand => run2(s, ins, |a, b, i| !(a & b) & i.imm),
                    Op::Nor => run2(s, ins, |a, b, i| !(a | b) & i.imm),
                    Op::Xnor => run2(s, ins, |a, b, i| !(a ^ b) & i.imm),
                    Op::Mux => run3(s, ins, |a, b, c, _| if a != 0 { c } else { b }),
                    Op::Switch => run2(s, ins, |a, b, _| if a != 0 { b } else { 0 }),
                    Op::AddSum => run3(s, ins, |a, b, c, i| a.wrapping_add(b).wrapping_add(c) & i.imm),
                    Op::AddCarry => run3(s, ins, |a, b, c, i| {
                        ((a as u128 + b as u128 + c as u128) >> i.imm) as u64 & 1
                    }),
                    Op::Eq => run2(s, ins, |a, b, _| (a == b) as u64),
                    Op::LtU => run2(s, ins, |a, b, _| (a < b) as u64),
                    Op::ReduceOr => run1(s, ins, |a, _| (a != 0) as u64),
                    Op::ReduceAnd => run1(s, ins, |a, i| (a == i.imm) as u64),
                    Op::ReduceXor => run1(s, ins, |a, _| (a.count_ones() & 1) as u64),
                    Op::Shr => run1(s, ins, |a, i| (a >> i.b) & i.imm),
                    Op::Concat => run2(s, ins, |a, b, i| a | (b << i.c)),
                    Op::Sext => run1(s, ins, |a, i| {
                        if (a >> (i.b - 1)) & 1 == 1 { (a | !mask(i.b as u8)) & i.imm } else { a }
                    }),
                    Op::Clock => {
                        for i in ins {
                            let v = ((tick + i.c as u64) % i.a as u64) < i.b as u64;
                            *s.add(i.dst as usize) = v as u64;
                        }
                    }
                    Op::MemRead => {
                        for i in ins {
                            let m = self.mems.get_unchecked(i.b as usize);
                            let a = *s.add(i.a as usize) as usize & (m.len() - 1);
                            *s.add(i.dst as usize) = *m.get_unchecked(a);
                        }
                    }
                    Op::SrNext => run3(s, ins, |a, b, c, _| {
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
        debug_assert_eq!(self.slots[ZERO_SLOT as usize], 0);
    }

    // ── State carry-over ──────────────────────────────────────────────────

    /// Snapshot every state element, keyed by node path.
    pub fn export_state(&self) -> StateSnapshot {
        let mut snap = StateSnapshot { tick: self.tick, ..Default::default() };
        for key in &self.program.state_keys {
            match key.loc {
                StateLoc::Slot(s) => {
                    snap.words.insert(key.path.clone(), self.slots[s as usize]);
                }
                StateLoc::Mem(m) => {
                    let d = &self.program.mems[m as usize];
                    snap.mems
                        .insert(key.path.clone(), (d.addr_bits, d.width, self.mems[m as usize].clone()));
                }
            }
        }
        snap
    }

    /// Restore state from a snapshot (typically taken from a previous
    /// program), then re-settle so every net is consistent. Elements whose
    /// path is not in the snapshot, or whose RAM geometry changed, reset.
    /// Word values are masked to the current width. The tick count is
    /// restored.
    pub fn import_state(&mut self, snap: &StateSnapshot) {
        let program = Arc::clone(&self.program);
        for key in &program.state_keys {
            match key.loc {
                StateLoc::Slot(s) => {
                    self.slots[s as usize] =
                        snap.words.get(&key.path).map_or(0, |&v| v & mask(key.width));
                }
                StateLoc::Mem(m) => {
                    let d = &program.mems[m as usize];
                    let mem = &mut self.mems[m as usize];
                    match snap.mems.get(&key.path) {
                        Some((ab, w, data)) if *ab == d.addr_bits && *w == d.width => {
                            mem.copy_from_slice(data);
                        }
                        _ => {
                            mem.fill(0);
                            mem[..d.init.len()].copy_from_slice(&d.init);
                        }
                    }
                }
            }
        }
        self.tick = snap.tick;
        self.settle();
    }
}

#[inline(always)]
unsafe fn run1(s: *mut u64, ins: &[Instr], f: impl Fn(u64, &Instr) -> u64) {
    for i in ins {
        unsafe { *s.add(i.dst as usize) = f(*s.add(i.a as usize), i) };
    }
}

#[inline(always)]
unsafe fn run2(s: *mut u64, ins: &[Instr], f: impl Fn(u64, u64, &Instr) -> u64) {
    for i in ins {
        unsafe { *s.add(i.dst as usize) = f(*s.add(i.a as usize), *s.add(i.b as usize), i) };
    }
}

#[inline(always)]
unsafe fn run3(s: *mut u64, ins: &[Instr], f: impl Fn(u64, u64, u64, &Instr) -> u64) {
    for i in ins {
        unsafe {
            *s.add(i.dst as usize) =
                f(*s.add(i.a as usize), *s.add(i.b as usize), *s.add(i.c as usize), i)
        };
    }
}

/// Check every index the unchecked loops rely on.
fn check(p: &Program) -> Result<(), BadProgram> {
    let n = p.slot_count;
    let bad = |m: String| Err(BadProgram(m));
    if n == 0 {
        return bad("no zero slot".into());
    }
    let slot_ok = |s: u32| s < n;
    for b in &p.blocks {
        let end = b.start as u64 + b.len as u64;
        if end > p.instrs.len() as u64 {
            return bad(format!("block {b:?} out of range"));
        }
        let (sa, sb, sc) = b.op.slot_operands();
        for i in &p.instrs[b.start as usize..end as usize] {
            if !slot_ok(i.dst) || i.dst == ZERO_SLOT {
                return bad(format!("bad dst in {i:?}"));
            }
            if (sa && !slot_ok(i.a)) || (sb && !slot_ok(i.b)) || (sc && !slot_ok(i.c)) {
                return bad(format!("bad operand in {i:?}"));
            }
            match b.op {
                Op::MemRead if i.b as usize >= p.mems.len() => {
                    return bad(format!("bad memory in {i:?}"));
                }
                Op::Clock if i.a == 0 => return bad(format!("zero clock period in {i:?}")),
                Op::Sext if !(1..=64).contains(&i.b) => return bad(format!("bad sext in {i:?}")),
                Op::Shr if i.b >= 64 => return bad(format!("bad shift in {i:?}")),
                Op::Concat if i.c >= 64 => return bad(format!("bad shift in {i:?}")),
                Op::AddCarry if i.imm > 64 => return bad(format!("bad width in {i:?}")),
                _ => {}
            }
        }
    }
    for c in &p.commits {
        let ok = match *c {
            Commit::Copy { state, next } => slot_ok(state) && slot_ok(next) && state != ZERO_SLOT,
            Commit::RamWrite { mem, we, addr, data } => {
                (mem as usize) < p.mems.len()
                    && p.mems[mem as usize].writable
                    && slot_ok(we)
                    && slot_ok(addr)
                    && slot_ok(data)
            }
        };
        if !ok {
            return bad(format!("bad commit {c:?}"));
        }
    }
    for m in &p.mems {
        if m.addr_bits > crate::netlist::MAX_ADDR_BITS || m.init.len() > 1usize << m.addr_bits {
            return bad(format!("bad memory {m:?}"));
        }
    }
    for &(s, _) in &p.consts {
        if !slot_ok(s) || s == ZERO_SLOT {
            return bad(format!("bad const slot {s}"));
        }
    }
    for port in &p.ports.inputs {
        if !slot_ok(port.slot) || port.slot == ZERO_SLOT {
            return bad(format!("bad port {port:?}"));
        }
    }
    for port in &p.ports.outputs {
        if !slot_ok(port.slot) {
            return bad(format!("bad port {port:?}"));
        }
    }
    for key in &p.state_keys {
        let ok = match key.loc {
            StateLoc::Slot(s) => slot_ok(s),
            StateLoc::Mem(m) => (m as usize) < p.mems.len(),
        };
        if !ok {
            return bad(format!("bad state key {key:?}"));
        }
    }
    Ok(())
}
