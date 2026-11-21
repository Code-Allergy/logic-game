//! Bit-blasting: rebuild a flat word-level design from 1-bit gates, for
//! benchmarks (design doc section 9: "a word-level design and its
//! bit-blasted equivalent").
//!
//! Every combinational primitive becomes 1-bit gates (`Add` becomes a ripple
//! carry, `LtU` a borrow chain, and so on). `Split`, `Merge` and `Extend`
//! become pure wiring. `Delay`, `Register` and `DLatch` become one 1-bit
//! element per bit. Memories, `Clock` and `SrLatch` have no gate-level
//! equivalent and stay as they are, with `Merge`/`Split` around their buses.
//! Root ports keep their names and widths.

use super::super::netlist::{Builder, Design, NetId, NodeId, NodeKind};
use slotmap::SecondaryMap;

/// Bit-blast the root definition. Panics if it contains instances.
pub fn bitblast(src: &Design) -> Design {
    let s = src.root_def();
    let mut out = Design::new(format!("{}_bitblasted", s.name));
    let root = out.root;
    let mut b = out.builder(root);
    let mut bits: SecondaryMap<NetId, Vec<NetId>> = SecondaryMap::new();
    let mut zero: Option<NetId> = None;

    // Ports.
    for (i, &port) in s.inputs.iter().enumerate() {
        let NodeKind::Input { w } = s.nodes[port].kind else {
            unreachable!()
        };
        let name = s
            .name_of(port)
            .map_or_else(|| format!("in{i}"), str::to_string);
        let net = b.input(name, w);
        bits.insert(s.nodes[port].outputs[0], b.bus_expand(net));
    }

    // State placeholders first, so feedback through state can be wired.
    let mut placeholders: SecondaryMap<NodeId, Vec<NodeId>> = SecondaryMap::new();
    for (id, node) in &s.nodes {
        let per_bit = |b: &mut Builder<'_>, w: u8, k: NodeKind| -> Vec<NodeId> {
            (0..w).map(|_| b.node(k.clone(), &[])).collect()
        };
        let nodes = match &node.kind {
            NodeKind::Delay { w } => per_bit(&mut b, *w, NodeKind::Delay { w: 1 }),
            NodeKind::Register { w } => per_bit(&mut b, *w, NodeKind::Register { w: 1 }),
            NodeKind::DLatch { w } => per_bit(&mut b, *w, NodeKind::DLatch { w: 1 }),
            NodeKind::SrLatch | NodeKind::Ram { .. } | NodeKind::Screen { .. } => {
                vec![b.node(node.kind.clone(), &[])]
            }
            NodeKind::Instance(_) => panic!("bitblast: instances are not supported"),
            _ => continue,
        };
        if node.outputs.is_empty() {
            placeholders.insert(id, nodes);
            continue;
        }
        let q = if matches!(node.kind, NodeKind::Ram { .. }) {
            let word = b.out(nodes[0], 0);
            b.bus_expand(word)
        } else {
            nodes.iter().map(|&n| b.out(n, 0)).collect()
        };
        bits.insert(node.outputs[0], q);
        placeholders.insert(id, nodes);
    }

    // Combinational nodes in topological order (Kahn over driver nodes).
    let is_comb = |k: &NodeKind| {
        !k.is_state() && !matches!(k, NodeKind::Input { .. } | NodeKind::Output { .. })
    };
    let mut pending: SecondaryMap<NodeId, usize> = SecondaryMap::new();
    let mut users: SecondaryMap<NodeId, Vec<NodeId>> = SecondaryMap::new();
    for (id, node) in &s.nodes {
        if !is_comb(&node.kind) {
            continue;
        }
        let mut n = 0;
        for net in node.inputs.iter().flatten() {
            let d = s.nets[*net].driver.0;
            if is_comb(&s.nodes[d].kind) {
                n += 1;
                users.entry(d).unwrap().or_default().push(id);
            }
        }
        pending.insert(id, n);
    }
    let mut ready: Vec<NodeId> = pending
        .iter()
        .filter(|(_, n)| **n == 0)
        .map(|(id, _)| id)
        .collect();
    while let Some(id) = ready.pop() {
        let node = &s.nodes[id];
        let (ins, _) = node.kind.pins().expect("flat");
        let input: Vec<Vec<NetId>> = ins
            .iter()
            .enumerate()
            .map(|(k, &w)| match node.inputs[k] {
                Some(net) => bits[net].clone(),
                None => {
                    let z = *zero.get_or_insert_with(|| b.zero(1));
                    vec![z; w as usize]
                }
            })
            .collect();
        let outs = blast(&mut b, &node.kind, &input, &mut zero);
        for (&net, v) in node.outputs.iter().zip(outs) {
            bits.insert(net, v);
        }
        if let Some(us) = users.get(id) {
            for &u in us {
                pending[u] -= 1;
                if pending[u] == 0 {
                    ready.push(u);
                }
            }
        }
    }

    // Wire the state inputs.
    for (id, nodes) in &placeholders {
        let node = &s.nodes[id];
        let pin = |k: usize| node.inputs[k].map(|n| bits[n].clone());
        match node.kind {
            NodeKind::Delay { .. } => {
                if let Some(d) = pin(0) {
                    for (&n, &bit) in nodes.iter().zip(&d) {
                        b.connect(bit, (n, 0));
                    }
                }
            }
            NodeKind::Register { .. } | NodeKind::DLatch { .. } => {
                for (k, src) in [(0u8, pin(0)), (1, pin(1))] {
                    let Some(src) = src else { continue };
                    for (i, &n) in nodes.iter().enumerate() {
                        b.connect(if k == 0 { src[0] } else { src[i] }, (n, k));
                    }
                }
            }
            NodeKind::SrLatch => {
                for k in 0..2 {
                    if let Some(src) = pin(k) {
                        b.connect(src[0], (nodes[0], k as u8));
                    }
                }
            }
            NodeKind::Ram { .. } | NodeKind::Screen { .. } => {
                for k in 0..node.inputs.len() {
                    if let Some(src) = pin(k) {
                        let word = b.bus_collect(&src);
                        b.connect(word, (nodes[0], k as u8));
                    }
                }
            }
            _ => unreachable!(),
        }
    }

    for (i, &port) in s.outputs.iter().enumerate() {
        let NodeKind::Output { w } = s.nodes[port].kind else {
            unreachable!()
        };
        let name = s
            .name_of(port)
            .map_or_else(|| format!("out{i}"), str::to_string);
        let v = match s.nodes[port].inputs[0] {
            Some(net) => bits[net].clone(),
            None => vec![*zero.get_or_insert_with(|| b.zero(1)); w as usize],
        };
        let word = b.bus_collect(&v);
        b.output(name, word);
    }
    out
}

/// Gate-level outputs (one bit vector per output pin) of one combinational
/// node, given its input bit vectors.
fn blast(
    b: &mut Builder<'_>,
    kind: &NodeKind,
    i: &[Vec<NetId>],
    zero: &mut Option<NetId>,
) -> Vec<Vec<NetId>> {
    use NodeKind as K;
    let mut zero_bit = |b: &mut Builder<'_>| *zero.get_or_insert_with(|| b.zero(1));
    fn map2<'x>(
        b: &mut Builder<'x>,
        i: &[Vec<NetId>],
        f: impl Fn(&mut Builder<'x>, NetId, NetId) -> NetId,
    ) -> Vec<NetId> {
        i[0].iter().zip(&i[1]).map(|(&x, &y)| f(b, x, y)).collect()
    }
    let one = match kind {
        K::Const { w, value } => (0..*w).map(|k| b.konst(1, value >> k & 1)).collect(),
        K::Not { .. } => i[0].iter().map(|&x| b.not(x)).collect(),
        K::And { .. } => map2(b, i, Builder::and),
        K::Or { .. } => map2(b, i, Builder::or),
        K::Xor { .. } => map2(b, i, Builder::xor),
        K::Nand { .. } => map2(b, i, Builder::nand),
        K::Nor { .. } => map2(b, i, Builder::nor),
        K::Xnor { .. } => map2(b, i, Builder::xnor),
        K::Mux { .. } => i[1]
            .iter()
            .zip(&i[2])
            .map(|(&x, &y)| b.mux(i[0][0], x, y))
            .collect(),
        K::Switch { .. } => i[1].iter().map(|&d| b.and(i[0][0], d)).collect(),
        K::Add { .. } => {
            let mut carry = i[2][0];
            let mut sum = Vec::with_capacity(i[0].len());
            for (&x, &y) in i[0].iter().zip(&i[1]) {
                let p = b.xor(x, y);
                sum.push(b.xor(p, carry));
                let g = b.and(x, y);
                let t = b.and(p, carry);
                carry = b.or(g, t);
            }
            return vec![sum, vec![carry]];
        }
        K::Eq { .. } => {
            let eq = map2(b, i, Builder::xnor);
            vec![b.and_n(&eq)]
        }
        K::LtU { .. } => {
            let mut lt = zero_bit(b);
            for (&x, &y) in i[0].iter().zip(&i[1]) {
                let nx = b.not(x);
                let here = b.and(nx, y);
                let same = b.xnor(x, y);
                let keep = b.and(same, lt);
                lt = b.or(here, keep);
            }
            vec![lt]
        }
        K::ReduceOr { .. } => vec![b.or_n(&i[0])],
        K::ReduceAnd { .. } => vec![b.and_n(&i[0])],
        K::ReduceXor { .. } => vec![b.xor_n(&i[0])],
        K::Split { lo, len, .. } => i[0][*lo as usize..(*lo + *len) as usize].to_vec(),
        K::Merge { .. } => i.concat(),
        K::Extend { from, to, signed } => {
            let mut v = i[0].clone();
            let fill = if *signed {
                v[*from as usize - 1]
            } else {
                zero_bit(b)
            };
            v.resize(*to as usize, fill);
            v
        }
        K::Clock { .. } => vec![b.node1(kind.clone(), &[])],
        K::Rom { .. } => {
            let addr = b.bus_collect(&i[0]);
            let word = b.node1(kind.clone(), &[addr]);
            b.bus_expand(word)
        }
        other => unreachable!("not combinational: {other:?}"),
    };
    vec![one]
}
