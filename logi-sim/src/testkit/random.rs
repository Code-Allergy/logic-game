//! Seeded random design generator for differential tests and benchmarks
//! (design doc sections 8 and 9).
//!
//! Generated designs are valid and free of combinational loops by
//! construction: combinational pins are only ever connected to nets that
//! already exist, and the only pins wired afterwards (possibly to later nets,
//! forming feedback) are non-combinational state inputs (`Delay` d,
//! `Register` en/d, `SrLatch` s/r, `Ram` we/wdata, `Ram2` waddr/we/wdata,
//! every `Screen` pin). Optionally a loop is
//! injected for diagnostics tests.

use std::sync::Arc;

use smallvec::SmallVec;

use super::rng::Rng;
use crate::netlist::{DefId, Design, NetId, NodeId, NodeKind, ScreenMode};

#[derive(Debug, Clone)]
pub struct GenConfig {
    pub seed: u64,
    /// Nodes generated in the root definition (adapters not counted).
    pub nodes: usize,
    /// Number of shared child definitions.
    pub child_defs: usize,
    /// Nodes generated in each child definition.
    pub child_nodes: usize,
    /// Maximum definition nesting depth (1 = root only).
    pub max_depth: usize,
    /// Widths are drawn from `1..=max_width` (1..=64), biased towards 1.
    pub max_width: u8,
    /// Root input ports (children get `1..=inputs`).
    pub inputs: usize,
    /// Root output ports (children get `1..=outputs`).
    pub outputs: usize,
    /// Percentage of nodes that are state elements.
    pub state_percent: u32,
    /// Percentage of nodes that are instances (when children are available).
    pub instance_percent: u32,
    /// Percentage of input pins left unconnected.
    pub unconnected_percent: u32,
    /// Inject one combinational loop (see [`Generated::injected_loop`]).
    pub inject_loop: bool,
    /// Which combinational primitives to use.
    pub ops: OpMix,
    /// Also draw `Screen` state elements (all three modes). Off by default
    /// so the designs of existing seeds stay the same.
    pub screens: bool,
}

/// Combinational primitive mix for the generator.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum OpMix {
    /// Every combinational primitive.
    #[default]
    All,
    /// Only the 2-input bitwise gates and `Not` (the gate set of the deleted
    /// legacy runners, which benchmark gate 2 was measured on).
    Gates,
}

impl Default for GenConfig {
    fn default() -> Self {
        Self {
            seed: 1,
            nodes: 60,
            child_defs: 3,
            child_nodes: 20,
            max_depth: 3,
            max_width: 16,
            inputs: 4,
            outputs: 4,
            state_percent: 15,
            instance_percent: 8,
            unconnected_percent: 3,
            inject_loop: false,
            ops: OpMix::All,
            screens: false,
        }
    }
}

impl GenConfig {
    /// A flat, combinational-only, mixed-op DAG of about `nodes` nodes
    /// (benchmark shape).
    pub fn dag(seed: u64, nodes: usize, max_width: u8) -> Self {
        Self {
            seed,
            nodes,
            child_defs: 0,
            child_nodes: 0,
            max_depth: 1,
            max_width,
            inputs: 16,
            outputs: 16,
            state_percent: 0,
            instance_percent: 0,
            unconnected_percent: 0,
            inject_loop: false,
            ops: OpMix::All,
            screens: false,
        }
    }

    /// A flat, combinational, gates-only DAG (`Not` and 2-input bitwise
    /// gates) of exactly `nodes` gates at width `w`. With `w == 1` it is the
    /// classic gate-level circuit.
    pub fn gates(seed: u64, nodes: usize, w: u8) -> Self {
        Self {
            ops: OpMix::Gates,
            max_width: w,
            ..Self::dag(seed, nodes, w)
        }
    }
}

#[derive(Debug, Clone)]
pub struct Generated {
    pub design: Design,
    /// With `inject_loop`, the definition holding the loop and the nodes on
    /// it, in order along the cycle.
    pub injected_loop: Option<(DefId, Vec<NodeId>)>,
}

pub fn generate(cfg: &GenConfig) -> Generated {
    assert!(
        (1..=64).contains(&cfg.max_width),
        "max_width must be 1..=64"
    );
    let mut rng = Rng::new(cfg.seed);
    let mut design = Design::new("root");

    // Children first, so a definition only instances earlier ones: the
    // instance graph is a DAG and definitions are shared between instances.
    let mut children: Vec<(DefId, usize)> = Vec::new(); // (def, depth below it incl. itself)
    for i in 0..cfg.child_defs {
        let def = design.add_definition(format!("def{i}"));
        let usable: Vec<_> = children
            .iter()
            .copied()
            .filter(|&(_, depth)| depth + 2 <= cfg.max_depth)
            .collect();
        let n_in = 1 + rng.below(cfg.inputs.max(1));
        let n_out = 1 + rng.below(cfg.outputs.max(1));
        let mut g = DefGen::new(&mut design, def, &mut rng, cfg);
        g.fill(n_in, cfg.child_nodes, n_out, &usable);
        let depth = 1 + usable_depth(&design, def, &children);
        children.push((def, depth));
    }

    let root = design.root;
    let usable: Vec<_> = children
        .iter()
        .copied()
        .filter(|&(_, depth)| depth < cfg.max_depth)
        .collect();
    let mut g = DefGen::new(&mut design, root, &mut rng, cfg);
    g.fill(cfg.inputs.max(1), cfg.nodes, cfg.outputs.max(1), &usable);

    let injected_loop = cfg.inject_loop.then(|| {
        // Inject into the root or into a definition the root reaches.
        let reachable = design.defs_bottom_up();
        let def = *rng.pick(&reachable);
        let mut g = DefGen::new(&mut design, def, &mut rng, cfg);
        (def, g.inject_loop())
    });

    Generated {
        design,
        injected_loop,
    }
}

/// Nesting depth of the instances inside `def`.
fn usable_depth(design: &Design, def: DefId, children: &[(DefId, usize)]) -> usize {
    design.defs[def]
        .nodes
        .values()
        .filter_map(|n| match n.kind {
            NodeKind::Instance(c) => children.iter().find(|(d, _)| *d == c).map(|(_, k)| *k),
            _ => None,
        })
        .max()
        .unwrap_or(0)
}

struct DefGen<'a> {
    design: &'a mut Design,
    def: DefId,
    rng: &'a mut Rng,
    cfg: &'a GenConfig,
    pool: Vec<NetId>,
    /// State input pins wired after all nodes exist: (node, pin, width).
    deferred: Vec<(NodeId, u8, u8)>,
}

impl<'a> DefGen<'a> {
    fn new(design: &'a mut Design, def: DefId, rng: &'a mut Rng, cfg: &'a GenConfig) -> Self {
        let pool = design.defs[def].nets.keys().collect();
        Self {
            design,
            def,
            rng,
            cfg,
            pool,
            deferred: Vec::new(),
        }
    }

    fn width(&mut self) -> u8 {
        let max = self.cfg.max_width;
        if self.cfg.ops == OpMix::Gates {
            return max;
        }
        match self.rng.below(10) {
            0..=3 => 1,
            4 => max,
            _ => self.rng.range(1, max as u64) as u8,
        }
    }

    fn add(&mut self, kind: NodeKind) -> NodeId {
        let id = match kind {
            NodeKind::Instance(c) => self.design.add_instance(self.def, c),
            k => self.design.add_node(self.def, k),
        }
        .expect("generator builds valid nodes");
        let outs = self.design.defs[self.def].nodes[id].outputs.clone();
        self.pool.extend(outs);
        id
    }

    fn connect(&mut self, net: Option<NetId>, node: NodeId, pin: u8) {
        if let Some(net) = net {
            self.design
                .connect(self.def, net, (node, pin))
                .expect("generator matches widths");
        }
    }

    fn net_width(&self, net: NetId) -> u8 {
        self.design.defs[self.def].nets[net].width
    }

    /// A net of width `w` from the pool (adapted with Split/Extend if needed),
    /// or `None` (unconnected) with the configured probability.
    fn source(&mut self, w: u8) -> Option<NetId> {
        if self.pool.is_empty() || self.rng.percent(self.cfg.unconnected_percent) {
            return None;
        }
        // Prefer recent nets for depth, but reach back across the whole pool.
        let n = self.pool.len();
        let idx = if self.rng.coin() {
            n - 1 - self.rng.below(n.min(8))
        } else {
            self.rng.below(n)
        };
        let net = self.pool[idx];
        Some(self.adapt(net, w))
    }

    fn adapt(&mut self, net: NetId, w: u8) -> NetId {
        let have = self.net_width(net);
        let kind = if have == w {
            return net;
        } else if have > w {
            let lo = self.rng.range(0, (have - w) as u64) as u8;
            NodeKind::Split {
                w: have,
                lo,
                len: w,
            }
        } else {
            NodeKind::Extend {
                from: have,
                to: w,
                signed: self.rng.coin(),
            }
        };
        let id = self.add(kind);
        self.connect(Some(net), id, 0);
        self.design.defs[self.def].nodes[id].outputs[0]
    }

    /// Add `kind` and connect every input pin now.
    /// Sources are drawn before the node exists, so it can never read its
    /// own output.
    fn comb(&mut self, kind: NodeKind) -> NodeId {
        let ins = match &kind {
            NodeKind::Instance(c) => self.design.defs[*c].signature().0,
            k => k.pins().expect("fixed pins").0,
        };
        let srcs: Vec<Option<NetId>> = ins.iter().map(|&w| self.source(w)).collect();
        let id = self.add(kind);
        for (pin, src) in srcs.into_iter().enumerate() {
            self.connect(src, id, pin as u8);
        }
        id
    }

    fn fill(&mut self, n_in: usize, n_nodes: usize, n_out: usize, children: &[(DefId, usize)]) {
        for _ in 0..n_in {
            let w = self.width();
            self.add(NodeKind::Input { w });
        }
        for _ in 0..n_nodes {
            if self.rng.percent(self.cfg.state_percent) {
                self.state_node();
            } else if !children.is_empty() && self.rng.percent(self.cfg.instance_percent) {
                let (child, _) = *self.rng.pick(children);
                self.comb(NodeKind::Instance(child));
            } else {
                let kind = self.comb_kind();
                self.comb(kind);
            }
        }
        // Wire state inputs from the whole pool: feedback through state.
        for (node, pin, w) in std::mem::take(&mut self.deferred) {
            let src = self.source(w);
            self.connect(src, node, pin);
        }
        // Outputs from random nets, biased to the most recent.
        for _ in 0..n_out {
            let n = self.pool.len();
            let net = self.pool[n - 1 - self.rng.below(n.min(16))];
            let w = self.net_width(net);
            let id = self.add(NodeKind::Output { w });
            self.connect(Some(net), id, 0);
        }
    }

    fn state_node(&mut self) {
        let w = self.width();
        let addr_bits = self.rng.range(1, 6) as u8;
        // Screens come last so that designs without them are unchanged.
        let kinds = if self.cfg.screens { 7 } else { 6 };
        match self.rng.below(kinds) {
            0 => self.deferred_node(NodeKind::Delay { w }),
            1 => self.deferred_node(NodeKind::Register { w }),
            2 => self.deferred_node(NodeKind::SrLatch),
            // DLatch is transparent: its inputs are combinational.
            3 => {
                self.comb(NodeKind::DLatch { w });
            }
            4 => {
                let init: Vec<u64> = (0..self.rng.below(1 << addr_bits))
                    .map(|_| self.rng.next_u64())
                    .collect();
                // addr is combinational (drawn before the node exists);
                // we/wdata are deferred.
                let src = self.source(addr_bits);
                let id = self.add(NodeKind::Ram {
                    addr_bits,
                    w,
                    init: Arc::from(init),
                });
                self.connect(src, id, 0);
                self.deferred.push((id, 1, 1));
                self.deferred.push((id, 2, w));
            }
            5 => {
                let reads = self.rng.range(1, 3) as u8;
                let mask = self.rng.coin();
                let init: Vec<u64> = (0..self.rng.below(1 << addr_bits))
                    .map(|_| self.rng.next_u64())
                    .collect();
                // Read addresses are combinational (drawn before the node
                // exists); the write port is deferred.
                let srcs: Vec<_> = (0..reads).map(|_| self.source(addr_bits)).collect();
                let id = self.add(NodeKind::Ram2 {
                    addr_bits,
                    w,
                    reads,
                    mask,
                    init: Arc::from(init),
                });
                for (k, src) in srcs.into_iter().enumerate() {
                    self.connect(src, id, 3 + k as u8);
                }
                self.deferred.push((id, 0, addr_bits));
                self.deferred.push((id, 1, if mask { w } else { 1 }));
                self.deferred.push((id, 2, w));
            }
            // A screen has no outputs and no combinational inputs.
            _ => {
                let mode = match self.rng.below(3) {
                    0 => ScreenMode::Pixel,
                    1 => ScreenMode::Memory,
                    _ => ScreenMode::Scan,
                };
                let x_bits = self.rng.range(1, 3) as u8;
                let y_bits = self.rng.range(1, 3) as u8;
                self.deferred_node(NodeKind::Screen {
                    mode,
                    x_bits,
                    y_bits,
                    w,
                });
            }
        }
    }

    fn deferred_node(&mut self, kind: NodeKind) {
        let (ins, _) = kind.pins().expect("state kinds have fixed pins");
        let id = self.add(kind);
        for (pin, &w) in ins.iter().enumerate() {
            self.deferred.push((id, pin as u8, w));
        }
    }

    fn comb_kind(&mut self) -> NodeKind {
        let w = self.width();
        if self.cfg.ops == OpMix::Gates {
            return match self.rng.below(7) {
                0 => NodeKind::Not { w },
                1 => NodeKind::And { w },
                2 => NodeKind::Or { w },
                3 => NodeKind::Xor { w },
                4 => NodeKind::Nand { w },
                5 => NodeKind::Nor { w },
                _ => NodeKind::Xnor { w },
            };
        }
        match self.rng.below(22) {
            0 => NodeKind::Not { w },
            1 | 2 => NodeKind::And { w },
            3 | 4 => NodeKind::Or { w },
            5 | 6 => NodeKind::Xor { w },
            7 => NodeKind::Nand { w },
            8 => NodeKind::Nor { w },
            9 => NodeKind::Xnor { w },
            10 => NodeKind::Mux { w },
            11 => NodeKind::Switch { w },
            12 | 13 => NodeKind::Add { w },
            14 => NodeKind::Eq { w },
            15 => NodeKind::LtU { w },
            16 => match self.rng.below(3) {
                0 => NodeKind::ReduceOr { w },
                1 => NodeKind::ReduceAnd { w },
                _ => NodeKind::ReduceXor { w },
            },
            17 => {
                let len = self.rng.range(1, w as u64) as u8;
                let lo = self.rng.range(0, (w - len) as u64) as u8;
                NodeKind::Split { w, lo, len }
            }
            18 => {
                let mut widths: SmallVec<[u8; 4]> = SmallVec::new();
                let mut total = 0u32;
                for _ in 0..self.rng.range(2, 4) {
                    let x = self.width();
                    if total + x as u32 > self.cfg.max_width.max(2) as u32 {
                        break;
                    }
                    total += x as u32;
                    widths.push(x);
                }
                if widths.is_empty() {
                    widths.push(1);
                }
                NodeKind::Merge { widths }
            }
            19 => NodeKind::Const {
                w,
                value: self.rng.next_u64(),
            },
            20 => {
                let period = self.rng.range(1, 8) as u32;
                NodeKind::Clock {
                    period,
                    duty: self.rng.range(0, period as u64) as u32,
                    phase: self.rng.range(0, 8) as u32,
                }
            }
            _ => {
                let addr_bits = self.rng.range(1, 6) as u8;
                let contents: Vec<u64> = (0..self.rng.below(1 << addr_bits) + 1)
                    .map(|_| self.rng.next_u64())
                    .collect();
                NodeKind::Rom {
                    addr_bits,
                    w,
                    contents: Arc::from(contents),
                }
            }
        }
    }

    /// Add a chain of 1..=4 combinational nodes whose last output feeds the
    /// first: a combinational loop. Returns the chain's nodes in order.
    fn inject_loop(&mut self) -> Vec<NodeId> {
        let w = self.width();
        let len = 1 + self.rng.below(4);
        // Draw side inputs before the chain exists, so the only cycle is the
        // chain itself.
        let sides: Vec<Option<NetId>> = (0..len).map(|_| self.source(w)).collect();
        let mut chain: Vec<NodeId> = Vec::with_capacity(len);
        for side in sides {
            let prev = chain
                .last()
                .map(|&n| self.design.defs[self.def].nodes[n].outputs[0]);
            let kind = match (chain.is_empty(), self.rng.coin()) {
                (true, _) => NodeKind::Xor { w },
                (false, true) => NodeKind::Not { w },
                (false, false) => NodeKind::And { w },
            };
            let is_not = matches!(kind, NodeKind::Not { .. });
            let id = self.add(kind);
            if is_not {
                self.connect(prev, id, 0);
            } else if chain.is_empty() {
                self.connect(side, id, 0); // pin 1 closes the loop below
            } else {
                self.connect(prev, id, 0);
                self.connect(side, id, 1);
            }
            chain.push(id);
        }
        let last = self.design.defs[self.def].nodes[*chain.last().unwrap()].outputs[0];
        self.connect(Some(last), chain[0], 1);
        chain
    }
}
