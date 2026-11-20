//! Test-only, deliberately plain executor for [`Program`], following the
//! execution model documented in `program.rs`. It lets M2 check emitted
//! programs against the reference interpreter before the real runtime (M3)
//! exists.

use super::program::*;
use crate::netlist::mask;

pub struct Exec<'p> {
    pub p: &'p Program,
    pub slots: Vec<u64>,
    pub mems: Vec<Vec<u64>>,
    pub tick: u64,
    staged: Vec<u64>,
}

impl<'p> Exec<'p> {
    pub fn new(p: &'p Program) -> Self {
        let mut slots = vec![0u64; p.slot_count as usize];
        for &(s, v) in &p.consts {
            slots[s as usize] = v;
        }
        let mems = p
            .mems
            .iter()
            .map(|m| {
                let mut v = vec![0u64; 1usize << m.addr_bits];
                v[..m.init.len()].copy_from_slice(&m.init);
                v
            })
            .collect();
        let mut x = Exec { p, slots, mems, tick: 0, staged: vec![0; p.ports.inputs.len()] };
        x.settle();
        x
    }

    pub fn set_input(&mut self, port: usize, v: u64) {
        self.staged[port] = v & mask(self.p.ports.inputs[port].width);
    }

    pub fn output(&self, port: usize) -> u64 {
        self.slots[self.p.ports.outputs[port].slot as usize]
    }

    pub fn tick(&mut self) {
        for c in &self.p.commits {
            match *c {
                Commit::Copy { state, next } => self.slots[state as usize] = self.slots[next as usize],
                Commit::RamWrite { mem, we, addr, data } => {
                    if self.slots[we as usize] != 0 {
                        let a = self.slots[addr as usize] as usize;
                        self.mems[mem as usize][a] = self.slots[data as usize];
                    }
                }
            }
        }
        self.tick += 1;
        self.settle();
    }

    fn settle(&mut self) {
        for (port, &v) in self.p.ports.inputs.iter().zip(&self.staged) {
            self.slots[port.slot as usize] = v;
        }
        assert_eq!(self.slots[ZERO_SLOT as usize], 0);
        for b in &self.p.blocks {
            for ins in &self.p.instrs[b.start as usize..(b.start + b.len) as usize] {
                let s = &self.slots;
                let (a, bb, c) = (|| s[ins.a as usize], || s[ins.b as usize], || s[ins.c as usize]);
                let v = match b.op {
                    Op::Copy => a(),
                    Op::Not => !a() & ins.imm,
                    Op::And => a() & bb(),
                    Op::Or => a() | bb(),
                    Op::Xor => a() ^ bb(),
                    Op::Nand => !(a() & bb()) & ins.imm,
                    Op::Nor => !(a() | bb()) & ins.imm,
                    Op::Xnor => !(a() ^ bb()) & ins.imm,
                    Op::Mux => if a() != 0 { c() } else { bb() },
                    Op::Switch => if a() != 0 { bb() } else { 0 },
                    Op::AddSum => a().wrapping_add(bb()).wrapping_add(c()) & ins.imm,
                    Op::AddCarry => {
                        ((a() as u128 + bb() as u128 + c() as u128) >> ins.imm) as u64 & 1
                    }
                    Op::Eq => (a() == bb()) as u64,
                    Op::LtU => (a() < bb()) as u64,
                    Op::ReduceOr => (a() != 0) as u64,
                    Op::ReduceAnd => (a() == ins.imm) as u64,
                    Op::ReduceXor => (a().count_ones() & 1) as u64,
                    Op::Shr => (a() >> ins.b) & ins.imm,
                    Op::Concat => a() | (bb() << ins.c),
                    Op::Sext => {
                        let x = a();
                        if (x >> (ins.b - 1)) & 1 == 1 { (x | !mask(ins.b as u8)) & ins.imm } else { x }
                    }
                    Op::Clock => {
                        (((self.tick + ins.c as u64) % ins.a as u64) < ins.b as u64) as u64
                    }
                    Op::MemRead => self.mems[ins.b as usize][a() as usize],
                    Op::SrNext => if bb() != 0 { 0 } else if a() != 0 { 1 } else { c() },
                };
                self.slots[ins.dst as usize] = v;
            }
        }
    }
}
