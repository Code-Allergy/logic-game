//! Ergonomic construction of definitions.
//!
//! [`Builder`] wraps the [`Design`] mutation API and works in terms of nets:
//! each method takes input nets and returns the output net(s), inferring
//! widths from its inputs. It panics on misuse (width mismatch, bad
//! parameters), which suits programmatic construction of fixtures and tests;
//! use the [`Design`] methods directly where errors must be handled.
//!
//! Non-primitives (n-input gates, decoders, demux, n-way mux, counters, bus
//! expand/collect) are composed here from the primitive table.

use std::sync::Arc;

use smallvec::SmallVec;

use super::{DefId, Design, NetId, NodeId, NodeKind, PinRef};

pub struct Builder<'a> {
    design: &'a mut Design,
    def: DefId,
}

impl<'a> Builder<'a> {
    #[track_caller]
    pub fn new(design: &'a mut Design, def: DefId) -> Self {
        assert!(design.defs.contains_key(def), "no such definition");
        Self { design, def }
    }

    pub fn def(&self) -> DefId {
        self.def
    }

    pub fn design(&self) -> &Design {
        self.design
    }

    pub fn width(&self, net: NetId) -> u8 {
        self.design.defs[self.def].nets[net].width
    }

    // ── Generic ───────────────────────────────────────────────────────────

    /// Add a node and connect its first `inputs.len()` input pins. Pins past
    /// the slice, and `None` entries, stay unconnected.
    #[track_caller]
    pub fn node(&mut self, kind: NodeKind, inputs: &[Option<NetId>]) -> NodeId {
        let id = match kind {
            NodeKind::Instance(child) => self.design.add_instance(self.def, child),
            k => self.design.add_node(self.def, k),
        }
        .unwrap_or_else(|e| panic!("builder: {e}"));
        for (pin, net) in inputs.iter().enumerate() {
            if let Some(net) = net {
                self.connect(*net, (id, pin as u8));
            }
        }
        id
    }

    /// Add a node with all inputs connected and return output pin 0's net.
    #[track_caller]
    pub fn node1(&mut self, kind: NodeKind, inputs: &[NetId]) -> NetId {
        let ins: SmallVec<[Option<NetId>; 4]> = inputs.iter().map(|&n| Some(n)).collect();
        let id = self.node(kind, &ins);
        self.out(id, 0)
    }

    /// Net driven by output pin `pin` of `node`.
    #[track_caller]
    pub fn out(&self, node: NodeId, pin: u8) -> NetId {
        self.design.defs[self.def]
            .output_net(node, pin)
            .expect("builder: no such output pin")
    }

    #[track_caller]
    pub fn connect(&mut self, net: NetId, sink: PinRef) {
        self.design
            .connect(self.def, net, sink)
            .unwrap_or_else(|e| panic!("builder: {e}"));
    }

    #[track_caller]
    pub fn name(&mut self, node: NodeId, name: impl Into<String>) {
        self.design
            .set_name(self.def, node, Some(name.into()))
            .unwrap_or_else(|e| panic!("builder: {e}"));
    }

    /// Name the node driving `net`.
    pub fn name_net(&mut self, net: NetId, name: impl Into<String>) -> NetId {
        let driver = self.design.defs[self.def].nets[net].driver.0;
        self.name(driver, name);
        net
    }

    // ── Ports and constants ───────────────────────────────────────────────

    pub fn input(&mut self, name: impl Into<String>, w: u8) -> NetId {
        let id = self.node(NodeKind::Input { w }, &[]);
        self.name(id, name);
        self.out(id, 0)
    }

    pub fn output(&mut self, name: impl Into<String>, net: NetId) -> NodeId {
        let w = self.width(net);
        let id = self.node(NodeKind::Output { w }, &[Some(net)]);
        self.name(id, name);
        id
    }

    pub fn konst(&mut self, w: u8, value: u64) -> NetId {
        self.node1(
            NodeKind::Const {
                w,
                value: value & super::mask(w),
            },
            &[],
        )
    }

    pub fn zero(&mut self, w: u8) -> NetId {
        self.konst(w, 0)
    }

    pub fn one(&mut self) -> NetId {
        self.konst(1, 1)
    }

    // ── Bitwise ───────────────────────────────────────────────────────────

    pub fn not(&mut self, a: NetId) -> NetId {
        let w = self.width(a);
        self.node1(NodeKind::Not { w }, &[a])
    }
    pub fn and(&mut self, a: NetId, b: NetId) -> NetId {
        let w = self.width(a);
        self.node1(NodeKind::And { w }, &[a, b])
    }
    pub fn or(&mut self, a: NetId, b: NetId) -> NetId {
        let w = self.width(a);
        self.node1(NodeKind::Or { w }, &[a, b])
    }
    pub fn xor(&mut self, a: NetId, b: NetId) -> NetId {
        let w = self.width(a);
        self.node1(NodeKind::Xor { w }, &[a, b])
    }
    pub fn nand(&mut self, a: NetId, b: NetId) -> NetId {
        let w = self.width(a);
        self.node1(NodeKind::Nand { w }, &[a, b])
    }
    pub fn nor(&mut self, a: NetId, b: NetId) -> NetId {
        let w = self.width(a);
        self.node1(NodeKind::Nor { w }, &[a, b])
    }
    pub fn xnor(&mut self, a: NetId, b: NetId) -> NetId {
        let w = self.width(a);
        self.node1(NodeKind::Xnor { w }, &[a, b])
    }

    /// n-input AND as a balanced tree of 2-input gates.
    pub fn and_n(&mut self, xs: &[NetId]) -> NetId {
        self.tree(xs, Self::and)
    }
    pub fn or_n(&mut self, xs: &[NetId]) -> NetId {
        self.tree(xs, Self::or)
    }
    pub fn xor_n(&mut self, xs: &[NetId]) -> NetId {
        self.tree(xs, Self::xor)
    }

    #[track_caller]
    fn tree(&mut self, xs: &[NetId], f: fn(&mut Self, NetId, NetId) -> NetId) -> NetId {
        assert!(
            !xs.is_empty(),
            "builder: n-input gate needs at least one input"
        );
        let mut layer = xs.to_vec();
        while layer.len() > 1 {
            layer = layer
                .chunks(2)
                .map(|c| {
                    if c.len() == 2 {
                        f(self, c[0], c[1])
                    } else {
                        c[0]
                    }
                })
                .collect();
        }
        layer[0]
    }

    // ── Selection and arithmetic ──────────────────────────────────────────

    /// `sel ? b : a`.
    pub fn mux(&mut self, sel: NetId, a: NetId, b: NetId) -> NetId {
        let w = self.width(a);
        self.node1(NodeKind::Mux { w }, &[sel, a, b])
    }

    /// `en ? d : 0`.
    pub fn switch(&mut self, en: NetId, d: NetId) -> NetId {
        let w = self.width(d);
        self.node1(NodeKind::Switch { w }, &[en, d])
    }

    /// n-way mux: `xs[sel]`, as a tree of 2-way muxes. `xs.len()` must be at
    /// most `2^width(sel)`; missing entries read 0.
    #[track_caller]
    pub fn mux_n(&mut self, sel: NetId, xs: &[NetId]) -> NetId {
        let sw = self.width(sel);
        assert!(!xs.is_empty(), "builder: mux_n needs inputs");
        assert!(
            sw >= 32 || xs.len() <= 1usize << sw,
            "builder: mux_n has more inputs than the selector can address"
        );
        let w = self.width(xs[0]);
        let mut layer = xs.to_vec();
        let mut bit = 0u8;
        while layer.len() > 1 {
            let s = self.bit(sel, bit);
            let mut next = Vec::with_capacity(layer.len().div_ceil(2));
            for c in layer.chunks(2) {
                let b = if c.len() == 2 { c[1] } else { self.zero(w) };
                next.push(self.mux(s, c[0], b));
            }
            layer = next;
            bit += 1;
        }
        layer[0]
    }

    /// `a + b + cin`, returning `(sum, cout)`. `cin = None` is carry-in 0.
    pub fn add(&mut self, a: NetId, b: NetId, cin: Option<NetId>) -> (NetId, NetId) {
        let w = self.width(a);
        let id = self.node(NodeKind::Add { w }, &[Some(a), Some(b), cin]);
        (self.out(id, 0), self.out(id, 1))
    }

    /// `a - b` (wrapping), as `a + !b + 1`.
    pub fn sub(&mut self, a: NetId, b: NetId) -> NetId {
        let nb = self.not(b);
        let one = self.one();
        self.add(a, nb, Some(one)).0
    }

    /// `a + 1` (wrapping).
    pub fn inc(&mut self, a: NetId) -> NetId {
        let w = self.width(a);
        let z = self.zero(w);
        let one = self.one();
        self.add(a, z, Some(one)).0
    }

    pub fn eq(&mut self, a: NetId, b: NetId) -> NetId {
        let w = self.width(a);
        self.node1(NodeKind::Eq { w }, &[a, b])
    }

    /// `a == value`.
    pub fn eq_const(&mut self, a: NetId, value: u64) -> NetId {
        let w = self.width(a);
        let k = self.konst(w, value);
        self.eq(a, k)
    }

    pub fn lt_u(&mut self, a: NetId, b: NetId) -> NetId {
        let w = self.width(a);
        self.node1(NodeKind::LtU { w }, &[a, b])
    }

    pub fn reduce_or(&mut self, a: NetId) -> NetId {
        let w = self.width(a);
        self.node1(NodeKind::ReduceOr { w }, &[a])
    }
    pub fn reduce_and(&mut self, a: NetId) -> NetId {
        let w = self.width(a);
        self.node1(NodeKind::ReduceAnd { w }, &[a])
    }
    pub fn reduce_xor(&mut self, a: NetId) -> NetId {
        let w = self.width(a);
        self.node1(NodeKind::ReduceXor { w }, &[a])
    }

    // ── Width conversion ──────────────────────────────────────────────────

    pub fn split(&mut self, a: NetId, lo: u8, len: u8) -> NetId {
        let w = self.width(a);
        self.node1(NodeKind::Split { w, lo, len }, &[a])
    }

    /// Bit `i` of `a`. A 1-bit net is returned as is.
    pub fn bit(&mut self, a: NetId, i: u8) -> NetId {
        if self.width(a) == 1 && i == 0 {
            a
        } else {
            self.split(a, i, 1)
        }
    }

    /// Concatenate, `xs[0]` in the low bits.
    pub fn merge(&mut self, xs: &[NetId]) -> NetId {
        let widths = xs.iter().map(|&n| self.width(n)).collect();
        self.node1(NodeKind::Merge { widths }, xs)
    }

    pub fn zext(&mut self, a: NetId, to: u8) -> NetId {
        let from = self.width(a);
        if from == to {
            return a;
        }
        self.node1(
            NodeKind::Extend {
                from,
                to,
                signed: false,
            },
            &[a],
        )
    }

    pub fn sext(&mut self, a: NetId, to: u8) -> NetId {
        let from = self.width(a);
        if from == to {
            return a;
        }
        self.node1(
            NodeKind::Extend {
                from,
                to,
                signed: true,
            },
            &[a],
        )
    }

    /// Bus expand: one 1-bit net per bit, low bit first.
    pub fn bus_expand(&mut self, a: NetId) -> Vec<NetId> {
        (0..self.width(a)).map(|i| self.bit(a, i)).collect()
    }

    /// Bus collect: the inverse of [`Self::bus_expand`].
    pub fn bus_collect(&mut self, bits: &[NetId]) -> NetId {
        if bits.len() == 1 {
            bits[0]
        } else {
            self.merge(bits)
        }
    }

    /// Decoder: `2^width(sel)` 1-bit outputs, output `i` high iff `sel == i`.
    #[track_caller]
    pub fn decoder(&mut self, sel: NetId) -> Vec<NetId> {
        let sw = self.width(sel);
        assert!(sw <= 16, "builder: decoder selector too wide");
        (0..1u64 << sw).map(|i| self.eq_const(sel, i)).collect()
    }

    /// Demux: `2^width(sel)` outputs, output `i` is `d` when `sel == i`, else 0.
    pub fn demux(&mut self, sel: NetId, d: NetId) -> Vec<NetId> {
        self.decoder(sel)
            .into_iter()
            .map(|en| self.switch(en, d))
            .collect()
    }

    // ── Sources ───────────────────────────────────────────────────────────

    pub fn clock(&mut self, period: u32, duty: u32, phase: u32) -> NetId {
        self.node1(
            NodeKind::Clock {
                period,
                duty,
                phase,
            },
            &[],
        )
    }

    pub fn rom(&mut self, addr: NetId, w: u8, contents: &[u64]) -> NetId {
        let addr_bits = self.width(addr);
        self.node1(
            NodeKind::Rom {
                addr_bits,
                w,
                contents: Arc::from(contents),
            },
            &[addr],
        )
    }

    /// Instance `child` with the given input nets; returns its output nets
    /// in port order.
    pub fn instance(&mut self, child: DefId, inputs: &[Option<NetId>]) -> Vec<NetId> {
        let id = self.node(NodeKind::Instance(child), inputs);
        self.design.defs[self.def].nodes[id].outputs.to_vec()
    }

    // ── State ─────────────────────────────────────────────────────────────

    pub fn delay(&mut self, d: NetId) -> NetId {
        let w = self.width(d);
        self.node1(NodeKind::Delay { w }, &[d])
    }

    pub fn register(&mut self, en: NetId, d: NetId) -> NetId {
        let w = self.width(d);
        self.node1(NodeKind::Register { w }, &[en, d])
    }

    pub fn dlatch(&mut self, en: NetId, d: NetId) -> NetId {
        let w = self.width(d);
        self.node1(NodeKind::DLatch { w }, &[en, d])
    }

    pub fn sr_latch(&mut self, s: NetId, r: NetId) -> NetId {
        self.node1(NodeKind::SrLatch, &[s, r])
    }

    pub fn ram(&mut self, addr: NetId, we: NetId, wdata: NetId, init: &[u64]) -> NetId {
        let addr_bits = self.width(addr);
        let w = self.width(wdata);
        self.node1(
            NodeKind::Ram {
                addr_bits,
                w,
                init: Arc::from(init),
            },
            &[addr, we, wdata],
        )
    }

    /// A screen; `inputs` are its pins in order (see
    /// [`super::ScreenMode`]), `None` for unconnected. Returns the node.
    pub fn screen(
        &mut self,
        mode: super::ScreenMode,
        x_bits: u8,
        y_bits: u8,
        w: u8,
        inputs: &[Option<NetId>],
    ) -> NodeId {
        self.node(
            NodeKind::Screen {
                mode,
                x_bits,
                y_bits,
                w,
            },
            inputs,
        )
    }

    /// Add a state element with its inputs unconnected, for building
    /// feedback: use its output, then [`Self::connect`] its inputs later.
    /// Returns `(node, q)`.
    pub fn state_placeholder(&mut self, kind: NodeKind) -> (NodeId, NetId) {
        let id = self.node(kind, &[]);
        (id, self.out(id, 0))
    }

    /// A `w`-bit up-counter with synchronous reset: on each tick, `reset`
    /// clears it, otherwise `en` increments it (wrapping). Returns the count.
    pub fn counter(&mut self, w: u8, en: NetId, reset: NetId) -> NetId {
        let (reg, q) = self.state_placeholder(NodeKind::Register { w });
        let next = self.inc(q);
        let zero = self.zero(w);
        let d = self.mux(reset, next, zero);
        let load = self.or(en, reset);
        self.connect(load, (reg, 0));
        self.connect(d, (reg, 1));
        q
    }
}
