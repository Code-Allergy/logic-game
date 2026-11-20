//! Passes 5 and 6: slot assignment, lowering to instructions, levelizing and
//! block emission.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use slotmap::SecondaryMap;

use super::Options;
use super::elaborate::Elab;
use super::program::*;
use super::simplify::simplify;
use crate::netlist::{NetId, NodeKind, mask};

const UNASSIGNED: u32 = u32::MAX;

pub(super) struct Emitter {
    pub widths: Vec<u8>,
    pub ops: Vec<Op>,
    pub instrs: Vec<Instr>,
    pub imms: Vec<u64>,
    pub consts: Vec<(u32, u64)>,
    pub commits: Vec<Commit>,
    pub mems: Vec<MemDesc>,
    pub state_keys: Vec<StateKey>,
}

impl Emitter {
    fn temp(&mut self, w: u8) -> u32 {
        self.widths.push(w);
        (self.widths.len() - 1) as u32
    }

    fn push(&mut self, op: Op, dst: u32, a: u32, b: u32, c: u32, imm: u64) {
        self.ops.push(op);
        self.instrs.push(Instr { dst, a, b, c });
        self.imms.push(imm);
    }

    fn mem(&mut self, addr_bits: u8, width: u8, init: &[u64], writable: bool) -> u32 {
        let init: Arc<[u64]> = init.iter().map(|v| v & mask(width)).collect();
        self.mems.push(MemDesc {
            addr_bits,
            width,
            init,
            writable,
        });
        (self.mems.len() - 1) as u32
    }
}

pub(super) fn emit(mut e: Elab<'_>, opts: &Options) -> Program {
    let design = e.design;
    let n = e.n_nets as usize;

    // ── Slot assignment: one slot per driven class ─────────────────────────
    // A class is driven if it contains a primitive output or a root input.
    let mut driven = vec![false; n];
    for inst in 0..e.insts.len() as u32 {
        let def = &design.defs[e.def_of(inst)];
        for node in def.nodes.values() {
            let is_driver = match node.kind {
                NodeKind::Instance(_) | NodeKind::Output { .. } => false,
                NodeKind::Input { .. } => inst == 0,
                _ => true,
            };
            if is_driver {
                for &net in &node.outputs {
                    let r = e.find(e.global(inst, net));
                    driven[r as usize] = true;
                }
            }
        }
    }
    let mut class_slot = vec![UNASSIGNED; n];
    let mut widths: Vec<u8> = vec![1]; // ZERO_SLOT
    let root_def = design.root_def();
    let mut ports = PortMap::default();
    for &port in &root_def.inputs {
        let net = root_def.nodes[port].outputs[0];
        let r = e.find(e.global(0, net)) as usize;
        let w = root_def.nets[net].width;
        class_slot[r] = widths.len() as u32;
        widths.push(w);
        ports.inputs.push(Port {
            slot: class_slot[r],
            width: w,
            name: root_def.name_of(port).map(str::to_string),
            node: port,
        });
    }
    let mut net_slot = vec![ZERO_SLOT; n];
    for g in 0..n as u32 {
        let r = e.find(g) as usize;
        if !driven[r] {
            continue;
        }
        if class_slot[r] == UNASSIGNED {
            let (inst, net) = e.local(g);
            class_slot[r] = widths.len() as u32;
            widths.push(design.defs[e.def_of(inst)].nets[net].width);
        }
        net_slot[g as usize] = class_slot[r];
    }
    for &port in &root_def.outputs {
        let w = match root_def.nodes[port].kind {
            NodeKind::Output { w } => w,
            _ => unreachable!("validated"),
        };
        let slot = root_def.nodes[port].inputs[0]
            .map_or(ZERO_SLOT, |net| net_slot[e.global(0, net) as usize]);
        ports.outputs.push(Port {
            slot,
            width: w,
            name: root_def.name_of(port).map(str::to_string),
            node: port,
        });
    }

    // ── Debug map ──────────────────────────────────────────────────────────
    let mut debug = DebugMap {
        instances: e.insts.clone(),
        children: HashMap::new(),
        net_slots: Vec::with_capacity(e.insts.len()),
    };
    for (i, info) in e.insts.iter().enumerate() {
        if let Some((p, node)) = info.parent {
            debug.children.insert((p, node), i as u32);
        }
        let base = e.net_base[i] as usize;
        let map: SecondaryMap<NetId, u32> = e.def_nets[info.def]
            .iter()
            .enumerate()
            .map(|(k, &net)| (net, net_slot[base + k]))
            .collect();
        debug.net_slots.push(map);
    }

    // ── Lowering ───────────────────────────────────────────────────────────
    let mut em = Emitter {
        widths,
        ops: Vec::new(),
        instrs: Vec::new(),
        imms: Vec::new(),
        consts: Vec::new(),
        commits: Vec::new(),
        mems: Vec::new(),
        state_keys: Vec::new(),
    };
    for inst in 0..e.insts.len() as u32 {
        let def = &design.defs[e.def_of(inst)];
        let base = e.net_base[inst as usize] as usize;
        let index = &e.net_index[e.def_of(inst)];
        let slot = |net: NetId| net_slot[base + index[net] as usize];
        for (id, node) in &def.nodes {
            let i = |k: usize| node.inputs[k].map_or(ZERO_SLOT, slot);
            let o = |k: usize| slot(node.outputs[k]);
            let key = |loc, width| StateKey {
                path: NodePath {
                    instances: e.path(inst),
                    node: id,
                },
                loc,
                width,
            };
            lower(&mut em, &node.kind, &i, &o, key);
        }
    }

    // ── Simplify ───────────────────────────────────────────────────────────
    if opts.const_fold || opts.alias || opts.cse || (opts.dce && !opts.debug) {
        let probe_slots: Vec<u32> = opts
            .probes
            .iter()
            .filter_map(|(path, net)| debug.slot(path, *net))
            .collect();
        let (_, order) = levelize(&em.ops, &em.instrs, em.widths.len());
        simplify(&mut em, &order, &mut ports, &mut debug, &probe_slots, opts);
    }

    // ── Levelize and group into blocks ─────────────────────────────────────
    let Emitter {
        widths,
        ops,
        instrs,
        imms,
        consts,
        commits,
        mems,
        state_keys,
    } = em;
    let (ranks, order) = levelize(&ops, &instrs, widths.len());
    let mut sorted: Vec<u32> = order;
    sorted.sort_by_key(|&i| (ranks[i as usize], ops[i as usize], i));
    let mut blocks: Vec<Block> = Vec::new();
    let mut out = Vec::with_capacity(instrs.len());
    let mut out_imms = Vec::with_capacity(instrs.len());
    let mut out_ops = Vec::with_capacity(instrs.len());
    for &i in &sorted {
        let (op, rank) = (ops[i as usize], ranks[i as usize]);
        match blocks.last_mut() {
            Some(b) if b.op == op && b.rank == rank => b.len += 1,
            _ => blocks.push(Block {
                op,
                rank,
                start: out.len() as u32,
                len: 1,
            }),
        }
        out.push(instrs[i as usize]);
        out_imms.push(imms[i as usize]);
        out_ops.push(op);
    }

    let mut p = Program {
        slot_count: widths.len() as u32,
        consts,
        blocks,
        instrs: out,
        imms: out_imms,
        commits,
        mems,
        ports,
        debug,
        state_keys,
    };
    renumber(&mut p, &out_ops);
    p
}

/// Renumber slots densely in the order they are first produced: zero, root
/// inputs, constants, state, then instruction destinations in execution
/// order. Writes in a block become sequential, values produced close in time
/// sit close in memory, and unreferenced slots disappear.
fn renumber(p: &mut Program, ops: &[Op]) {
    let mut map = vec![UNASSIGNED; p.slot_count as usize];
    let mut next = 0u32;
    let mut assign = |s: u32, map: &mut Vec<u32>| {
        if map[s as usize] == UNASSIGNED {
            map[s as usize] = next;
            next += 1;
        }
    };
    assign(ZERO_SLOT, &mut map);
    for port in &p.ports.inputs {
        assign(port.slot, &mut map);
    }
    for &(s, _) in &p.consts {
        assign(s, &mut map);
    }
    for c in &p.commits {
        if let Commit::Copy { state, .. } = *c {
            assign(state, &mut map);
        }
    }
    for ins in &p.instrs {
        assign(ins.dst, &mut map);
    }
    // Anything still unassigned is read but never written: it can only be
    // the zero slot's class, which is already slot 0.
    let m = |s: u32| {
        let n = map[s as usize];
        assert_ne!(
            n, UNASSIGNED,
            "internal error: slot {s} read but never produced"
        );
        n
    };
    for (ins, &op) in p.instrs.iter_mut().zip(ops) {
        let (sa, sb, sc) = op.slot_operands();
        ins.dst = m(ins.dst);
        if sa {
            ins.a = m(ins.a);
        }
        if sb {
            ins.b = m(ins.b);
        }
        if sc {
            ins.c = m(ins.c);
        }
    }
    for (s, _) in &mut p.consts {
        *s = m(*s);
    }
    for c in &mut p.commits {
        *c = match *c {
            Commit::Copy { state, next } => Commit::Copy {
                state: m(state),
                next: m(next),
            },
            Commit::RamWrite {
                mem,
                we,
                addr,
                data,
            } => Commit::RamWrite {
                mem,
                we: m(we),
                addr: m(addr),
                data: m(data),
            },
        };
    }
    for port in p.ports.inputs.iter_mut().chain(p.ports.outputs.iter_mut()) {
        port.slot = m(port.slot);
    }
    for slots in &mut p.debug.net_slots {
        for (_, s) in slots.iter_mut() {
            *s = m(*s);
        }
    }
    for key in &mut p.state_keys {
        if let StateLoc::Slot(s) = &mut key.loc {
            *s = m(*s);
        }
    }
    p.slot_count = next;
}

fn lower(
    em: &mut Emitter,
    kind: &NodeKind,
    i: &dyn Fn(usize) -> u32,
    o: &dyn Fn(usize) -> u32,
    key: impl Fn(StateLoc, u8) -> StateKey,
) {
    use NodeKind as K;
    let z = ZERO_SLOT;
    match kind {
        K::Input { .. } | K::Output { .. } | K::Instance(_) => {}
        K::Const { w, value } => em.consts.push((o(0), value & mask(*w))),
        K::Not { w } => em.push(Op::Not, o(0), i(0), z, z, mask(*w)),
        K::And { .. } => em.push(Op::And, o(0), i(0), i(1), z, 0),
        K::Or { .. } => em.push(Op::Or, o(0), i(0), i(1), z, 0),
        K::Xor { .. } => em.push(Op::Xor, o(0), i(0), i(1), z, 0),
        K::Nand { w } => em.push(Op::Nand, o(0), i(0), i(1), z, mask(*w)),
        K::Nor { w } => em.push(Op::Nor, o(0), i(0), i(1), z, mask(*w)),
        K::Xnor { w } => em.push(Op::Xnor, o(0), i(0), i(1), z, mask(*w)),
        K::Mux { .. } => em.push(Op::Mux, o(0), i(0), i(1), i(2), 0),
        K::Switch { .. } => em.push(Op::Switch, o(0), i(0), i(1), z, 0),
        K::Add { w } => {
            em.push(Op::AddSum, o(0), i(0), i(1), i(2), mask(*w));
            em.push(Op::AddCarry, o(1), i(0), i(1), i(2), *w as u64);
        }
        K::Eq { .. } => em.push(Op::Eq, o(0), i(0), i(1), z, 0),
        K::LtU { .. } => em.push(Op::LtU, o(0), i(0), i(1), z, 0),
        K::ReduceOr { .. } => em.push(Op::ReduceOr, o(0), i(0), z, z, 0),
        K::ReduceAnd { w } => em.push(Op::ReduceAnd, o(0), i(0), z, z, mask(*w)),
        K::ReduceXor { .. } => em.push(Op::ReduceXor, o(0), i(0), z, z, 0),
        K::Split { w, lo, len } => {
            if *lo == 0 && len == w {
                em.push(Op::Copy, o(0), i(0), z, z, 0);
            } else {
                em.push(Op::Shr, o(0), i(0), *lo as u32, z, mask(*len));
            }
        }
        K::Merge { widths } => {
            if widths.len() == 1 {
                em.push(Op::Copy, o(0), i(0), z, z, 0);
            } else {
                let mut acc = i(0);
                let mut shift = widths[0] as u32;
                for k in 1..widths.len() {
                    let dst = if k + 1 == widths.len() {
                        o(0)
                    } else {
                        em.temp((shift + widths[k] as u32) as u8)
                    };
                    em.push(Op::Concat, dst, acc, i(k), shift, 0);
                    acc = dst;
                    shift += widths[k] as u32;
                }
            }
        }
        K::Extend { from, to, signed } => {
            if *signed && from != to {
                em.push(Op::Sext, o(0), i(0), *from as u32, z, mask(*to));
            } else {
                em.push(Op::Copy, o(0), i(0), z, z, 0);
            }
        }
        K::Clock {
            period,
            duty,
            phase,
        } => em.push(Op::Clock, o(0), *period, *duty, *phase, 0),
        K::Rom {
            addr_bits,
            w,
            contents,
        } => {
            let m = em.mem(*addr_bits, *w, contents, false);
            em.push(Op::MemRead, o(0), i(0), m, z, 0);
        }
        K::Delay { w } => {
            let next = em.temp(*w);
            em.push(Op::Copy, next, i(0), z, z, 0);
            em.commits.push(Commit::Copy { state: o(0), next });
            em.state_keys.push(key(StateLoc::Slot(o(0)), *w));
        }
        K::Register { w } => {
            let next = em.temp(*w);
            em.push(Op::Mux, next, i(0), o(0), i(1), 0);
            em.commits.push(Commit::Copy { state: o(0), next });
            em.state_keys.push(key(StateLoc::Slot(o(0)), *w));
        }
        K::SrLatch => {
            let next = em.temp(1);
            em.push(Op::SrNext, next, i(0), i(1), o(0), 0);
            em.commits.push(Commit::Copy { state: o(0), next });
            em.state_keys.push(key(StateLoc::Slot(o(0)), 1));
        }
        K::DLatch { w } => {
            // The output is combinational (transparent); state is hidden.
            let state = em.temp(*w);
            em.push(Op::Mux, o(0), i(0), state, i(1), 0);
            em.commits.push(Commit::Copy { state, next: o(0) });
            em.state_keys.push(key(StateLoc::Slot(state), *w));
        }
        K::Ram { addr_bits, w, init } => {
            let m = em.mem(*addr_bits, *w, init, true);
            em.push(Op::MemRead, o(0), i(0), m, z, 0);
            let we = em.temp(1);
            let addr = em.temp(*addr_bits);
            let data = em.temp(*w);
            em.push(Op::Copy, we, i(1), z, z, 0);
            em.push(Op::Copy, addr, i(0), z, z, 0);
            em.push(Op::Copy, data, i(2), z, z, 0);
            em.commits.push(Commit::RamWrite {
                mem: m,
                we,
                addr,
                data,
            });
            em.state_keys.push(key(StateLoc::Mem(m), *w));
        }
    }
}

/// Rank of every instruction (1 + max rank of the instructions producing
/// its slot operands; 0 if none) and a topological order. Kahn's algorithm,
/// iterative.
fn levelize(ops: &[Op], instrs: &[Instr], slots: usize) -> (Vec<u32>, Vec<u32>) {
    let n = instrs.len();
    let mut producer = vec![u32::MAX; slots];
    for (k, ins) in instrs.iter().enumerate() {
        debug_assert_eq!(producer[ins.dst as usize], u32::MAX, "slot written twice");
        producer[ins.dst as usize] = k as u32;
    }
    let operands = |k: usize| {
        let (sa, sb, sc) = ops[k].slot_operands();
        let ins = &instrs[k];
        [(sa, ins.a), (sb, ins.b), (sc, ins.c)]
            .into_iter()
            .filter(|&(is, _)| is)
            .map(|(_, s)| producer[s as usize])
            .filter(|&p| p != u32::MAX)
    };
    // Consumers in CSR form.
    let mut indeg = vec![0u32; n];
    let mut start = vec![0u32; n + 1];
    for (k, deg) in indeg.iter_mut().enumerate() {
        for p in operands(k) {
            *deg += 1;
            start[p as usize + 1] += 1;
        }
    }
    for k in 0..n {
        start[k + 1] += start[k];
    }
    let mut fill = start.clone();
    let mut consumers = vec![0u32; start[n] as usize];
    for k in 0..n {
        for p in operands(k) {
            consumers[fill[p as usize] as usize] = k as u32;
            fill[p as usize] += 1;
        }
    }

    let mut rank = vec![0u32; n];
    let mut order = Vec::with_capacity(n);
    let mut queue: VecDeque<u32> = (0..n as u32).filter(|&k| indeg[k as usize] == 0).collect();
    while let Some(k) = queue.pop_front() {
        order.push(k);
        for &c in &consumers[start[k as usize] as usize..start[k as usize + 1] as usize] {
            let c = c as usize;
            rank[c] = rank[c].max(rank[k as usize] + 1);
            indeg[c] -= 1;
            if indeg[c] == 0 {
                queue.push_back(c as u32);
            }
        }
    }
    assert_eq!(
        order.len(),
        n,
        "internal error: instruction cycle after loop check"
    );
    (rank, order)
}
