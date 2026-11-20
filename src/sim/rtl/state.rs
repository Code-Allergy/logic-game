//! Flip-flops with resets, enables and loads, latches, and memories.
//!
//! Every flip-flop is clocked by the global tick (its clock pin is
//! ignored). Asynchronous resets, sets and loads are approximated: the
//! reset value shows on the output at once (a `Mux`, or `Or`/`And` for
//! per-bit set and clear, after the state element) and is stored at the
//! next tick (the same logic before it). See `docs/rtl-import.md`.

use super::build::{CellIn, Op, const_bits, const_word, konst, zeros};
use super::{Bit, GateKind, ModuleBuilder, RtlError, err, fit, strip, width};

/// Word-level cell types this module maps (`$dff` and `$dffe` are mapped
/// by the importer itself).
const CELLS: &[&str] = &[
    "$ff",
    "$adff",
    "$adffe",
    "$sdff",
    "$sdffe",
    "$sdffce",
    "$dffsr",
    "$dffsre",
    "$aldff",
    "$aldffe",
    "$dlatch",
    "$adlatch",
    "$dlatchsr",
    "$sr",
    "$mem",
    "$mem_v2",
];

/// Memories with a read address other than the write address are built
/// from registers up to this many words.
pub(super) const MAX_REGISTER_FILE: usize = 64;

pub(super) fn handles(ty: &str) -> bool {
    CELLS.contains(&ty) || gate_ff(ty).is_some()
}

pub(super) fn refusal(ty: &str) -> Option<&'static str> {
    Some(match ty {
        "$memrd" | "$memrd_v2" | "$memwr" | "$memwr_v2" | "$meminit" | "$meminit_v2" => {
            "run `memory_collect` (part of `memory -nomap`) to combine a memory's ports \
             into one $mem_v2 cell"
        }
        _ => return None,
    })
}

/// A gate-level flip-flop or latch type (`$_DFF_PN0_`, `$_SDFFE_PP1N_`,
/// `$_DLATCH_P_`, ...): its family and the polarity (P/N) and value (0/1)
/// letters.
struct GateFf {
    family: &'static str,
    code: Vec<char>,
}

fn gate_ff(ty: &str) -> Option<GateFf> {
    if ty == "$_FF_" {
        return Some(GateFf {
            family: "FF",
            code: Vec::new(),
        });
    }
    let (family, code) = ty.strip_prefix("$_")?.strip_suffix('_')?.split_once('_')?;
    // P/N at each position, except 'v' where a reset value 0/1 goes.
    let pattern = match (family, code.len()) {
        ("DFF", 1) => "p",
        ("DFF", 3) | ("SDFF", 3) => "ppv",
        ("DFFE", 2) => "pp",
        ("DFFE", 4) | ("SDFFE", 4) | ("SDFFCE", 4) => "ppvp",
        ("DFFSR", 3) => "ppp",
        ("DFFSRE", 4) => "pppp",
        ("ALDFF", 2) => "pp",
        ("ALDFFE", 3) => "ppp",
        ("DLATCH", 1) => "p",
        ("DLATCH", 3) => "ppv",
        ("DLATCHSR", 3) => "ppp",
        ("SR", 2) => "pp",
        _ => return None,
    };
    let code: Vec<char> = code.chars().collect();
    let ok = pattern.chars().zip(&code).all(|(p, c)| match p {
        'p' => matches!(c, 'P' | 'N'),
        _ => matches!(c, '0' | '1'),
    });
    let family = [
        "DFF", "SDFF", "DFFE", "SDFFE", "SDFFCE", "DFFSR", "DFFSRE", "ALDFF", "ALDFFE", "DLATCH",
        "DLATCHSR", "SR",
    ]
    .into_iter()
    .find(|f| *f == family)?;
    ok.then_some(GateFf { family, code })
}

/// A flip-flop or latch, with every control made active high.
struct Ff {
    q: Vec<Bit>,
    /// `None` for `$sr` (it only holds).
    d: Option<Vec<Bit>>,
    /// Takes `d` at the tick (a flip-flop), or whenever `latch` is 1.
    latch: Option<Bit>,
    /// Has a clock pin (everything but `$ff` and latches).
    clock_pin: bool,
    en: Option<Bit>,
    /// Synchronous reset and its value.
    srst: Option<(Bit, Vec<Bit>)>,
    /// The enable gates the synchronous reset (`$sdffce`).
    ce_over_srst: bool,
    /// Asynchronous reset (a constant value) or load (`AD`).
    arst: Option<(Bit, Vec<Bit>)>,
    /// Asynchronous per-bit set and clear (clear wins).
    sr: Option<(Vec<Bit>, Vec<Bit>)>,
}

impl ModuleBuilder<'_> {
    pub(super) fn map_state(
        &mut self,
        c: &CellIn,
        name: String,
        label: Option<String>,
    ) -> Result<(), RtlError> {
        if matches!(c.ty, "$mem" | "$mem_v2") {
            return self.map_memory(c, name, label);
        }
        let ff = match gate_ff(c.ty) {
            Some(g) => self.gate_level_ff(c, &name, g)?,
            None => self.word_level_ff(c, &name)?,
        };
        self.emit_ff(name, label, ff);
        Ok(())
    }

    fn word_level_ff(&mut self, c: &CellIn, of: &str) -> Result<Ff, RtlError> {
        let ty = c.ty;
        let q = c.conn("Q")?;
        let w = width(q.len(), "Q")? as usize;
        let latch_type = matches!(ty, "$dlatch" | "$adlatch" | "$dlatchsr");
        let clock_pin = !latch_type && !matches!(ty, "$ff" | "$sr");
        if clock_pin {
            c.conn("CLK")?;
        }
        let pol = |key: &str| c.param_or(key, 1) != 0;
        let d = if ty == "$sr" {
            None
        } else {
            Some(c.conn_w("D", w)?)
        };
        let mut ff = Ff {
            q,
            d,
            latch: None,
            clock_pin,
            en: None,
            srst: None,
            ce_over_srst: ty == "$sdffce",
            arst: None,
            sr: None,
        };
        if latch_type {
            ff.latch = Some(self.polarity(of, c.conn1("EN")?, pol("EN_POLARITY")));
        } else if matches!(ty, "$adffe" | "$sdffe" | "$sdffce" | "$dffsre" | "$aldffe") {
            ff.en = Some(self.polarity(of, c.conn1("EN")?, pol("EN_POLARITY")));
        }
        match ty {
            "$adff" | "$adffe" | "$adlatch" => {
                let r = self.polarity(of, c.conn1("ARST")?, pol("ARST_POLARITY"));
                ff.arst = Some((r, const_bits(&c.param_bits("ARST_VALUE", w)?)));
            }
            "$aldff" | "$aldffe" => {
                let l = self.polarity(of, c.conn1("ALOAD")?, pol("ALOAD_POLARITY"));
                ff.arst = Some((l, c.conn_w("AD", w)?));
            }
            "$sdff" | "$sdffe" | "$sdffce" => {
                let r = self.polarity(of, c.conn1("SRST")?, pol("SRST_POLARITY"));
                ff.srst = Some((r, const_bits(&c.param_bits("SRST_VALUE", w)?)));
            }
            "$dffsr" | "$dffsre" | "$dlatchsr" | "$sr" => {
                let mut set = c.conn_w("SET", w)?;
                let mut clr = c.conn_w("CLR", w)?;
                if !pol("SET_POLARITY") {
                    set = self.not_v(of, &set);
                }
                if !pol("CLR_POLARITY") {
                    clr = self.not_v(of, &clr);
                }
                ff.sr = Some((set, clr));
            }
            _ => {}
        }
        Ok(ff)
    }

    fn gate_level_ff(&mut self, c: &CellIn, of: &str, g: GateFf) -> Result<Ff, RtlError> {
        let high = |i: usize| g.code[i] == 'P';
        let value = |i: usize| {
            vec![if g.code[i] == '1' {
                Bit::One
            } else {
                Bit::Zero
            }]
        };
        let latch_type = g.family.starts_with("DLATCH");
        let clock_pin = !latch_type && !matches!(g.family, "FF" | "SR");
        if clock_pin {
            c.conn1("C")?;
        }
        let q = c.conn_w("Q", 1)?;
        let d = if g.family == "SR" {
            None
        } else {
            Some(c.conn_w("D", 1)?)
        };
        let mut ff = Ff {
            q,
            d,
            latch: None,
            clock_pin,
            en: None,
            srst: None,
            ce_over_srst: g.family == "SDFFCE",
            arst: None,
            sr: None,
        };
        if latch_type {
            ff.latch = Some(self.polarity(of, c.conn1("E")?, high(0)));
        }
        // The position of the enable letter, if the type has one.
        let en_at = match (g.family, g.code.len()) {
            ("DFFE", 2) => Some(1),
            ("DFFE", 4) | ("SDFFE", _) | ("SDFFCE", _) | ("DFFSRE", _) => Some(3),
            ("ALDFFE", _) => Some(2),
            _ => None,
        };
        if let Some(i) = en_at {
            ff.en = Some(self.polarity(of, c.conn1("E")?, high(i)));
        }
        match (g.family, g.code.len()) {
            ("DFF", 3) | ("DFFE", 4) => {
                ff.arst = Some((self.polarity(of, c.conn1("R")?, high(1)), value(2)));
            }
            ("DLATCH", 3) => {
                ff.arst = Some((self.polarity(of, c.conn1("R")?, high(1)), value(2)));
            }
            ("SDFF", _) | ("SDFFE", _) | ("SDFFCE", _) => {
                ff.srst = Some((self.polarity(of, c.conn1("R")?, high(1)), value(2)));
            }
            ("ALDFF", _) | ("ALDFFE", _) => {
                let l = self.polarity(of, c.conn1("L")?, high(1));
                ff.arst = Some((l, c.conn_w("AD", 1)?));
            }
            ("DFFSR", _) | ("DFFSRE", _) | ("DLATCHSR", _) | ("SR", _) => {
                let first = usize::from(g.family != "SR");
                let s = self.polarity(of, c.conn1("S")?, high(first));
                let r = self.polarity(of, c.conn1("R")?, high(first + 1));
                ff.sr = Some((vec![s], vec![r]));
            }
            _ => {}
        }
        Ok(ff)
    }

    /// `(x | set) & !clr`.
    fn set_clear(&mut self, of: &str, x: &[Bit], set: &[Bit], clr: &[Bit]) -> Vec<Bit> {
        let x = self.bitwise(of, Op::Or, x, set);
        let keep = self.not_v(of, clr);
        self.bitwise(of, Op::And, &x, &keep)
    }

    fn emit_ff(&mut self, name: String, label: Option<String>, mut ff: Ff) {
        let of = name.clone();
        let of = of.as_str();
        let w = ff.q.len();
        let wb = w as u8;
        // Controls that are constant inactive do nothing.
        if ff.en == Some(Bit::One) {
            ff.en = None;
        }
        ff.srst = ff.srst.filter(|(r, _)| *r != Bit::Zero);
        ff.arst = ff.arst.filter(|(r, _)| *r != Bit::Zero);
        ff.sr = ff
            .sr
            .filter(|(s, r)| s.iter().chain(r).any(|&b| b != Bit::Zero));
        if ff.arst.is_some() || ff.sr.is_some() {
            self.asyncs += 1;
        }

        if let Some(en) = ff.latch {
            match (ff.d, ff.sr) {
                (Some(mut d), None) => {
                    let mut en = en;
                    if let Some((r, v)) = &ff.arst {
                        en = self.or1(of, en, *r);
                        d = self.mux_v(of, *r, &d, v);
                    }
                    let kind = GateKind::DLatch { w: wb };
                    let inputs = vec![Some(vec![en]), Some(d)];
                    self.add_node(name, label, kind, inputs, Some(vec![ff.q]), false);
                }
                (d, Some((set, clr))) => {
                    // A latch with set and clear holds in a Delay.
                    let state = self.fresh(w);
                    let base = match d {
                        Some(d) => self.mux_v(of, en, &state, &d),
                        None => state.clone(),
                    };
                    let out = self.set_clear(of, &base, &set, &clr);
                    let inputs = vec![Some(out.clone())];
                    self.add_node(
                        name,
                        label,
                        GateKind::Delay { w: wb },
                        inputs,
                        Some(vec![state]),
                        false,
                    );
                    self.alias(&ff.q, &out);
                }
                (None, None) => unreachable!("a latch has D or set/clear"),
            }
            return;
        }
        if ff.clock_pin {
            self.clocks += 1;
        }
        let Some(mut d) = ff.d else {
            // `$sr`: holds; set and clear act at once.
            let (set, clr) = ff.sr.unwrap_or_else(|| (zeros(w), zeros(w)));
            let state = self.fresh(w);
            let out = self.set_clear(of, &state, &set, &clr);
            let inputs = vec![Some(out.clone())];
            self.add_node(
                name,
                label,
                GateKind::Delay { w: wb },
                inputs,
                Some(vec![state]),
                false,
            );
            self.alias(&ff.q, &out);
            return;
        };
        let mut en = ff.en;
        if let Some((r, v)) = &ff.srst {
            d = self.mux_v(of, *r, &d, v);
            if !ff.ce_over_srst {
                en = en.map(|e| self.or1(of, e, *r));
            }
        }
        let asynchronous = ff.arst.is_some() || ff.sr.is_some();
        let state = if asynchronous {
            self.fresh(w)
        } else {
            ff.q.clone()
        };
        let mut out = None;
        if let Some((set, clr)) = &ff.sr {
            let base = match en.take() {
                Some(e) => self.mux_v(of, e, &state, &d),
                None => d,
            };
            d = self.set_clear(of, &base, set, clr);
            out = Some(self.set_clear(of, &state, set, clr));
        } else if let Some((r, v)) = &ff.arst {
            d = self.mux_v(of, *r, &d, v);
            en = en.map(|e| self.or1(of, e, *r));
            out = Some(self.mux_v(of, *r, &state, v));
        }
        let (kind, inputs) = match en {
            Some(e) => (GateKind::Register { w: wb }, vec![Some(vec![e]), Some(d)]),
            None => (GateKind::Delay { w: wb }, vec![Some(d)]),
        };
        self.add_node(name, label, kind, inputs, Some(vec![state]), false);
        if let Some(out) = out {
            self.alias(&ff.q, &out);
        }
    }

    /// `$mem_v2` and `$mem`: a `Rom` per read port without write ports; a
    /// `Ram` when every read port reads at the write address; otherwise
    /// registers (up to [`MAX_REGISTER_FILE`] words).
    fn map_memory(
        &mut self,
        c: &CellIn,
        name: String,
        label: Option<String>,
    ) -> Result<(), RtlError> {
        let v2 = c.ty == "$mem_v2";
        let memid = c
            .param_str("MEMID")
            .map(|m| strip(m).to_string())
            .unwrap_or_else(|| name.clone());
        let of = name.clone();
        let of = of.as_str();
        let size = c.param("SIZE")? as usize;
        let abits = c.param("ABITS")? as usize;
        let w = width(c.param("WIDTH")? as usize, "WIDTH")? as usize;
        let nrd = c.param("RD_PORTS")? as usize;
        let nwr = c.param("WR_PORTS")? as usize;
        let refuse = |why: String| err(format!("memory {memid}: {why}"));
        if c.param_or("OFFSET", 0) != 0 {
            return refuse("addresses with an OFFSET are not supported".into());
        }
        if nwr > 1 {
            return refuse(format!("{nwr} write ports; at most one is supported"));
        }
        if c.param_bits("RD_WIDE_CONTINUATION", nrd)?.contains(&true)
            || c.param_bits("WR_WIDE_CONTINUATION", nwr)?.contains(&true)
        {
            return refuse("wide ports are not supported".into());
        }
        let need = (usize::BITS - size.saturating_sub(1).leading_zeros()).max(1) as usize;
        if need > 20 || size == 0 {
            return refuse(format!("{size} words; 1 to 2^20 are supported"));
        }
        if v2 {
            let resets = [c.conn("RD_ARST")?, c.conn("RD_SRST")?];
            if resets.iter().flatten().any(|&b| b != Bit::Zero)
                || c.param_bits("RD_INIT_VALUE", nrd * w)?.contains(&true)
            {
                return refuse(
                    "read ports with a reset or an initial value are not supported".into(),
                );
            }
        }
        let init_bits = c.param_bits("INIT", size * w)?;
        let init: Vec<u64> = init_bits
            .chunks(w)
            .map(|word| {
                word.iter()
                    .enumerate()
                    .fold(0u64, |v, (i, &b)| v | u64::from(b) << i)
            })
            .collect();
        let has_init = init.iter().any(|&v| v != 0);
        let rd_sync = c.param_bits("RD_CLK_ENABLE", nrd)?;
        let rd_transparent = if v2 {
            let mask = c.param_bits("RD_TRANSPARENCY_MASK", nrd * nwr)?;
            let x = c.param_bits("RD_COLLISION_X_MASK", nrd * nwr)?;
            (0..nrd)
                .map(|i| nwr == 1 && mask[i] && !x[i])
                .collect::<Vec<_>>()
        } else {
            c.param_bits("RD_TRANSPARENT", nrd)?
        };
        let rd_en = c.conn_w("RD_EN", nrd)?;
        let rd_addr = c.conn_w("RD_ADDR", nrd * abits)?;
        let rd_data = c.conn_w("RD_DATA", nrd * w)?;
        let addr_of = |i: usize| &rd_addr[i * abits..(i + 1) * abits];
        if nrd == 0 {
            // Nothing reads it.
            return Ok(());
        }
        if nwr > 0 || rd_sync.contains(&true) {
            self.clocks += 1;
        }
        // The read port register: `next` taken at the tick if the port is
        // clocked, else `next` itself.
        let read_port = |s: &mut Self, i: usize, next: Vec<Bit>| -> Vec<Bit> {
            if !rd_sync[i] {
                return next;
            }
            match konst(rd_en[i]) {
                Some(true) => s.helper1(of, "rd", GateKind::Delay { w: w as u8 }, vec![next]),
                _ => s.helper1(
                    of,
                    "rd",
                    GateKind::Register { w: w as u8 },
                    vec![vec![rd_en[i]], next],
                ),
            }
        };
        let rom = |contents: &[u64]| {
            let mut contents = contents.to_vec();
            while contents.last() == Some(&0) {
                contents.pop();
            }
            GateKind::Rom {
                addr_bits: need as u8,
                w: w as u8,
                contents,
            }
        };

        let write = if nwr == 1 {
            let wen = c.conn_w("WR_EN", w)?;
            let waddr = c.conn_w("WR_ADDR", abits)?;
            let wdata = c.conn_w("WR_DATA", w)?;
            (!wen.iter().all(|&b| b == Bit::Zero)).then_some((wen, waddr, wdata))
        } else {
            None
        };
        let Some((wen, waddr, wdata)) = write else {
            // Read only: a ROM per read port.
            let mut name = Some((name, label));
            for i in 0..nrd {
                let a = fit(addr_of(i), need, false);
                let (n, l) = name
                    .take()
                    .unwrap_or_else(|| (self.names.fresh(&format!("{of}_rom")), None));
                let data = self
                    .add_node(n, l, rom(&init), vec![Some(a)], None, i > 0)
                    .swap_remove(0);
                let out = read_port(self, i, data);
                self.alias(&rd_data[i * w..(i + 1) * w], &out);
            }
            return Ok(());
        };

        // One write port. A per-bit enable that is the same bit throughout
        // is one write enable.
        let uniform = wen.iter().all(|&b| b == wen[0]);
        let mut we = if uniform { wen[0] } else { self.any(of, &wen) };
        let high = waddr.get(need..).unwrap_or(&[]).to_vec();
        if !high.is_empty() {
            // Writes past the end of the memory do nothing.
            let past = self.any(of, &high);
            let inside = self.not1(of, past);
            we = self.and1(of, we, inside);
        }
        let single_port = (0..nrd).all(|i| addr_of(i) == waddr.as_slice());
        let waddr = fit(&waddr, need, false);
        // The word written, given the word `old` stored there.
        let merged = |s: &mut Self, old: &[Bit]| -> Vec<Bit> {
            if uniform {
                return wdata.clone();
            }
            let keep = s.not_v(of, &wen);
            let x = s.bitwise(of, Op::And, &wdata, &wen);
            let z = s.bitwise(of, Op::And, old, &keep);
            s.bitwise(of, Op::Or, &x, &z)
        };

        if single_port {
            // A Ram. Initial contents: the Ram holds `word ^ init`, so that
            // its power-up 0 reads as `init` (a Rom at the same address).
            let state = self.fresh(w);
            let init_word =
                has_init.then(|| self.helper1(of, "init", rom(&init), vec![waddr.clone()]));
            let word = match &init_word {
                Some(k) => self.bitwise(of, Op::Xor, &state, k),
                None => state.clone(),
            };
            let new = merged(self, &word);
            let stored = match &init_word {
                Some(k) => self.bitwise(of, Op::Xor, &new, k),
                None => new.clone(),
            };
            let kind = GateKind::Ram {
                addr_bits: need as u8,
                w: w as u8,
            };
            let inputs = vec![Some(waddr.clone()), Some(vec![we]), Some(stored)];
            self.add_node(name, label, kind, inputs, Some(vec![state]), false);
            for i in 0..nrd {
                let next = if rd_sync[i] && rd_transparent[i] {
                    if uniform {
                        self.mux_v(of, we, &word, &wdata)
                    } else {
                        new.clone()
                    }
                } else {
                    word.clone()
                };
                let out = read_port(self, i, next);
                self.alias(&rd_data[i * w..(i + 1) * w], &out);
            }
            return Ok(());
        }

        if size > MAX_REGISTER_FILE {
            return refuse(format!(
                "{size} words, read at another address than the one written: such memories \
                 are built from registers, up to {MAX_REGISTER_FILE} words (a Ram has one \
                 address for reading and writing)"
            ));
        }
        self.register_files.push(memid.clone());
        let mut words = Vec::new();
        for (k, &init_k) in init.iter().enumerate() {
            let hit = self.eq_v(of, &waddr, &const_word(k as u64, need));
            let sel = self.and1(of, we, hit);
            let state = self.fresh(w);
            let k_bits = const_word(init_k, w);
            let word = self.bitwise(of, Op::Xor, &state, &k_bits);
            let new = merged(self, &word);
            let stored = self.bitwise(of, Op::Xor, &new, &k_bits);
            let reg = self.names.fresh(&format!("{of}_{k}"));
            let label = (k == 0).then(|| label.clone()).flatten();
            let kind = GateKind::Register { w: w as u8 };
            let inputs = vec![Some(vec![sel]), Some(stored)];
            self.add_node(reg, label, kind, inputs, Some(vec![state]), false);
            words.push(word);
        }
        words.resize(1 << need, zeros(w));
        for i in 0..nrd {
            let raddr = fit(addr_of(i), need, false);
            let data = self.select(of, words.clone(), &raddr);
            let next = if rd_sync[i] && rd_transparent[i] {
                let same = self.eq_v(of, &raddr, &waddr);
                let hit = self.and1(of, we, same);
                let new = merged(self, &data);
                self.mux_v(of, hit, &data, &new)
            } else {
                data
            };
            let out = read_port(self, i, next);
            self.alias(&rd_data[i * w..(i + 1) * w], &out);
        }
        Ok(())
    }
}
