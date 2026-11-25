//! State-keyed specialization: specialized bodies against the general
//! body on every state that satisfies the key (exhaustive over the key's
//! values on small designs), keying on the support against keying on the
//! whole slot, and mutations (a key that drops a needed bit) the check
//! must catch.

use std::sync::Arc;

use super::*;
use crate::compile::{Options, Program, compile, phases};
use crate::netlist::Design;
use crate::sim::Sim;
use crate::testkit::{GenConfig, Rng, fixtures, generate};

/// Every fused-run body of `p` with its tick residue: one per clock-phase
/// variant, or the program's own.
fn fused_bodies(p: &Program) -> Vec<(Body, Option<u64>)> {
    let fuse = |b: Body| phases::prune(p, &b).unwrap_or(b);
    match p.phases.as_deref() {
        Some(ph) => ph
            .variants
            .iter()
            .enumerate()
            .map(|(k, v)| (fuse(v.clone()), Some(k as u64)))
            .collect(),
        None => vec![(
            fuse(Body {
                blocks: p.blocks.clone(),
                instrs: p.instrs.clone(),
                imms: p.imms.clone(),
                commits: p.commits.clone(),
                guards: Vec::new(),
            }),
            None,
        )],
    }
}

/// Run `body`'s instructions over `slots` (the settle of tick `tick`),
/// checking the layout a runtime relies on: every operand is written
/// before it is read in a block of lower rank, or not written at all,
/// and no slot is written twice.
fn settle(p: &Program, body: &Body, slots: &mut [u64], mems: &[Vec<u64>], tick: u64) {
    let mut written_rank = vec![u32::MAX; slots.len()];
    for b in &body.blocks {
        assert!(!b.packed);
        let (sa, sb, sc) = b.op.slot_operands();
        for k in b.start..b.start + b.len {
            let i = body.instrs[k as usize];
            for (is, x) in [(sa, i.a), (sb, i.b), (sc, i.c)] {
                if !is {
                    continue; // a literal field
                }
                let r = written_rank[x as usize];
                assert!(
                    r == u32::MAX || r < b.rank,
                    "operand {x} of {i:?} (rank {}) not written before",
                    b.rank
                );
            }
            assert_eq!(
                written_rank[i.dst as usize],
                u32::MAX,
                "{} written twice",
                i.dst
            );
        }
        for k in b.start..b.start + b.len {
            let i = body.instrs[k as usize];
            let imm = body.imms[k as usize];
            let v = match b.op {
                Op::MemRead => {
                    let m = &mems[i.b as usize];
                    m.get(slots[i.a as usize] as usize).copied().unwrap_or(0)
                }
                Op::Clock => (((tick + u64::from(i.c)) % u64::from(i.a)) < u64::from(i.b)) as u64,
                op => {
                    super::super::simplify::fold(op, &i, imm, &|s| Some(slots[s as usize]), &p.mems)
                        .expect("a pure op")
                }
            };
            slots[i.dst as usize] = v;
            written_rank[i.dst as usize] = b.rank;
        }
    }
}

/// Apply `body`'s commits.
fn commit(p: &Program, body: &Body, slots: &mut [u64], mems: &mut [Vec<u64>]) {
    let writes: Vec<u32> = body
        .commits
        .iter()
        .filter_map(|c| match *c {
            Commit::Copy { state, .. } => Some(state),
            _ => None,
        })
        .collect();
    for c in &body.commits {
        for s in c.reads() {
            assert!(
                !writes.contains(&s) || s == ZERO_SLOT,
                "commit reads {s}, which a commit writes"
            );
        }
    }
    let at = |mem: u32, a: u64| (a & mask(p.mems[mem as usize].addr_bits)) as usize;
    let mut copies = Vec::new();
    for c in &body.commits {
        match *c {
            Commit::Copy { state, next } => copies.push((state, slots[next as usize])),
            Commit::MemClear { mem, en } => {
                if slots[en as usize] != 0 {
                    mems[mem as usize].fill(0);
                }
            }
            Commit::RamWrite {
                mem,
                we,
                addr,
                data,
            } => {
                if slots[we as usize] != 0 {
                    let a = at(mem, slots[addr as usize]);
                    mems[mem as usize][a] = slots[data as usize];
                }
            }
            Commit::RamWriteMasked {
                mem,
                mask: m,
                addr,
                data,
            } => {
                let a = at(mem, slots[addr as usize]);
                let (m, d) = (slots[m as usize], slots[data as usize]);
                let w = &mut mems[mem as usize][a];
                *w = (*w & !m) | (d & m);
            }
        }
    }
    for (s, v) in copies {
        slots[s as usize] = v;
    }
}

/// The state slots of `p` (written by a commit).
fn state_slots(p: &Program) -> Vec<u32> {
    let mut v: Vec<u32> = p
        .commits
        .iter()
        .filter_map(|c| match *c {
            Commit::Copy { state, .. } => Some(state),
            _ => None,
        })
        .collect();
    v.sort_unstable();
    v.dedup();
    v
}

/// A machine state: slots and memories.
#[derive(Clone, Debug, PartialEq, Eq)]
struct State {
    slots: Vec<u64>,
    mems: Vec<Vec<u64>>,
}

/// A random state of `p`: inputs, state slots and RAM contents anything
/// within their possible bits (what pokes and imports can set), the rest
/// as a settle leaves it.
fn random_state(p: &Program, sim: &mut Sim, possible: &[u64], rng: &mut Rng) -> State {
    for port in 0..p.ports.inputs.len() {
        sim.set_input(port, rng.next_u64());
    }
    sim.tick();
    let mut slots: Vec<u64> = (0..p.slot_count).map(|s| sim.read(s)).collect();
    for s in state_slots(p) {
        slots[s as usize] = rng.next_u64() & possible[s as usize];
    }
    for port in &p.ports.inputs {
        let s = port.slot as usize;
        slots[s] = rng.next_u64() & possible[s];
    }
    let mems = p
        .mems
        .iter()
        .map(|m| {
            (0..1usize << m.addr_bits)
                .map(|a| {
                    if m.writable {
                        rng.next_u64() & mask(m.width)
                    } else {
                        m.init.get(a).copied().unwrap_or(0)
                    }
                })
                .collect()
        })
        .collect();
    State { slots, mems }
}

/// The state after one fused tick of `bodies` (each run in order over
/// the slots, then the last one's commits).
fn step(p: &Program, bodies: &[&Body], s: &State, tick: u64) -> State {
    let mut out = s.clone();
    for b in bodies {
        settle(p, b, &mut out.slots, &out.mems, tick);
    }
    commit(p, bodies.last().unwrap(), &mut out.slots, &mut out.mems);
    out
}

/// What a commit's effect leaves observable: state slots and memories.
fn observed(p: &Program, s: &State) -> (Vec<u64>, Vec<Vec<u64>>) {
    let st = state_slots(p)
        .iter()
        .map(|&x| s.slots[x as usize])
        .collect();
    (st, s.mems.clone())
}

/// Whether the specialized `(prefix, body)` agrees with the general
/// `general` on state `s` (one fused tick).
fn agrees(p: &Program, general: &Body, prefix: &Body, body: &Body, s: &State, tick: u64) -> bool {
    let want = step(p, &[general], s, tick);
    let got = step(p, &[prefix, body], s, tick);
    observed(p, &want) == observed(p, &got)
}

/// `s` with the key bits of `layout` set to `key`'s (state keys only).
fn force(layout: &KeyLayout, key: u64, s: &State) -> State {
    let mut out = s.clone();
    for (slot, k) in layout.assume(key) {
        let x = &mut out.slots[slot as usize];
        *x = (*x & !k.mask()) | k.ones;
    }
    out
}

/// Every value of an up-to-`max_bits`-bit key, or a sample.
fn key_values(width: u32, max_bits: u32, rng: &mut Rng) -> Vec<u64> {
    if width <= max_bits {
        (0..1u64 << width).collect()
    } else {
        (0..48)
            .map(|_| rng.next_u64() & mask(width as u8))
            .collect()
    }
}

/// The exactness check of one design: for every fused body, every
/// candidate and every state slot it reads as a key:
///
/// - for every value of the masked key, the specialized body (after its
///   prefix) commits the same state as the general body, on `states`
///   random states that satisfy the key (state keys are forced to it;
///   computed keys take the value the state gives them);
/// - specializing on the whole value gives the same body as on its
///   support.
///
/// Returns the number of (key, value) bodies checked.
fn check_design(name: &str, d: &Design, states: usize, max_bits: u32) -> usize {
    let mut checked = 0;
    for options in [Options::default(), Options::debug()] {
        let p = Arc::new(compile(d, &options).unwrap());
        let possible = possible_bits(&p);
        let mut sim = Sim::new(Arc::clone(&p));
        let mut rng = Rng::new(0x5eed ^ p.instrs.len() as u64);
        for (body, phase) in fused_bodies(&p) {
            let Some(cx) = SpecContext::new(&p, &body, &possible, phase) else {
                continue;
            };
            let tick = phase.unwrap_or(0);
            let mut keys: Vec<u32> = cx.candidates(&p).iter().map(|c| c.slot).collect();
            for s in state_slots(&p) {
                if cx.exact_support(s) != 0 && !keys.contains(&s) {
                    keys.push(s);
                }
            }
            for &slot in &keys {
                let exact = cx.exact_support(slot);
                let fold = cx.support(slot);
                // The exact and fold supports, and a random sub-mask (any
                // assumption that holds is exact).
                let sub = fold & rng.next_u64();
                let mut masks = vec![exact, fold, sub];
                masks.dedup();
                for support in masks {
                    if support == 0 {
                        continue;
                    }
                    let layout = KeyLayout::new(vec![KeyField {
                        slot,
                        mask: support,
                    }]);
                    let (prefix, pre) = cx.prefix(&[slot]);
                    let computed = cx.computes(slot);
                    for key in key_values(layout.width(), max_bits, &mut rng) {
                        let spec = cx.specialize(&p, &layout.assume(key), &pre);
                        if support == exact {
                            // The whole value: the same body.
                            let full = pdep(key, support)
                                | (rng.next_u64() & possible[slot as usize] & !support);
                            let whole = cx.specialize(&p, &[(slot, Known::constant(full))], &pre);
                            assert!(
                                spec == whole,
                                "{name}: key {slot} ({support:#x}) = {key:#x}, whole {full:#x}: the support and the whole value give different bodies:\n{}",
                                diff(&spec, &whole)
                            );
                        }
                        for _ in 0..states {
                            let mut s = random_state(&p, &mut sim, &possible, &mut rng);
                            if computed {
                                // The key the state gives the slot.
                                let mut t = s.clone();
                                settle(&p, &prefix, &mut t.slots, &t.mems, tick);
                                if layout.pack(&t.slots) != key {
                                    continue;
                                }
                            } else {
                                s = force(&layout, key, &s);
                            }
                            if !agrees(&p, &body, &prefix, &spec, &s, tick) {
                                let want = step(&p, &[&body], &s, tick);
                                let got = step(&p, &[&prefix, &spec], &s, tick);
                                let bad: Vec<String> = state_slots(&p)
                                    .iter()
                                    .filter(|&&x| want.slots[x as usize] != got.slots[x as usize])
                                    .map(|&x| {
                                        format!(
                                            "{x}: {:#x} != {:#x}",
                                            got.slots[x as usize], want.slots[x as usize]
                                        )
                                    })
                                    .collect();
                                panic!(
                                    "{name} ({options:?}): key slot {slot} = {:#x}, mask {support:#x} = {key:#x} (phase {phase:?}): wrong state {bad:?}\ngeneral:\n{}\nspecialized:\n{}",
                                    s.slots[slot as usize],
                                    diff(&body, &Body::default()),
                                    diff(&spec, &Body::default())
                                );
                            }
                        }
                        checked += 1;
                    }
                }
            }
        }
    }
    checked
}

/// What differs between two bodies (instructions with their ops, and
/// commits), for failure messages.
fn diff(a: &Body, b: &Body) -> String {
    let items = |x: &Body| {
        let mut v: Vec<String> = Vec::new();
        for bl in &x.blocks {
            for k in bl.start..bl.start + bl.len {
                let i = x.instrs[k as usize];
                v.push(format!(
                    "r{} {:?} {:?} imm {:#x}",
                    bl.rank, bl.op, i, x.imms[k as usize]
                ));
            }
        }
        v.extend(x.commits.iter().map(|c| format!("{c:?}")));
        v
    };
    let (ia, ib) = (items(a), items(b));
    let mut out = String::new();
    for x in &ia {
        if !ib.contains(x) {
            out += &format!("  - {x}\n");
        }
    }
    for x in &ib {
        if !ia.contains(x) {
            out += &format!("  + {x}\n");
        }
    }
    out
}

/// A design with one of each candidate kind: a 6-bit control register
/// written from an input (`ctl`), a 4-bit counter with a region flag, and
/// a ROM word indexed by the counter. Bit 3 of `ctl` reaches only an
/// output, so no fused body reads it.
fn controlled() -> Design {
    let mut d = Design::new("top");
    let r = d.root;
    let mut b = d.builder(r);
    let we = b.input("we", 1);
    let din = b.input("din", 6);
    let x = b.input("x", 8);
    let ctl = b.register(we, din);
    let one = b.one();
    let zero = b.zero(1);
    let cnt = b.counter(4, one, zero);
    let c8 = b.zext(cnt, 8);
    // ctl[0] picks an adder or an xor.
    let (sum, _) = b.add(x, c8, None);
    let x2 = b.xor(x, c8);
    let s0 = b.bit(ctl, 0);
    let pick = b.mux(s0, x2, sum);
    let r1 = b.register(one, pick);
    // ctl[2] gates the second register's enable.
    let s2 = b.bit(ctl, 2);
    let r2 = b.register(s2, x);
    // ctl[4..6] == 2 selects inverted data.
    let hi = b.split(ctl, 4, 2);
    let sel = b.eq_const(hi, 2);
    let nx = b.not(x);
    let r3d = b.mux(sel, x, nx);
    let r3 = b.register(one, r3d);
    // A flag of the counter and a ROM word it indexes.
    let flag = b.eq_const(cnt, 9);
    let rom = b.rom(
        cnt,
        8,
        &[3, 5, 0x81, 2, 7, 1, 0x80, 4, 6, 3, 2, 1, 0, 5, 7, 0x83],
    );
    let op = b.split(rom, 0, 2);
    let a1 = b.and(x, r1);
    let o1 = b.or(x, r2);
    let o2 = b.xor(r1, r3);
    let alu = b.mux_n(op, &[x, a1, o1, o2]);
    let alu = b.mux(flag, alu, r2);
    let r4 = b.register(one, alu);
    let s3 = b.bit(ctl, 3);
    b.output("s3", s3);
    b.output("r1", r1);
    b.output("r2", r2);
    b.output("r3", r3);
    b.output("r4", r4);
    d
}

#[test]
fn the_support_is_what_the_body_reads() {
    let d = controlled();
    let p = compile(&d, &Options::default()).unwrap();
    let possible = possible_bits(&p);
    let (body, phase) = fused_bodies(&p).remove(0);
    let cx = SpecContext::new(&p, &body, &possible, phase).unwrap();
    let cands = cx.candidates(&p);
    let ctl = cands
        .iter()
        .find(|c| c.class == KeyClass::Register && c.support.count_ones() >= 3)
        .expect("ctl is a register candidate");
    // Bits 0, 2, 4 and 5: not 1 (unused) or 3 (an output only).
    assert_eq!(ctl.support, 0b11_0101, "{cands:?}");
    assert_eq!(ctl.values.as_ref().map(Vec::len), Some(16));
    let rom = cands
        .iter()
        .find(|c| c.class == KeyClass::RomWord)
        .expect("the ROM word is a candidate");
    // Only the low two bits select an operation.
    assert_eq!(rom.support, 0b11, "{cands:?}");
    assert_eq!(rom.values.as_deref(), Some(&[0, 1, 2, 3][..]));
    assert!(rom.prefix_ops >= 1);
    assert!(cands.iter().any(|c| c.class == KeyClass::Flag), "{cands:?}");
    // Every value of ctl's key folds something away.
    let layout = KeyLayout::new(vec![KeyField {
        slot: ctl.slot,
        mask: ctl.support,
    }]);
    for key in 0..16 {
        assert!(cx.gain(&p, &layout, key) > 0, "key {key}");
    }
}

/// Dropping bits that do not pay: with no budget everything goes, with a
/// negative one nothing does, and what stays is a sub-mask.
#[test]
fn unprofitable_key_bits_are_dropped() {
    let d = controlled();
    let p = compile(&d, &Options::default()).unwrap();
    let possible = possible_bits(&p);
    let (body, phase) = fused_bodies(&p).remove(0);
    let cx = SpecContext::new(&p, &body, &possible, phase).unwrap();
    let ctl = cx
        .candidates(&p)
        .into_iter()
        .find(|c| c.class == KeyClass::Register && c.support.count_ones() >= 3)
        .unwrap();
    let layout = KeyLayout::new(vec![KeyField {
        slot: ctl.slot,
        mask: ctl.support,
    }]);
    let samples: Vec<(u64, u64)> = (0..16).map(|k| (k, 1 + k % 3)).collect();
    assert!(
        cx.prune_bits(&p, &layout, &samples, f64::INFINITY)
            .fields
            .is_empty()
    );
    assert_eq!(cx.prune_bits(&p, &layout, &samples, -1.0), layout);
    let some = cx.prune_bits(&p, &layout, &samples, 1.5);
    for f in &some.fields {
        assert_eq!(f.mask & !ctl.support, 0);
    }
}

#[test]
fn specialized_bodies_are_exact_on_small_designs() {
    let n = check_design("controlled", &controlled(), 12, 8);
    assert!(n >= 30, "{n}");
    for (name, d) in [
        ("counter", fixtures::counter(4)),
        ("register file", fixtures::register_file(2, 4)),
        ("fifo", fixtures::fifo(2, 4)),
        ("cpu", fixtures::cpu(&fixtures::LOOP_PROGRAM)),
    ] {
        check_design(name, &d, 6, 6);
    }
}

#[test]
fn specialized_bodies_are_exact_on_random_designs() {
    let mut checked = 0;
    for seed in 0..60 {
        let g = generate(&GenConfig {
            seed: seed * 7 + 3,
            max_width: if seed % 2 == 0 { 16 } else { 64 },
            state_percent: 30,
            ..GenConfig::default()
        });
        checked += check_design(&format!("random {seed}"), &g.design, 4, 4);
    }
    assert!(checked > 200, "{checked}");
}

/// Two clocks of periods 2 and 4: keys on every phase variant.
#[test]
fn specialized_phase_variants_are_exact() {
    let mut d = Design::new("top");
    let r = d.root;
    let mut b = d.builder(r);
    let x = b.input("x", 4);
    let we = b.input("we", 1);
    let mode = b.register(we, x);
    let c2 = b.clock(2, 1, 0);
    let c4 = b.clock(4, 1, 1);
    let m0 = b.bit(mode, 0);
    let m1 = b.bit(mode, 1);
    let e0 = b.and(c2, m0);
    let held = b.dlatch(e0, x);
    let nx = b.not(x);
    let pick = b.mux(m1, x, nx);
    let e1 = b.or(c4, m1);
    let h2 = b.register(e1, pick);
    let y = b.xor(held, h2);
    b.output("y", y);
    let p = compile(&d, &Options::default()).unwrap();
    assert!(p.phases.is_some());
    check_design("phases", &d, 10, 6);
}

/// A key that drops a bit the body reads is wrong for some state: the
/// check catches it. The body is specialized on a whole observed value,
/// then run on a state that differs from it only in the dropped bit (one
/// the mutated key cannot tell apart).
#[test]
fn a_key_without_a_needed_bit_is_caught() {
    let d = controlled();
    let p = Arc::new(compile(&d, &Options::default()).unwrap());
    let possible = possible_bits(&p);
    let (body, phase) = fused_bodies(&p).remove(0);
    let cx = SpecContext::new(&p, &body, &possible, phase).unwrap();
    let mut sim = Sim::new(Arc::clone(&p));
    let mut rng = Rng::new(17);
    let cands = cx.candidates(&p);
    let mut caught_bits = 0;
    for c in cands.iter().filter(|c| c.class == KeyClass::Register) {
        let layout = KeyLayout::new(vec![KeyField {
            slot: c.slot,
            mask: c.support,
        }]);
        let (prefix, pre) = cx.prefix(&[c.slot]);
        for bit in 0..64 {
            let b = 1u64 << bit;
            if c.support & b == 0 {
                continue;
            }
            let mut caught = false;
            'values: for key in 0..1u64 << layout.width() {
                let spec = cx.specialize(&p, &layout.assume(key), &pre);
                for _ in 0..16 {
                    let s = force(
                        &layout,
                        key,
                        &random_state(&p, &mut sim, &possible, &mut rng),
                    );
                    assert!(agrees(&p, &body, &prefix, &spec, &s, 0));
                    let mut flipped = s.clone();
                    flipped.slots[c.slot as usize] ^= b;
                    if !agrees(&p, &body, &prefix, &spec, &flipped, 0) {
                        caught = true;
                        break 'values;
                    }
                }
            }
            assert!(
                caught,
                "dropping bit {bit} of slot {} went unnoticed",
                c.slot
            );
            caught_bits += 1;
        }
    }
    assert!(caught_bits >= 4, "{caught_bits}");
}

/// Possible bits cover every value a running program puts in a slot.
#[test]
fn possible_bits_cover_running_values() {
    for seed in 0..30 {
        let g = generate(&GenConfig {
            seed: seed * 13 + 1,
            state_percent: 30,
            ..GenConfig::default()
        });
        let p = Arc::new(compile(&g.design, &Options::default()).unwrap());
        let possible = possible_bits(&p);
        let mut sim = Sim::new(Arc::clone(&p));
        let mut rng = Rng::new(seed);
        for _ in 0..50 {
            for port in 0..p.ports.inputs.len() {
                sim.set_input(port, rng.next_u64());
            }
            sim.tick();
            for s in 0..p.slot_count {
                let v = sim.read(s);
                assert_eq!(
                    v & !possible[s as usize],
                    0,
                    "seed {seed}: slot {s} = {v:#x}"
                );
            }
        }
    }
}

#[test]
fn pext_and_pdep_round_trip() {
    let mut rng = Rng::new(3);
    for _ in 0..1000 {
        let (x, m) = (rng.next_u64(), rng.next_u64() & rng.next_u64());
        assert_eq!(pdep(pext(x, m), m), x & m);
        assert_eq!(
            pext(x, m) >> m.count_ones().min(63),
            if m == ALL { x >> 63 } else { 0 }
        );
    }
    let layout = KeyLayout::new(vec![
        KeyField {
            slot: 1,
            mask: 0b1010,
        },
        KeyField { slot: 2, mask: 0b1 },
    ]);
    let slots = [0, 0b1000, 1];
    assert_eq!(layout.width(), 3);
    assert_eq!(layout.pack(&slots), 0b110);
    assert_eq!(
        layout.assume(0b110),
        vec![(1, Known::bits(0b1010, 0b1000)), (2, Known::bits(1, 1))]
    );
}

#[test]
fn ram_reads_and_unknown_state_stay_general() {
    // A key on a slot the body does not read yields the general body
    // (minus nothing).
    let d = fixtures::register_file(2, 4);
    let p = compile(&d, &Options::default()).unwrap();
    let possible = possible_bits(&p);
    let (body, phase) = fused_bodies(&p).remove(0);
    let cx = SpecContext::new(&p, &body, &possible, phase).unwrap();
    let spec = cx.specialize(&p, &[], &[]);
    assert!(spec.ops() <= body.ops());
}

/// A layout of more than 64 bits cannot pack every field: it is refused,
/// and an oversized one assumes nothing about the bits its key drops (a
/// 64-bit field then a 1-bit field pack the same key for either value of
/// the second).
#[test]
fn oversized_key_layouts_assume_only_what_they_pack() {
    let fields = vec![
        KeyField { slot: 1, mask: ALL },
        KeyField { slot: 2, mask: 1 },
    ];
    assert!(KeyLayout::try_new(fields.clone()).is_none());
    assert!(KeyLayout::try_new(vec![fields[0]]).is_some());
    let layout = KeyLayout::new(fields);
    assert!(!layout.fits());
    let (a, b) = ([0, 7, 0], [0, 7, 1]);
    assert_eq!(layout.pack(&a), layout.pack(&b));
    let assumed = layout.assume(layout.pack(&a));
    assert_eq!(assumed[0], (1, Known::constant(7)));
    assert_eq!(
        assumed[1],
        (2, Known::UNKNOWN),
        "the dropped bit is not known"
    );
    // A field straddling bit 64 keeps the bits that fit.
    let layout = KeyLayout::new(vec![
        KeyField {
            slot: 1,
            mask: ALL >> 2,
        },
        KeyField {
            slot: 2,
            mask: 0b1111,
        },
    ]);
    let assumed = layout.assume(ALL);
    assert_eq!(assumed[1], (2, Known::bits(0b11, 0b11)));
}

/// A `Clock`'s fields are literals: a period past the slot count is not
/// read as a slot (possible bits, specialization without a known phase).
#[test]
fn a_long_clock_period_is_not_a_slot() {
    let mut d = Design::new("top");
    let r = d.root;
    let mut b = d.builder(r);
    let x = b.input("x", 4);
    let clk = b.clock(1000, 500, 3);
    let held = b.register(clk, x);
    let c4 = b.zext(clk, 4);
    let y = b.xor(held, c4);
    let q = b.register(clk, y);
    b.output("q", q);
    let p = compile(&d, &Options::default()).unwrap();
    assert!(p.phases.is_none());
    let clocks: Vec<u32> = p
        .blocks
        .iter()
        .filter(|bl| bl.op == Op::Clock)
        .flat_map(|bl| &p.instrs[bl.start as usize..(bl.start + bl.len) as usize])
        .map(|i| i.a)
        .collect();
    assert!(
        clocks.iter().any(|&a| a >= p.slot_count),
        "{clocks:?} vs {}",
        p.slot_count
    );
    check_design("clock 1000", &d, 6, 4);
}
