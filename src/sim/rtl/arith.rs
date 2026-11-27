//! Arithmetic, shift, comparison and selection cells, built from word
//! primitives (`Add`, `LtU`, `Mux`, `Switch`, ...). See
//! `docs/rtl-import.md` for how each one is composed.

use super::build::{CellIn, Op, const_word, konst, zeros};
use super::{Bit, GateKind, ModuleBuilder, RtlError, err, fit, width};

/// The cell types this module maps.
const CELLS: &[&str] = &[
    "$neg",
    "$mul",
    "$div",
    "$mod",
    "$alu",
    "$shl",
    "$shr",
    "$sshl",
    "$sshr",
    "$shift",
    "$shiftx",
    "$lt",
    "$le",
    "$gt",
    "$ge",
    "$logic_and",
    "$logic_or",
    "$pmux",
    "$bmux",
    "$demux",
    "$bwmux",
    "$bweqx",
    "$slice",
    "$concat",
    "$lut",
    "$_AOI3_",
    "$_OAI3_",
    "$_AOI4_",
    "$_OAI4_",
];

/// Most select bits of a `$bmux` or `$demux` (2^10 words).
const MAX_SELECT_BITS: usize = 10;

pub(super) fn handles(ty: &str) -> bool {
    CELLS.contains(&ty)
}

/// Why a related cell type is refused, and what to do instead.
pub(super) fn refusal(ty: &str) -> Option<&'static str> {
    Some(match ty {
        "$divfloor" | "$modfloor" => {
            "floored division is not mapped; Verilog `/` and `%` (truncating, $div and $mod) are"
        }
        "$pow" => "write a shift for a power of two, or a multiplication",
        "$macc" | "$macc_v2" => "run `maccmap -unmap` to turn it back into $mul and $add",
        "$lcu" | "$sop" | "$fa" => "run `techmap` to lower it to gates",
        "$tribuf" | "$_TBUF_" => "the simulator has no tri-state wires",
        _ => return None,
    })
}

impl ModuleBuilder<'_> {
    pub(super) fn map_arith(
        &mut self,
        c: &CellIn,
        name: String,
        label: Option<String>,
    ) -> Result<(), RtlError> {
        let ty = c.ty;
        let of = name.clone();
        let of = of.as_str();
        let signed = c.flag("A_SIGNED") && c.flag("B_SIGNED");
        match ty {
            "$neg" => {
                let y = c.conn("Y")?;
                width(y.len(), "Y")?;
                let a = fit(&c.conn("A")?, y.len(), c.flag("A_SIGNED"));
                let r = self.neg_v(of, &a);
                self.alias(&y, &r);
            }
            "$mul" => {
                let y = c.conn("Y")?;
                let n = width(y.len(), "Y")? as usize;
                let mut a = fit(&c.conn("A")?, n, signed);
                let mut b = fit(&c.conn("B")?, n, signed);
                // One stage per bit of b: put the operand with more
                // constant bits there (constant 0 bits need no stage).
                let constant = |v: &[Bit]| v.iter().filter(|&&x| konst(x).is_some()).count();
                if constant(&a) > constant(&b) {
                    std::mem::swap(&mut a, &mut b);
                }
                let r = self.multiply(of, &a, &b);
                self.alias(&y, &r);
            }
            "$div" | "$mod" => {
                let y = c.conn("Y")?;
                let ny = width(y.len(), "Y")? as usize;
                let (a, b) = (c.conn("A")?, c.conn("B")?);
                let n = width(a.len().max(b.len()), "A")? as usize;
                let (ua, ub, sa, sb) = if signed {
                    let a = fit(&a, n, true);
                    let b = fit(&b, n, true);
                    let (sa, sb) = (a[n - 1], b[n - 1]);
                    (self.cond_neg(of, &a, sa), self.cond_neg(of, &b, sb), sa, sb)
                } else {
                    (fit(&a, n, false), fit(&b, n, false), Bit::Zero, Bit::Zero)
                };
                let (q, r) = self.divide(of, &ua, &ub);
                let out = if signed {
                    // The quotient's sign is sa ^ sb, the remainder's sa
                    // (truncating division, like Verilog).
                    let (res, neg) = if ty == "$div" {
                        (q, self.bitwise(of, Op::Xor, &[sa], &[sb])[0])
                    } else {
                        (r, sa)
                    };
                    let m = if ny > n { (n + 1).min(64) } else { n };
                    let wide = fit(&res, m, false);
                    let v = self.cond_neg(of, &wide, neg);
                    fit(&v, ny, true)
                } else {
                    fit(if ty == "$div" { &q } else { &r }, ny, false)
                };
                self.alias(&y, &out);
            }
            "$alu" => {
                let y = c.conn("Y")?;
                let n = width(y.len(), "Y")? as usize;
                let a = fit(&c.conn("A")?, n, signed);
                let b = fit(&c.conn("B")?, n, signed);
                let (bi, ci) = (c.conn1("BI")?, c.conn1("CI")?);
                let b = self.bitwise(of, Op::Xor, &b, &vec![bi; n]);
                let (sum, cout) = self.add_v(of, &a, &b, ci);
                self.alias(&y, &sum);
                // X = A ^ B; CO[i] is the carry out of bit i, and the
                // carry into bit i is Y[i] ^ X[i].
                let x = self.bitwise(of, Op::Xor, &a, &b);
                let carries = self.bitwise(of, Op::Xor, &sum, &x);
                let mut co: Vec<Bit> = carries[1..].to_vec();
                co.push(cout);
                let (xs, cos) = (c.conn_w("X", n)?, c.conn_w("CO", n)?);
                self.alias(&xs, &x);
                self.alias(&cos, &co);
            }
            "$shl" | "$sshl" => {
                // Both: A extended to Y (by its own signedness), then
                // shifted left by the unsigned B.
                let y = c.conn("Y")?;
                let n = width(y.len(), "Y")? as usize;
                let a = fit(&c.conn("A")?, n, c.flag("A_SIGNED"));
                let r = self.shift(of, a, Bit::Zero, &c.conn("B")?, false, n);
                self.alias(&y, &r);
            }
            "$shr" | "$sshr" => {
                let y = c.conn("Y")?;
                let n = width(y.len(), "Y")? as usize;
                let a = c.conn("A")?;
                width(a.len(), "A")?;
                let a_signed = c.flag("A_SIGNED");
                let (src, fill) = if ty == "$sshr" && a_signed {
                    let msb = *a.last().expect("A has bits");
                    (a, msb)
                } else {
                    (fit(&a, n.max(a.len()), a_signed), Bit::Zero)
                };
                let r = self.shift(of, src, fill, &c.conn("B")?, true, n);
                self.alias(&y, &r);
            }
            "$shift" | "$shiftx" => {
                // Y[i] = A[i + B]; B may be signed (negative: left).
                // Bits from outside A are 0 (`$shiftx`: x, read as 0).
                let y = c.conn("Y")?;
                let n = width(y.len(), "Y")? as usize;
                let a = c.conn("A")?;
                width(a.len(), "A")?;
                let src = if ty == "$shift" {
                    fit(&a, n.max(a.len()), c.flag("A_SIGNED"))
                } else {
                    a
                };
                let b = c.conn("B")?;
                width(b.len(), "B")?;
                let r = match b.split_last() {
                    Some((&msb, rest)) if c.flag("B_SIGNED") && msb != Bit::Zero => {
                        let right = self.shift(of, src.clone(), Bit::Zero, rest, true, n);
                        let magnitude = self.neg_v(of, &b);
                        let left = self.shift(of, src, Bit::Zero, &magnitude, false, n);
                        self.mux_v(of, msb, &right, &left)
                    }
                    _ => self.shift(of, src, Bit::Zero, &b, true, n),
                };
                self.alias(&y, &r);
            }
            "$lt" | "$le" | "$gt" | "$ge" => {
                let y = c.conn("Y")?;
                let (a, b) = (c.conn("A")?, c.conn("B")?);
                let n = width(a.len().max(b.len()), "A")? as usize;
                let mut a = fit(&a, n, signed);
                let mut b = fit(&b, n, signed);
                if signed {
                    // Signed order is unsigned order with the sign bits
                    // inverted.
                    a[n - 1] = self.not1(of, a[n - 1]);
                    b[n - 1] = self.not1(of, b[n - 1]);
                }
                let (x, z, invert) = match ty {
                    "$lt" => (a, b, false),
                    "$ge" => (a, b, true),
                    "$gt" => (b, a, false),
                    _ => (b, a, true),
                };
                let r = self.lt_v(of, &x, &z);
                self.result_bit(of, vec![r], &y, invert);
            }
            "$logic_and" | "$logic_or" => {
                let y = c.conn("Y")?;
                let a = self.any(of, &c.conn("A")?);
                let b = self.any(of, &c.conn("B")?);
                let r = if ty == "$logic_and" {
                    self.and1(of, a, b)
                } else {
                    self.or1(of, a, b)
                };
                self.result_bit(of, vec![r], &y, false);
            }
            "$pmux" => {
                // Y = B[i] for the set bit i of S, A when none is. With
                // several set (x in Yosys), the lowest wins.
                let y = c.conn("Y")?;
                let w = width(y.len(), "Y")? as usize;
                let a = c.conn_w("A", w)?;
                let s = c.conn("S")?;
                let b = c.conn_w("B", w * s.len())?;
                let mut r = a;
                for (i, &sel) in s.iter().enumerate().rev() {
                    r = self.mux_v(of, sel, &r, &b[i * w..(i + 1) * w]);
                }
                self.alias(&y, &r);
            }
            "$bmux" => {
                let y = c.conn("Y")?;
                let w = width(y.len(), "Y")? as usize;
                let s = c.conn("S")?;
                if s.len() > MAX_SELECT_BITS {
                    return err(format!(
                        "$bmux: {} select bits; at most {MAX_SELECT_BITS} are supported",
                        s.len()
                    ));
                }
                let a = c.conn_w("A", w << s.len())?;
                let words = a.chunks(w).map(<[Bit]>::to_vec).collect();
                let r = self.select(of, words, &s);
                self.alias(&y, &r);
            }
            "$demux" => {
                let a = c.conn("A")?;
                let w = width(a.len(), "A")? as usize;
                let s = c.conn("S")?;
                if s.len() > MAX_SELECT_BITS {
                    return err(format!(
                        "$demux: {} select bits; at most {MAX_SELECT_BITS} are supported",
                        s.len()
                    ));
                }
                let y = c.conn_w("Y", w << s.len())?;
                for i in 0..1usize << s.len() {
                    let en = match s.as_slice() {
                        [] => Bit::One,
                        [one] if i == 1 => *one,
                        [one] => self.not1(of, *one),
                        _ => self.eq_v(of, &s, &const_word(i as u64, s.len())),
                    };
                    let out = self.switch_v(of, en, &a);
                    self.alias(&y[i * w..(i + 1) * w], &out);
                }
            }
            "$bwmux" => {
                let y = c.conn("Y")?;
                let w = width(y.len(), "Y")? as usize;
                let (a, b, s) = (c.conn_w("A", w)?, c.conn_w("B", w)?, c.conn_w("S", w)?);
                let ns = self.not_v(of, &s);
                let x = self.bitwise(of, Op::And, &a, &ns);
                let z = self.bitwise(of, Op::And, &b, &s);
                let r = self.bitwise(of, Op::Or, &x, &z);
                self.alias(&y, &r);
            }
            "$bweqx" => {
                let y = c.conn("Y")?;
                let w = width(y.len(), "Y")?;
                let (a, b) = (c.conn_w("A", w.into())?, c.conn_w("B", w.into())?);
                self.add_node(
                    name,
                    label,
                    GateKind::Xnor { w },
                    vec![Some(a), Some(b)],
                    Some(vec![y]),
                    false,
                );
            }
            "$slice" => {
                let y = c.conn("Y")?;
                let a = c.conn("A")?;
                let offset = c.param("OFFSET")? as usize;
                if offset + y.len() > a.len() {
                    return err("$slice: OFFSET + Y_WIDTH is past the end of A");
                }
                self.alias(&y, &a[offset..offset + y.len()]);
            }
            "$concat" => {
                let y = c.conn("Y")?;
                let mut v = c.conn("A")?;
                v.extend(c.conn("B")?);
                self.alias(&y, &v);
            }
            "$lut" => {
                let a = c.conn("A")?;
                if !(1..=20).contains(&a.len()) {
                    return err(format!("$lut: {} inputs; 1 to 20 are supported", a.len()));
                }
                let y = c.conn_w("Y", 1)?;
                let table = c.param_bits("LUT", 1 << a.len())?;
                let mut contents: Vec<u64> = table.iter().map(|&b| u64::from(b)).collect();
                while contents.last() == Some(&0) {
                    contents.pop();
                }
                let kind = GateKind::Rom {
                    addr_bits: a.len() as u8,
                    w: 1,
                    contents,
                };
                self.add_node(name, label, kind, vec![Some(a)], Some(vec![y]), false);
            }
            "$_AOI3_" | "$_OAI3_" | "$_AOI4_" | "$_OAI4_" => {
                let (a, b, cc) = (c.conn1("A")?, c.conn1("B")?, c.conn1("C")?);
                let and = ty.starts_with("$_A");
                let pair = |s: &mut Self, x, z| {
                    if and {
                        s.and1(of, x, z)
                    } else {
                        s.or1(of, x, z)
                    }
                };
                let left = pair(self, a, b);
                let right = if ty.ends_with("4_") {
                    let d = c.conn1("D")?;
                    pair(self, cc, d)
                } else {
                    cc
                };
                let kind = if and {
                    GateKind::Nor { w: 1 }
                } else {
                    GateKind::Nand { w: 1 }
                };
                let y = c.conn_w("Y", 1)?;
                self.add_node(
                    name,
                    label,
                    kind,
                    vec![Some(vec![left]), Some(vec![right])],
                    Some(vec![y]),
                    false,
                );
            }
            _ => unreachable!("not an arithmetic cell: {ty}"),
        }
        Ok(())
    }

    /// `a * b` modulo 2^n (both n bits): shift and add, one `Switch` and
    /// one `Add` per bit of `b` that is not constant 0. Stage i only adds
    /// into bits i and up.
    fn multiply(&mut self, of: &str, a: &[Bit], b: &[Bit]) -> Vec<Bit> {
        let n = a.len();
        let mut acc: Option<Vec<Bit>> = None;
        for (i, &bit) in b.iter().enumerate() {
            if bit == Bit::Zero {
                continue;
            }
            let partial = self.switch_v(of, bit, &a[..n - i]);
            acc = Some(match acc {
                None => {
                    let mut v = zeros(i);
                    v.extend(partial);
                    v
                }
                Some(mut acc) => {
                    let (sum, _) = self.add_v(of, &acc[i..], &partial, Bit::Zero);
                    acc.truncate(i);
                    acc.extend(sum);
                    acc
                }
            });
        }
        acc.unwrap_or_else(|| zeros(n))
    }

    /// Unsigned restoring division of two n-bit words: (quotient,
    /// remainder). One `Add`, one `Or` and one `Mux` per quotient bit.
    /// Dividing by 0 gives an all-ones quotient and remainder `a`.
    fn divide(&mut self, of: &str, a: &[Bit], b: &[Bit]) -> (Vec<Bit>, Vec<Bit>) {
        let n = a.len();
        let nb = self.not_v(of, b);
        let mut r = zeros(n);
        let mut q = zeros(n);
        for i in (0..n).rev() {
            // (r << 1 | a[i]) is n + 1 bits: `top` and `low`.
            let top = r[n - 1];
            let mut low = vec![a[i]];
            low.extend_from_slice(&r[..n - 1]);
            let (diff, no_borrow) = self.add_v(of, &low, &nb, Bit::One);
            let ge = self.or1(of, top, no_borrow);
            q[i] = ge;
            r = self.mux_v(of, ge, &low, &diff);
        }
        (q, r)
    }

    /// A barrel shifter: `out_len` bits of `src` shifted by the unsigned
    /// `amount`, one `Mux` per amount bit (none for constant bits).
    /// Right: `out[i] = src[i + amount]`, `fill` past the top. Left:
    /// `out[i] = src[i - amount]`, 0 below the bottom.
    fn shift(
        &mut self,
        of: &str,
        src: Vec<Bit>,
        fill: Bit,
        amount: &[Bit],
        right: bool,
        out_len: usize,
    ) -> Vec<Bit> {
        let n = if right {
            src.len().max(out_len)
        } else {
            out_len
        };
        let mut cur = src;
        cur.resize(n, fill);
        let mut over = Vec::new();
        for (k, &bit) in amount.iter().enumerate() {
            if bit == Bit::Zero {
                continue;
            }
            let step = if k < 32 { 1usize << k } else { usize::MAX };
            if step >= n {
                over.push(bit);
                continue;
            }
            let shifted: Vec<Bit> = (0..n)
                .map(|j| match right {
                    true if j + step < n => cur[j + step],
                    true => fill,
                    false if j >= step => cur[j - step],
                    false => Bit::Zero,
                })
                .collect();
            cur = self.mux_v(of, bit, &cur, &shifted);
        }
        if !over.is_empty() {
            let o = self.any(of, &over);
            let f = if right { fill } else { Bit::Zero };
            cur = self.mux_v(of, o, &cur, &vec![f; n]);
        }
        cur.truncate(out_len);
        cur
    }

    /// `words[sel]`: a tree of muxes, one level per select bit, each level
    /// one word-wide mux (in 64-bit pieces) over all pairs.
    pub(super) fn select(&mut self, of: &str, mut words: Vec<Vec<Bit>>, sel: &[Bit]) -> Vec<Bit> {
        let w = words[0].len();
        for &bit in sel {
            let evens: Vec<Bit> = words.iter().step_by(2).flatten().copied().collect();
            let odds: Vec<Bit> = words.iter().skip(1).step_by(2).flatten().copied().collect();
            let m = self.mux_v(of, bit, &evens, &odds);
            words = m.chunks(w).map(<[Bit]>::to_vec).collect();
        }
        words.swap_remove(0)
    }
}
