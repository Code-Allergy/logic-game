//! Passes 5 and 6: slot assignment, lowering to instructions, levelizing and
//! block emission.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use slotmap::SecondaryMap;

use super::Options;
use super::elaborate::Elab;
use super::pack;
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
    let parts = opts.partitions.max(1) as usize;
    let part = partition(parts, &ops, &instrs, &ranks, &sorted, widths.len());
    sorted.sort_by_key(|&i| (ranks[i as usize], ops[i as usize], part[i as usize], i));
    let numbering = locality_order(
        opts.slot_order,
        &ops,
        &instrs,
        &ranks,
        &part,
        &mut sorted,
        &commits,
        &ports,
        widths.len(),
    );

    // ── Lane packing and blocks ────────────────────────────────────────────
    let mut widths = widths;
    let mut plan = pack::Plan::new(widths.len());
    let pack_mode = match opts.pack {
        super::PackMode::Auto => {
            let packable = (0..instrs.len())
                .filter(|&k| pack::packable(ops[k], &instrs[k], &widths))
                .count();
            // Live slots: one per instruction, input, constant and state
            // element (`widths` still counts slots simplify removed).
            let live = instrs.len() + ports.inputs.len() + consts.len() + commits.len();
            if live * 8 > super::AUTO_PACK_BYTES && 2 * packable >= instrs.len() {
                super::PackMode::All
            } else {
                super::PackMode::Off
            }
        }
        mode => mode,
    };
    if pack_mode != super::PackMode::Off {
        for (k, ins) in instrs.iter().enumerate() {
            if !pack::packable(ops[k], ins, &widths) {
                for s in operand_slots(ops[k], ins) {
                    plan.sink_read[s as usize] = true;
                }
            }
        }
        for s in commit_reads(&commits).chain(ports.outputs.iter().map(|p| p.slot)) {
            plan.sink_read[s as usize] = true;
        }
    }
    // Per `(rank, op)` run: the packed words, then the scalar instructions.
    // (op, rank, partition, packed words, scalar instructions)
    type Unit = (Op, u32, u8, std::ops::Range<usize>, Vec<u32>);
    let mut units: Vec<Unit> = Vec::new();
    let mut lo = 0;
    while lo < sorted.len() {
        let k0 = sorted[lo] as usize;
        let (op, rank, pt) = (ops[k0], ranks[k0], part[k0]);
        let mut hi = lo + 1;
        while hi < sorted.len()
            && ops[sorted[hi] as usize] == op
            && ranks[sorted[hi] as usize] == rank
            && part[sorted[hi] as usize] == pt
        {
            hi += 1;
        }
        let run = &sorted[lo..hi];
        let w0 = plan.words.len();
        if pack_mode != super::PackMode::Off {
            let packable: Vec<u32> = run
                .iter()
                .copied()
                .filter(|&k| pack::packable(op, &instrs[k as usize], &widths))
                .collect();
            plan.pack_run(
                op,
                &packable,
                &instrs,
                pack_mode,
                opts.pack_min as usize,
                || {
                    widths.push(64);
                    (widths.len() - 1) as u32
                },
            );
        }
        let scalar: Vec<u32> = run
            .iter()
            .copied()
            .filter(|&k| plan.lane_of[instrs[k as usize].dst as usize] == pack::NONE)
            .collect();
        units.push((op, rank, pt, w0..plan.words.len(), scalar));
        lo = hi;
    }
    let (packs, gathers, scatters) = if plan.words.is_empty() {
        Default::default()
    } else {
        // Packed nets read as plain slots need a scalar copy.
        let mut scalar_read = vec![false; widths.len()];
        for (_, _, _, _, scalar) in &units {
            for &k in scalar {
                for s in operand_slots(ops[k as usize], &instrs[k as usize]) {
                    scalar_read[s as usize] = true;
                }
            }
        }
        for s in commit_reads(&commits).chain(ports.outputs.iter().map(|p| p.slot)) {
            scalar_read[s as usize] = true;
        }
        plan.build(&instrs, &scalar_read)
    };

    let mut blocks: Vec<Block> = Vec::new();
    let mut out = Vec::with_capacity(instrs.len());
    let mut out_imms = Vec::with_capacity(instrs.len());
    let mut out_ops = Vec::with_capacity(instrs.len());
    let mut block_parts = Vec::new();
    // Within a `(rank, op)` run: packed blocks, then scalar ones, each in
    // partition order.
    let mut lo = 0;
    while lo < units.len() {
        let mut hi = lo;
        while hi < units.len() && (units[hi].0, units[hi].1) == (units[lo].0, units[lo].1) {
            hi += 1;
        }
        for (op, rank, pt, words, _) in &units[lo..hi] {
            if !words.is_empty() {
                blocks.push(Block {
                    op: *op,
                    rank: *rank,
                    start: words.start as u32,
                    len: words.len() as u32,
                    packed: true,
                });
                block_parts.push(*pt);
            }
        }
        for (op, rank, pt, _, scalar) in &units[lo..hi] {
            if !scalar.is_empty() {
                blocks.push(Block {
                    op: *op,
                    rank: *rank,
                    start: out.len() as u32,
                    len: scalar.len() as u32,
                    packed: false,
                });
                block_parts.push(*pt);
            }
            for &i in scalar {
                out.push(instrs[i as usize]);
                out_imms.push(imms[i as usize]);
                out_ops.push(*op);
            }
        }
        lo = hi;
    }
    if parts == 1 {
        block_parts.clear();
    }
    // Numbering order of destination slots (old numbering).
    let dsts: Vec<u32> = match numbering {
        Some(order) => order
            .iter()
            .map(|&k| instrs[k as usize].dst)
            .filter(|&d| plan.lane_of[d as usize] == pack::NONE)
            .chain(exec_dsts(&blocks, &[], &packs, &scatters))
            .collect(),
        None => exec_dsts(&blocks, &out, &packs, &scatters),
    };

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
        lane_words: packs.iter().map(|p| p.dst).collect(),
        block_parts,
        packs,
        gathers,
        scatters,
    };
    renumber(&mut p, &out_ops, &dsts, &plan.lane_of);
    p
}

/// Estimated relative cost of one instruction of `op` (for balancing
/// partitions): streaming ops by operand count, ops with literal fields
/// (which run from the full instruction) and memory reads more.
fn op_cost(op: Op) -> u32 {
    match op {
        Op::Shr | Op::Concat | Op::Sext | Op::Clock => 5,
        Op::MemRead => 6,
        _ => {
            let (a, b, c) = op.slot_operands();
            1 + a as u32 + b as u32 + c as u32
        }
    }
}

/// Partition every instruction for `parts` threads (`Options::partitions`):
/// rank by rank, an instruction joins the partition that produced most of
/// its operands, unless that partition already has its share of the rank's
/// estimated cost; then the least loaded one. Values then mostly stay in
/// the cache of the core that computed them.
fn partition(
    parts: usize,
    ops: &[Op],
    instrs: &[Instr],
    ranks: &[u32],
    sorted: &[u32],
    slots: usize,
) -> Vec<u8> {
    let mut part = vec![0u8; instrs.len()];
    if parts <= 1 {
        return part;
    }
    let mut producer = vec![u32::MAX; slots];
    for (k, ins) in instrs.iter().enumerate() {
        producer[ins.dst as usize] = k as u32;
    }
    let mut lo = 0;
    while lo < sorted.len() {
        let rank = ranks[sorted[lo] as usize];
        let mut hi = lo;
        while hi < sorted.len() && ranks[sorted[hi] as usize] == rank {
            hi += 1;
        }
        let span = &sorted[lo..hi];
        let total: u32 = span.iter().map(|&k| op_cost(ops[k as usize])).sum();
        let cap = total.div_ceil(parts as u32) + 6;
        let mut load = vec![0u32; parts];
        for &k in span {
            let c = op_cost(ops[k as usize]);
            let mut votes = vec![0u32; parts];
            for s in operand_slots(ops[k as usize], &instrs[k as usize]) {
                let p = producer[s as usize];
                if p != u32::MAX {
                    votes[part[p as usize] as usize] += 1;
                }
            }
            let best = (0..parts)
                .filter(|&q| load[q] + c <= cap)
                .max_by_key(|&q| (votes[q], std::cmp::Reverse(load[q]), std::cmp::Reverse(q)))
                .unwrap_or_else(|| (0..parts).min_by_key(|&q| load[q]).unwrap());
            part[k as usize] = best as u8;
            load[best] += c;
        }
        lo = hi;
    }
    part
}

/// Slots read by commits.
fn commit_reads(commits: &[Commit]) -> impl Iterator<Item = u32> + '_ {
    commits.iter().flat_map(|c| match *c {
        Commit::Copy { next, .. } => [next, ZERO_SLOT, ZERO_SLOT],
        Commit::RamWrite { we, addr, data, .. } => [we, addr, data],
    })
}

/// Destination slots in execution order: scalar instructions (from
/// `instrs`; none if it is empty), packed words and their scatters.
fn exec_dsts(blocks: &[Block], instrs: &[Instr], packs: &[Pack], scatters: &[Scatter]) -> Vec<u32> {
    let mut out = Vec::new();
    for b in blocks {
        let range = b.start as usize..(b.start + b.len) as usize;
        if b.packed {
            for p in &packs[range] {
                out.push(p.dst);
                out.extend(
                    scatters[p.scatter_start as usize..][..p.scatter_len as usize]
                        .iter()
                        .map(|s| s.slot),
                );
            }
        } else if !instrs.is_empty() {
            out.extend(instrs[range].iter().map(|i| i.dst));
        }
    }
    out
}

/// Producer of every slot (`u32::MAX` if no instruction writes it) and each
/// instruction's readers in CSR form (`start`, `readers`).
struct Readers {
    producer: Vec<u32>,
    start: Vec<u32>,
    readers: Vec<u32>,
}

impl Readers {
    fn new(ops: &[Op], instrs: &[Instr], slots: usize) -> Self {
        let n = instrs.len();
        let mut producer = vec![u32::MAX; slots];
        for (k, ins) in instrs.iter().enumerate() {
            producer[ins.dst as usize] = k as u32;
        }
        let mut start = vec![0u32; n + 1];
        for (k, ins) in instrs.iter().enumerate() {
            for s in operand_slots(ops[k], ins) {
                let p = producer[s as usize];
                if p != u32::MAX {
                    start[p as usize + 1] += 1;
                }
            }
        }
        for k in 0..n {
            start[k + 1] += start[k];
        }
        let mut fill = start.clone();
        let mut readers = vec![0u32; start[n] as usize];
        for (k, ins) in instrs.iter().enumerate() {
            for s in operand_slots(ops[k], ins) {
                let p = producer[s as usize];
                if p != u32::MAX {
                    readers[fill[p as usize] as usize] = k as u32;
                    fill[p as usize] += 1;
                }
            }
        }
        Self {
            producer,
            start,
            readers,
        }
    }

    fn of(&self, k: u32) -> &[u32] {
        &self.readers[self.start[k as usize] as usize..self.start[k as usize + 1] as usize]
    }
}

/// Slot operands of `ins` (up to three).
fn operand_slots(op: Op, ins: &Instr) -> impl Iterator<Item = u32> {
    let (sa, sb, sc) = op.slot_operands();
    [(sa, ins.a), (sb, ins.b), (sc, ins.c)]
        .into_iter()
        .filter(|&(is, _)| is)
        .map(|(_, s)| s)
}

/// Reorder instructions inside each `(rank, op)` run of `sorted` for memory
/// locality (issue #41). Returns the order in which destination slots are
/// numbered (instruction indices), when it differs from execution order.
#[allow(clippy::too_many_arguments)]
fn locality_order(
    order: super::SlotOrder,
    ops: &[Op],
    instrs: &[Instr],
    ranks: &[u32],
    part: &[u8],
    sorted: &mut [u32],
    commits: &[Commit],
    ports: &PortMap,
    slots: usize,
) -> Option<Vec<u32>> {
    use super::SlotOrder;
    let n = sorted.len();
    let readers = || Readers::new(ops, instrs, slots);
    // Spans of `sorted` with one rank.
    let rank_spans = |sorted: &[u32]| {
        let mut spans = Vec::new();
        let mut lo = 0;
        for k in 1..=n {
            if k == n || ranks[sorted[k] as usize] != ranks[sorted[lo] as usize] {
                spans.push(lo..k);
                lo = k;
            }
        }
        spans
    };
    match order {
        SlotOrder::Production => None,
        SlotOrder::FirstUse => {
            let r = readers();
            // Final position of each instruction; ranks are placed last to
            // first, so every reader is placed before its producer.
            let mut pos = vec![u32::MAX; n];
            let mut key = vec![u32::MAX; n];
            for span in rank_spans(sorted).into_iter().rev() {
                for &k in &sorted[span.clone()] {
                    key[k as usize] = r
                        .of(k)
                        .iter()
                        .map(|&c| pos[c as usize])
                        .min()
                        .unwrap_or(u32::MAX);
                }
                sorted[span.clone()]
                    .sort_by_key(|&k| (ops[k as usize], part[k as usize], key[k as usize], k));
                for (j, &k) in sorted[span.clone()].iter().enumerate() {
                    pos[k as usize] = (span.start + j) as u32;
                }
            }
            None
        }
        SlotOrder::Dfs => {
            let r = readers();
            // Post-order over operands, from the sinks: root outputs, slots
            // read by commits, then anything not reached (observable only).
            let mut post = vec![u32::MAX; n];
            let mut next = 0u32;
            let mut stack: Vec<(u32, u8)> = Vec::new();
            let roots = ports
                .outputs
                .iter()
                .map(|p| p.slot)
                .chain(commits.iter().flat_map(|c| match *c {
                    Commit::Copy { next, .. } => vec![next],
                    Commit::RamWrite { we, addr, data, .. } => vec![we, addr, data],
                }))
                .filter_map(|s| {
                    let p = r.producer[s as usize];
                    (p != u32::MAX).then_some(p)
                })
                .chain(0..n as u32)
                .collect::<Vec<_>>();
            let mut visited = vec![false; n];
            for root in roots {
                if visited[root as usize] {
                    continue;
                }
                visited[root as usize] = true;
                stack.push((root, 0));
                while let Some(&mut (k, ref mut i)) = stack.last_mut() {
                    let operand =
                        operand_slots(ops[k as usize], &instrs[k as usize]).nth(*i as usize);
                    if let Some(s) = operand {
                        let p = r.producer[s as usize];
                        *i += 1;
                        if p != u32::MAX && !visited[p as usize] {
                            visited[p as usize] = true;
                            stack.push((p, 0));
                        }
                    } else {
                        post[k as usize] = next;
                        next += 1;
                        stack.pop();
                    }
                }
            }
            for span in rank_spans(sorted) {
                sorted[span]
                    .sort_by_key(|&k| (ops[k as usize], part[k as usize], post[k as usize]));
            }
            let mut numbering: Vec<u32> = sorted.to_vec();
            numbering.sort_by_key(|&k| post[k as usize]);
            Some(numbering)
        }
    }
}

/// Renumber slots densely: zero, root inputs, constants, state, then
/// `dsts` (instruction, packed word and scatter destinations, in the order
/// they should be numbered). Unreferenced slots disappear. A net held only
/// in a lane (`lane_of`, indexed by old slot) gets a lane slot in the debug
/// map.
fn renumber(p: &mut Program, ops: &[Op], dsts: &[u32], lane_of: &[u32]) {
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
    for &d in dsts {
        assign(d, &mut map);
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
    for pk in &mut p.packs {
        pk.dst = m(pk.dst);
        for o in [&mut pk.a, &mut pk.b, &mut pk.c] {
            o.base = m(o.base);
            o.splat = m(o.splat);
        }
    }
    for g in &mut p.gathers {
        g.slot = m(g.slot);
    }
    for sc in &mut p.scatters {
        sc.slot = m(sc.slot);
    }
    for w in &mut p.lane_words {
        *w = m(*w);
    }
    for slots in &mut p.debug.net_slots {
        for (_, s) in slots.iter_mut() {
            *s = match (map[*s as usize], lane_of.get(*s as usize)) {
                (UNASSIGNED, Some(&x)) if x != pack::NONE => {
                    // Lane slot: `slot_count + 64 * word + lane`.
                    next + 64 * (x >> 6) + (x & 63)
                }
                _ => m(*s),
            };
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
