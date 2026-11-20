//! Building blocks for cells that become several primitives: a cell's
//! parameters and connections ([`CellIn`]), and helper nodes on bit lists
//! that fold constants away where they can.

use std::cell::Cell as Count;
use std::collections::HashSet;

use serde_json::Value;

use super::{Bit, Cell, GateKind, ModuleBuilder, RtlError, int_value, parse_bits};

/// The widest word one primitive takes.
pub(super) const WORD: usize = 64;

/// A cell's parameters and connections.
pub(super) struct CellIn<'a> {
    pub ty: &'a str,
    params: Option<&'a Value>,
    conns: Option<&'a Value>,
    /// `x` and `z` bits read (as 0) so far.
    pub xs: Count<usize>,
}

impl<'a> CellIn<'a> {
    pub fn new(ty: &'a str, params: Option<&'a Value>, conns: Option<&'a Value>) -> Self {
        CellIn {
            ty,
            params,
            conns,
            xs: Count::new(0),
        }
    }

    /// The bits connected to `port`.
    pub fn conn(&self, port: &str) -> Result<Vec<Bit>, RtlError> {
        let mut xs = self.xs.get();
        let bits = parse_bits(self.conns.and_then(|c| c.get(port)), port, &mut xs);
        self.xs.set(xs);
        bits
    }

    /// The bits connected to `port`, which must be `w` wide.
    pub fn conn_w(&self, port: &str, w: usize) -> Result<Vec<Bit>, RtlError> {
        let bits = self.conn(port)?;
        if bits.len() != w {
            return Err(RtlError(format!(
                "{}: {port} is {} bits wide, expected {w}",
                self.ty,
                bits.len()
            )));
        }
        Ok(bits)
    }

    /// The one bit connected to `port`.
    pub fn conn1(&self, port: &str) -> Result<Bit, RtlError> {
        Ok(self.conn_w(port, 1)?[0])
    }

    fn raw(&self, key: &str) -> Option<&'a Value> {
        self.params.and_then(|p| p.get(key))
    }

    pub fn param(&self, key: &str) -> Result<u64, RtlError> {
        self.raw(key)
            .and_then(int_value)
            .ok_or_else(|| RtlError(format!("{}: missing or bad parameter {key}", self.ty)))
    }

    pub fn param_or(&self, key: &str, default: u64) -> u64 {
        self.raw(key).and_then(int_value).unwrap_or(default)
    }

    pub fn flag(&self, key: &str) -> bool {
        self.param_or(key, 0) != 0
    }

    pub fn param_str(&self, key: &str) -> Option<&'a str> {
        self.raw(key).and_then(Value::as_str)
    }

    /// A parameter as `w` bits, least significant first. A missing
    /// parameter is all 0; `x` and `z` read as 0 (in a parameter they mean
    /// "no value", such as a memory without initial contents, so they are
    /// not counted as `x` bits read); a shorter value is zero-extended.
    pub fn param_bits(&self, key: &str, w: usize) -> Result<Vec<bool>, RtlError> {
        let mut out = vec![false; w];
        match self.raw(key) {
            None => {}
            Some(Value::String(s))
                if s.chars()
                    .all(|c| matches!(c, '0' | '1' | 'x' | 'z' | 'X' | 'Z')) =>
            {
                for (i, c) in s.chars().rev().enumerate().take(w) {
                    out[i] = c == '1';
                }
            }
            Some(v) => {
                let n = int_value(v)
                    .ok_or_else(|| RtlError(format!("{}: bad parameter {key}: {v}", self.ty)))?;
                for (i, bit) in out.iter_mut().enumerate().take(64) {
                    *bit = n >> i & 1 == 1;
                }
            }
        }
        Ok(out)
    }
}

/// The value of a constant bit.
pub(super) fn konst(b: Bit) -> Option<bool> {
    match b {
        Bit::Zero => Some(false),
        Bit::One => Some(true),
        Bit::Net(_) => None,
    }
}

pub(super) fn zeros(n: usize) -> Vec<Bit> {
    vec![Bit::Zero; n]
}

pub(super) fn const_bits(bits: &[bool]) -> Vec<Bit> {
    bits.iter()
        .map(|&b| if b { Bit::One } else { Bit::Zero })
        .collect()
}

/// `value` as `w` constant bits.
pub(super) fn const_word(value: u64, w: usize) -> Vec<Bit> {
    (0..w)
        .map(|i| {
            if i < 64 && value >> i & 1 == 1 {
                Bit::One
            } else {
                Bit::Zero
            }
        })
        .collect()
}

fn all(bits: &[Bit], b: Bit) -> bool {
    bits.iter().all(|&x| x == b)
}

/// A bitwise operation of two words.
#[derive(Clone, Copy, PartialEq)]
pub(super) enum Op {
    And,
    Or,
    Xor,
}

impl ModuleBuilder<'_> {
    /// Add a node. `inputs` has one entry per input pin (`None`: left
    /// unconnected, reads 0); `outputs` gives the bits each output pin
    /// drives, or fresh nets when `None`. Returns the output bits.
    pub(super) fn add_node(
        &mut self,
        name: String,
        label: Option<String>,
        kind: GateKind,
        inputs: Vec<Option<Vec<Bit>>>,
        outputs: Option<Vec<Vec<Bit>>>,
        helper: bool,
    ) -> Vec<Vec<Bit>> {
        let mut cell = Cell::prim(name, label, kind);
        debug_assert_eq!(cell.inputs.len(), inputs.len(), "{}", cell.name);
        for (pin, bits) in inputs.into_iter().enumerate() {
            if let Some(b) = &bits {
                debug_assert_eq!(b.len(), usize::from(cell.inputs[pin].1), "{}", cell.name);
            }
            cell.inputs[pin].2 = bits;
        }
        let outputs = outputs.unwrap_or_else(|| {
            cell.outputs
                .iter()
                .map(|o| usize::from(o.1))
                .collect::<Vec<_>>()
                .into_iter()
                .map(|w| self.fresh(w))
                .collect()
        });
        for (pin, bits) in outputs.iter().enumerate() {
            debug_assert_eq!(bits.len(), usize::from(cell.outputs[pin].1));
            cell.outputs[pin].2 = bits.clone();
        }
        cell.helper = helper;
        self.cells.push(cell);
        outputs
    }

    /// Add a helper node named after the cell `of`.
    pub(super) fn helper(
        &mut self,
        of: &str,
        what: &str,
        kind: GateKind,
        inputs: Vec<Option<Vec<Bit>>>,
    ) -> Vec<Vec<Bit>> {
        let name = self.names.fresh(&format!("{of}_{what}"));
        self.add_node(name, None, kind, inputs, None, true)
    }

    /// A helper node with every input connected; its first output.
    pub(super) fn helper1(
        &mut self,
        of: &str,
        what: &str,
        kind: GateKind,
        inputs: Vec<Vec<Bit>>,
    ) -> Vec<Bit> {
        let inputs = inputs.into_iter().map(Some).collect();
        self.helper(of, what, kind, inputs).swap_remove(0)
    }

    pub(super) fn not_v(&mut self, of: &str, a: &[Bit]) -> Vec<Bit> {
        if a.iter().all(|&b| konst(b).is_some()) {
            return a
                .iter()
                .map(|&b| if b == Bit::Zero { Bit::One } else { Bit::Zero })
                .collect();
        }
        let mut out = Vec::new();
        for chunk in a.chunks(WORD) {
            let w = chunk.len() as u8;
            out.extend(self.helper1(of, "not", GateKind::Not { w }, vec![chunk.to_vec()]));
        }
        out
    }

    pub(super) fn not1(&mut self, of: &str, a: Bit) -> Bit {
        self.not_v(of, &[a])[0]
    }

    /// `a op b`, bit by bit (the same length).
    pub(super) fn bitwise(&mut self, of: &str, op: Op, a: &[Bit], b: &[Bit]) -> Vec<Bit> {
        debug_assert_eq!(a.len(), b.len());
        let n = a.len();
        let (za, zb) = (all(a, Bit::Zero), all(b, Bit::Zero));
        let (oa, ob) = (all(a, Bit::One), all(b, Bit::One));
        let folded = match op {
            Op::And if za || zb => Some(zeros(n)),
            Op::Or if oa || ob => Some(vec![Bit::One; n]),
            Op::And if oa => Some(b.to_vec()),
            Op::And if ob => Some(a.to_vec()),
            Op::Or | Op::Xor if za => Some(b.to_vec()),
            Op::Or | Op::Xor if zb => Some(a.to_vec()),
            Op::Xor if oa => Some(self.not_v(of, b)),
            Op::Xor if ob => Some(self.not_v(of, a)),
            Op::And | Op::Or if a == b => Some(a.to_vec()),
            _ => None,
        };
        if let Some(v) = folded {
            return v;
        }
        let mut out = Vec::new();
        for (x, y) in a.chunks(WORD).zip(b.chunks(WORD)) {
            let w = x.len() as u8;
            let (kind, what) = match op {
                Op::And => (GateKind::And { w }, "and"),
                Op::Or => (GateKind::Or { w }, "or"),
                Op::Xor => (GateKind::Xor { w }, "xor"),
            };
            out.extend(self.helper1(of, what, kind, vec![x.to_vec(), y.to_vec()]));
        }
        out
    }

    pub(super) fn and1(&mut self, of: &str, a: Bit, b: Bit) -> Bit {
        self.bitwise(of, Op::And, &[a], &[b])[0]
    }

    pub(super) fn or1(&mut self, of: &str, a: Bit, b: Bit) -> Bit {
        self.bitwise(of, Op::Or, &[a], &[b])[0]
    }

    /// `sel ? b : a`, in words of up to 64 bits.
    pub(super) fn mux_v(&mut self, of: &str, sel: Bit, a: &[Bit], b: &[Bit]) -> Vec<Bit> {
        debug_assert_eq!(a.len(), b.len());
        match konst(sel) {
            Some(false) => return a.to_vec(),
            Some(true) => return b.to_vec(),
            None if a == b => return a.to_vec(),
            None => {}
        }
        let mut out = Vec::new();
        for (x, y) in a.chunks(WORD).zip(b.chunks(WORD)) {
            let w = x.len() as u8;
            out.extend(self.helper1(
                of,
                "mux",
                GateKind::Mux { w },
                vec![vec![sel], x.to_vec(), y.to_vec()],
            ));
        }
        out
    }

    /// `a + b + cin` (at most 64 bits): the sum and the carry out.
    pub(super) fn add_v(&mut self, of: &str, a: &[Bit], b: &[Bit], cin: Bit) -> (Vec<Bit>, Bit) {
        debug_assert_eq!(a.len(), b.len());
        let w = a.len() as u8;
        let cin = (cin != Bit::Zero).then(|| vec![cin]);
        let mut out = self.helper(
            of,
            "add",
            GateKind::Add { w },
            vec![Some(a.to_vec()), Some(b.to_vec()), cin],
        );
        let cout = out.pop().expect("carry out")[0];
        (out.swap_remove(0), cout)
    }

    /// `a == b` (at most 64 bits).
    pub(super) fn eq_v(&mut self, of: &str, a: &[Bit], b: &[Bit]) -> Bit {
        let w = a.len() as u8;
        self.helper1(of, "eq", GateKind::Eq { w }, vec![a.to_vec(), b.to_vec()])[0]
    }

    /// `a < b`, unsigned (at most 64 bits).
    pub(super) fn lt_v(&mut self, of: &str, a: &[Bit], b: &[Bit]) -> Bit {
        let w = a.len() as u8;
        self.helper1(of, "lt", GateKind::LtU { w }, vec![a.to_vec(), b.to_vec()])[0]
    }

    /// Whether any bit of `a` is 1.
    pub(super) fn any(&mut self, of: &str, a: &[Bit]) -> Bit {
        if a.contains(&Bit::One) {
            return Bit::One;
        }
        let nets: Vec<Bit> = a.iter().copied().filter(|&b| b != Bit::Zero).collect();
        match nets.as_slice() {
            [] => Bit::Zero,
            [one] => *one,
            _ => {
                let mut parts = Vec::new();
                for chunk in nets.chunks(WORD) {
                    let w = chunk.len() as u8;
                    parts.push(
                        self.helper1(of, "any", GateKind::ReduceOr { w }, vec![chunk.to_vec()])[0],
                    );
                }
                parts
                    .into_iter()
                    .reduce(|x, y| self.or1(of, x, y))
                    .expect("one part")
            }
        }
    }

    /// `en ? d : 0`.
    pub(super) fn switch_v(&mut self, of: &str, en: Bit, d: &[Bit]) -> Vec<Bit> {
        match konst(en) {
            Some(false) => zeros(d.len()),
            Some(true) => d.to_vec(),
            None => {
                let mut out = Vec::new();
                for chunk in d.chunks(WORD) {
                    let w = chunk.len() as u8;
                    out.extend(self.helper1(
                        of,
                        "gate",
                        GateKind::Switch { w },
                        vec![vec![en], chunk.to_vec()],
                    ));
                }
                out
            }
        }
    }

    /// `-a` in two's complement, the width of `a`.
    pub(super) fn neg_v(&mut self, of: &str, a: &[Bit]) -> Vec<Bit> {
        let na = self.not_v(of, a);
        self.add_v(of, &na, &zeros(a.len()), Bit::One).0
    }

    /// `s ? -a : a`.
    pub(super) fn cond_neg(&mut self, of: &str, a: &[Bit], s: Bit) -> Vec<Bit> {
        match konst(s) {
            Some(false) => a.to_vec(),
            Some(true) => self.neg_v(of, a),
            None => {
                let x = self.bitwise(of, Op::Xor, a, &vec![s; a.len()]);
                self.add_v(of, &x, &zeros(a.len()), s).0
            }
        }
    }

    /// Make `active_high` true by inverting `b` if needed.
    pub(super) fn polarity(&mut self, of: &str, b: Bit, active_high: bool) -> Bit {
        if active_high { b } else { self.not1(of, b) }
    }

    /// Remove helper nodes whose outputs nothing reads (until none is left).
    pub(super) fn prune_helpers(&mut self) {
        loop {
            let mut used: HashSet<u64> = HashSet::new();
            for cell in &self.cells {
                for bits in cell.inputs.iter().filter_map(|i| i.2.as_ref()) {
                    for &bit in bits {
                        let mut bit = bit;
                        for _ in 0..=self.alias.len() {
                            let Bit::Net(n) = bit else { break };
                            if !used.insert(n) {
                                break;
                            }
                            match self.alias.get(&n) {
                                Some(&next) => bit = next,
                                None => break,
                            }
                        }
                    }
                }
            }
            let before = self.cells.len();
            self.cells.retain(|c| {
                !c.helper
                    || c.outputs.iter().any(|o| {
                        o.2.iter()
                            .any(|b| matches!(b, Bit::Net(n) if used.contains(n)))
                    })
            });
            if self.cells.len() == before {
                return;
            }
        }
    }
}
