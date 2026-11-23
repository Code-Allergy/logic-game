//! RTL import: a Yosys JSON netlist (`write_json`) becomes a workspace.
//!
//! See `docs/rtl-import.md`. Each module reachable from the top module
//! becomes a circuit; module instances become instances (`use`) of those
//! circuits; word-level and gate-level cells become primitives, keeping
//! their widths. Where a cell pin takes bits that are not one whole output
//! pin (a slice, a concatenation, constants), `Split`, `Merge` and `Const`
//! nodes are added. Cells with no primitive of their own (multipliers,
//! shifts, comparisons, flip-flops with resets, memories) are built from
//! several in the submodules: `build` (helpers that fold constants),
//! `arith` and `state`. Every circuit is then laid out with
//! [`super::layout::layout`].
//!
//! The result is a save format version 6 file ([`SourceFile`], with a
//! position for every node and only whole-pin wires) and its expansion,
//! an ordinary version 5 [`WorkspaceDocument`] that the editor opens and
//! saves. Ids are derived from the workspace name, so importing the same
//! netlist twice gives the same documents.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;

use serde_json::{Map, Value};
use uuid::Uuid;

use super::layout::{self, LayoutNode, Role};
use super::source::{
    self, Externals, NodeBody, SourceCircuit, SourceFile, SourceNode, primitive_kind,
};
use super::{CircuitLibrary, GateKind, WorkspaceDocument};
use crate::editor::workspace::workspace_key;

mod arith;
mod build;
mod state;

/// Cell types the importer maps (for the error message).
pub const SUPPORTED_CELLS: &[&str] = &[
    "$not",
    "$pos",
    "$neg",
    "$and",
    "$or",
    "$xor",
    "$xnor",
    "$mux",
    "$pmux",
    "$bmux",
    "$demux",
    "$bwmux",
    "$add",
    "$sub",
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
    "$eq",
    "$ne",
    "$eqx",
    "$nex",
    "$bweqx",
    "$lt",
    "$le",
    "$gt",
    "$ge",
    "$reduce_and",
    "$reduce_or",
    "$reduce_xor",
    "$reduce_xnor",
    "$reduce_bool",
    "$logic_not",
    "$logic_and",
    "$logic_or",
    "$slice",
    "$concat",
    "$lut",
    "$ff",
    "$dff",
    "$dffe",
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
    "$_BUF_",
    "$_NOT_",
    "$_AND_",
    "$_OR_",
    "$_XOR_",
    "$_NAND_",
    "$_NOR_",
    "$_XNOR_",
    "$_ANDNOT_",
    "$_ORNOT_",
    "$_AOI3_",
    "$_OAI3_",
    "$_AOI4_",
    "$_OAI4_",
    "$_MUX_",
    "$_NMUX_",
    "$_FF_",
    "$_DFF_P_",
    "$_DFF_N_",
    "$_DFFE_PP_",
    "$_DFFE_PN_",
    "$_DFFE_NP_",
    "$_DFFE_NN_",
    "$_DFF_[NP][NP][01]_",
    "$_DFFE_[NP][NP][01][NP]_",
    "$_SDFF_[NP][NP][01]_",
    "$_SDFFE_[NP][NP][01][NP]_",
    "$_SDFFCE_[NP][NP][01][NP]_",
    "$_DFFSR_[NP][NP][NP]_",
    "$_DFFSRE_[NP][NP][NP][NP]_",
    "$_ALDFF_[NP][NP]_",
    "$_ALDFFE_[NP][NP][NP]_",
    "$_DLATCH_[NP]_",
    "$_DLATCH_[NP][NP][01]_",
    "$_DLATCHSR_[NP][NP][NP]_",
    "$_SR_[NP][NP]_",
];

/// An import error: what is wrong and where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RtlError(pub String);

impl fmt::Display for RtlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for RtlError {}

fn err<T>(message: impl Into<String>) -> Result<T, RtlError> {
    Err(RtlError(message.into()))
}

/// Import options.
#[derive(Debug, Clone, Default)]
pub struct ImportOptions {
    /// The top module. Defaults to the module with the `top` attribute, or
    /// else the only module that no other module instances.
    pub top: Option<String>,
    /// Workspace name (it derives the ids). Defaults to the top module's
    /// name.
    pub name: Option<String>,
}

/// An imported netlist.
#[derive(Debug, Clone)]
pub struct Import {
    /// The workspace name used to derive ids.
    pub name: String,
    /// The version 6 file: one circuit per module, top first, positions
    /// from the auto-layout.
    pub source: SourceFile,
    /// Its expansion: what the editor opens.
    pub workspace: WorkspaceDocument,
    /// Things that were accepted but approximated (undriven nets, `x`
    /// bits, clocks).
    pub warnings: Vec<String>,
}

/// Import a Yosys JSON netlist.
pub fn import_yosys_json(text: &str, options: &ImportOptions) -> Result<Import, RtlError> {
    let value: Value =
        serde_json::from_str(text).map_err(|e| RtlError(format!("not valid JSON: {e}")))?;
    let Some(Value::Object(modules)) = value.get("modules") else {
        return err("not a Yosys JSON netlist: no \"modules\" object");
    };
    let top = find_top(modules, options.top.as_deref())?;

    // Modules reachable from the top, top first (depth-first pre-order).
    let mut order: Vec<&str> = Vec::new();
    let mut stack = vec![top.as_str()];
    let mut seen = HashSet::new();
    while let Some(m) = stack.pop() {
        if !seen.insert(m) {
            continue;
        }
        order.push(m);
        let module = &modules[m];
        let mut children: Vec<&str> = cells(module)
            .filter_map(|(_, c)| c.get("type").and_then(Value::as_str))
            .filter(|t| !t.starts_with('$') && modules.contains_key(*t))
            .map(|t| modules.get_key_value(t).unwrap().0.as_str())
            .collect();
        children.reverse();
        stack.extend(children);
    }

    let mut circuit_names = Names::default();
    let names: HashMap<&str, String> = order
        .iter()
        .map(|m| (*m, circuit_names.fresh(&ident(m))))
        .collect();
    let mut warnings = Vec::new();
    let mut circuits = Vec::new();
    for m in &order {
        let circuit = import_module(m, &names[m], modules, &names, &mut warnings)
            .map_err(|e| RtlError(format!("module \"{}\": {}", strip(m), e.0)))?;
        circuits.push(circuit);
    }
    let source = SourceFile {
        name: None,
        root: names[top.as_str()].clone(),
        circuits,
    };
    let name = options
        .name
        .clone()
        .unwrap_or_else(|| strip(&top).to_string());
    let expansion = source::expand(&source, &workspace_key(&name), &Externals::default())
        .map_err(|e| RtlError(format!("internal error, the import does not expand: {e}")))?;
    Ok(Import {
        name,
        source,
        workspace: expansion.workspace,
        warnings,
    })
}

fn cells(module: &Value) -> impl Iterator<Item = (&String, &Value)> {
    module
        .get("cells")
        .and_then(Value::as_object)
        .into_iter()
        .flat_map(|m| m.iter())
}

fn is_blackbox(module: &Value) -> bool {
    let attr = |k: &str| {
        module
            .get("attributes")
            .and_then(|a| a.get(k))
            .is_some_and(|v| int_value(v).is_some_and(|v| v != 0))
    };
    attr("blackbox") || attr("whitebox")
}

/// How many nets of `module` have an `init` attribute with a 1 bit.
fn initial_values(module: &Value) -> Option<usize> {
    let count = module
        .get("netnames")
        .and_then(Value::as_object)
        .into_iter()
        .flat_map(|m| m.values())
        .filter(|n| {
            n.get("attributes")
                .and_then(|a| a.get("init"))
                .is_some_and(|v| match v {
                    Value::String(s) => s.contains('1'),
                    other => int_value(other).is_some_and(|v| v != 0),
                })
        })
        .count();
    (count > 0).then_some(count)
}

fn find_top(modules: &Map<String, Value>, wanted: Option<&str>) -> Result<String, RtlError> {
    if let Some(w) = wanted {
        return modules
            .keys()
            .find(|k| *k == w || strip(k) == w)
            .cloned()
            .ok_or_else(|| RtlError(format!("no module \"{w}\" in the netlist")));
    }
    let marked: Vec<&String> = modules
        .iter()
        .filter(|(_, m)| {
            m.get("attributes")
                .and_then(|a| a.get("top"))
                .is_some_and(|v| int_value(v).is_some_and(|v| v != 0))
        })
        .map(|(k, _)| k)
        .collect();
    if let [one] = marked.as_slice() {
        return Ok((*one).clone());
    }
    let instanced: HashSet<&str> = modules
        .values()
        .flat_map(cells)
        .filter_map(|(_, c)| c.get("type").and_then(Value::as_str))
        .collect();
    let roots: Vec<&String> = modules
        .iter()
        .filter(|(k, m)| !instanced.contains(k.as_str()) && !is_blackbox(m))
        .map(|(k, _)| k)
        .collect();
    match roots.as_slice() {
        [one] => Ok((*one).clone()),
        [] => err("the netlist has no module"),
        many => err(format!(
            "several top-level modules ({}); choose one",
            many.iter().map(|s| strip(s)).collect::<Vec<_>>().join(", ")
        )),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Names
// ─────────────────────────────────────────────────────────────────────────────

/// A Yosys name without its `\` prefix.
fn strip(name: &str) -> &str {
    name.strip_prefix('\\').unwrap_or(name)
}

/// A version 6 name (letters, digits, `_`, not starting with a digit).
fn ident(name: &str) -> String {
    let mut s: String = strip(name)
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    if !s.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_') {
        s.insert(0, '_');
    }
    s
}

#[derive(Default)]
struct Names(HashSet<String>);

impl Names {
    fn fresh(&mut self, base: &str) -> String {
        if self.0.insert(base.to_string()) {
            return base.to_string();
        }
        (2..)
            .map(|i| format!("{base}_{i}"))
            .find(|n| self.0.insert(n.clone()))
            .expect("a free name")
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Values
// ─────────────────────────────────────────────────────────────────────────────

/// A parameter or attribute value: a number, or a string of binary digits
/// (what `write_json` writes) or decimal digits.
fn int_value(v: &Value) -> Option<u64> {
    match v {
        Value::Number(n) => n.as_u64().or_else(|| n.as_i64().map(|i| i as u64)),
        Value::String(s) if !s.is_empty() && s.chars().all(|c| c == '0' || c == '1') => {
            let s = s.trim_start_matches('0');
            if s.len() > 64 {
                None
            } else if s.is_empty() {
                Some(0)
            } else {
                u64::from_str_radix(s, 2).ok()
            }
        }
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// One bit of a signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Bit {
    Net(u64),
    Zero,
    One,
}

fn parse_bits(value: Option<&Value>, what: &str, xs: &mut usize) -> Result<Vec<Bit>, RtlError> {
    let Some(Value::Array(items)) = value else {
        return err(format!("{what}: missing or not a bit list"));
    };
    items
        .iter()
        .map(|b| match b {
            Value::Number(n) => n
                .as_u64()
                .map(Bit::Net)
                .ok_or_else(|| RtlError(format!("{what}: bad bit {n}"))),
            Value::String(s) => match s.as_str() {
                "0" => Ok(Bit::Zero),
                "1" => Ok(Bit::One),
                "x" | "z" => {
                    *xs += 1;
                    Ok(Bit::Zero)
                }
                other => err(format!("{what}: bad bit \"{other}\"")),
            },
            other => err(format!("{what}: bad bit {other}")),
        })
        .collect()
}

fn width(bits: usize, what: &str) -> Result<u8, RtlError> {
    match u8::try_from(bits) {
        Ok(w @ 1..=64) => Ok(w),
        _ => err(format!(
            "{what} is {bits} bits wide; words are 1 to 64 bits"
        )),
    }
}

/// `bits` cut or extended to `w` bits (sign extension repeats the top bit).
fn fit(bits: &[Bit], w: usize, signed: bool) -> Vec<Bit> {
    let mut out: Vec<Bit> = bits.iter().copied().take(w).collect();
    let fill = match bits.last() {
        Some(&b) if signed => b,
        _ => Bit::Zero,
    };
    out.resize(w, fill);
    out
}

// ─────────────────────────────────────────────────────────────────────────────
// Modules
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
enum Body {
    Prim(GateKind),
    /// An instance of the circuit with this name.
    Use(String),
}

/// A node of an imported circuit, before wiring.
#[derive(Debug, Clone)]
struct Cell {
    name: String,
    label: Option<String>,
    body: Body,
    /// Input pins: name, width, and the bits driving them (`None`: left
    /// unconnected).
    inputs: Vec<(String, u8, Option<Vec<Bit>>)>,
    /// Output pins: name, width, and the bits they drive.
    outputs: Vec<(String, u8, Vec<Bit>)>,
    /// Added by the importer to build a cell out of primitives; removed
    /// again if nothing reads its outputs.
    helper: bool,
}

impl Cell {
    fn prim(name: String, label: Option<String>, kind: GateKind) -> Cell {
        let library = CircuitLibrary::default();
        let (ins, outs) = library.pins(&kind).unwrap_or_default();
        let (in_names, out_names) = library.pin_names(&kind);
        Cell {
            name,
            label,
            body: Body::Prim(kind),
            inputs: ins
                .iter()
                .zip(in_names)
                .map(|(&w, n)| (n, w, None))
                .collect(),
            outputs: outs
                .iter()
                .zip(out_names)
                .map(|(&w, n)| (n, w, Vec::new()))
                .collect(),
            helper: false,
        }
    }

    fn input(mut self, pin: usize, bits: Vec<Bit>) -> Cell {
        self.inputs[pin].2 = Some(bits);
        self
    }

    fn output(mut self, pin: usize, bits: Vec<Bit>) -> Cell {
        self.outputs[pin].2 = bits;
        self
    }

    fn layout_node(&self) -> LayoutNode {
        match &self.body {
            Body::Prim(kind) => LayoutNode::of(kind),
            Body::Use(_) => LayoutNode {
                role: Role::Logic,
                size: GateKind::SubCircuit {
                    definition: Uuid::nil(),
                }
                .size(),
            },
        }
    }
}

/// Ports of a module, in declaration order: name, is input, bits.
fn ports(module: &Value, xs: &mut usize) -> Result<Vec<(String, bool, Vec<Bit>)>, RtlError> {
    let Some(Value::Object(ports)) = module.get("ports") else {
        return Ok(Vec::new());
    };
    ports
        .iter()
        .map(|(name, port)| {
            let what = format!("port \"{}\"", strip(name));
            let input = match port.get("direction").and_then(Value::as_str) {
                Some("input") => true,
                Some("output") => false,
                Some("inout") => {
                    return err(format!(
                        "{what}: inout ports are not supported (the simulator has no \
                         tri-state wires; split the port into an input and an output)"
                    ));
                }
                _ => return err(format!("{what}: missing \"direction\"")),
            };
            let bits = parse_bits(port.get("bits"), &what, xs)?;
            width(bits.len(), &what)?;
            Ok((name.clone(), input, bits))
        })
        .collect()
}

/// State shared while mapping the cells of one module.
struct ModuleBuilder<'a> {
    cells: Vec<Cell>,
    names: Names,
    /// Nets that are another bit (`$pos`, `$_BUF_`, unused result bits).
    alias: HashMap<u64, Bit>,
    next_net: u64,
    modules: &'a Map<String, Value>,
    circuit_names: &'a HashMap<&'a str, String>,
    xs: usize,
    clocks: usize,
    /// Flip-flops and latches with an asynchronous reset, set or load.
    asyncs: usize,
    /// Memories built from registers (several write ports).
    register_files: Vec<String>,
}

impl ModuleBuilder<'_> {
    fn fresh(&mut self, w: usize) -> Vec<Bit> {
        (0..w)
            .map(|_| {
                self.next_net += 1;
                Bit::Net(self.next_net)
            })
            .collect()
    }

    fn alias(&mut self, nets: &[Bit], to: &[Bit]) {
        for (n, t) in nets
            .iter()
            .zip(to.iter().copied().chain(std::iter::repeat(Bit::Zero)))
        {
            if let Bit::Net(n) = n {
                self.alias.insert(*n, t);
            }
        }
    }

    fn map_cell(&mut self, cell_name: &str, cell: &Value) -> Result<(), RtlError> {
        let ty = cell
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| RtlError("missing \"type\"".into()))?;
        let hidden = cell_name.starts_with('$');
        let base = if hidden {
            ident(
                &ty.trim_matches(|c| c == '$' || c == '_')
                    .to_ascii_lowercase(),
            )
        } else {
            ident(cell_name)
        };
        let name = self.names.fresh(&base);
        let label = (!hidden).then(|| strip(cell_name).to_string());
        let params = cell.get("parameters");
        let param = |key: &str| -> Result<u64, RtlError> {
            params
                .and_then(|p| p.get(key))
                .and_then(int_value)
                .ok_or_else(|| RtlError(format!("{ty}: missing or bad parameter {key}")))
        };
        let flag = |key: &str| param(key).map(|v| v != 0).unwrap_or(false);
        let mut xs = 0;
        let conns = cell.get("connections");
        let mut conn = |port: &str| parse_bits(conns.and_then(|c| c.get(port)), port, &mut xs);

        use GateKind as G;
        let gate_1 = |ty: &str| -> Option<GateKind> {
            Some(match ty {
                "$_AND_" => G::And { w: 1 },
                "$_OR_" => G::Or { w: 1 },
                "$_XOR_" => G::Xor { w: 1 },
                "$_NAND_" => G::Nand { w: 1 },
                "$_NOR_" => G::Nor { w: 1 },
                "$_XNOR_" => G::Xnor { w: 1 },
                _ => return None,
            })
        };
        match ty {
            "$scopeinfo" => {}
            "$pos" | "$_BUF_" => {
                let y = conn("Y")?;
                let a = conn("A")?;
                let a = if ty == "$pos" {
                    fit(&a, y.len(), flag("A_SIGNED"))
                } else {
                    a
                };
                self.alias(&y, &a);
            }
            "$not" | "$_NOT_" => {
                let y = conn("Y")?;
                let w = width(y.len(), "Y")?;
                let a = fit(&conn("A")?, y.len(), flag("A_SIGNED"));
                self.cells.push(
                    Cell::prim(name, label, G::Not { w })
                        .input(0, a)
                        .output(0, y),
                );
            }
            "$and" | "$or" | "$xor" | "$xnor" => {
                let y = conn("Y")?;
                let w = width(y.len(), "Y")?;
                let signed = flag("A_SIGNED") && flag("B_SIGNED");
                let a = fit(&conn("A")?, y.len(), signed);
                let b = fit(&conn("B")?, y.len(), signed);
                let kind = match ty {
                    "$and" => G::And { w },
                    "$or" => G::Or { w },
                    "$xor" => G::Xor { w },
                    _ => G::Xnor { w },
                };
                self.cells.push(
                    Cell::prim(name, label, kind)
                        .input(0, a)
                        .input(1, b)
                        .output(0, y),
                );
            }
            t if gate_1(t).is_some() => {
                let kind = gate_1(t).unwrap();
                let (a, b, y) = (conn("A")?, conn("B")?, conn("Y")?);
                self.cells.push(
                    Cell::prim(name, label, kind)
                        .input(0, a)
                        .input(1, b)
                        .output(0, y),
                );
            }
            "$_ANDNOT_" | "$_ORNOT_" => {
                let (a, b, y) = (conn("A")?, conn("B")?, conn("Y")?);
                let nb = self.fresh(1);
                let not = self.names.fresh(&format!("{name}_not"));
                self.cells.push(
                    Cell::prim(not, None, G::Not { w: 1 })
                        .input(0, b)
                        .output(0, nb.clone()),
                );
                let kind = if ty == "$_ANDNOT_" {
                    G::And { w: 1 }
                } else {
                    G::Or { w: 1 }
                };
                self.cells.push(
                    Cell::prim(name, label, kind)
                        .input(0, a)
                        .input(1, nb)
                        .output(0, y),
                );
            }
            "$mux" | "$_MUX_" | "$_NMUX_" => {
                let y = conn("Y")?;
                let w = width(y.len(), "Y")?;
                let (a, b, s) = (conn("A")?, conn("B")?, conn("S")?);
                if a.len() != y.len() || b.len() != y.len() || s.len() != 1 {
                    return err(format!("{ty}: A, B and Y must have one width, S one bit"));
                }
                let out = if ty == "$_NMUX_" {
                    self.fresh(1)
                } else {
                    y.clone()
                };
                self.cells.push(
                    Cell::prim(name.clone(), label, G::Mux { w })
                        .input(0, s)
                        .input(1, a)
                        .input(2, b)
                        .output(0, out.clone()),
                );
                if ty == "$_NMUX_" {
                    let not = self.names.fresh(&format!("{name}_not"));
                    self.cells.push(
                        Cell::prim(not, None, G::Not { w: 1 })
                            .input(0, out)
                            .output(0, y),
                    );
                }
            }
            "$add" | "$sub" => {
                let y = conn("Y")?;
                let w = width(y.len(), "Y")?;
                let signed = flag("A_SIGNED") && flag("B_SIGNED");
                let a = fit(&conn("A")?, y.len(), signed);
                let mut b = fit(&conn("B")?, y.len(), signed);
                let cout = self.fresh(1);
                let mut add = Cell::prim(name.clone(), label, G::Add { w });
                if ty == "$sub" {
                    // a - b = a + !b + 1
                    let nb = self.fresh(y.len());
                    let not = self.names.fresh(&format!("{name}_not"));
                    self.cells.push(
                        Cell::prim(not, None, G::Not { w })
                            .input(0, b)
                            .output(0, nb.clone()),
                    );
                    b = nb;
                    add = add.input(2, vec![Bit::One]);
                }
                self.cells
                    .push(add.input(0, a).input(1, b).output(0, y).output(1, cout));
            }
            "$eq" | "$ne" | "$eqx" | "$nex" => {
                let y = conn("Y")?;
                let (a, b) = (conn("A")?, conn("B")?);
                let n = a.len().max(b.len());
                let w = width(n, "A")?;
                let signed = flag("A_SIGNED") && flag("B_SIGNED");
                let eq = self.fresh(1);
                self.cells.push(
                    Cell::prim(name.clone(), label, G::Eq { w })
                        .input(0, fit(&a, n, signed))
                        .input(1, fit(&b, n, signed))
                        .output(0, eq.clone()),
                );
                self.result_bit(&name, eq, &y, matches!(ty, "$ne" | "$nex"));
            }
            "$reduce_and" | "$reduce_or" | "$reduce_xor" | "$reduce_xnor" | "$reduce_bool"
            | "$logic_not" => {
                let y = conn("Y")?;
                let a = conn("A")?;
                let w = width(a.len(), "A")?;
                let kind = match ty {
                    "$reduce_and" => G::ReduceAnd { w },
                    "$reduce_xor" | "$reduce_xnor" => G::ReduceXor { w },
                    _ => G::ReduceOr { w },
                };
                let r = self.fresh(1);
                self.cells.push(
                    Cell::prim(name.clone(), label, kind)
                        .input(0, a)
                        .output(0, r.clone()),
                );
                let invert = matches!(ty, "$reduce_xnor" | "$logic_not");
                self.result_bit(&name, r, &y, invert);
            }
            "$dff" | "$_DFF_P_" | "$_DFF_N_" => {
                conn(if ty == "$dff" { "CLK" } else { "C" })?;
                self.clocks += 1;
                let (d, q) = (conn("D")?, conn("Q")?);
                let w = width(q.len(), "Q")?;
                if d.len() != q.len() {
                    return err(format!("{ty}: D and Q must have one width"));
                }
                self.cells.push(
                    Cell::prim(name, label, G::Delay { w })
                        .input(0, d)
                        .output(0, q),
                );
            }
            "$dffe" | "$_DFFE_PP_" | "$_DFFE_PN_" | "$_DFFE_NP_" | "$_DFFE_NN_" => {
                conn(if ty == "$dffe" { "CLK" } else { "C" })?;
                self.clocks += 1;
                let (d, q, mut en) = (conn("D")?, conn("Q")?, conn("E").or_else(|_| conn("EN"))?);
                let w = width(q.len(), "Q")?;
                if d.len() != q.len() || en.len() != 1 {
                    return err(format!("{ty}: D and Q must have one width, EN one bit"));
                }
                let active_high = if ty == "$dffe" {
                    param("EN_POLARITY").map(|v| v != 0).unwrap_or(true)
                } else {
                    ty.ends_with("P_")
                };
                if !active_high {
                    let n = self.fresh(1);
                    let not = self.names.fresh(&format!("{name}_en"));
                    self.cells.push(
                        Cell::prim(not, None, G::Not { w: 1 })
                            .input(0, en)
                            .output(0, n.clone()),
                    );
                    en = n;
                }
                self.cells.push(
                    Cell::prim(name, label, G::Register { w })
                        .input(0, en)
                        .input(1, d)
                        .output(0, q),
                );
            }
            t if arith::handles(t) || state::handles(t) => {
                let c = build::CellIn::new(t, params, conns);
                if arith::handles(t) {
                    self.map_arith(&c, name, label)?;
                } else {
                    self.map_state(&c, name, label)?;
                }
                self.xs += c.xs.get();
            }
            t if t.starts_with('$') => {
                if let Some(why) = arith::refusal(t).or_else(|| state::refusal(t)) {
                    return err(format!("cell type {t} is not supported: {why}"));
                }
                return err(format!(
                    "cell type {t} is not supported (supported: {}; run `proc; opt` and, \
                     for other cells, `techmap` first)",
                    SUPPORTED_CELLS.join(" ")
                ));
            }
            t => {
                let Some((key, module)) = self.modules.get_key_value(t) else {
                    return err(format!(
                        "instances module \"{}\", which is not in the netlist",
                        strip(t)
                    ));
                };
                if is_blackbox(module) {
                    return err(format!(
                        "instances \"{}\", a black box (no contents to import)",
                        strip(t)
                    ));
                }
                let mut sub_xs = 0;
                let mut ports = ports(module, &mut sub_xs)?;
                ports.sort_by_key(|p| ident(&p.0));
                let label = label.filter(|l| *l != name);
                let mut node = Cell {
                    name,
                    label,
                    body: Body::Use(self.circuit_names[key.as_str()].clone()),
                    inputs: Vec::new(),
                    outputs: Vec::new(),
                    helper: false,
                };
                for (port, input, bits) in ports {
                    let w = bits.len();
                    let pin = ident(&port);
                    let connected = conns.and_then(|c| c.get(&port)).is_some();
                    if input {
                        let bits = if connected {
                            let b = conn(&port)?;
                            if b.len() != w {
                                return err(format!(
                                    "port {pin}: {} bits connected to a {w}-bit port",
                                    b.len()
                                ));
                            }
                            Some(b)
                        } else {
                            None
                        };
                        node.inputs.push((pin, w as u8, bits));
                    } else {
                        let bits = if connected {
                            conn(&port)?
                        } else {
                            self.fresh(w)
                        };
                        node.outputs.push((pin, w as u8, bits));
                    }
                }
                self.cells.push(node);
            }
        }
        self.xs += xs;
        Ok(())
    }

    /// Drive `y[0]` from the 1-bit `r` (inverted if asked) and the other
    /// bits of `y` with 0.
    fn result_bit(&mut self, name: &str, r: Vec<Bit>, y: &[Bit], invert: bool) {
        let Some((first, rest)) = y.split_first() else {
            return;
        };
        if invert {
            let not = self.names.fresh(&format!("{name}_not"));
            self.cells.push(
                Cell::prim(not, None, GateKind::Not { w: 1 })
                    .input(0, r)
                    .output(0, vec![*first]),
            );
        } else {
            self.alias(&[*first], &r);
        }
        self.alias(rest, &[]);
    }
}

/// Where a bit comes from.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Src {
    Pin(usize, usize, u8),
    Const(bool),
}

/// A run of bits from one place.
#[derive(Debug, Clone, PartialEq)]
enum Run {
    Pin {
        node: usize,
        pin: usize,
        lo: u8,
        len: u8,
    },
    Const {
        value: u64,
        len: u8,
    },
    /// One bit of an output pin, repeated (a sign extension).
    Rep {
        node: usize,
        pin: usize,
        bit: u8,
        len: u8,
    },
    /// A whole output pin, sign-extended to `len` bits.
    Sext {
        node: usize,
        pin: usize,
        len: u8,
    },
}

fn import_module(
    module_name: &str,
    circuit_name: &str,
    modules: &Map<String, Value>,
    circuit_names: &HashMap<&str, String>,
    warnings: &mut Vec<String>,
) -> Result<SourceCircuit, RtlError> {
    let module = &modules[module_name];
    if is_blackbox(module) {
        return err("is a black box (no contents to import)");
    }
    let mut xs = 0;
    let ports = ports(module, &mut xs)?;
    let mut max_net = 0;
    let mut note = |bits: &[Bit]| {
        for b in bits {
            if let Bit::Net(n) = b {
                max_net = max_net.max(*n);
            }
        }
    };
    for (_, _, bits) in &ports {
        note(bits);
    }
    for (_, cell) in cells(module) {
        if let Some(Value::Object(c)) = cell.get("connections") {
            for v in c.values() {
                if let Ok(bits) = parse_bits(Some(v), "", &mut 0) {
                    note(&bits);
                }
            }
        }
    }
    let mut b = ModuleBuilder {
        cells: Vec::new(),
        names: Names::default(),
        alias: HashMap::new(),
        next_net: max_net,
        modules,
        circuit_names,
        xs,
        clocks: 0,
        asyncs: 0,
        register_files: Vec::new(),
    };
    for (port, input, bits) in &ports {
        let pin = ident(port);
        let name = b.names.fresh(&pin);
        let label = (name != pin).then_some(pin);
        let w = bits.len() as u8;
        let kind = if *input {
            GateKind::Input { w }
        } else {
            GateKind::Output { w }
        };
        let cell = Cell::prim(name, label, kind);
        b.cells.push(if *input {
            cell.output(0, bits.clone())
        } else {
            cell.input(0, bits.clone())
        });
    }
    for (cell_name, cell) in cells(module) {
        b.map_cell(cell_name, cell)
            .map_err(|e| RtlError(format!("cell \"{}\": {}", strip(cell_name), e.0)))?;
    }
    b.prune_helpers();

    // Drivers of every net.
    let mut drivers: HashMap<u64, (usize, usize, u8)> = HashMap::new();
    for (c, cell) in b.cells.iter().enumerate() {
        for (p, (_, _, bits)) in cell.outputs.iter().enumerate() {
            for (i, bit) in bits.iter().enumerate() {
                if let Bit::Net(n) = *bit
                    && drivers.insert(n, (c, p, i as u8)).is_some()
                {
                    return err(format!("net {n} has more than one driver"));
                }
            }
        }
    }
    for &n in b.alias.keys() {
        if drivers.contains_key(&n) {
            return err(format!("net {n} has more than one driver"));
        }
    }
    let mut undriven = 0usize;
    let mut resolve = |bit: Bit| -> Src {
        let mut bit = bit;
        for _ in 0..=b.alias.len() {
            match bit {
                Bit::Zero => return Src::Const(false),
                Bit::One => return Src::Const(true),
                Bit::Net(n) => {
                    if let Some(&(c, p, i)) = drivers.get(&n) {
                        return Src::Pin(c, p, i);
                    }
                    match b.alias.get(&n) {
                        Some(&next) => bit = next,
                        None => break,
                    }
                }
            }
        }
        undriven += 1;
        Src::Const(false)
    };

    // Wire every input pin, adding Split, Merge and Const nodes as needed.
    let mut nodes = b.cells.clone();
    let mut names = std::mem::take(&mut b.names);
    let mut edges: Vec<(usize, usize, usize, usize)> = Vec::new();
    let mut splits: BTreeMap<(usize, usize, u8, u8), usize> = BTreeMap::new();
    let mut reps: BTreeMap<(usize, usize, u8, u8), usize> = BTreeMap::new();
    let mut sexts: BTreeMap<(usize, usize, u8), usize> = BTreeMap::new();
    for c in 0..b.cells.len() {
        for p in 0..b.cells[c].inputs.len() {
            let Some(bits) = b.cells[c].inputs[p].2.clone() else {
                continue;
            };
            let mut runs: Vec<Run> = Vec::new();
            for bit in bits {
                let src = resolve(bit);
                match (runs.last_mut(), src) {
                    (Some(Run::Pin { node, pin, lo, len }), Src::Pin(n, q, i))
                        if *node == n && *pin == q && *lo + *len == i =>
                    {
                        *len += 1;
                    }
                    (
                        Some(Run::Rep {
                            node,
                            pin,
                            bit,
                            len,
                        }),
                        Src::Pin(n, q, i),
                    ) if *node == n && *pin == q && *bit == i => {
                        *len += 1;
                    }
                    (Some(Run::Pin { node, pin, lo, len }), Src::Pin(n, q, i))
                        if *node == n && *pin == q && *lo + *len == i + 1 =>
                    {
                        // The last bit again: it starts a repeated run.
                        let (node, pin) = (*node, *pin);
                        if *len == 1 {
                            runs.pop();
                        } else {
                            *len -= 1;
                        }
                        runs.push(Run::Rep {
                            node,
                            pin,
                            bit: i,
                            len: 2,
                        });
                    }
                    (Some(Run::Const { value, len }), Src::Const(v)) => {
                        *value |= u64::from(v) << *len;
                        *len += 1;
                    }
                    (_, Src::Pin(node, pin, lo)) => runs.push(Run::Pin {
                        node,
                        pin,
                        lo,
                        len: 1,
                    }),
                    (_, Src::Const(v)) => runs.push(Run::Const {
                        value: u64::from(v),
                        len: 1,
                    }),
                }
            }
            // A whole pin and then its top bit repeated: one sign extension.
            let mut i = 0;
            while i + 1 < runs.len() {
                if let (
                    Run::Pin {
                        node,
                        pin,
                        lo: 0,
                        len,
                    },
                    Run::Rep {
                        node: n,
                        pin: q,
                        bit,
                        len: r,
                    },
                ) = (&runs[i], &runs[i + 1])
                    && (node, pin) == (n, q)
                    && *bit == *len
                    && *len + 1 == nodes[*node].outputs[*pin].1
                {
                    runs[i] = Run::Sext {
                        node: *node,
                        pin: *pin,
                        len: *len + *r,
                    };
                    runs.remove(i + 1);
                }
                i += 1;
            }
            let sink = nodes[c].name.clone();
            let sink_pin = nodes[c].inputs[p].0.clone();
            let mut sources: Vec<(usize, usize, u8)> = Vec::new();
            for run in &runs {
                match *run {
                    Run::Sext { node, pin, len } => {
                        let x = match sexts.get(&(node, pin, len)) {
                            Some(&x) => x,
                            None => {
                                let full = nodes[node].outputs[pin].1;
                                let name = names.fresh(&format!("{}_x{}", nodes[node].name, len));
                                let kind = GateKind::Extend {
                                    from: full,
                                    to: len,
                                    signed: true,
                                };
                                nodes.push(Cell::prim(name, None, kind));
                                let x = nodes.len() - 1;
                                edges.push((node, pin, x, 0));
                                sexts.insert((node, pin, len), x);
                                x
                            }
                        };
                        sources.push((x, 0, len));
                    }
                    Run::Pin { node, pin, lo, len } => {
                        let full = nodes[node].outputs[pin].1;
                        if lo == 0 && len == full {
                            sources.push((node, pin, len));
                            continue;
                        }
                        let s = match splits.get(&(node, pin, lo, len)) {
                            Some(&s) => s,
                            None => {
                                let base = format!("{}_{}_{}", nodes[node].name, lo, lo + len);
                                let name = names.fresh(&base);
                                let kind = GateKind::Split { w: full, lo, len };
                                nodes.push(Cell::prim(name, None, kind));
                                let s = nodes.len() - 1;
                                edges.push((node, pin, s, 0));
                                splits.insert((node, pin, lo, len), s);
                                s
                            }
                        };
                        sources.push((s, 0, len));
                    }
                    Run::Const { value, len } => {
                        let name = names.fresh(&format!("{sink}_{sink_pin}_k"));
                        nodes.push(Cell::prim(name, None, GateKind::Const { w: len, value }));
                        sources.push((nodes.len() - 1, 0, len));
                    }
                    Run::Rep {
                        node,
                        pin,
                        bit,
                        len,
                    } => {
                        let r = match reps.get(&(node, pin, bit, len)) {
                            Some(&r) => r,
                            None => {
                                let full = nodes[node].outputs[pin].1;
                                let (from, from_pin) = if full == 1 {
                                    (node, pin)
                                } else {
                                    let s = match splits.get(&(node, pin, bit, 1)) {
                                        Some(&s) => s,
                                        None => {
                                            let base =
                                                format!("{}_{}_{}", nodes[node].name, bit, bit + 1);
                                            let name = names.fresh(&base);
                                            let kind = GateKind::Split {
                                                w: full,
                                                lo: bit,
                                                len: 1,
                                            };
                                            nodes.push(Cell::prim(name, None, kind));
                                            let s = nodes.len() - 1;
                                            edges.push((node, pin, s, 0));
                                            splits.insert((node, pin, bit, 1), s);
                                            s
                                        }
                                    };
                                    (s, 0)
                                };
                                let base = format!("{}_{}_x{}", nodes[node].name, bit, len);
                                let name = names.fresh(&base);
                                let kind = GateKind::Extend {
                                    from: 1,
                                    to: len,
                                    signed: true,
                                };
                                nodes.push(Cell::prim(name, None, kind));
                                let r = nodes.len() - 1;
                                edges.push((from, from_pin, r, 0));
                                reps.insert((node, pin, bit, len), r);
                                r
                            }
                        };
                        sources.push((r, 0, len));
                    }
                }
            }
            if let [(node, pin, _)] = sources.as_slice() {
                edges.push((*node, *pin, c, p));
                continue;
            }
            let name = names.fresh(&format!("{sink}_{sink_pin}_cat"));
            let kind = GateKind::Merge {
                widths: sources.iter().map(|s| s.2).collect(),
            };
            nodes.push(Cell::prim(name, None, kind));
            let m = nodes.len() - 1;
            for (i, (node, pin, _)) in sources.iter().enumerate() {
                edges.push((*node, *pin, m, i));
            }
            edges.push((m, 0, c, p));
        }
    }
    let place = format!("module \"{}\"", strip(module_name));
    if undriven > 0 {
        warnings.push(format!("{place}: {undriven} undriven bit(s) read as 0"));
    }
    if b.xs > 0 {
        warnings.push(format!("{place}: {} x/z bit(s) read as 0", b.xs));
    }
    if b.clocks > 0 {
        warnings.push(format!(
            "{place}: {} flip-flop(s) are clocked by the global tick (their clock pin is ignored)",
            b.clocks
        ));
    }
    if b.asyncs > 0 {
        warnings.push(format!(
            "{place}: {} flip-flop(s) or latch(es) have an asynchronous reset, set or load: \
             it shows on the output at once and is stored at the next tick",
            b.asyncs
        ));
    }
    for memory in &b.register_files {
        warnings.push(format!(
            "{place}: memory {memory} has several write ports, so it is built from \
             registers"
        ));
    }
    if let Some(inits) = initial_values(module) {
        warnings.push(format!(
            "{place}: {inits} initial value(s) (`init` attributes) are ignored: state \
             powers up at 0"
        ));
    }

    // Lay out and emit.
    let layout_nodes: Vec<LayoutNode> = nodes.iter().map(Cell::layout_node).collect();
    let layout_edges: Vec<(usize, usize)> = edges.iter().map(|e| (e.0, e.2)).collect();
    let placed = layout::layout(&layout_nodes, &layout_edges);
    let source_nodes = nodes
        .iter()
        .zip(&placed.positions)
        .map(|(cell, p)| {
            let body = match &cell.body {
                Body::Prim(kind) => {
                    let Ok(Value::Object(mut params)) = serde_json::to_value(kind) else {
                        unreachable!("a kind serializes to an object");
                    };
                    let ty = params
                        .shift_remove("type")
                        .and_then(|t| t.as_str().map(str::to_string))
                        .expect("tagged");
                    if params.get("w").and_then(Value::as_u64) == Some(1) {
                        params.shift_remove("w");
                    }
                    // Optional parameters at their defaults.
                    if params.get("mask") == Some(&Value::Bool(false)) {
                        params.shift_remove("mask");
                    }
                    if params
                        .get("init")
                        .and_then(Value::as_array)
                        .is_some_and(Vec::is_empty)
                    {
                        params.shift_remove("init");
                    }
                    debug_assert_eq!(primitive_kind(&ty, &params).ok().as_ref(), Some(kind));
                    NodeBody::Primitive { ty, params }
                }
                Body::Use(circuit) => NodeBody::Use(circuit.clone()),
            };
            SourceNode {
                name: cell.name.clone(),
                body,
                label: cell.label.clone(),
                at: [p.x, p.y],
                array: None,
                key: None,
            }
        })
        .collect();
    // One wire entry per driving pin, in order of first use.
    let mut wires: Vec<Vec<String>> = Vec::new();
    let mut entry: HashMap<(usize, usize), usize> = HashMap::new();
    for &(from, from_pin, to, to_pin) in &edges {
        let i = *entry.entry((from, from_pin)).or_insert_with(|| {
            wires.push(vec![format!(
                "{}.{}",
                nodes[from].name, nodes[from].outputs[from_pin].0
            )]);
            wires.len() - 1
        });
        wires[i].push(format!("{}.{}", nodes[to].name, nodes[to].inputs[to_pin].0));
    }
    let title = strip(module_name);
    Ok(SourceCircuit {
        name: circuit_name.to_string(),
        title: (title != circuit_name).then(|| title.to_string()),
        nodes: source_nodes,
        wires,
        tests: Vec::new(),
    })
}
