//! Chips made by the batch editor: a chip of a set of parts
//! (`make_chip`), and the gate-level form of a word-level part
//! (`expand`). See `docs/mcp-server.md`, "`edit`".
//!
//! ## Gate-level chips
//!
//! [`gate_chip_plans`] says, for one word-level kind, how to build a
//! circuit of 1-bit gates that does the same, as a person would in the
//! batch editor: a list of chips, each a list of `add` and `wire` ops,
//! dependencies first. The batch editor applies them (`batch.rs`,
//! `Group::expand`), so the chips have labelled parts, arrays, generated
//! adapters and a layout like any chip made through `edit`.
//!
//! A wide part is an array of a small cell, with bus wiring:
//!
//! | Kind | Gate-level form |
//! |---|---|
//! | `Not`, `And`, ..., `Mux`, `Switch`, `Delay`, `Register`, `DLatch` | `bit[0..w]`, the same kind 1 bit wide; select and enable fan out |
//! | `Add` | `fa[0..w]` of `full_adder_cell` (two `Xor`, two `And`, one `Or`), carries chained |
//! | `LtU` | `c[0..w]` of `ltu_cell` (a less-than chain from bit 0 up), the first fed a constant 0 |
//! | `Eq` | `same[0..w]` of `Xnor`, then a tree of `And` |
//! | `ReduceOr`, `ReduceAnd`, `ReduceXor` | a tree of 2-input gates, `t0[..]`, `t1[..]`, ... level by level |
//!
//! Up to [`CHUNK`] bits a part is built like that in one circuit. A wider
//! one is built from chunks: `byte[0..k]` of the [`CHUNK`]-bit gate chip
//! of the same kind (and `rest` for what is left over), chained the same
//! way, with an `And`, `Or` or `Xor` tree over the chunks' results for the
//! comparisons and reductions. `LtU` chunks are `ltu_chain_<n>`, the cell
//! chain with its carry-in as a port. So a 64-bit adder is eight
//! `Add_8_gates`, each of eight `full_adder_cell`s, and no generated
//! circuit has more than about 30 parts.
//!
//! The ports of a gate chip are named after the pins of its kind, so
//! [`pin_maps`] can put an instance in the part's place.

use serde_json::{Value, json};
use uuid::Uuid;

use crate::sim::source::item_id;
use crate::sim::{CircuitDocument, CircuitLibrary, DocNode, GateKind};

/// The widest part built as one circuit of cells; wider ones are built
/// from chunks this wide.
pub const CHUNK: u8 = 8;

/// The full adder cell: `a`, `b`, `cin` to `sum`, `cout`.
pub const FULL_ADDER_CELL: &str = "full_adder_cell";

/// The less-than cell: `lt = a < b` at this bit, else `lt_in` when the
/// bits are equal.
pub const LTU_CELL: &str = "ltu_cell";

/// Whether `kind` has a gate-level form that differs from itself: the
/// word-wide gates and state, and arithmetic, comparison and reduction.
pub fn expandable(kind: &GateKind) -> bool {
    use GateKind as G;
    match kind {
        G::Not { w }
        | G::And { w }
        | G::Or { w }
        | G::Xor { w }
        | G::Nand { w }
        | G::Nor { w }
        | G::Xnor { w }
        | G::Mux { w }
        | G::Switch { w }
        | G::Delay { w }
        | G::Register { w }
        | G::DLatch { w } => *w > 1,
        G::Add { .. } | G::Eq { .. } | G::LtU { .. } => true,
        G::ReduceOr { w } | G::ReduceAnd { w } | G::ReduceXor { w } => *w > 1,
        _ => false,
    }
}

/// The name of the gate-level chip of `kind`: `Add_8_gates`.
pub fn gate_chip_name(kind: &GateKind) -> String {
    let mut name = String::new();
    if let Ok(serde_json::Value::Object(fields)) = serde_json::to_value(kind) {
        for (k, v) in &fields {
            match (k.as_str(), v) {
                ("type", serde_json::Value::String(ty)) => name.insert_str(0, ty),
                (_, v) => {
                    name.push('_');
                    name.push_str(&v.to_string());
                }
            }
        }
    }
    format!("{name}_gates")
}

/// One chip to make: its name and the ops (`add`, `wire`) that build it
/// in an empty circuit.
#[derive(Debug, Clone, PartialEq)]
pub struct ChipPlan {
    pub name: String,
    pub ops: Vec<Value>,
}

/// The chips that make the gate-level form of `kind`, each after the
/// chips it places; the last is [`gate_chip_name`]`(kind)`. A chip of the
/// list that a workspace has already (by name) is used as it is.
pub fn gate_chip_plans(kind: &GateKind) -> Result<Vec<ChipPlan>, String> {
    if !expandable(kind) {
        return Err(format!(
            "a {} has no gate-level form (it is a gate, a memory or wiring already)",
            kind.display_name()
        ));
    }
    kind.check()?;
    let mut planner = Planner { plans: Vec::new() };
    planner.gate_chip(kind);
    Ok(planner.plans)
}

/// The id of the gate-level copy of chip `source` in a workspace whose
/// root has id `root` (`expand` with `deep`). It is derived from the
/// source's id, not from a name, so a copy made by `expand` is told apart
/// from a chip a person made under the same name.
pub fn gates_copy_id(root: Uuid, source: Uuid) -> Uuid {
    item_id(root, &format!("gates_of/{source}"))
}

/// The name a gate-level copy of chip `source_name` is given when it is
/// free: `alu_gates`, with a label that is not a name made one
/// (`16-bit ALU` gives `_16_bit_ALU_gates`).
pub fn gates_copy_name(source_name: &str) -> String {
    let mut name = String::new();
    for c in source_name.trim().chars() {
        if c.is_ascii_alphanumeric() || c == '_' {
            name.push(c);
        } else if !name.ends_with('_') {
            name.push('_');
        }
    }
    let name = name.trim_matches('_');
    if name.is_empty() {
        "chip_gates".into()
    } else if name.starts_with(|c: char| c.is_ascii_digit()) {
        format!("_{name}_gates")
    } else {
        format!("{name}_gates")
    }
}

/// For an instance of a chip in place of a primitive: the instance pin
/// (ports are in label order) of each primitive pin, inputs then outputs.
pub fn pin_maps(kind: &GateKind, chip: &CircuitDocument) -> (Vec<u8>, Vec<u8>) {
    let (in_names, out_names) = CircuitLibrary::default().pin_names(kind);
    let (ins, outs) = chip.interface();
    let map = |names: &[String], ports: &[&DocNode]| {
        names
            .iter()
            .map(|n| {
                ports
                    .iter()
                    .position(|p| p.label.as_deref() == Some(n))
                    .unwrap_or(0) as u8
            })
            .collect()
    };
    (map(&in_names, &ins), map(&out_names, &outs))
}

// ─────────────────────────────────────────────────────────────────────────────
// Plans
// ─────────────────────────────────────────────────────────────────────────────

/// The `add` and `wire` entries of one circuit being planned.
#[derive(Default)]
struct Sheet {
    add: Vec<Value>,
    wire: Vec<Value>,
}

impl Sheet {
    fn input(&mut self, name: &str, w: u8) {
        self.add
            .push(json!({"name": name, "type": "Input", "w": w}));
    }

    fn output(&mut self, name: &str, w: u8) {
        self.add
            .push(json!({"name": name, "type": "Output", "w": w}));
    }

    /// A part: `body` is a node entry without its name.
    fn part(&mut self, name: &str, body: &Value, count: Option<u8>) {
        let mut entry = body.as_object().cloned().unwrap_or_default();
        entry.insert("name".into(), name.into());
        if let Some(count) = count {
            entry.insert("count".into(), count.into());
        }
        self.add.push(Value::Object(entry));
    }

    fn wire(&mut self, from: impl Into<String>, to: impl Into<String>) {
        self.wire.push(json!([from.into(), to.into()]));
    }

    fn ops(self) -> Vec<Value> {
        vec![json!({"add": self.add}), json!({"wire": self.wire})]
    }

    /// A tree of 2-input `gate`s over `signals` (references), its result
    /// into `out`. Level `l` is the array `t<l>`; gate `i` of a level of
    /// `m` signals takes signals `i` and `i + m/2`, and an odd one out
    /// goes up a level as it is.
    fn tree(&mut self, gate: &str, mut signals: Vec<String>, out: &str) {
        let mut level = 0;
        while signals.len() > 1 {
            let half = signals.len() / 2;
            let name = format!("t{level}");
            self.part(&name, &json!({"type": gate}), Some(half as u8));
            let mut next: Vec<String> = Vec::with_capacity(half + 1);
            for i in 0..half {
                self.wire(signals[i].clone(), format!("{name}[{i}].a"));
                self.wire(signals[i + half].clone(), format!("{name}[{i}].b"));
                next.push(format!("{name}[{i}].y"));
            }
            if signals.len() % 2 == 1 {
                next.push(signals[2 * half].clone());
            }
            signals = next;
            level += 1;
        }
        if let Some(last) = signals.pop() {
            self.wire(last, out);
        }
    }
}

/// How a word of `w` bits is cut into chunks: `k` whole ones of
/// [`CHUNK`] bits and a rest of `r` bits (maybe 0).
fn chunks(w: u8) -> (u8, u8) {
    (w / CHUNK, w % CHUNK)
}

/// The kind as a node entry body: `{"type": "And", "w": 1}`.
fn primitive(kind: &GateKind) -> Value {
    serde_json::to_value(kind).unwrap_or(Value::Null)
}

fn use_chip(name: &str) -> Value {
    json!({"use": name})
}

struct Planner {
    plans: Vec<ChipPlan>,
}

impl Planner {
    /// Plan chip `name` with `build` (once), after what `build` plans.
    fn chip(&mut self, name: String, build: impl FnOnce(&mut Self) -> Sheet) -> String {
        if !self.plans.iter().any(|p| p.name == name) {
            let sheet = build(self);
            self.plans.push(ChipPlan {
                name: name.clone(),
                ops: sheet.ops(),
            });
        }
        name
    }

    /// The gate chip of an expandable `kind`; its name.
    fn gate_chip(&mut self, kind: &GateKind) -> String {
        use GateKind as G;
        let name = gate_chip_name(kind);
        let w = kind.width().unwrap_or(1);
        match kind {
            G::Add { .. } => self.chip(name, |p| p.adder(w)),
            G::LtU { .. } => self.chip(name, |p| p.less_than(w, false)),
            G::Eq { .. } => self.chip(name, |p| p.equal(w)),
            G::ReduceOr { .. } => self.chip(name, |p| p.reduce(kind, "Or", w)),
            G::ReduceAnd { .. } => self.chip(name, |p| p.reduce(kind, "And", w)),
            G::ReduceXor { .. } => self.chip(name, |p| p.reduce(kind, "Xor", w)),
            _ => self.chip(name, |p| p.bitwise(kind, w)),
        }
    }

    /// The chunk of `r` bits of `kind`: its gate chip, or the 1-bit
    /// primitive when that is a gate already.
    fn unit(&mut self, kind: &GateKind, r: u8) -> Value {
        let kind = kind.with_width(r);
        if expandable(&kind) {
            use_chip(&self.gate_chip(&kind))
        } else {
            primitive(&kind)
        }
    }

    /// `byte[0..k]` of `unit` and `rest` of `rest`, with the word inputs
    /// `data_in` dealt out to them and their word outputs `data_out`
    /// gathered, `w` bits each. Without `rest`, the bits past the whole
    /// chunks are left to the caller. Returns the references of the
    /// chunks, lowest first.
    fn chunked(
        sheet: &mut Sheet,
        w: u8,
        unit: &Value,
        rest: Option<&Value>,
        data_in: &[&str],
        data_out: &[&str],
    ) -> Vec<String> {
        let (k, r) = chunks(w);
        let cut = u32::from(k) * u32::from(CHUNK);
        sheet.part("byte", unit, Some(k));
        let mut refs: Vec<String> = (0..k).map(|i| format!("byte[{i}]")).collect();
        if r == 0 {
            for p in data_in {
                sheet.wire(*p, format!("byte.{p}"));
            }
            for p in data_out {
                sheet.wire(format!("byte.{p}"), *p);
            }
            return refs;
        }
        for p in data_in {
            sheet.wire(format!("{p}:0..{cut}"), format!("byte.{p}"));
        }
        for p in data_out {
            sheet.wire(format!("byte.{p}"), format!("{p}:0..{cut}"));
        }
        if let Some(rest) = rest {
            sheet.part("rest", rest, None);
            for p in data_in {
                sheet.wire(format!("{p}:{cut}..{w}"), format!("rest.{p}"));
            }
            for p in data_out {
                sheet.wire(format!("rest.{p}"), format!("{p}:{cut}..{w}"));
            }
            refs.push("rest".into());
        }
        refs
    }

    /// Ports named after the pins of `kind`.
    fn ports(sheet: &mut Sheet, kind: &GateKind) {
        let (ins, outs) = kind.primitive_pins().unwrap_or_default();
        for (name, w) in kind.input_pin_names().iter().zip(ins.iter()) {
            sheet.input(name, *w);
        }
        for (name, w) in kind.output_pin_names().iter().zip(outs.iter()) {
            sheet.output(name, *w);
        }
    }

    /// Word inputs and outputs of `kind` at width `w > 1` (the pins `w`
    /// wide), then its 1-bit control inputs.
    fn word_pins(kind: &GateKind) -> (Vec<&'static str>, Vec<&'static str>, Vec<&'static str>) {
        let w = kind.width().unwrap_or(1);
        let (ins, outs) = kind.primitive_pins().unwrap_or_default();
        let (mut word_in, mut control) = (Vec::new(), Vec::new());
        for (name, pw) in kind.input_pin_names().iter().zip(ins.iter()) {
            if *pw == w {
                word_in.push(*name);
            } else {
                control.push(*name);
            }
        }
        let word_out = kind
            .output_pin_names()
            .iter()
            .zip(outs.iter())
            .filter(|(_, pw)| **pw == w)
            .map(|(n, _)| *n)
            .collect();
        (word_in, word_out, control)
    }

    /// A word-wide gate, multiplexer or state element: an array of the
    /// same kind 1 bit wide, or of chunks.
    fn bitwise(&mut self, kind: &GateKind, w: u8) -> Sheet {
        let mut s = Sheet::default();
        Self::ports(&mut s, kind);
        let (word_in, word_out, control) = Self::word_pins(kind);
        if w <= CHUNK {
            s.part("bit", &primitive(&kind.with_width(1)), Some(w));
            for p in word_in.iter().chain(&control) {
                s.wire(*p, format!("bit.{p}"));
            }
            for p in word_out {
                s.wire(format!("bit.{p}"), p);
            }
        } else {
            let (_, r) = chunks(w);
            let unit = self.unit(kind, CHUNK);
            let rest = (r > 0).then(|| self.unit(kind, r));
            let refs = Self::chunked(&mut s, w, &unit, rest.as_ref(), &word_in, &word_out);
            // A select or enable fans out to every chunk.
            for p in control {
                let mut entry = vec![p.to_string()];
                entry.extend(refs.iter().map(|u| format!("{u}.{p}")));
                s.wire.push(json!(entry));
            }
        }
        s
    }

    fn full_adder_cell(&mut self) -> String {
        self.chip(FULL_ADDER_CELL.into(), |_| {
            let mut s = Sheet::default();
            for p in ["a", "b", "cin"] {
                s.input(p, 1);
            }
            s.output("sum", 1);
            s.output("cout", 1);
            for (name, ty) in [("x", "Xor"), ("s", "Xor"), ("g", "And"), ("p", "And")] {
                s.part(name, &json!({"type": ty}), None);
            }
            s.part("c", &json!({"type": "Or"}), None);
            s.wire.extend([
                json!(["a", "x.a", "g.a"]),
                json!(["b", "x.b", "g.b"]),
                json!(["x", "s.a", "p.a"]),
                json!(["cin", "s.b", "p.b"]),
                json!(["s", "sum"]),
                json!(["g", "c.a"]),
                json!(["p", "c.b"]),
                json!(["c", "cout"]),
            ]);
            s
        })
    }

    /// `units` (references with pins `a`, `b`, `<carry_in>`, `<carry_out>`)
    /// chained from the first: each one's carry out into the next one's
    /// carry in.
    fn chain(s: &mut Sheet, units: &[String], carry_in: &str, carry_out: &str) {
        for pair in units.windows(2) {
            s.wire(
                format!("{}.{carry_out}", pair[0]),
                format!("{}.{carry_in}", pair[1]),
            );
        }
    }

    /// `Add`: a ripple-carry adder of full adder cells, or of chunks.
    fn adder(&mut self, w: u8) -> Sheet {
        let cell = self.full_adder_cell();
        let mut s = Sheet::default();
        Self::ports(&mut s, &GateKind::Add { w });
        let units: Vec<String> = if w <= CHUNK {
            s.part("fa", &use_chip(&cell), Some(w));
            s.wire("a", "fa.a");
            s.wire("b", "fa.b");
            s.wire("fa.sum", "sum");
            (0..w).map(|i| format!("fa[{i}]")).collect()
        } else {
            let (_, r) = chunks(w);
            let unit = self.unit(&GateKind::Add { w }, CHUNK);
            // A 1-bit rest is the cell itself: it has the same ports.
            let rest = if r > 1 {
                self.unit(&GateKind::Add { w }, r)
            } else {
                use_chip(&cell)
            };
            Self::chunked(&mut s, w, &unit, Some(&rest), &["a", "b"], &["sum"])
        };
        s.wire("cin", format!("{}.cin", units[0]));
        Self::chain(&mut s, &units, "cin", "cout");
        s.wire(format!("{}.cout", units[units.len() - 1]), "cout");
        s
    }

    fn ltu_cell(&mut self) -> String {
        self.chip(LTU_CELL.into(), |_| {
            let mut s = Sheet::default();
            for p in ["a", "b", "lt_in"] {
                s.input(p, 1);
            }
            s.output("lt", 1);
            s.part("na", &json!({"type": "Not"}), None);
            s.part("below", &json!({"type": "And"}), None);
            s.part("same", &json!({"type": "Xnor"}), None);
            s.part("keep", &json!({"type": "And"}), None);
            s.part("any", &json!({"type": "Or"}), None);
            s.wire.extend([
                json!(["a", "na", "same.a"]),
                json!(["b", "below.b", "same.b"]),
                json!(["na", "below.a"]),
                json!(["same", "keep.a"]),
                json!(["lt_in", "keep.b"]),
                json!(["below", "any.a"]),
                json!(["keep", "any.b"]),
                json!(["any", "lt"]),
            ]);
            s
        })
    }

    /// `LtU` (`chain` false: ports `a`, `b`, `y`), or a less-than chain
    /// with a carry in (`chain` true: `ltu_chain_<w>`, ports `a`, `b`,
    /// `lt_in`, `lt`). Bits are compared from bit 0 up, so the highest
    /// bit that differs decides.
    fn less_than(&mut self, w: u8, chain: bool) -> Sheet {
        let cell = self.ltu_cell();
        let mut s = Sheet::default();
        s.input("a", w);
        s.input("b", w);
        let units: Vec<String> = if w <= CHUNK {
            s.part("c", &use_chip(&cell), Some(w));
            s.wire("a", "c.a");
            s.wire("b", "c.b");
            (0..w).map(|i| format!("c[{i}]")).collect()
        } else {
            let (_, r) = chunks(w);
            let unit = use_chip(&self.ltu_chain(CHUNK));
            let rest = if r > 1 {
                use_chip(&self.ltu_chain(r))
            } else {
                use_chip(&cell)
            };
            Self::chunked(&mut s, w, &unit, Some(&rest), &["a", "b"], &[])
        };
        if chain {
            s.input("lt_in", 1);
            s.output("lt", 1);
            s.wire("lt_in", format!("{}.lt_in", units[0]));
            s.wire(format!("{}.lt", units[units.len() - 1]), "lt");
        } else {
            s.output("y", 1);
            s.part("zero", &json!({"type": "Const", "value": 0}), None);
            s.wire("zero", format!("{}.lt_in", units[0]));
            s.wire(format!("{}.lt", units[units.len() - 1]), "y");
        }
        Self::chain(&mut s, &units, "lt_in", "lt");
        s
    }

    fn ltu_chain(&mut self, w: u8) -> String {
        self.chip(format!("ltu_chain_{w}"), |p| p.less_than(w, true))
    }

    /// `Eq`: an `Xnor` per bit (or an `Eq` chip per chunk) and a tree of
    /// `And` over them.
    fn equal(&mut self, w: u8) -> Sheet {
        let mut s = Sheet::default();
        Self::ports(&mut s, &GateKind::Eq { w });
        let results: Vec<String> = if w <= CHUNK {
            s.part("same", &json!({"type": "Xnor"}), Some(w));
            s.wire("a", "same.a");
            s.wire("b", "same.b");
            (0..w).map(|i| format!("same[{i}].y")).collect()
        } else {
            let (_, r) = chunks(w);
            let unit = self.unit(&GateKind::Eq { w }, CHUNK);
            let rest = if r > 1 {
                self.unit(&GateKind::Eq { w }, r)
            } else {
                json!({"type": "Xnor"})
            };
            Self::chunked(&mut s, w, &unit, Some(&rest), &["a", "b"], &[])
                .into_iter()
                .map(|u| format!("{u}.y"))
                .collect()
        };
        s.tree("And", results, "y");
        s
    }

    /// `ReduceOr`, `ReduceAnd`, `ReduceXor`: a tree of `gate` over the
    /// bits, or over the results of chunks.
    fn reduce(&mut self, kind: &GateKind, gate: &str, w: u8) -> Sheet {
        let mut s = Sheet::default();
        Self::ports(&mut s, kind);
        let signals: Vec<String> = if w <= CHUNK {
            (0..w).map(|i| format!("a:{i}")).collect()
        } else {
            let (k, r) = chunks(w);
            let unit = self.unit(kind, CHUNK);
            // A 1-bit rest is its bit, with no part.
            let rest = (r > 1).then(|| self.unit(kind, r));
            let mut signals: Vec<String> =
                Self::chunked(&mut s, w, &unit, rest.as_ref(), &["a"], &[])
                    .into_iter()
                    .map(|u| format!("{u}.y"))
                    .collect();
            if r == 1 {
                signals.push(format!("a:{}", u32::from(k) * u32::from(CHUNK)));
            }
            signals
        };
        s.tree(gate, signals, "y");
        s
    }
}
