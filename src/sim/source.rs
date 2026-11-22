//! Save format version 6: the hand-editable authoring form.
//!
//! See `docs/save-format.md` ("Version 6") for the full specification. In
//! short: circuits, nodes and pins are named instead of identified by UUID;
//! a node can be an array (`count`, `step`); wires are lists of references
//! such as `["fa[0..7].cout", "fa[1..8].cin"]` with bus and bit-range
//! semantics; `use` instances a definition from the same file
//! (`"half_adder"`) or from another demo (`"half_adder/half_adder"`).
//!
//! A [`SourceFile`] is kept as written ([`parse`] / [`to_text`] round-trip
//! the canonical text byte for byte) and is [`expand`]ed into the version 5
//! in-memory form ([`WorkspaceDocument`]) that the editor and the bridge
//! use. Ids of the expanded documents are derived deterministically (UUID
//! v5) from a key and the names, so expanding the same file twice gives
//! identical documents.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt;
use std::sync::Arc;

use serde_json::{Map, Number, Value};
use uuid::Uuid;

use super::{
    CircuitDocument, CircuitLibrary, DEFAULT_LAYER_UUID, DocNode, DocWire, GateKind, PinAddr,
    WorkspaceDocument,
};

/// Format tag shared with version 5.
pub const FORMAT: &str = "gatequest-workspace";
/// The authoring version.
pub const SOURCE_VERSION: u64 = 6;

/// Namespace of every derived id (UUID v5). Ids derived from it never
/// collide with editor-made ids, which are random (UUID v4).
pub const ID_NAMESPACE: Uuid = Uuid::from_u128(0x6a7e_5f3c_1b2d_4e8f_9a0b_c1d2_e3f4_a5b6);

/// Column limit of the canonical printer.
const LINE_WIDTH: usize = 100;

/// Kinds whose parameters have no `w` (all others default `w` to 1).
const NO_WIDTH_KINDS: &[&str] = &[
    "Merge",
    "Extend",
    "Clock",
    "SrLatch",
    "Screen",
    "SubCircuit",
];

// ─────────────────────────────────────────────────────────────────────────────
// Errors
// ─────────────────────────────────────────────────────────────────────────────

/// A parse or expansion error, with the place it was found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceError(pub String);

impl fmt::Display for SourceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SourceError {}

fn err<T>(message: impl Into<String>) -> Result<T, SourceError> {
    Err(SourceError(message.into()))
}

pub(crate) trait Context<T> {
    fn context(self, place: impl fmt::Display) -> Result<T, SourceError>;
}

impl<T> Context<T> for Result<T, SourceError> {
    fn context(self, place: impl fmt::Display) -> Result<T, SourceError> {
        self.map_err(|e| SourceError(format!("{place}: {}", e.0)))
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Syntax tree
// ─────────────────────────────────────────────────────────────────────────────

/// A version 6 file, as written.
#[derive(Debug, Clone, PartialEq)]
pub struct SourceFile {
    /// Workspace name (optional; demos take their title from the manifest).
    pub name: Option<String>,
    /// Name of the circuit that is opened.
    pub root: String,
    pub circuits: Vec<SourceCircuit>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SourceCircuit {
    /// Name used by `use` references; unique in the file.
    pub name: String,
    /// Display name (defaults to `name`).
    pub title: Option<String>,
    pub nodes: Vec<SourceNode>,
    /// Each entry is `[source, sink, sink, ...]`.
    pub wires: Vec<Vec<String>>,
    /// Stored tests, as written (`docs/save-format.md`, "Stored tests").
    pub tests: Vec<Value>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SourceNode {
    pub name: String,
    pub body: NodeBody,
    pub label: Option<String>,
    pub at: [f32; 2],
    /// `count` and `step` of an array node.
    pub array: Option<NodeArray>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum NodeBody {
    /// A primitive: `type` plus its parameters, as written.
    Primitive {
        ty: String,
        params: Map<String, Value>,
    },
    /// An instance of a definition: `"circuit"` or `"demo/circuit"`.
    Use(String),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NodeArray {
    pub count: u32,
    pub step: [f32; 2],
}

impl SourceFile {
    pub fn circuit(&self, name: &str) -> Option<&SourceCircuit> {
        self.circuits.iter().find(|c| c.name == name)
    }

    /// The `"demo/circuit"` references to other files, sorted.
    pub fn external_uses(&self) -> BTreeSet<String> {
        self.circuits
            .iter()
            .flat_map(|c| &c.nodes)
            .filter_map(|n| match &n.body {
                NodeBody::Use(r) if r.contains('/') => Some(r.clone()),
                _ => None,
            })
            .collect()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Parsing
// ─────────────────────────────────────────────────────────────────────────────

/// Parse a version 6 file. The `format` and `version` fields are checked.
pub fn parse(text: &str) -> Result<SourceFile, SourceError> {
    let value: Value =
        serde_json::from_str(text).map_err(|e| SourceError(format!("not valid JSON: {e}")))?;
    parse_value(value)
}

fn take_object(value: Value, what: &str) -> Result<Map<String, Value>, SourceError> {
    match value {
        Value::Object(map) => Ok(map),
        other => err(format!(
            "{what} must be an object, found {}",
            kind_of(&other)
        )),
    }
}

fn kind_of(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

fn take_string(map: &mut Map<String, Value>, key: &str) -> Result<Option<String>, SourceError> {
    match map.shift_remove(key) {
        None => Ok(None),
        Some(Value::String(s)) => Ok(Some(s)),
        Some(other) => err(format!(
            "\"{key}\" must be a string, found {}",
            kind_of(&other)
        )),
    }
}

fn required_string(map: &mut Map<String, Value>, key: &str) -> Result<String, SourceError> {
    take_string(map, key)?.ok_or_else(|| SourceError(format!("missing \"{key}\"")))
}

fn no_unknown_fields(map: &Map<String, Value>) -> Result<(), SourceError> {
    match map.keys().next() {
        Some(key) => err(format!("unknown field \"{key}\"")),
        None => Ok(()),
    }
}

fn parse_point(value: Value, key: &str) -> Result<[f32; 2], SourceError> {
    let bad = || SourceError(format!("\"{key}\" must be [x, y] (two numbers)"));
    let Value::Array(items) = value else {
        return Err(bad());
    };
    match items.as_slice() {
        [Value::Number(x), Value::Number(y)] => Ok([
            x.as_f64().ok_or_else(bad)? as f32,
            y.as_f64().ok_or_else(bad)? as f32,
        ]),
        _ => Err(bad()),
    }
}

fn parse_value(value: Value) -> Result<SourceFile, SourceError> {
    let mut map = take_object(value, "the file")?;
    let format = take_string(&mut map, "format")?;
    if format.as_deref() != Some(FORMAT) {
        return err(format!("\"format\" must be \"{FORMAT}\""));
    }
    match map.shift_remove("version") {
        Some(Value::Number(n)) if n.as_u64() == Some(SOURCE_VERSION) => {}
        _ => return err(format!("\"version\" must be {SOURCE_VERSION}")),
    }
    let name = take_string(&mut map, "name")?;
    let root = required_string(&mut map, "root")?;
    let circuits = match map.shift_remove("circuits") {
        Some(Value::Array(items)) => items
            .into_iter()
            .enumerate()
            .map(|(i, c)| parse_circuit(c).context(format_args!("circuits[{i}]")))
            .collect::<Result<Vec<_>, _>>()?,
        _ => return err("missing \"circuits\" (an array)"),
    };
    no_unknown_fields(&map)?;
    Ok(SourceFile {
        name,
        root,
        circuits,
    })
}

fn parse_circuit(value: Value) -> Result<SourceCircuit, SourceError> {
    let mut map = take_object(value, "a circuit")?;
    let name = required_string(&mut map, "name")?;
    let place = format!("circuit \"{name}\"");
    let mut inner = || -> Result<SourceCircuit, SourceError> {
        let title = take_string(&mut map, "title")?;
        let nodes = match map.shift_remove("nodes") {
            Some(Value::Array(items)) => items
                .into_iter()
                .enumerate()
                .map(|(i, n)| parse_node(n).context(format_args!("nodes[{i}]")))
                .collect::<Result<Vec<_>, _>>()?,
            None => Vec::new(),
            Some(_) => return err("\"nodes\" must be an array"),
        };
        let wires = match map.shift_remove("wires") {
            Some(Value::Array(items)) => items
                .into_iter()
                .enumerate()
                .map(|(i, w)| parse_wire(w).context(format_args!("wires[{i}]")))
                .collect::<Result<Vec<_>, _>>()?,
            None => Vec::new(),
            Some(_) => return err("\"wires\" must be an array"),
        };
        let tests = match map.shift_remove("tests") {
            Some(Value::Array(items)) if items.iter().all(Value::is_object) => items,
            None => Vec::new(),
            Some(_) => return err("\"tests\" must be an array of objects"),
        };
        no_unknown_fields(&map)?;
        Ok(SourceCircuit {
            name: name.clone(),
            title,
            nodes,
            wires,
            tests,
        })
    };
    inner().context(place)
}

pub(crate) fn parse_wire(value: Value) -> Result<Vec<String>, SourceError> {
    let bad = || SourceError("a wire is [source, sink, ...] (two or more strings)".into());
    let Value::Array(items) = value else {
        return Err(bad());
    };
    if items.len() < 2 {
        return Err(bad());
    }
    items
        .into_iter()
        .map(|v| match v {
            Value::String(s) => Ok(s),
            _ => Err(bad()),
        })
        .collect()
}

fn parse_node(value: Value) -> Result<SourceNode, SourceError> {
    parse_node_entry(value, true).map(|(node, _)| node)
}

/// A node entry. Without `need_at` the position may be left out (the
/// batch editor lays such nodes out): the second value says whether it
/// was given, and an array's `step` defaults to a column.
pub(crate) fn parse_node_entry(
    value: Value,
    need_at: bool,
) -> Result<(SourceNode, bool), SourceError> {
    let mut map = take_object(value, "a node")?;
    let name = required_string(&mut map, "name")?;
    let mut inner = || -> Result<(SourceNode, bool), SourceError> {
        if !valid_name(&name) {
            return err("a node name is letters, digits and '_' (not starting with a digit)");
        }
        let ty = take_string(&mut map, "type")?;
        let use_ = take_string(&mut map, "use")?;
        let label = take_string(&mut map, "label")?;
        let (at, placed) = match map.shift_remove("at") {
            Some(v) => (parse_point(v, "at")?, true),
            None if need_at => return err("missing \"at\" (its position, [x, y])"),
            None => ([0.0, 0.0], false),
        };
        let count = match map.shift_remove("count") {
            None => None,
            Some(Value::Number(n)) => match n.as_u64() {
                Some(c @ 1..=4096) => Some(c as u32),
                _ => return err("\"count\" must be an integer from 1 to 4096"),
            },
            Some(_) => return err("\"count\" must be an integer"),
        };
        let step = map
            .shift_remove("step")
            .map(|v| parse_point(v, "step"))
            .transpose()?;
        let array = match (count, step) {
            (Some(count), Some(step)) => Some(NodeArray { count, step }),
            (None, None) => None,
            (Some(_), None) if need_at => {
                return err("an array (\"count\") needs a \"step\" [dx, dy]");
            }
            (Some(count), None) => Some(NodeArray {
                count,
                step: [0.0, 80.0],
            }),
            (None, Some(_)) => return err("\"step\" is only allowed with \"count\""),
        };
        let body = match (ty, use_) {
            (Some(ty), None) => NodeBody::Primitive {
                ty,
                params: std::mem::take(&mut map),
            },
            (None, Some(r)) => {
                no_unknown_fields(&map)?;
                NodeBody::Use(r)
            }
            (Some(_), Some(_)) => return err("a node has either \"type\" or \"use\", not both"),
            (None, None) => return err("a node needs \"type\" (a primitive) or \"use\""),
        };
        Ok((
            SourceNode {
                name: name.clone(),
                body,
                label,
                at,
                array,
            },
            placed,
        ))
    };
    inner().context(format_args!("node \"{name}\""))
}

/// `#3fa2c1`: a node by the first hex digits of its id (the batch editor,
/// for parts without a usable label).
pub(crate) fn is_id_ref(name: &str) -> bool {
    name.strip_prefix('#')
        .is_some_and(|h| !h.is_empty() && h.chars().all(|c| c.is_ascii_hexdigit()))
}

pub(crate) fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

// ─────────────────────────────────────────────────────────────────────────────
// Canonical printing
// ─────────────────────────────────────────────────────────────────────────────

/// The canonical text of a file. `parse(&to_text(f)) == f`, and for a file
/// in canonical form `to_text(&parse(text)) == text`.
pub fn to_text(file: &SourceFile) -> String {
    to_canonical_json(&file_to_value(file))
}

fn number(v: f32) -> Value {
    if v.fract() == 0.0 && v.abs() < 1e9 {
        Value::from(v as i64)
    } else {
        // The shortest text that reads back as the same f32.
        format!("{v}")
            .parse::<f64>()
            .ok()
            .and_then(Number::from_f64)
            .map_or(Value::Null, Value::Number)
    }
}

fn point(p: [f32; 2]) -> Value {
    Value::Array(vec![number(p[0]), number(p[1])])
}

fn file_to_value(file: &SourceFile) -> Value {
    let mut map = Map::new();
    map.insert("format".into(), FORMAT.into());
    map.insert("version".into(), SOURCE_VERSION.into());
    if let Some(name) = &file.name {
        map.insert("name".into(), name.clone().into());
    }
    map.insert("root".into(), file.root.clone().into());
    map.insert(
        "circuits".into(),
        Value::Array(file.circuits.iter().map(circuit_to_value).collect()),
    );
    Value::Object(map)
}

fn circuit_to_value(circuit: &SourceCircuit) -> Value {
    let mut map = Map::new();
    map.insert("name".into(), circuit.name.clone().into());
    if let Some(title) = &circuit.title {
        map.insert("title".into(), title.clone().into());
    }
    map.insert(
        "nodes".into(),
        Value::Array(circuit.nodes.iter().map(node_to_value).collect()),
    );
    map.insert(
        "wires".into(),
        Value::Array(
            circuit
                .wires
                .iter()
                .map(|w| Value::Array(w.iter().map(|s| s.clone().into()).collect()))
                .collect(),
        ),
    );
    if !circuit.tests.is_empty() {
        map.insert("tests".into(), Value::Array(circuit.tests.clone()));
    }
    Value::Object(map)
}

fn node_to_value(node: &SourceNode) -> Value {
    let mut map = Map::new();
    map.insert("name".into(), node.name.clone().into());
    match &node.body {
        NodeBody::Primitive { ty, params } => {
            map.insert("type".into(), ty.clone().into());
            for (k, v) in params {
                map.insert(k.clone(), v.clone());
            }
        }
        NodeBody::Use(r) => {
            map.insert("use".into(), r.clone().into());
        }
    }
    if let Some(label) = &node.label {
        map.insert("label".into(), label.clone().into());
    }
    map.insert("at".into(), point(node.at));
    if let Some(array) = node.array {
        map.insert("count".into(), array.count.into());
        map.insert("step".into(), point(array.step));
    }
    Value::Object(map)
}

/// Pretty-print JSON in the canonical layout: two-space indent; an object
/// or array that fits on its line (100 columns) is written on one line;
/// otherwise objects get one member per line, arrays of scalars are filled
/// line by line, and other arrays get one element per line.
pub fn to_canonical_json(value: &Value) -> String {
    let mut out = String::new();
    write_value(&mut out, value, 0, 0, 0);
    out.push('\n');
    out
}

fn inline(value: &Value) -> String {
    match value {
        Value::Array(items) => {
            let parts: Vec<String> = items.iter().map(inline).collect();
            format!("[{}]", parts.join(", "))
        }
        Value::Object(map) => {
            let parts: Vec<String> = map
                .iter()
                .map(|(k, v)| format!("{}: {}", Value::String(k.clone()), inline(v)))
                .collect();
            format!("{{{}}}", parts.join(", "))
        }
        scalar => scalar.to_string(),
    }
}

fn is_scalar(value: &Value) -> bool {
    !matches!(value, Value::Array(_) | Value::Object(_))
}

/// `prefix` is the width already used on the line after the indent, and
/// `suffix` what follows the value on it (a comma).
fn write_value(out: &mut String, value: &Value, indent: usize, prefix: usize, suffix: usize) {
    let flat = inline(value);
    if is_scalar(value) || indent + prefix + flat.len() + suffix <= LINE_WIDTH {
        out.push_str(&flat);
        return;
    }
    let pad = " ".repeat(indent + 2);
    match value {
        Value::Object(map) => {
            out.push_str("{\n");
            let n = map.len();
            for (i, (k, v)) in map.iter().enumerate() {
                let key = format!("{}: ", Value::String(k.clone()));
                out.push_str(&pad);
                out.push_str(&key);
                let comma = usize::from(i + 1 < n);
                write_value(out, v, indent + 2, key.len(), comma);
                if comma == 1 {
                    out.push(',');
                }
                out.push('\n');
            }
        }
        Value::Array(items) if items.iter().all(is_scalar) => {
            out.push_str("[\n");
            let mut line = String::new();
            for (i, item) in items.iter().enumerate() {
                let mut text = item.to_string();
                if i + 1 < items.len() {
                    text.push(',');
                }
                if !line.is_empty() && pad.len() + line.len() + 1 + text.len() > LINE_WIDTH {
                    out.push_str(&pad);
                    out.push_str(&line);
                    out.push('\n');
                    line.clear();
                }
                if !line.is_empty() {
                    line.push(' ');
                }
                line.push_str(&text);
            }
            if !line.is_empty() {
                out.push_str(&pad);
                out.push_str(&line);
                out.push('\n');
            }
        }
        Value::Array(items) => {
            out.push_str("[\n");
            let n = items.len();
            for (i, item) in items.iter().enumerate() {
                out.push_str(&pad);
                let comma = usize::from(i + 1 < n);
                write_value(out, item, indent + 2, 0, comma);
                if comma == 1 {
                    out.push(',');
                }
                out.push('\n');
            }
        }
        _ => unreachable!("scalars are written inline"),
    }
    out.push_str(&" ".repeat(indent));
    out.push(if matches!(value, Value::Object(_)) {
        '}'
    } else {
        ']'
    });
}

// ─────────────────────────────────────────────────────────────────────────────
// Ids
// ─────────────────────────────────────────────────────────────────────────────

/// Id of circuit `name` in the file with key `key` (`"demos/<id>"` for a
/// demo, `"workspaces/<name>"` for a workspace file).
pub fn circuit_id(key: &str, name: &str) -> Uuid {
    Uuid::new_v5(&ID_NAMESPACE, format!("{key}/{name}").as_bytes())
}

pub(crate) fn item_id(circuit: Uuid, what: &str) -> Uuid {
    Uuid::new_v5(&circuit, what.as_bytes())
}

// ─────────────────────────────────────────────────────────────────────────────
// Node kinds
// ─────────────────────────────────────────────────────────────────────────────

pub(crate) fn parse_int_text(text: &str) -> Option<u64> {
    let t = text.replace('_', "");
    if let Some(hex) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        u64::from_str_radix(hex, 16).ok()
    } else if let Some(bin) = t.strip_prefix("0b").or_else(|| t.strip_prefix("0B")) {
        u64::from_str_radix(bin, 2).ok()
    } else {
        t.parse().ok()
    }
}

/// Integers written as strings (`"0x1F"`, `"0b101"`) become numbers.
fn int_from_text(value: &Value) -> Result<Value, SourceError> {
    match value {
        Value::String(s) => parse_int_text(s)
            .map(Value::from)
            .ok_or_else(|| SourceError(format!("\"{s}\" is not an integer"))),
        other => Ok(other.clone()),
    }
}

/// The [`GateKind`] of a primitive node.
pub fn primitive_kind(ty: &str, params: &Map<String, Value>) -> Result<GateKind, SourceError> {
    if ty == "SubCircuit" {
        return err("instances are written with \"use\", not \"type\": \"SubCircuit\"");
    }
    let mut map = Map::new();
    map.insert("type".into(), ty.into());
    for (k, v) in params {
        let v = match (k.as_str(), v) {
            ("value", v) => int_from_text(v)?,
            ("contents" | "init", Value::Array(items)) => Value::Array(
                items
                    .iter()
                    .map(int_from_text)
                    .collect::<Result<Vec<_>, _>>()?,
            ),
            _ => v.clone(),
        };
        map.insert(k.clone(), v);
    }
    if !NO_WIDTH_KINDS.contains(&ty) && !map.contains_key("w") {
        map.insert("w".into(), 1.into());
    }
    let kind: GateKind = serde_json::from_value(Value::Object(map))
        .map_err(|e| SourceError(format!("bad {ty} parameters: {e}")))?;
    // Serde ignores unknown fields of a tagged enum; refuse them (typos).
    if let Ok(Value::Object(known)) = serde_json::to_value(&kind)
        && let Some(k) = params.keys().find(|k| !known.contains_key(*k))
    {
        return err(format!("{ty} has no parameter \"{k}\""));
    }
    kind.check()
        .map_err(|e| SourceError(format!("bad {ty} parameters: {e}")))?;
    Ok(kind)
}

// ─────────────────────────────────────────────────────────────────────────────
// Expansion
// ─────────────────────────────────────────────────────────────────────────────

/// Definitions from other files that `"demo/circuit"` references resolve
/// to.
#[derive(Default)]
pub struct Externals<'a> {
    /// `"demo/circuit"` -> definition id.
    pub names: Option<&'a HashMap<String, Uuid>>,
    /// The definitions themselves (and what they instance).
    pub library: Option<&'a CircuitLibrary>,
}

/// The expanded (version 5) form of a file.
#[derive(Debug, Clone)]
pub struct Expansion {
    /// The root circuit and every definition it reaches.
    pub workspace: WorkspaceDocument,
    /// Every circuit of the file by name, in file order.
    pub circuits: Vec<(String, Arc<CircuitDocument>)>,
}

/// Expand `file` into documents. `key` names the file for id derivation
/// (see [`circuit_id`]).
pub fn expand(
    file: &SourceFile,
    key: &str,
    externals: &Externals<'_>,
) -> Result<Expansion, SourceError> {
    let mut by_name: HashMap<&str, &SourceCircuit> = HashMap::new();
    for c in &file.circuits {
        if !valid_name(&c.name) {
            return err(format!(
                "circuit name \"{}\" must be letters, digits and '_'",
                c.name
            ));
        }
        if by_name.insert(c.name.as_str(), c).is_some() {
            return err(format!("two circuits are named \"{}\"", c.name));
        }
    }
    if !by_name.contains_key(file.root.as_str()) {
        return err(format!("root circuit \"{}\" is not in the file", file.root));
    }

    // Local definitions first (depth-first post-order), refusing cycles.
    let mut order: Vec<&str> = Vec::new();
    let mut state: HashMap<&str, bool> = HashMap::new(); // false: visiting
    fn visit<'f>(
        name: &'f str,
        by_name: &HashMap<&'f str, &'f SourceCircuit>,
        state: &mut HashMap<&'f str, bool>,
        order: &mut Vec<&'f str>,
    ) -> Result<(), SourceError> {
        match state.get(name) {
            Some(true) => return Ok(()),
            Some(false) => return err(format!("circuit \"{name}\" contains itself")),
            None => {}
        }
        state.insert(name, false);
        for node in &by_name[name].nodes {
            if let NodeBody::Use(r) = &node.body
                && !r.contains('/')
            {
                if !by_name.contains_key(r.as_str()) {
                    return err(format!(
                        "circuit \"{name}\", node \"{}\": no circuit \"{r}\" in this file",
                        node.name
                    ));
                }
                visit(r.as_str(), by_name, state, order)?;
            }
        }
        state.insert(name, true);
        order.push(name);
        Ok(())
    }
    for c in &file.circuits {
        visit(c.name.as_str(), &by_name, &mut state, &mut order)?;
    }

    let mut library = CircuitLibrary {
        definitions: externals
            .library
            .map(|l| l.definitions.clone())
            .unwrap_or_default(),
    };
    let mut expanded: HashMap<&str, Arc<CircuitDocument>> = HashMap::new();
    for name in order {
        let id = circuit_id(key, name);
        let doc = expand_circuit(by_name[name], id, key, &library, externals)
            .context(format_args!("circuit \"{name}\""))?;
        let doc = Arc::new(doc);
        library.definitions.insert(id, Arc::clone(&doc));
        expanded.insert(name, doc);
    }
    let root = (*expanded[file.root.as_str()]).clone();
    let definitions = library.reachable_from(&root);
    let tests = file
        .circuits
        .iter()
        .filter(|c| !c.tests.is_empty())
        .map(|c| (expanded[c.name.as_str()].id, c.tests.clone()))
        .filter(|(id, _)| *id == root.id || definitions.contains_key(id))
        .collect();
    Ok(Expansion {
        workspace: WorkspaceDocument {
            root,
            definitions,
            tests,
        },
        circuits: file
            .circuits
            .iter()
            .map(|c| (c.name.clone(), Arc::clone(&expanded[c.name.as_str()])))
            .collect(),
    })
}

/// One expanded node of a source node.
pub(crate) struct Element {
    pub(crate) id: Uuid,
    pub(crate) name: String,
    pub(crate) pos: [f32; 2],
}

/// A named node: its elements (one unless an array) and its pins as
/// `(widths, names)`.
pub(crate) struct NodeEntry {
    pub(crate) is_array: bool,
    pub(crate) elements: Vec<Element>,
    pub(crate) inputs: (Vec<u8>, Vec<String>),
    pub(crate) outputs: (Vec<u8>, Vec<String>),
}

/// The named nodes of a circuit.
pub(crate) type NodeTable = HashMap<String, NodeEntry>;

/// A parsed reference `node[range].pin:bits`.
#[derive(Debug, PartialEq)]
pub(crate) struct Ref<'a> {
    pub(crate) node: &'a str,
    pub(crate) index: Option<(u32, u32)>,
    pub(crate) pin: Option<&'a str>,
    pub(crate) bits: Option<(u32, u32)>,
}

fn parse_range(text: &str) -> Option<(u32, u32)> {
    match text.split_once("..") {
        Some((a, b)) => {
            let (a, b) = (a.trim().parse().ok()?, b.trim().parse().ok()?);
            (a < b).then_some((a, b))
        }
        None => {
            let i: u32 = text.trim().parse().ok()?;
            Some((i, i + 1))
        }
    }
}

pub(crate) fn parse_ref(text: &str) -> Result<Ref<'_>, SourceError> {
    let bad = |why: &str| SourceError(format!("bad reference \"{text}\": {why}"));
    let end = text.find(['[', '.', ':']).unwrap_or(text.len());
    let node = &text[..end];
    if !valid_name(node) && !is_id_ref(node) {
        return Err(bad("expected a node name first"));
    }
    let mut rest = &text[end..];
    let mut index = None;
    if let Some(r) = rest.strip_prefix('[') {
        let close = r.find(']').ok_or_else(|| bad("missing ']'"))?;
        index = Some(parse_range(&r[..close]).ok_or_else(|| bad("bad index range"))?);
        rest = &r[close + 1..];
    }
    let mut pin = None;
    if let Some(r) = rest.strip_prefix('.') {
        let end = r.rfind(':').unwrap_or(r.len());
        if end == 0 {
            return Err(bad("empty pin name"));
        }
        pin = Some(&r[..end]);
        rest = &r[end..];
    }
    let mut bits = None;
    if let Some(r) = rest.strip_prefix(':') {
        bits = Some(parse_range(r).ok_or_else(|| bad("bad bit range"))?);
        rest = "";
    }
    if !rest.is_empty() {
        return Err(bad("unexpected text after the node"));
    }
    Ok(Ref {
        node,
        index,
        pin,
        bits,
    })
}

/// Bits `lo .. lo + len` of an output pin.
#[derive(Debug, Clone, PartialEq)]
struct Piece {
    node: Uuid,
    name: String,
    pos: [f32; 2],
    pin: u8,
    pin_width: u8,
    lo: u8,
    len: u8,
}

/// Bits `lo .. lo + len` of an input pin.
#[derive(Debug, Clone)]
struct Slot {
    node: Uuid,
    name: String,
    pos: [f32; 2],
    pin: u8,
    pin_width: u8,
    lo: u8,
    len: u8,
}

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Dir {
    Out,
    In,
}

/// Pin index on `entry` named `pin` (a pin name, or a number).
fn find_pin(entry: &NodeEntry, node: &str, pin: Option<&str>, dir: Dir) -> Result<u8, SourceError> {
    let (widths, names) = match dir {
        Dir::Out => &entry.outputs,
        Dir::In => &entry.inputs,
    };
    let side = if dir == Dir::Out { "output" } else { "input" };
    match pin {
        None => match widths.len() {
            1 => Ok(0),
            0 => err(format!("\"{node}\" has no {side} pin")),
            _ => err(format!(
                "\"{node}\" has several {side} pins ({}); name one, e.g. \"{node}.{}\"",
                names.join(", "),
                names[0]
            )),
        },
        Some(p) => {
            if let Some(i) = names.iter().position(|n| n == p) {
                return Ok(i as u8);
            }
            match p.parse::<usize>() {
                Ok(i) if i < widths.len() => Ok(i as u8),
                _ => err(format!(
                    "\"{node}\" has no {side} pin \"{p}\" (its {side} pins: {})",
                    names.join(", ")
                )),
            }
        }
    }
}

/// A pin a reference names: `(element, pin, pin width, lo, len)`.
pub(crate) type PinBits<'e> = (&'e Element, u8, u8, u8, u8);

/// The pins a reference names.
pub(crate) fn resolve<'e>(
    nodes: &'e NodeTable,
    text: &str,
    dir: Dir,
) -> Result<Vec<PinBits<'e>>, SourceError> {
    let r = parse_ref(text)?;
    let entry = nodes
        .get(r.node)
        .ok_or_else(|| SourceError(format!("no node \"{}\"", r.node)))?;
    let elements: &[Element] = match r.index {
        None => &entry.elements,
        Some(_) if !entry.is_array => {
            return err(format!("\"{}\" is not an array", r.node));
        }
        Some((a, b)) => {
            let n = entry.elements.len() as u32;
            if b > n {
                return err(format!(
                    "\"{text}\": index past the end of \"{}\" ({n} elements)",
                    r.node
                ));
            }
            &entry.elements[a as usize..b as usize]
        }
    };
    // An array named by the batch editor may have gaps where elements
    // were removed (see `crate::mcp::batch::names_of`).
    if let Some(gap) = elements.iter().find(|e| e.id.is_nil()) {
        return err(format!(
            "\"{}\" is not there any more; name the elements that are",
            gap.name
        ));
    }
    let pin = find_pin(entry, r.node, r.pin, dir)?;
    let width = match dir {
        Dir::Out => entry.outputs.0[usize::from(pin)],
        Dir::In => entry.inputs.0[usize::from(pin)],
    };
    let (lo, len) = match r.bits {
        None => (0, width),
        Some((a, b)) => {
            if b > u32::from(width) {
                return err(format!(
                    "\"{text}\": bits {a}..{b} are past the pin's width ({width})"
                ));
            }
            (a as u8, (b - a) as u8)
        }
    };
    Ok(elements.iter().map(|e| (e, pin, width, lo, len)).collect())
}

/// Generated adapters keep this much space from their node's box...
const ADAPTER_GAP: f32 = 30.0;
/// ...with their centre on this grid (the editor's grid)...
const ADAPTER_GRID: f32 = 20.0;
/// ...and stand this far apart (a 50-unit box plus a gap).
const ADAPTER_STEP: f32 = 60.0;

fn template(label: &str, i: u32) -> String {
    label.replace("{i}", &i.to_string())
}

/// The parameters of array element `i`: `{i}` in a string value is `i`,
/// and a value that is then an integer (`"{i}"`) is that number.
fn element_params(params: &Map<String, Value>, i: u32) -> Map<String, Value> {
    params
        .iter()
        .map(|(k, v)| match v {
            Value::String(s) if s.contains("{i}") => {
                let text = template(s, i);
                let value = parse_int_text(&text).map_or(Value::String(text), Value::from);
                (k.clone(), value)
            }
            v => (k.clone(), v.clone()),
        })
        .collect()
}

fn expand_circuit(
    circuit: &SourceCircuit,
    id: Uuid,
    key: &str,
    library: &CircuitLibrary,
    externals: &Externals<'_>,
) -> Result<CircuitDocument, SourceError> {
    let mut doc = CircuitDocument {
        id,
        name: circuit
            .title
            .clone()
            .unwrap_or_else(|| circuit.name.clone()),
        revision: 0,
        nodes: Vec::new(),
        wires: Vec::new(),
    };
    let mut nodes: NodeTable = HashMap::new();
    let definition = |r: &str| -> Result<Uuid, SourceError> {
        if r.contains('/') {
            externals
                .names
                .and_then(|n| n.get(r))
                .copied()
                .ok_or_else(|| SourceError(format!("unknown definition \"{r}\"")))
        } else {
            Ok(circuit_id(key, r))
        }
    };
    let new_id = |what: &str| item_id(id, what);
    for node in &circuit.nodes {
        add_node(
            &mut doc,
            &mut nodes,
            node,
            library,
            &definition,
            false,
            &new_id,
        )?;
    }
    check_port_labels(&doc)?;
    let plan = plan_wires(&nodes, &circuit.wires)?;
    emit_wires(&mut doc, plan, &new_id)?;
    doc.nodes.sort_by_key(|n| n.id);
    doc.wires.sort_by_key(|w| w.id);
    Ok(doc)
}

/// Add the node (or array of nodes) `node` to `doc` and to the table of
/// named nodes. `definition` resolves a `use` reference, `new_id` makes
/// the id of `"node/<name>"`; with `label_all` every node is labelled
/// with its name (the batch editor, `crate::mcp::batch`), not only ports
/// and instances.
pub(crate) fn add_node(
    doc: &mut CircuitDocument,
    nodes: &mut NodeTable,
    node: &SourceNode,
    library: &CircuitLibrary,
    definition: &dyn Fn(&str) -> Result<Uuid, SourceError>,
    label_all: bool,
    new_id: &dyn Fn(&str) -> Uuid,
) -> Result<(), SourceError> {
    let place = format!("node \"{}\"", node.name);
    let kind = match &node.body {
        NodeBody::Primitive { ty, params } if node.array.is_some() => {
            // Element 0 (parameters may use `{i}`, see below).
            primitive_kind(ty, &element_params(params, 0)).context(&place)?
        }
        NodeBody::Primitive { ty, params } => primitive_kind(ty, params).context(&place)?,
        NodeBody::Use(r) => GateKind::SubCircuit {
            definition: definition(r).context(&place)?,
        },
    };
    let pins = library.pins(&kind);
    let ((in_w, out_w), (in_n, out_n)) = match &pins {
        Some(pins) => (pins.clone(), library.pin_names(&kind)),
        None => return err(format!("{place}: its pins are unknown")),
    };
    let is_port = kind.is_port();
    let is_instance = matches!(kind, GateKind::SubCircuit { .. });
    let count = node.array.map_or(1, |a| a.count);
    let step = node.array.map_or([0.0, 0.0], |a| a.step);
    // In an array, `{i}` in a string parameter is the element's index.
    let templated = match &node.body {
        NodeBody::Primitive { ty, params }
            if node.array.is_some()
                && params
                    .values()
                    .any(|v| v.as_str().is_some_and(|s| s.contains("{i}"))) =>
        {
            Some((ty, params))
        }
        _ => None,
    };
    let mut elements = Vec::new();
    for i in 0..count {
        let kind = match templated {
            Some((ty, params)) => {
                let element = primitive_kind(ty, &element_params(params, i))
                    .context(format_args!("{place}, element {i}"))?;
                if library.pins(&element) != pins {
                    return err(format!(
                        "{place}: the elements of an array must have the same pins \
                         (element {i} differs from element 0)"
                    ));
                }
                element
            }
            None => kind.clone(),
        };
        let name = if node.array.is_some() {
            format!("{}[{i}]", node.name)
        } else {
            node.name.clone()
        };
        let label = match &node.label {
            Some(l) => Some(template(l, i)),
            None if is_port && node.array.is_some() => Some(format!("{}{i}", node.name)),
            None if is_port || is_instance || label_all => Some(name.clone()),
            None => None,
        };
        let pos = [
            node.at[0] + step[0] * i as f32,
            node.at[1] + step[1] * i as f32,
        ];
        let node_id = new_id(&format!("node/{name}"));
        doc.nodes.push(DocNode {
            id: node_id,
            kind,
            label,
            x: pos[0],
            y: pos[1],
            layer: DEFAULT_LAYER_UUID,
            layer_order: 0,
            properties: HashMap::new(),
        });
        elements.push(Element {
            id: node_id,
            name,
            pos,
        });
    }
    let entry = NodeEntry {
        is_array: node.array.is_some(),
        elements,
        inputs: (in_w.to_vec(), in_n),
        outputs: (out_w.to_vec(), out_n),
    };
    if nodes.insert(node.name.clone(), entry).is_some() {
        return err(format!("two nodes are named \"{}\"", node.name));
    }
    Ok(())
}

/// Port labels name the instance pins, so they must be unique.
pub(crate) fn check_port_labels(doc: &CircuitDocument) -> Result<(), SourceError> {
    let mut seen = HashSet::new();
    for n in &doc.nodes {
        if let (GateKind::Input { .. } | GateKind::Output { .. }, Some(label)) = (&n.kind, &n.label)
            && !seen.insert((n.kind.is_input(), label.clone()))
        {
            return err(format!("two ports are labelled \"{label}\""));
        }
    }
    Ok(())
}

/// What wire entries connect: per input pin, the source bits driving each
/// of its bits.
pub(crate) struct WirePlan {
    /// The input pins, in the order they were first named.
    pub(crate) sinks: Vec<(Uuid, u8)>,
    assigned: HashMap<(Uuid, u8), (Slot, BTreeMap<u8, Piece>)>,
}

/// Read wire entries against the named nodes.
pub(crate) fn plan_wires(
    nodes: &NodeTable,
    wires: &[Vec<String>],
) -> Result<WirePlan, SourceError> {
    let mut sinks: Vec<(Uuid, u8)> = Vec::new();
    let mut assigned: HashMap<(Uuid, u8), (Slot, BTreeMap<u8, Piece>)> = HashMap::new();
    for (wi, wire) in wires.iter().enumerate() {
        let place = format!("wires[{wi}] {}", Value::from(wire.clone()));
        let sources: Vec<Piece> = resolve(nodes, &wire[0], Dir::Out)
            .context(&place)?
            .into_iter()
            .map(|(e, pin, pin_width, lo, len)| Piece {
                node: e.id,
                name: e.name.clone(),
                pos: e.pos,
                pin,
                pin_width,
                lo,
                len,
            })
            .collect();
        let total: u32 = sources.iter().map(|p| u32::from(p.len)).sum();
        for sink in &wire[1..] {
            let slots: Vec<Slot> = resolve(nodes, sink, Dir::In)
                .context(&place)?
                .into_iter()
                .map(|(e, pin, pin_width, lo, len)| Slot {
                    node: e.id,
                    name: e.name.clone(),
                    pos: e.pos,
                    pin,
                    pin_width,
                    lo,
                    len,
                })
                .collect();
            let sink_total: u32 = slots.iter().map(|s| u32::from(s.len)).sum();
            let mut assign = |slot: &Slot, at: u8, piece: Piece| -> Result<(), SourceError> {
                let key = (slot.node, slot.pin);
                let (_, bits) = assigned.entry(key).or_insert_with(|| {
                    sinks.push(key);
                    (slot.clone(), BTreeMap::new())
                });
                let end = at + piece.len;
                let overlaps = bits
                    .range(..end)
                    .next_back()
                    .is_some_and(|(&lo, p)| lo + p.len > at);
                if overlaps {
                    return err(format!(
                        "\"{sink}\": bits {at}..{end} of input pin {} of \"{}\" are already driven",
                        slot.pin, slot.name
                    ));
                }
                bits.insert(at, piece);
                Ok(())
            };
            if slots.iter().all(|s| u32::from(s.len) == total) {
                // Fan-out: every slot takes all the source bits.
                for slot in &slots {
                    let mut at = slot.lo;
                    for piece in &sources {
                        assign(slot, at, piece.clone()).context(&place)?;
                        at += piece.len;
                    }
                }
            } else if sink_total == total {
                // Bus: the source bits are dealt out to the slots in order.
                let mut queue = sources.iter().cloned();
                let mut current = queue.next();
                for slot in &slots {
                    let mut need = slot.len;
                    let mut at = slot.lo;
                    while need > 0 {
                        let Some(piece) = current.as_mut() else {
                            break;
                        };
                        let take = need.min(piece.len);
                        let mut part = piece.clone();
                        part.len = take;
                        assign(slot, at, part).context(&place)?;
                        at += take;
                        need -= take;
                        piece.lo += take;
                        piece.len -= take;
                        if piece.len == 0 {
                            current = queue.next();
                        }
                    }
                }
            } else {
                return err(format!(
                    "{place}: the source is {total} bit(s) wide but \"{sink}\" takes {} \
                     ({} pin(s)); widths must match, or each sink pin must take all {total} bits",
                    sink_total,
                    slots.len()
                ));
            }
        }
    }
    Ok(WirePlan { sinks, assigned })
}

/// Emit the wires of `plan` into `doc`, with the adapters they need.
/// `new_id` makes the id of `"wire/<sink>/<pin>"` and of an adapter node.
pub(crate) fn emit_wires(
    doc: &mut CircuitDocument,
    plan: WirePlan,
    new_id: &dyn Fn(&str) -> Uuid,
) -> Result<(), SourceError> {
    let WirePlan {
        sinks,
        mut assigned,
    } = plan;
    // Emit wires, with Split/Merge adapters where bits are regrouped. A
    // node's splits stand in a column to its right and its merges in a
    // column to its left, one box apart (see `ADAPTER_*`).
    let mut splits: HashMap<(Uuid, u8, u8, u8), Uuid> = HashMap::new();
    let mut adapters_at: HashMap<(Uuid, bool), u32> = HashMap::new();
    let half_width: HashMap<Uuid, f32> = doc
        .nodes
        .iter()
        .map(|n| (n.id, n.kind.size().x / 2.0))
        .collect();
    let beside = |node: Uuid| {
        let gap = half_width.get(&node).copied().unwrap_or(25.0) + ADAPTER_GAP + 25.0;
        (gap / ADAPTER_GRID).ceil() * ADAPTER_GRID
    };
    for key in sinks {
        let (slot, bits) = assigned.remove(&key).expect("collected above");
        // Every bit driven exactly once.
        let mut next = 0u8;
        for (&lo, piece) in &bits {
            if lo != next {
                break;
            }
            next = lo + piece.len;
        }
        if next != slot.pin_width {
            return err(format!(
                "input pin {} of \"{}\" is {} bits wide but only some of its bits are driven",
                slot.pin, slot.name, slot.pin_width
            ));
        }
        // Coalesce neighbouring bits of the same source pin.
        let mut pieces: Vec<Piece> = Vec::new();
        for piece in bits.into_values() {
            if let Some(last) = pieces.last_mut()
                && last.node == piece.node
                && last.pin == piece.pin
                && last.lo + last.len == piece.lo
            {
                last.len += piece.len;
                continue;
            }
            pieces.push(piece);
        }
        let wire = |from: Uuid, from_pin: u8, to: Uuid, to_pin: u8, what: String| DocWire {
            id: new_id(&format!("wire/{what}")),
            from: PinAddr {
                node: from,
                pin: from_pin,
            },
            to: PinAddr {
                node: to,
                pin: to_pin,
            },
            layer: DEFAULT_LAYER_UUID,
            layer_order: 0,
        };
        let mut drivers: Vec<(Uuid, u8)> = Vec::new();
        for piece in &pieces {
            if piece.lo == 0 && piece.len == piece.pin_width {
                drivers.push((piece.node, piece.pin));
                continue;
            }
            let split_key = (piece.node, piece.pin, piece.lo, piece.len);
            let split = match splits.get(&split_key) {
                Some(&s) => s,
                None => {
                    let what = format!(
                        "~split/{}/{}/{}/{}",
                        piece.name, piece.pin, piece.lo, piece.len
                    );
                    let k = adapters_at.entry((piece.node, true)).or_default();
                    let s = new_id(&format!("node/{what}"));
                    doc.nodes.push(DocNode {
                        id: s,
                        kind: GateKind::Split {
                            w: piece.pin_width,
                            lo: piece.lo,
                            len: piece.len,
                        },
                        label: None,
                        x: piece.pos[0] + beside(piece.node),
                        y: piece.pos[1] - ADAPTER_STEP * *k as f32,
                        layer: DEFAULT_LAYER_UUID,
                        layer_order: 0,
                        properties: HashMap::new(),
                    });
                    *k += 1;
                    doc.wires
                        .push(wire(piece.node, piece.pin, s, 0, format!("{what}/0")));
                    splits.insert(split_key, s);
                    s
                }
            };
            drivers.push((split, 0));
        }
        let sink_what = format!("{}/{}", slot.name, slot.pin);
        if let [(node, pin)] = drivers.as_slice() {
            doc.wires
                .push(wire(*node, *pin, slot.node, slot.pin, sink_what));
            continue;
        }
        let what = format!("~merge/{sink_what}");
        let k = adapters_at.entry((slot.node, false)).or_default();
        let merge = new_id(&format!("node/{what}"));
        doc.nodes.push(DocNode {
            id: merge,
            kind: GateKind::Merge {
                widths: pieces.iter().map(|p| p.len).collect(),
            },
            label: None,
            x: slot.pos[0] - beside(slot.node),
            y: slot.pos[1] - ADAPTER_STEP * *k as f32,
            layer: DEFAULT_LAYER_UUID,
            layer_order: 0,
            properties: HashMap::new(),
        });
        *k += 1;
        for (i, (node, pin)) in drivers.iter().enumerate() {
            doc.wires
                .push(wire(*node, *pin, merge, i as u8, format!("{what}/{i}")));
        }
        doc.wires
            .push(wire(merge, 0, slot.node, slot.pin, sink_what));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn references_parse() {
        let r = parse_ref("fa[0..7].cout").unwrap();
        assert_eq!(
            r,
            Ref {
                node: "fa",
                index: Some((0, 7)),
                pin: Some("cout"),
                bits: None
            }
        );
        let r = parse_ref("instr:12..16").unwrap();
        assert_eq!((r.node, r.pin, r.bits), ("instr", None, Some((12, 16))));
        let r = parse_ref("m[3].Y:0").unwrap();
        assert_eq!(
            (r.index, r.pin, r.bits),
            (Some((3, 4)), Some("Y"), Some((0, 1)))
        );
        assert!(parse_ref("3x").is_err());
        assert!(parse_ref("a[2..1]").is_err());
        assert!(parse_ref("a.").is_err());
    }

    fn expand_text(circuit: &str) -> Result<Expansion, SourceError> {
        let text = format!(
            r#"{{"format": "gatequest-workspace", "version": 6, "root": "c", "circuits": [{circuit}]}}"#
        );
        expand(&parse(&text)?, "test", &Externals::default())
    }

    #[test]
    fn array_parameters_may_use_the_index() {
        let ws = expand_text(
            r#"{"name": "c", "nodes": [
                {"name": "w", "type": "Input", "w": 4, "at": [0, 0]},
                {"name": "s", "type": "Split", "w": 4, "lo": "{i}", "len": 1, "at": [100, 0],
                 "count": 4, "step": [0, -60]},
                {"name": "k", "type": "Const", "w": 3, "value": "{i}", "at": [200, 0],
                 "count": 3, "step": [0, -60]}],
              "wires": [["w", "s"]]}"#,
        )
        .unwrap()
        .workspace;
        let mut los: Vec<u8> = ws
            .root
            .nodes
            .iter()
            .filter_map(|n| match n.kind {
                GateKind::Split { lo, .. } => Some(lo),
                _ => None,
            })
            .collect();
        los.sort_unstable();
        assert_eq!(los, [0, 1, 2, 3]);
        let mut values: Vec<u64> = ws
            .root
            .nodes
            .iter()
            .filter_map(|n| match n.kind {
                GateKind::Const { value, .. } => Some(value),
                _ => None,
            })
            .collect();
        values.sort_unstable();
        assert_eq!(values, [0, 1, 2]);

        // Elements must keep the same pins.
        let e = expand_text(
            r#"{"name": "c", "nodes": [{"name": "g", "type": "Not", "w": "1{i}", "at": [0, 0],
                "count": 3, "step": [0, 60]}]}"#,
        )
        .unwrap_err();
        assert!(e.to_string().contains("the same pins"), "{e}");
        // `{i}` outside an array is not an index.
        let e = expand_text(
            r#"{"name": "c", "nodes": [{"name": "k", "type": "Const", "value": "{i}", "at": [0, 0]}]}"#,
        )
        .unwrap_err();
        assert!(e.to_string().contains("not an integer"), "{e}");
    }

    /// The generated splits of a word stand in a column beside it, one box
    /// apart, on the grid; merges likewise on the other side.
    #[test]
    fn generated_adapters_do_not_overlap() {
        let ws = expand_text(
            r#"{"name": "c", "nodes": [
                {"name": "a", "type": "Input", "w": 8, "at": [0, 0]},
                {"name": "n", "type": "Not", "at": [400, 0], "count": 8, "step": [0, -80]},
                {"name": "y", "type": "Display", "w": 8, "at": [800, 0]}],
              "wires": [["a", "n"], ["n", "y"]]}"#,
        )
        .unwrap()
        .workspace;
        let boxes: Vec<(bevy::math::Vec2, bevy::math::Vec2)> = ws
            .root
            .nodes
            .iter()
            .map(|n| (bevy::math::Vec2::new(n.x, n.y), n.kind.size()))
            .collect();
        assert_eq!(crate::sim::layout::overlaps(&boxes), None);
        let splits: Vec<(f32, f32)> = ws
            .root
            .nodes
            .iter()
            .filter(|n| matches!(n.kind, GateKind::Split { .. }))
            .map(|n| (n.x, n.y))
            .collect();
        assert_eq!(splits.len(), 8);
        assert!(splits.iter().all(|&(x, y)| x == 80.0 && y % 60.0 == 0.0));
        // The merge into the wide display keeps clear of its box.
        let merge = ws
            .root
            .nodes
            .iter()
            .find(|n| matches!(n.kind, GateKind::Merge { .. }))
            .unwrap();
        assert_eq!((merge.x, merge.y), (680.0, 0.0));
    }

    #[test]
    fn canonical_printer_wraps_long_scalar_arrays() {
        let v: Value = serde_json::from_str(&format!(
            "{{\"contents\": [{}]}}",
            (0..60).map(|i| i.to_string()).collect::<Vec<_>>().join(",")
        ))
        .unwrap();
        let text = to_canonical_json(&v);
        assert!(text.lines().all(|l| l.len() <= LINE_WIDTH), "{text}");
        assert_eq!(serde_json::from_str::<Value>(&text).unwrap(), v);
        assert_eq!(
            to_canonical_json(&serde_json::from_str(&text).unwrap()),
            text
        );
    }
}
