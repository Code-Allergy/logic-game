//! The batch editor: one pure function that applies a list of operations
//! to a workspace document (`docs/mcp-server.md`, "`edit`").
//!
//! ```json
//! {"circuit": "adder", "ops": [
//!   {"add": [{"name": "a", "type": "Input", "w": 8},
//!            {"name": "fa", "use": "full_adder", "count": 8}]},
//!   {"wire": [["a", "fa.A"], ["fa[0..7].Cout", "fa[1..8].Cin"]]}
//! ]}
//! ```
//!
//! Node and wire entries are those of save format version 6
//! ([`crate::sim::source`]), read by the same code. The differences: a
//! node's position (`at`) may be left out, and such nodes are laid out
//! automatically; the name of a node is its label, so the node can be
//! named again in a later batch.
//!
//! A request is applied to a copy of the workspace. The copy is returned
//! only if every operation succeeded and no chip contains itself, so a
//! failed request changes nothing. A workspace that does not compile (a
//! loop of gates, a missing wire) is a normal state while building and is
//! accepted; [`Report::build`] says how it stands.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::sync::Arc;

use bevy::math::Vec2;
use serde_json::{Map, Value};
use uuid::Uuid;

use super::{chips, memfile, testing};
use crate::sim::chip::plan_chip;
use crate::sim::layout::{self, LayoutNode};
use crate::sim::source::{
    self, Context, Dir, Element, Externals, NodeBody, NodeEntry, NodeTable, SourceError,
};
use crate::sim::{CircuitDocument, CircuitLibrary, DocNode, GateKind, WorkspaceDocument};

/// A circuit with more parts than this gets the advice to split it into
/// chips: a layered layout of one flat circuit stops being readable long
/// before it stops being correct.
pub const ADVISORY_PARTS: usize = 150;

/// Arrays are named as arrays up to this many elements.
const MAX_ARRAY: u32 = 4096;

/// Space left between the placed parts of a circuit and a block of newly
/// laid out ones.
const BLOCK_GAP: f32 = 100.0;

/// Why a request was refused. Nothing was changed.
#[derive(Debug, Clone, PartialEq)]
pub struct BatchError(pub String);

impl fmt::Display for BatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for BatchError {}

impl From<SourceError> for BatchError {
    fn from(e: SourceError) -> Self {
        BatchError(e.0)
    }
}

fn err<T>(message: impl Into<String>) -> Result<T, SourceError> {
    Err(SourceError(message.into()))
}

/// What a request did to one circuit.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CircuitReport {
    pub circuit: String,
    /// The circuit did not exist and was made (a chip definition).
    pub created: bool,
    pub parts_added: usize,
    pub parts_removed: usize,
    pub wires_added: usize,
    pub wires_removed: usize,
    /// `Split`/`Merge` parts generated for bus and bit-field wiring.
    pub adapters_added: usize,
    /// Parts placed by the auto-layout.
    pub laid_out: usize,
    /// Wires taken out because a `set` changed the pin they were on.
    pub dropped_wires: Vec<String>,
    /// Parts replaced by their gate-level chips (and, with `deep`, chip
    /// instances pointed at their gate-level copies).
    pub expanded: usize,
    /// Chips `expand` made or made again: gate chips, their cells, and
    /// gate-level copies of chips.
    pub chips_made: usize,
    /// Stored tests set and removed.
    pub tests_set: usize,
    pub tests_removed: usize,
    /// Stored tests of the circuit afterwards.
    pub tests: usize,
    /// Parts (adapters included) and wires of the circuit afterwards.
    pub parts: usize,
    pub wires: usize,
}

/// What a request did.
#[derive(Debug, Clone, PartialEq)]
pub struct Report {
    pub circuits: Vec<CircuitReport>,
    /// `Ok` with the flattened part count, or the build errors.
    pub build: Result<usize, Vec<String>>,
    pub advisories: Vec<String>,
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for c in &self.circuits {
            write!(f, "{}:", c.circuit)?;
            if c.created {
                write!(f, " created;")?;
            }
            let mut change = |n: usize, sign: char, what: &str| {
                if n > 0 {
                    write!(f, " {sign}{n} {what}")
                } else {
                    Ok(())
                }
            };
            change(c.parts_added, '+', "parts")?;
            change(c.parts_removed, '-', "parts")?;
            change(c.wires_added, '+', "wires")?;
            change(c.wires_removed, '-', "wires")?;
            change(c.adapters_added, '+', "adapters")?;
            if c.laid_out > 0 {
                write!(f, " ({} laid out)", c.laid_out)?;
            }
            if c.expanded > 0 {
                write!(f, " ({} expanded to gates", c.expanded)?;
                if c.chips_made > 0 {
                    write!(f, ", {} chips made", c.chips_made)?;
                }
                write!(f, ")")?;
            }
            if c.tests_set > 0 {
                write!(f, " +{} tests", c.tests_set)?;
            }
            if c.tests_removed > 0 {
                write!(f, " -{} tests", c.tests_removed)?;
            }
            write!(f, " -> {} parts, {} wires", c.parts, c.wires)?;
            if c.tests > 0 {
                write!(f, ", {} tests", c.tests)?;
            }
            writeln!(f)?;
            for d in &c.dropped_wires {
                writeln!(f, "  dropped wire {d}")?;
            }
        }
        match &self.build {
            Ok(n) => writeln!(f, "build: ok ({n} parts flattened)")?,
            Err(errors) => {
                writeln!(f, "build: failed")?;
                for e in errors {
                    writeln!(f, "  {e}")?;
                }
            }
        }
        for a in &self.advisories {
            writeln!(f, "advice: {a}")?;
        }
        Ok(())
    }
}

/// Apply `request` to a copy of `workspace` and return the copy.
///
/// `request` is `{"circuit": name, "ops": [...]}` or `{"edits": [those]}`.
/// `externals` resolves `"demo/circuit"` references (the demo catalog).
pub fn apply(
    workspace: &WorkspaceDocument,
    request: &Value,
    externals: &Externals<'_>,
) -> Result<(WorkspaceDocument, Report), BatchError> {
    let Value::Object(request) = request else {
        return Err(BatchError(
            "an edit is an object: {\"circuit\": name, \"ops\": [...]} or {\"edits\": [...]}"
                .into(),
        ));
    };
    let single;
    let (groups, prefix): (&[Value], bool) = match request.get("edits") {
        Some(Value::Array(groups)) => {
            if let Some(k) = request.keys().find(|k| *k != "edits") {
                return Err(BatchError(format!(
                    "unknown field \"{k}\" beside \"edits\""
                )));
            }
            (groups, true)
        }
        Some(_) => return Err(BatchError("\"edits\" must be an array".into())),
        None => {
            single = [Value::Object(request.clone())];
            (&single, false)
        }
    };

    let mut ws = workspace.clone();
    let mut report = Report {
        circuits: Vec::new(),
        build: Ok(0),
        advisories: Vec::new(),
    };
    for (gi, group) in groups.iter().enumerate() {
        let done = apply_group(&mut ws, group, externals);
        let done = if prefix {
            done.context(format_args!("edits[{gi}]"))
        } else {
            done
        };
        merge_report(&mut report.circuits, done?);
    }

    let library = CircuitLibrary {
        definitions: ws.definitions.clone(),
    };
    if let Some(id) = library.find_cycle() {
        let name = library.get(id).map_or("?", |d| d.name.as_str());
        return Err(BatchError(format!("chip \"{name}\" would contain itself")));
    }
    report.build = build(&ws);
    report.advisories = advisories(&ws);
    Ok((ws, report))
}

fn merge_report(all: &mut Vec<CircuitReport>, one: CircuitReport) {
    match all.iter_mut().find(|c| c.circuit == one.circuit) {
        None => all.push(one),
        Some(have) => {
            have.parts_added += one.parts_added;
            have.parts_removed += one.parts_removed;
            have.wires_added += one.wires_added;
            have.wires_removed += one.wires_removed;
            have.adapters_added += one.adapters_added;
            have.laid_out += one.laid_out;
            have.dropped_wires.extend(one.dropped_wires);
            have.expanded += one.expanded;
            have.chips_made += one.chips_made;
            have.tests_set += one.tests_set;
            have.tests_removed += one.tests_removed;
            have.tests = one.tests;
            have.parts = one.parts;
            have.wires = one.wires;
        }
    }
}

/// Compile the workspace: the root, and every chip the root does not
/// reach (a chip not placed yet is compiled on its own, so that it is
/// checked too). The root's flattened part count, or the errors.
pub fn build(ws: &WorkspaceDocument) -> Result<usize, Vec<String>> {
    let options = logi_sim::compile::Options::default();
    let compile = |ws: &WorkspaceDocument| super::compiled::compile(ws, &options).map(|_| ());
    let mut errors = compile(ws).err().unwrap_or_default();
    let library = CircuitLibrary {
        definitions: ws.definitions.clone(),
    };
    let reached = library.reachable_from(&ws.root);
    let mut loose: Vec<&Arc<CircuitDocument>> = ws
        .definitions
        .values()
        .filter(|d| !reached.contains_key(&d.id))
        .collect();
    loose.sort_by(|a, b| (&a.name, a.id).cmp(&(&b.name, b.id)));
    // A chip placed in another loose chip is compiled with that one.
    let mut covered: HashSet<Uuid> = HashSet::new();
    let inside: Vec<HashSet<Uuid>> = loose
        .iter()
        .map(|d| library.reachable_from(d).into_keys().collect())
        .collect();
    for (i, def) in loose.iter().enumerate() {
        if inside
            .iter()
            .enumerate()
            .any(|(j, set)| j != i && set.contains(&def.id))
        {
            covered.insert(def.id);
        }
    }
    for def in loose {
        if covered.contains(&def.id) {
            continue;
        }
        let mut alone = ws.clone();
        alone.root = (**def).clone();
        if let Err(found) = compile(&alone) {
            errors.extend(
                found
                    .into_iter()
                    .map(|e| format!("chip \"{}\": {e}", def.name)),
            );
        }
    }
    if errors.is_empty() {
        Ok(flat_parts(ws))
    } else {
        Err(errors)
    }
}

/// Parts of the root with every chip instance replaced by its contents.
pub fn flat_parts(ws: &WorkspaceDocument) -> usize {
    fn count(
        doc: &CircuitDocument,
        ws: &WorkspaceDocument,
        memo: &mut HashMap<Uuid, usize>,
        depth: usize,
    ) -> usize {
        doc.nodes
            .iter()
            .map(|n| match n.kind {
                GateKind::SubCircuit { definition } if depth < 64 => {
                    if let Some(&c) = memo.get(&definition) {
                        return c;
                    }
                    let c = ws
                        .definitions
                        .get(&definition)
                        .map_or(0, |d| count(d, ws, memo, depth + 1));
                    memo.insert(definition, c);
                    c
                }
                _ => 1,
            })
            .sum()
    }
    count(&ws.root, ws, &mut HashMap::new(), 0)
}

/// Advice about circuits that are too large to read as one sheet.
pub fn advisories(ws: &WorkspaceDocument) -> Vec<String> {
    let mut circuits: Vec<&CircuitDocument> = std::iter::once(&ws.root)
        .chain(ws.definitions.values().map(|d| &**d))
        .filter(|d| d.nodes.len() > ADVISORY_PARTS)
        .collect();
    circuits.sort_by(|a, b| (&a.name, a.id).cmp(&(&b.name, b.id)));
    circuits
        .into_iter()
        .map(|d| {
            let groups = groups(d);
            let hint = match groups.first() {
                Some(g) => format!(
                    " ({} groups of parts would make chips, the largest of {} parts with {} \
                     nets crossing; list them with view \"groups\")",
                    groups.len(),
                    g.parts.len(),
                    g.crossing
                ),
                None => String::new(),
            };
            format!(
                "\"{}\" has {} parts in one circuit (more than {ADVISORY_PARTS}); \
                 split it into chips so it stays readable{hint}",
                d.name,
                d.nodes.len()
            )
        })
        .collect()
}

/// Parts that hang together and touch the rest of their circuit through
/// few nets: a candidate for `make_chip`.
#[derive(Debug, Clone, PartialEq)]
pub struct PartGroup {
    pub parts: Vec<Uuid>,
    /// Nets that cross the group's boundary (what would be its ports).
    pub crossing: usize,
}

/// A part whose output goes to more parts than this is a bus or a
/// control line: it joins everything, so it is not what holds a group
/// together.
const HUB_FANOUT: usize = 8;
/// The smallest group worth a chip.
const MIN_GROUP: usize = 6;

/// Groups of parts of `doc` that would make chips, largest first: the
/// parts connected to each other when ports and widely used signals are
/// left out. A circuit that is one tangle has none.
pub fn groups(doc: &CircuitDocument) -> Vec<PartGroup> {
    let index: HashMap<Uuid, usize> = doc
        .nodes
        .iter()
        .enumerate()
        .map(|(i, n)| (n.id, i))
        .collect();
    let mut fanout = vec![0usize; doc.nodes.len()];
    for w in &doc.wires {
        if let Some(&i) = index.get(&w.from.node) {
            fanout[i] += 1;
        }
    }
    let member: Vec<bool> = doc
        .nodes
        .iter()
        .zip(&fanout)
        .map(|(n, f)| !n.kind.is_port() && *f <= HUB_FANOUT)
        .collect();
    // Union-find over the wires between members.
    let mut parent: Vec<usize> = (0..doc.nodes.len()).collect();
    fn find(parent: &mut [usize], mut i: usize) -> usize {
        while parent[i] != i {
            parent[i] = parent[parent[i]];
            i = parent[i];
        }
        i
    }
    for w in &doc.wires {
        let (Some(&a), Some(&b)) = (index.get(&w.from.node), index.get(&w.to.node)) else {
            continue;
        };
        if member[a] && member[b] {
            let (ra, rb) = (find(&mut parent, a), find(&mut parent, b));
            parent[ra] = rb;
        }
    }
    let mut sets: HashMap<usize, Vec<usize>> = HashMap::new();
    for (i, is_member) in member.iter().enumerate() {
        if *is_member {
            let root = find(&mut parent, i);
            sets.entry(root).or_default().push(i);
        }
    }
    let members = member.iter().filter(|m| **m).count();
    let mut out: Vec<PartGroup> = sets
        .into_values()
        .filter(|set| set.len() >= MIN_GROUP && set.len() * 10 <= members * 7)
        .map(|set| {
            let inside: HashSet<usize> = set.iter().copied().collect();
            let crossing: HashSet<(Uuid, u8)> = doc
                .wires
                .iter()
                .filter(|w| {
                    let a = index.get(&w.from.node).is_some_and(|i| inside.contains(i));
                    let b = index.get(&w.to.node).is_some_and(|i| inside.contains(i));
                    a != b
                })
                .map(|w| (w.from.node, w.from.pin))
                .collect();
            PartGroup {
                parts: set.iter().map(|&i| doc.nodes[i].id).collect(),
                crossing: crossing.len(),
            }
        })
        .collect();
    // Largest first; equal ones in the order of their first part.
    let first = |g: &PartGroup| g.parts.first().and_then(|id| index.get(id)).copied();
    out.sort_by(|a, b| {
        b.parts
            .len()
            .cmp(&a.parts.len())
            .then(first(a).cmp(&first(b)))
    });
    out
}

/// References as few strings as name them: array elements as ranges
/// (`fa[0..4]`), in order.
pub fn compress_refs(mut refs: Vec<String>) -> Vec<String> {
    let key = |r: &String| match array_label(r) {
        Some((base, i)) => (base.to_string(), Some(i)),
        None => (r.clone(), None),
    };
    refs.sort_by_cached_key(key);
    let mut out: Vec<String> = Vec::new();
    let mut run: Option<(String, u32, u32)> = None;
    let flush = |run: &mut Option<(String, u32, u32)>, out: &mut Vec<String>| {
        if let Some((base, a, b)) = run.take() {
            out.push(if b == a + 1 {
                format!("{base}[{a}]")
            } else {
                format!("{base}[{a}..{b}]")
            });
        }
    };
    for r in refs {
        match array_label(&r) {
            Some((base, i)) => match &mut run {
                Some((b, _, end)) if b == base && *end == i => *end = i + 1,
                _ => {
                    flush(&mut run, &mut out);
                    run = Some((base.to_string(), i, i + 1));
                }
            },
            None => {
                flush(&mut run, &mut out);
                out.push(r);
            }
        }
    }
    flush(&mut run, &mut out);
    out
}

/// A chip's ports changed from `old` to `new` (the same chip, by id):
/// make the wires on its placements, everywhere in `ws`, follow their
/// ports. Pins are numbered in label order, so a port that was added,
/// removed or renamed moves the others; each wire stays on the port of
/// the label it was on. A wire whose port is gone or changed width is
/// removed and returned as text.
pub fn rebind_instances(
    ws: &mut WorkspaceDocument,
    old: &CircuitDocument,
    new: &CircuitDocument,
) -> Vec<String> {
    /// Ports as `(label, width)`.
    type Ports = Vec<(String, u8)>;
    let ports = |doc: &CircuitDocument| -> (Ports, Ports) {
        let (ins, outs) = doc.interface();
        let port = |n: &&DocNode| {
            (
                n.label.clone().unwrap_or_default(),
                n.kind.width().unwrap_or(1),
            )
        };
        (
            ins.iter().map(port).collect(),
            outs.iter().map(port).collect(),
        )
    };
    let (old_ports, new_ports) = (ports(old), ports(new));
    if old_ports == new_ports {
        return Vec::new();
    }
    // Old pin -> new pin, where the port is still there with its width.
    let map = |old: &[(String, u8)], new: &[(String, u8)]| -> Vec<Option<u8>> {
        old.iter()
            .map(|port| new.iter().position(|p| p == port).map(|i| i as u8))
            .collect()
    };
    let (in_map, out_map) = (
        map(&old_ports.0, &new_ports.0),
        map(&old_ports.1, &new_ports.1),
    );
    let chip = old.id;
    let mut dropped = Vec::new();
    let mut fix = |doc: &mut CircuitDocument| {
        let instances: HashSet<Uuid> = doc
            .nodes
            .iter()
            .filter(|n| matches!(n.kind, GateKind::SubCircuit { definition } if definition == chip))
            .map(|n| n.id)
            .collect();
        if instances.is_empty() {
            return false;
        }
        let label = |id: Uuid| {
            doc.nodes
                .iter()
                .find(|n| n.id == id)
                .and_then(|n| n.label.clone())
                .unwrap_or_else(|| "?".into())
        };
        let mut gone = Vec::new();
        let mut wires = std::mem::take(&mut doc.wires);
        wires.retain_mut(|w| {
            let mut keep = true;
            if instances.contains(&w.to.node) {
                match in_map.get(usize::from(w.to.pin)).copied().flatten() {
                    Some(pin) => w.to.pin = pin,
                    None => keep = false,
                }
            }
            if instances.contains(&w.from.node) {
                match out_map.get(usize::from(w.from.pin)).copied().flatten() {
                    Some(pin) => w.from.pin = pin,
                    None => keep = false,
                }
            }
            if !keep {
                gone.push((w.from.node, w.to.node));
            }
            keep
        });
        for (from, to) in gone {
            dropped.push(format!(
                "{} -> {} in \"{}\" (the port of \"{}\" is gone or changed width)",
                label(from),
                label(to),
                doc.name,
                old.name
            ));
        }
        doc.wires = wires;
        true
    };
    fix(&mut ws.root);
    for def in ws.definitions.values_mut() {
        if def.id != chip
            && def.nodes.iter().any(
                |n| matches!(n.kind, GateKind::SubCircuit { definition } if definition == chip),
            )
        {
            fix(Arc::make_mut(def));
        }
    }
    dropped
}

// ─────────────────────────────────────────────────────────────────────────────
// Circuits by name
// ─────────────────────────────────────────────────────────────────────────────

/// Where a circuit of the workspace is.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Place {
    Root,
    Definition(Uuid),
}

/// The circuit named `name` (`None`, `"main"` or the root's name: the
/// root).
pub fn find_circuit(ws: &WorkspaceDocument, name: Option<&str>) -> Result<Option<Place>, String> {
    let Some(name) = name else {
        return Ok(Some(Place::Root));
    };
    let mut found: Vec<Uuid> = ws
        .definitions
        .values()
        .filter(|d| d.name == name)
        .map(|d| d.id)
        .collect();
    match found.len() {
        0 if name == ws.root.name || name == "main" => Ok(Some(Place::Root)),
        0 => Ok(None),
        1 => Ok(Some(Place::Definition(found.remove(0)))),
        n => Err(format!(
            "{n} chips are named \"{name}\"; rename one in the editor first"
        )),
    }
}

/// The document at `place`.
pub fn circuit_at(ws: &WorkspaceDocument, place: Place) -> &CircuitDocument {
    match place {
        Place::Root => &ws.root,
        Place::Definition(id) => &ws.definitions[&id],
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Named parts of an existing circuit
// ─────────────────────────────────────────────────────────────────────────────

/// `base[i]` -> `(base, i)`.
fn array_label(label: &str) -> Option<(&str, u32)> {
    let (base, rest) = label.split_once('[')?;
    let index = rest.strip_suffix(']')?;
    if !source::valid_name(base) || index.is_empty() || !index.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some((base, index.parse().ok()?))
}

/// The short id of every node: `#` and the first hex digits of its id, as
/// many as tell it apart from the others (at least six).
pub fn short_ids(doc: &CircuitDocument) -> HashMap<Uuid, String> {
    let hex: Vec<(Uuid, String)> = doc
        .nodes
        .iter()
        .map(|n| (n.id, n.id.simple().to_string()))
        .collect();
    let mut len = 6;
    loop {
        let mut seen = HashSet::new();
        if len >= 32 || hex.iter().all(|(_, h)| seen.insert(&h[..len])) {
            break;
        }
        len += 2;
    }
    hex.into_iter()
        .map(|(id, h)| (id, format!("#{}", &h[..len])))
        .collect()
}

/// The parts of a circuit by the names a batch may use for them.
pub struct Names {
    pub(crate) table: NodeTable,
    /// Labels that several parts share: not usable as names.
    pub ambiguous: HashSet<String>,
    /// How to refer to each node: its label when that names it (`fa[3]`
    /// for an array element), else its short id.
    pub reference: HashMap<Uuid, String>,
}

/// A label that is not a name, as one: every run of other characters
/// becomes `_` (`"data RAM"` is `data_RAM`), so that parts labelled in
/// the editor can be named in references and expressions.
fn as_name(label: &str) -> Option<String> {
    let mut name = String::new();
    for c in label.trim().chars() {
        if c.is_ascii_alphanumeric() || c == '_' {
            name.push(c);
        } else if !name.ends_with('_') {
            name.push('_');
        }
    }
    let name = name.trim_end_matches('_');
    let name = if name.starts_with(|c: char| c.is_ascii_digit()) {
        format!("_{name}")
    } else {
        name.to_string()
    };
    source::valid_name(&name).then_some(name)
}

/// Name the parts of `doc`: by label where the label is a name
/// (letters, digits, `_`) that only one part has, as arrays where parts
/// are labelled `base[0]`, `base[1]`, ... and have the same pins, by the
/// label made a name ([`as_name`]) where that is free, and always by
/// short id.
pub fn names_of(doc: &CircuitDocument, library: &CircuitLibrary) -> Names {
    let ids = short_ids(doc);
    let entry = |nodes: &[&DocNode], names: Vec<String>, is_array: bool| {
        let kind = &nodes[0].kind;
        let (in_w, out_w) = library.pins(kind).unwrap_or_default();
        let (in_n, out_n) = library.pin_names(kind);
        // Pins without names (a chip whose definition is missing) cannot
        // be wired by name; keep widths and names the same length.
        let pad = |w: &[u8], mut n: Vec<String>| {
            n.resize_with(w.len(), || "?".into());
            (w.to_vec(), n)
        };
        NodeEntry {
            is_array,
            elements: nodes
                .iter()
                .zip(names)
                .map(|(n, name)| Element {
                    id: n.id,
                    name,
                    pos: [n.x, n.y],
                })
                .collect(),
            inputs: pad(&in_w, in_n),
            outputs: pad(&out_w, out_n),
        }
    };

    let mut by_label: BTreeMap<&str, Vec<&DocNode>> = BTreeMap::new();
    for n in &doc.nodes {
        if let Some(label) = n.label.as_deref() {
            by_label.entry(label).or_default().push(n);
        }
    }
    let mut table = NodeTable::new();
    let mut ambiguous = HashSet::new();
    let mut reference = HashMap::new();
    let mut arrays: BTreeMap<&str, BTreeMap<u32, &DocNode>> = BTreeMap::new();
    for (label, nodes) in &by_label {
        if nodes.len() > 1 {
            ambiguous.insert((*label).to_string());
        } else if source::valid_name(label) {
            table.insert(
                (*label).to_string(),
                entry(nodes, vec![(*label).to_string()], false),
            );
            reference.insert(nodes[0].id, (*label).to_string());
        } else if let Some((base, i)) = array_label(label) {
            arrays.entry(base).or_default().insert(i, nodes[0]);
        }
    }
    // Labels with spaces or punctuation, as names, where no part has
    // that name and no two labels give the same one.
    let mut renamed: BTreeMap<String, Vec<&DocNode>> = BTreeMap::new();
    for (label, nodes) in &by_label {
        if nodes.len() == 1
            && !source::valid_name(label)
            && array_label(label).is_none()
            && let Some(name) = as_name(label)
        {
            renamed.entry(name).or_default().push(nodes[0]);
        }
    }
    for (name, nodes) in renamed {
        if nodes.len() == 1
            && !by_label.contains_key(name.as_str())
            && !arrays.contains_key(name.as_str())
        {
            reference.insert(nodes[0].id, name.clone());
            table.insert(name.clone(), entry(&nodes, vec![name], false));
        }
    }
    for (base, elements) in arrays {
        let last = *elements.keys().next_back().expect("an array has elements");
        let first = *elements.values().next().expect("an array has elements");
        let pins = library.pins(&first.kind);
        let alike = elements.values().all(|n| library.pins(&n.kind) == pins);
        if last >= MAX_ARRAY || !alike || table.contains_key(base) || ambiguous.contains(base) {
            continue;
        }
        // Elements that were removed (or moved into a chip) leave gaps:
        // the others keep their index, and a gap is an element with no
        // part ([`Uuid::nil`]), refused when it is named.
        let mut array = entry(&[first], vec![String::new()], true);
        array.elements = (0..=last)
            .map(|i| {
                let name = format!("{base}[{i}]");
                match elements.get(&i) {
                    Some(n) => {
                        reference.insert(n.id, name.clone());
                        Element {
                            id: n.id,
                            name,
                            pos: [n.x, n.y],
                        }
                    }
                    None => Element {
                        id: Uuid::nil(),
                        name,
                        pos: [0.0, 0.0],
                    },
                }
            })
            .collect();
        table.insert(base.to_string(), array);
    }
    for n in &doc.nodes {
        let id = &ids[&n.id];
        table.insert(id.clone(), entry(&[n], vec![id.clone()], false));
        reference.entry(n.id).or_insert_with(|| id.clone());
    }
    Names {
        table,
        ambiguous,
        reference,
    }
}

impl Names {
    /// A better message for a reference to a label several parts share.
    fn explain(&self, e: SourceError) -> SourceError {
        for label in &self.ambiguous {
            if e.0.contains(&format!("no node \"{label}\"")) {
                return SourceError(format!(
                    "{}: several parts are labelled \"{label}\"; name them by id (see view)",
                    e.0
                ));
            }
        }
        e
    }

    /// The parts a reference without pin or bits names.
    fn parts(&self, text: &str) -> Result<Vec<Uuid>, SourceError> {
        let r = source::parse_ref(text)?;
        if r.pin.is_some() || r.bits.is_some() {
            return err(format!("\"{text}\": name a part here, not a pin"));
        }
        let entry = self
            .table
            .get(r.node)
            .ok_or_else(|| self.explain(SourceError(format!("no node \"{}\"", r.node))))?;
        let elements: &[Element] = match r.index {
            None => &entry.elements,
            Some(_) if !entry.is_array => return err(format!("\"{}\" is not an array", r.node)),
            Some((a, b)) => {
                if b as usize > entry.elements.len() {
                    return err(format!(
                        "\"{text}\": index past the end of \"{}\" ({} elements)",
                        r.node,
                        entry.elements.len()
                    ));
                }
                &entry.elements[a as usize..b as usize]
            }
        };
        // A whole array is the elements it still has; a gap named by
        // index is an error.
        if r.index.is_some()
            && let Some(gap) = elements.iter().find(|e| e.id.is_nil())
        {
            return err(format!("\"{}\" is not there any more", gap.name));
        }
        Ok(elements
            .iter()
            .map(|e| e.id)
            .filter(|id| !id.is_nil())
            .collect())
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// One group: the ops on one circuit
// ─────────────────────────────────────────────────────────────────────────────

fn strings(value: &Value, what: &str) -> Result<Vec<String>, SourceError> {
    let bad = || SourceError(format!("\"{what}\" takes an array of references (strings)"));
    match value {
        Value::String(s) => Ok(vec![s.clone()]),
        Value::Array(items) => items
            .iter()
            .map(|v| v.as_str().map(str::to_string).ok_or_else(bad))
            .collect(),
        _ => Err(bad()),
    }
}

/// The number of ops a group was given.
fn group_ops(group: &Map<String, Value>) -> usize {
    group
        .get("ops")
        .and_then(Value::as_array)
        .map_or(0, Vec::len)
}

/// What `expand` with `deep` has found out in one request, shared by
/// the groups it makes chips in.
#[derive(Default)]
struct Expansion {
    /// The gate-level copy of each chip (`None`: nothing to expand).
    copies: HashMap<Uuid, Option<Uuid>>,
    /// Whether each chip has a word-level part at any depth.
    word_parts: HashMap<Uuid, bool>,
}

struct Group<'a> {
    doc: CircuitDocument,
    library: CircuitLibrary,
    /// Chip definitions by name, for `use`.
    chips: HashMap<String, Vec<Uuid>>,
    externals: &'a Externals<'a>,
    /// Ids in use, so that a new part never takes the id of another.
    used: RefCell<HashSet<Uuid>>,
    /// Parts waiting for the auto-layout.
    unplaced: HashSet<Uuid>,
    /// The workspace's root id: the ids of new chips derive from it.
    root_id: Uuid,
    /// Chips made by the ops (`make_chip`, `expand`), for the workspace.
    new_defs: Vec<Arc<CircuitDocument>>,
    /// What `expand` with `deep` found out so far.
    expansion: Expansion,
    /// Stored tests to set (by name) and names to remove (`None`: all).
    tests_set: Vec<Value>,
    tests_removed: Vec<Option<String>>,
    report: CircuitReport,
}

fn apply_group(
    ws: &mut WorkspaceDocument,
    group: &Value,
    externals: &Externals<'_>,
) -> Result<CircuitReport, SourceError> {
    let Value::Object(group) = group else {
        return err("an edit is an object: {\"circuit\": name, \"ops\": [...]}");
    };
    if let Some(k) = group
        .keys()
        .find(|k| !matches!(k.as_str(), "circuit" | "ops"))
    {
        return err(format!(
            "unknown field \"{k}\" (an edit has \"circuit\" and \"ops\")"
        ));
    }
    let name = match group.get("circuit") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => Some(s.as_str()),
        Some(_) => return err("\"circuit\" must be a string"),
    };
    let Some(Value::Array(ops)) = group.get("ops") else {
        return err("missing \"ops\" (an array)");
    };

    let mut report = CircuitReport::default();
    let place = match find_circuit(ws, name).map_err(SourceError)? {
        Some(place) => place,
        None => {
            let name = name.expect("the root always exists");
            if !source::valid_name(name) {
                return err(format!(
                    "no circuit \"{name}\", and a new chip's name is letters, digits and '_'"
                ));
            }
            let id = source::item_id(ws.root.id, &format!("circuit/{name}"));
            if ws.definitions.contains_key(&id) || ws.root.id == id {
                return err(format!("cannot make chip \"{name}\": its id is taken"));
            }
            let mut doc = CircuitDocument::new(name);
            doc.id = id;
            ws.definitions.insert(id, Arc::new(doc));
            report.created = true;
            Place::Definition(id)
        }
    };
    // `copy_of` as the first op starts a new chip as a copy of a circuit.
    let mut ops = &ops[..];
    if let Some(Value::Object(first)) = ops.first()
        && let Some(source) = first.get("copy_of")
    {
        let Value::String(source) = source else {
            return err("ops[0]: copy_of: takes the name of a circuit");
        };
        if !report.created || first.len() != 1 {
            return err(format!(
                "ops[0]: copy_of starts a new chip; \"{}\" exists already",
                name.unwrap_or("main")
            ));
        }
        let from = find_circuit(ws, Some(source))
            .map_err(SourceError)?
            .ok_or_else(|| SourceError(format!("ops[0]: copy_of: no circuit \"{source}\"")))?;
        let Place::Definition(id) = place else {
            unreachable!("a new circuit is a definition")
        };
        let mut copy = circuit_at(ws, from).clone();
        copy.id = id;
        copy.name = name.expect("a new circuit has a name").to_string();
        copy.revision = 0;
        report.parts_added = copy.nodes.len();
        report.wires_added = copy.wires.len();
        ws.definitions.insert(id, Arc::new(copy));
        ops = &ops[1..];
    }
    let skipped = group_ops(group) - ops.len();
    let doc = circuit_at(ws, place).clone();
    report.circuit = match place {
        Place::Root => "main".to_string(),
        Place::Definition(_) => doc.name.clone(),
    };
    let context = format!("circuit \"{}\"", report.circuit);

    // The library: this workspace's chips, and the catalog's for
    // "demo/circuit" references.
    let mut definitions = externals
        .library
        .map(|l| l.definitions.clone())
        .unwrap_or_default();
    definitions.extend(ws.definitions.iter().map(|(k, v)| (*k, Arc::clone(v))));
    let mut chips: HashMap<String, Vec<Uuid>> = HashMap::new();
    for d in ws.definitions.values() {
        chips.entry(d.name.clone()).or_default().push(d.id);
    }
    let used = doc
        .nodes
        .iter()
        .map(|n| n.id)
        .chain(doc.wires.iter().map(|w| w.id))
        .collect();
    let before = doc.clone();
    let mut g = Group {
        doc,
        library: CircuitLibrary { definitions },
        chips,
        externals,
        used: RefCell::new(used),
        unplaced: HashSet::new(),
        root_id: ws.root.id,
        new_defs: Vec::new(),
        expansion: Expansion::default(),
        tests_set: Vec::new(),
        tests_removed: Vec::new(),
        report,
    };
    for (oi, op) in ops.iter().enumerate() {
        g.op(op)
            .context(format_args!("ops[{}]", oi + skipped))
            .context(&context)?;
    }
    g.place_unplaced();
    source::check_port_labels(&g.doc).context(&context)?;

    let Group {
        mut doc,
        library,
        mut report,
        new_defs,
        tests_set,
        tests_removed,
        ..
    } = g;
    report.parts = doc.nodes.len();
    report.wires = doc.wires.len();
    if doc != before {
        doc.revision = before.revision + 1;
    }
    let circuit_id = doc.id;
    // A gate-level copy made again replaces the old one in place.
    let mut replaced = Vec::new();
    for def in new_defs {
        if let Some(old) = ws.definitions.insert(def.id, Arc::clone(&def)) {
            replaced.push((old, def));
        }
    }
    // Chips of the catalog that the circuit now uses become part of the
    // workspace.
    for (id, def) in library.reachable_from(&doc) {
        ws.definitions.entry(id).or_insert(def);
    }
    match place {
        Place::Root => ws.root = doc,
        Place::Definition(id) => {
            // Placements of the chip keep their wires on the same ports.
            report
                .dropped_wires
                .extend(rebind_instances(ws, &before, &doc));
            ws.definitions.insert(id, Arc::new(doc));
        }
    }
    for (old, new) in replaced {
        report
            .dropped_wires
            .extend(rebind_instances(ws, &old, &new));
    }

    // Stored tests: removals first, then tests set by name, each checked
    // against the circuit as the ops left it.
    let mut tests = ws.tests.remove(&circuit_id).unwrap_or_default();
    let count = tests.len();
    for removed in &tests_removed {
        match removed {
            None => tests.clear(),
            Some(name) => {
                let before = tests.len();
                tests.retain(|t| testing::name_of(t) != name);
                if tests.len() == before {
                    return err(format!("{context}: remove_tests: no test \"{name}\""));
                }
            }
        }
    }
    report.tests_removed = count - tests.len();
    for test in tests_set {
        testing::validate(ws, place, &test)
            .map_err(|e| SourceError(format!("{context}: tests: {e}")))?;
        let name = testing::name_of(&test).to_string();
        match tests.iter_mut().find(|t| testing::name_of(t) == name) {
            Some(have) => *have = test,
            None => tests.push(test),
        }
        report.tests_set += 1;
    }
    report.tests = tests.len();
    if !tests.is_empty() {
        ws.tests.insert(circuit_id, tests);
    }
    Ok(report)
}

/// The id for `what` (`"node/<name>"`, `"wire/<sink>/<pin>"`) in
/// `circuit`: derived from the circuit's id, so the same request on the
/// same workspace gives the same ids, and never one that is in use.
fn fresh_id(used: &RefCell<HashSet<Uuid>>, circuit: Uuid, what: &str) -> Uuid {
    let mut used = used.borrow_mut();
    let mut id = source::item_id(circuit, what);
    let mut n = 0u32;
    while !used.insert(id) {
        n += 1;
        id = source::item_id(circuit, &format!("{what}#{n}"));
    }
    id
}

/// The definition a `use` reference names, for a part of circuit `this`.
fn definition_of(
    chips: &HashMap<String, Vec<Uuid>>,
    externals: &Externals<'_>,
    this: Uuid,
    r: &str,
) -> Result<Uuid, SourceError> {
    if r.contains('/') {
        return externals
            .names
            .and_then(|n| n.get(r))
            .copied()
            .ok_or_else(|| SourceError(format!("unknown definition \"{r}\"")));
    }
    match chips.get(r).map(Vec::as_slice) {
        Some([id]) if *id == this => err(format!("chip \"{r}\" cannot contain itself")),
        Some([id]) => Ok(*id),
        Some(ids) => err(format!("{} chips are named \"{r}\"", ids.len())),
        None => err(format!(
            "no chip \"{r}\" in this workspace (make it first, or use \"demo/circuit\")"
        )),
    }
}

impl Group<'_> {
    fn names(&self) -> Names {
        names_of(&self.doc, &self.library)
    }

    fn op(&mut self, op: &Value) -> Result<(), SourceError> {
        let Value::Object(op) = op else {
            return err("an op is an object with one field, e.g. {\"add\": [...]}");
        };
        let mut fields = op.iter();
        let (Some((name, arg)), None) = (fields.next(), fields.next()) else {
            return err("an op is an object with one field, e.g. {\"add\": [...]}");
        };
        match name.as_str() {
            "add" => self.add(arg),
            "wire" => self.wire(arg),
            "unwire" => self.unwire(arg),
            "remove" => self.remove(arg),
            "set" => self.set(arg),
            "layout" => self.layout(arg),
            "make_chip" => self.make_chip(arg),
            "expand" => self.expand(arg),
            "copy_of" => err("must be the first op of a new chip"),
            "tests" => match arg {
                Value::Array(tests) => {
                    self.tests_set.extend(tests.iter().cloned());
                    Ok(())
                }
                one @ Value::Object(_) => {
                    self.tests_set.push(one.clone());
                    Ok(())
                }
                _ => err("takes an array of tests: {\"name\": ..., \"table\": {...}}"),
            },
            "remove_tests" => match arg {
                Value::Bool(true) => {
                    self.tests_removed.push(None);
                    Ok(())
                }
                names => {
                    let names = strings(names, "remove_tests")?;
                    self.tests_removed.extend(names.into_iter().map(Some));
                    Ok(())
                }
            },
            other => err(format!(
                "unknown op \"{other}\" (ops: add, wire, unwire, remove, set, layout, make_chip, \
                 expand, copy_of, tests, remove_tests)"
            )),
        }
        .context(name)
    }

    fn definition(&self, r: &str) -> Result<Uuid, SourceError> {
        definition_of(&self.chips, self.externals, self.doc.id, r)
    }

    fn add(&mut self, arg: &Value) -> Result<(), SourceError> {
        let Value::Array(entries) = arg else {
            return err("takes an array of node entries");
        };
        let mut names = self.names();
        for (i, entry) in entries.iter().enumerate() {
            let place = format!("[{i}]");
            let (node, placed) = source::parse_node_entry(entry.clone(), false).context(&place)?;
            if node.label.is_some() {
                return err(format!(
                    "{place} node \"{}\": leave \"label\" out; the name is the label",
                    node.name
                ));
            }
            if names.ambiguous.contains(&node.name) {
                return err(format!(
                    "{place}: several parts are already labelled \"{}\"",
                    node.name
                ));
            }
            if let NodeBody::Primitive { ty, .. } = &node.body
                && matches!(ty.as_str(), "Input" | "Output")
                && node.array.is_some()
            {
                return err(format!(
                    "{place} node \"{}\": a port cannot be an array; give one port a width",
                    node.name
                ));
            }
            let first = self.doc.nodes.len();
            let (id, used, chips, externals) =
                (self.doc.id, &self.used, &self.chips, self.externals);
            source::add_node(
                &mut self.doc,
                &mut names.table,
                &node,
                &self.library,
                &|r| definition_of(chips, externals, id, r),
                true,
                &|what| fresh_id(used, id, what),
            )
            .context(&place)?;
            let added = &self.doc.nodes[first..];
            self.report.parts_added += added.len();
            if !placed {
                self.unplaced.extend(added.iter().map(|n| n.id));
            }
        }
        Ok(())
    }

    fn wire(&mut self, arg: &Value) -> Result<(), SourceError> {
        let Value::Array(entries) = arg else {
            return err("takes an array of wire entries: [source, sink, ...]");
        };
        let wires = entries
            .iter()
            .enumerate()
            .map(|(i, w)| source::parse_wire(w.clone()).context(format_args!("[{i}]")))
            .collect::<Result<Vec<_>, _>>()?;
        let names = self.names();
        let plan = source::plan_wires(&names.table, &wires).map_err(|e| names.explain(e))?;
        let driven: HashMap<(Uuid, u8), Uuid> = self
            .doc
            .wires
            .iter()
            .map(|w| ((w.to.node, w.to.pin), w.from.node))
            .collect();
        for (node, pin) in &plan.sinks {
            if let Some(from) = driven.get(&(*node, *pin)) {
                let (_, pins) = self.pin_names(*node);
                return err(format!(
                    "input \"{}\" of \"{}\" is already driven by \"{}\"; unwire it first",
                    pins.get(usize::from(*pin)).map_or("?", String::as_str),
                    names.reference[node],
                    names.reference[from],
                ));
            }
        }
        let (nodes, wires) = (self.doc.nodes.len(), self.doc.wires.len());
        let (id, used) = (self.doc.id, &self.used);
        source::emit_wires(&mut self.doc, plan, &|what| fresh_id(used, id, what))?;
        let adapters = &self.doc.nodes[nodes..];
        self.report.adapters_added += adapters.len();
        self.report.wires_added += self.doc.wires.len() - wires;
        // Adapters stand beside their part; when that part has no place
        // yet, they are laid out with it.
        if !self.unplaced.is_empty() {
            self.unplaced.extend(adapters.iter().map(|n| n.id));
        }
        Ok(())
    }

    /// `(outputs, inputs)` pin names of a node.
    fn pin_names(&self, node: Uuid) -> (Vec<String>, Vec<String>) {
        self.doc.node(node).map_or_else(Default::default, |n| {
            let (ins, outs) = self.library.pin_names(&n.kind);
            (outs, ins)
        })
    }

    fn unwire(&mut self, arg: &Value) -> Result<(), SourceError> {
        let names = self.names();
        for text in strings(arg, "unwire")? {
            let pins =
                source::resolve(&names.table, &text, Dir::In).map_err(|e| names.explain(e))?;
            for (element, pin, width, lo, len) in pins {
                if lo != 0 || len != width {
                    return err(format!(
                        "\"{text}\": unwire takes whole pins, not bit fields"
                    ));
                }
                let before = self.doc.wires.len();
                self.doc
                    .wires
                    .retain(|w| !(w.to.node == element.id && w.to.pin == pin));
                if self.doc.wires.len() == before {
                    return err(format!("\"{text}\": nothing drives \"{}\"", element.name));
                }
                self.report.wires_removed += before - self.doc.wires.len();
            }
        }
        self.drop_unused_adapters();
        Ok(())
    }

    fn remove(&mut self, arg: &Value) -> Result<(), SourceError> {
        let names = self.names();
        let mut gone = HashSet::new();
        for text in strings(arg, "remove")? {
            gone.extend(names.parts(&text)?);
        }
        self.remove_nodes(&gone);
        self.report.parts_removed += gone.len();
        self.drop_unused_adapters();
        Ok(())
    }

    fn remove_nodes(&mut self, gone: &HashSet<Uuid>) {
        let wires = self.doc.wires.len();
        self.doc.nodes.retain(|n| !gone.contains(&n.id));
        self.doc
            .wires
            .retain(|w| !gone.contains(&w.from.node) && !gone.contains(&w.to.node));
        self.report.wires_removed += wires - self.doc.wires.len();
        self.unplaced.retain(|id| !gone.contains(id));
    }

    /// Generated adapters (`Split` and `Merge` without a label) whose
    /// output no longer goes anywhere are removed, with their wires.
    fn drop_unused_adapters(&mut self) {
        loop {
            let driving: HashSet<Uuid> = self.doc.wires.iter().map(|w| w.from.node).collect();
            let unused: HashSet<Uuid> = self
                .doc
                .nodes
                .iter()
                .filter(|n| {
                    n.label.is_none()
                        && matches!(n.kind, GateKind::Split { .. } | GateKind::Merge { .. })
                        && !driving.contains(&n.id)
                })
                .map(|n| n.id)
                .collect();
            if unused.is_empty() {
                return;
            }
            self.remove_nodes(&unused);
        }
    }

    fn set(&mut self, arg: &Value) -> Result<(), SourceError> {
        let entries: &[Value] = match arg {
            Value::Array(entries) => entries,
            one @ Value::Object(_) => std::slice::from_ref(one),
            _ => return err("takes {\"part\": reference, ...parameters} or an array of them"),
        };
        for (i, entry) in entries.iter().enumerate() {
            self.set_one(entry).context(format_args!("[{i}]"))?;
        }
        Ok(())
    }

    fn set_one(&mut self, entry: &Value) -> Result<(), SourceError> {
        let Value::Object(entry) = entry else {
            return err("must be an object: {\"part\": reference, ...parameters}");
        };
        let mut params: Map<String, Value> = entry.clone();
        let Some(Value::String(part)) = params.shift_remove("part") else {
            return err("missing \"part\" (a reference)");
        };
        let names = self.names();
        let ids = names.parts(&part)?;
        // A whole array keeps its indices under a new label; one element
        // of it (`fa[3]`) takes the label as it is.
        let whole = source::parse_ref(&part)?;
        let is_array =
            whole.index.is_none() && names.table.get(whole.node).is_some_and(|e| e.is_array);
        let label = match params.shift_remove("label") {
            None => None,
            Some(Value::String(l)) if source::valid_name(&l) => Some(l),
            Some(_) => return err("\"label\" must be a name: letters, digits and '_'"),
        };
        let at = match params.shift_remove("at") {
            None => None,
            Some(Value::Array(p)) if ids.len() == 1 && p.len() == 2 => {
                match (p[0].as_f64(), p[1].as_f64()) {
                    (Some(x), Some(y)) => Some((x as f32, y as f32)),
                    _ => return err("\"at\" must be [x, y]"),
                }
            }
            Some(_) => return err("\"at\" must be [x, y], for one part"),
        };
        let chip = match params.shift_remove("use") {
            None => None,
            Some(Value::String(r)) => Some(self.definition(&r)?),
            Some(_) => return err("\"use\" must be a string"),
        };
        if params.contains_key("type") {
            return err("a part's type cannot be set; remove it and add the new one");
        }
        // A keyboard key for an input (`sim::keybind`); null removes it.
        let key = match params.shift_remove("key") {
            None => None,
            Some(Value::Null) => Some(None),
            Some(Value::String(k)) if crate::sim::keybind::key_code(&k).is_some() => Some(Some(k)),
            Some(Value::String(k)) => {
                return err(format!("\"key\": \"{k}\" is not a key (ArrowUp, KeyZ, Enter, Space, ...)"));
            }
            Some(_) => return err("\"key\" must be a key name, or null to remove it"),
        };
        // Memory contents from a file or inline words.
        let load = params.shift_remove("load");
        if let Some(label) = &label
            && (names.table.contains_key(label) || names.ambiguous.contains(label))
        {
            return err(format!("a part is already labelled \"{label}\""));
        }

        let mut changed: HashSet<Uuid> = HashSet::new();
        for (i, id) in ids.iter().enumerate() {
            let node = self
                .doc
                .nodes
                .iter_mut()
                .find(|n| n.id == *id)
                .expect("named parts are in the document");
            if let Some(label) = &label {
                node.label = Some(if is_array {
                    format!("{label}[{i}]")
                } else {
                    label.clone()
                });
            }
            if let Some((x, y)) = at {
                (node.x, node.y) = (x, y);
            }
            if let Some(key) = &key {
                if !matches!(node.kind, GateKind::Input { .. }) {
                    return err(format!("\"{part}\" is not an Input; \"key\" does not apply"));
                }
                let prop = crate::sim::keybind::KEY_PROPERTY.to_string();
                match key {
                    Some(k) => node.properties.insert(prop, k.clone()),
                    None => node.properties.remove(&prop),
                };
                changed.insert(*id);
            }
            let kind = if let Some(definition) = chip {
                if !matches!(node.kind, GateKind::SubCircuit { .. }) {
                    return err(format!("\"{part}\" is not a chip; \"use\" does not apply"));
                }
                GateKind::SubCircuit { definition }
            } else if params.is_empty() && load.is_none() {
                continue;
            } else {
                let Ok(Value::Object(mut have)) = serde_json::to_value(&node.kind) else {
                    return err(format!("\"{part}\" has no parameters"));
                };
                let Some(Value::String(ty)) = have.shift_remove("type") else {
                    return err(format!("\"{part}\" has no parameters"));
                };
                if ty == "SubCircuit" {
                    return err(format!("\"{part}\" is a chip; it has no parameters"));
                }
                for (k, v) in &params {
                    have.insert(k.clone(), v.clone());
                }
                let kind = source::primitive_kind(&ty, &have)?;
                match (&load, kind) {
                    (None, kind) => kind,
                    (
                        Some(spec),
                        GateKind::Rom {
                            addr_bits,
                            w,
                            mut contents,
                        },
                    ) => {
                        let load = memfile::read(spec, w).map_err(SourceError)?;
                        memfile::place(&mut contents, addr_bits, &load).map_err(SourceError)?;
                        GateKind::Rom {
                            addr_bits,
                            w,
                            contents,
                        }
                    }
                    (
                        Some(spec),
                        GateKind::Ram2 {
                            addr_bits,
                            w,
                            reads,
                            mask,
                            mut init,
                        },
                    ) => {
                        let load = memfile::read(spec, w).map_err(SourceError)?;
                        memfile::place(&mut init, addr_bits, &load).map_err(SourceError)?;
                        GateKind::Ram2 {
                            addr_bits,
                            w,
                            reads,
                            mask,
                            init,
                        }
                    }
                    (Some(_), other) => {
                        return err(format!(
                            "\"load\" sets the contents of a Rom or the initial contents of a \
                             Ram2; \"{part}\" is a {}",
                            other.display_name()
                        ));
                    }
                }
            };
            if kind != node.kind {
                node.kind = kind;
                changed.insert(*id);
            }
        }
        self.drop_broken_wires(&changed, &names);
        Ok(())
    }

    /// After a change of kind: wires on pins of `changed` parts that no
    /// longer exist, or whose width no longer matches the other end, are
    /// removed and reported.
    fn drop_broken_wires(&mut self, changed: &HashSet<Uuid>, names: &Names) {
        if changed.is_empty() {
            return;
        }
        let pins: HashMap<Uuid, _> = self
            .doc
            .nodes
            .iter()
            .map(|n| (n.id, self.library.pins(&n.kind).unwrap_or_default()))
            .collect();
        let mut dropped = Vec::new();
        self.doc.wires.retain(|w| {
            if !changed.contains(&w.from.node) && !changed.contains(&w.to.node) {
                return true;
            }
            let from = pins
                .get(&w.from.node)
                .and_then(|(_, outs)| outs.get(usize::from(w.from.pin)));
            let to = pins
                .get(&w.to.node)
                .and_then(|(ins, _)| ins.get(usize::from(w.to.pin)));
            let keep = from.is_some() && from == to;
            if !keep {
                dropped.push(format!(
                    "{} -> {} (input {})",
                    names.reference[&w.from.node], names.reference[&w.to.node], w.to.pin
                ));
            }
            keep
        });
        self.report.wires_removed += dropped.len();
        self.report.dropped_wires.extend(dropped);
        self.drop_unused_adapters();
    }

    fn layout(&mut self, arg: &Value) -> Result<(), SourceError> {
        match arg {
            Value::Bool(true) => self.unplaced = self.doc.nodes.iter().map(|n| n.id).collect(),
            Value::Bool(false) => {}
            parts => {
                let names = self.names();
                for text in strings(parts, "layout")? {
                    self.unplaced.extend(names.parts(&text)?);
                }
            }
        }
        Ok(())
    }

    /// Register a chip the ops made, so later ops can use it.
    fn define(&mut self, def: CircuitDocument) -> Uuid {
        let def = Arc::new(def);
        let named = self.chips.entry(def.name.clone()).or_default();
        if !named.contains(&def.id) {
            named.push(def.id);
        }
        self.library.definitions.insert(def.id, Arc::clone(&def));
        self.new_defs.retain(|d| d.id != def.id);
        let id = def.id;
        self.new_defs.push(def);
        id
    }

    /// The id for a new chip named `name`.
    fn chip_id(&self, name: &str) -> Result<Uuid, SourceError> {
        if !source::valid_name(name) {
            return err(format!(
                "a chip's name is letters, digits and '_', not \"{name}\""
            ));
        }
        if self.chips.contains_key(name) {
            return err(format!("a chip named \"{name}\" exists already"));
        }
        let id = source::item_id(self.root_id, &format!("circuit/{name}"));
        if self.library.definitions.contains_key(&id) || id == self.doc.id {
            return err(format!("cannot make chip \"{name}\": its id is taken"));
        }
        Ok(id)
    }

    /// `{"parts": [refs], "name": chip}`: the parts become a new chip
    /// and are replaced by one instance of it, wired the same way
    /// ([`crate::sim::chip::plan_chip`]).
    fn make_chip(&mut self, arg: &Value) -> Result<(), SourceError> {
        let (Some(parts), Some(Value::String(name))) = (arg.get("parts"), arg.get("name")) else {
            return err("takes {\"parts\": [references], \"name\": chip name}");
        };
        let names = self.names();
        let mut selection = HashSet::new();
        for text in strings(parts, "parts")? {
            selection.extend(names.parts(&text)?);
        }
        let def_id = self.chip_id(name)?;
        if let Some(entry) = names.table.get(name.as_str())
            && entry.elements.iter().any(|e| !selection.contains(&e.id))
        {
            return err(format!(
                "a part is already labelled \"{name}\"; the chip's instance takes that label"
            ));
        }
        let plan = plan_chip(&self.doc, &selection, name, &self.library).map_err(SourceError)?;

        // The plan's new ids are random; derive them, so that the same
        // request gives the same workspace.
        let moved: HashSet<Uuid> = plan.moved.iter().copied().collect();
        let mut def = plan.definition.clone();
        def.id = def_id;
        let (old_ins, old_outs) = {
            let (i, o) = def.interface();
            (
                i.iter().map(|n| n.id).collect::<Vec<_>>(),
                o.iter().map(|n| n.id).collect::<Vec<_>>(),
            )
        };
        let mut remap: HashMap<Uuid, Uuid> = HashMap::new();
        let mut taken: HashSet<String> = HashSet::new();
        let mut ports = 0;
        for node in &mut def.nodes {
            if moved.contains(&node.id) {
                continue;
            }
            // A port: a name for a label, unique among the ports.
            let base = node
                .label
                .as_deref()
                .and_then(as_name)
                .unwrap_or_else(|| "p".into());
            let mut label = base.clone();
            let mut n = 2;
            while !taken.insert(label.clone()) {
                label = format!("{base}_{n}");
                n += 1;
            }
            node.label = Some(label);
            let id = source::item_id(def_id, &format!("port/{ports}"));
            ports += 1;
            remap.insert(node.id, id);
            node.id = id;
        }
        let kept_wires: HashSet<Uuid> = self.doc.wires.iter().map(|w| w.id).collect();
        for (k, wire) in def.wires.iter_mut().enumerate() {
            if let Some(&id) = remap.get(&wire.from.node) {
                wire.from.node = id;
            }
            if let Some(&id) = remap.get(&wire.to.node) {
                wire.to.node = id;
            }
            if !kept_wires.contains(&wire.id) {
                wire.id = source::item_id(def_id, &format!("wire/{k}"));
            }
        }
        // Pins follow the ports' label order, which the renaming may
        // have changed.
        let (new_ins, new_outs) = {
            let (i, o) = def.interface();
            (
                i.iter().map(|n| n.id).collect::<Vec<_>>(),
                o.iter().map(|n| n.id).collect::<Vec<_>>(),
            )
        };
        let repin = |old: &[Uuid], new: &[Uuid], pin: u8| -> u8 {
            let port = remap[&old[usize::from(pin)]];
            new.iter().position(|id| *id == port).expect("a port") as u8
        };
        let old_instance = plan.instance.id;
        let mut instance = plan.instance.clone();
        instance.id = fresh_id(&self.used, self.doc.id, &format!("node/{name}"));
        instance.kind = GateKind::SubCircuit { definition: def_id };

        let wires = self.doc.wires.len();
        self.doc.nodes.retain(|n| !moved.contains(&n.id));
        self.doc
            .wires
            .retain(|w| !plan.removed_wires.contains(&w.id));
        self.report.wires_removed += wires - self.doc.wires.len();
        for wire in &plan.new_wires {
            let mut wire = wire.clone();
            if wire.to.node == old_instance {
                wire.to.node = instance.id;
                wire.to.pin = repin(&old_ins, &new_ins, wire.to.pin);
            } else {
                wire.from.node = instance.id;
                wire.from.pin = repin(&old_outs, &new_outs, wire.from.pin);
            }
            wire.id = fresh_id(
                &self.used,
                self.doc.id,
                &format!("wire/{}/{}/{name}", wire.to.node, wire.to.pin),
            );
            self.doc.wires.push(wire);
            self.report.wires_added += 1;
        }
        self.doc.nodes.push(instance);
        self.unplaced.retain(|id| !moved.contains(id));
        self.report.parts_removed += moved.len();
        self.report.parts_added += 1;
        self.define(def);
        Ok(())
    }

    /// `expand`: word-level parts become instances of their gate-level
    /// chips, made on first use ([`chips::gate_chip_plans`]). The
    /// argument is `true` (every part that has one), references, or
    /// `{"parts": true or references, "deep": true}`: with `deep`, chips
    /// placed here are pointed at gate-level copies of themselves
    /// ([`Group::gates_copy`]).
    fn expand(&mut self, arg: &Value) -> Result<(), SourceError> {
        let all = Value::Bool(true);
        let (parts, deep) = match arg {
            Value::Object(o) => {
                if let Some(k) = o.keys().find(|k| !matches!(k.as_str(), "parts" | "deep")) {
                    return err(format!(
                        "unknown field \"{k}\" (expand takes {{\"parts\": true or references, \
                         \"deep\": true}})"
                    ));
                }
                let deep = match o.get("deep") {
                    None => false,
                    Some(Value::Bool(deep)) => *deep,
                    Some(_) => return err("\"deep\" is true or false"),
                };
                (o.get("parts").unwrap_or(&all), deep)
            }
            parts => (parts, false),
        };
        let targets: Vec<Uuid> = match parts {
            Value::Bool(true) => self.expandable_parts(deep),
            Value::Bool(false) => Vec::new(),
            parts => {
                let names = self.names();
                let mut ids = Vec::new();
                for text in strings(parts, "expand")? {
                    ids.extend(names.parts(&text)?);
                }
                ids
            }
        };
        self.expand_parts(&targets, deep)
    }

    /// The word-level parts of the circuit, and with `deep` its chips.
    fn expandable_parts(&self, deep: bool) -> Vec<Uuid> {
        self.doc
            .nodes
            .iter()
            .filter(|n| {
                chips::expandable(&n.kind)
                    || (deep && matches!(n.kind, GateKind::SubCircuit { .. }))
            })
            .map(|n| n.id)
            .collect()
    }

    fn expand_parts(&mut self, targets: &[Uuid], deep: bool) -> Result<(), SourceError> {
        for &id in targets {
            let node = self.doc.node(id).expect("named parts are in the document");
            let kind = node.kind.clone();
            if let GateKind::SubCircuit { definition } = kind {
                if !deep {
                    return err(
                        "a chip has no gate-level form of its own; expand the chips inside it \
                         with {\"parts\": ..., \"deep\": true}",
                    );
                }
                // A chip with nothing to expand stays as it is.
                if let Some(copy) = self.gates_copy(definition)? {
                    self.node_mut(id).kind = GateKind::SubCircuit { definition: copy };
                    self.report.expanded += 1;
                }
                continue;
            }
            if !chips::expandable(&kind) {
                return err(format!(
                    "a {} has no gate-level form (it is a gate, a memory, wiring or a chip \
                     already)",
                    kind.display_name()
                ));
            }
            let def_id = self.gate_chip(&kind)?;
            let chip = Arc::clone(&self.library.definitions[&def_id]);
            let (in_map, out_map) = chips::pin_maps(&kind, &chip);
            for wire in &mut self.doc.wires {
                if wire.to.node == id {
                    wire.to.pin = in_map.get(usize::from(wire.to.pin)).copied().unwrap_or(0);
                }
                if wire.from.node == id {
                    wire.from.pin = out_map
                        .get(usize::from(wire.from.pin))
                        .copied()
                        .unwrap_or(0);
                }
            }
            self.node_mut(id).kind = GateKind::SubCircuit { definition: def_id };
            self.report.expanded += 1;
        }
        Ok(())
    }

    fn node_mut(&mut self, id: Uuid) -> &mut DocNode {
        self.doc
            .nodes
            .iter_mut()
            .find(|n| n.id == id)
            .expect("the part is in the document")
    }

    /// The gate-level chip of an expandable `kind`: the workspace's chip
    /// of that name ([`chips::gate_chip_name`]) when there is one, else
    /// made now, with the cells and chunk chips it places that the
    /// workspace does not have yet (those it has are used as they are).
    fn gate_chip(&mut self, kind: &GateKind) -> Result<Uuid, SourceError> {
        let name = chips::gate_chip_name(kind);
        if let Some(id) = self.chip_named(&name)? {
            return Ok(id);
        }
        for plan in chips::gate_chip_plans(kind).map_err(SourceError)? {
            if self.chip_named(&plan.name)?.is_none() {
                self.build_chip(&plan)?;
            }
        }
        Ok(self
            .chip_named(&name)?
            .expect("the plan ends with the chip asked for"))
    }

    /// The chip of the workspace named `name`, if there is one.
    fn chip_named(&self, name: &str) -> Result<Option<Uuid>, SourceError> {
        match self.chips.get(name).map(Vec::as_slice) {
            None | Some([]) => Ok(None),
            Some([id]) => Ok(Some(*id)),
            Some(_) => err(format!("several chips are named \"{name}\"")),
        }
    }

    /// Make the chip `plan` describes by applying its ops to a new, empty
    /// circuit, as an `edit` of a new chip would.
    fn build_chip(&mut self, plan: &chips::ChipPlan) -> Result<Uuid, SourceError> {
        let id = self.chip_id(&plan.name)?;
        let mut doc = CircuitDocument::new(plan.name.clone());
        doc.id = id;
        doc.revision = 1;
        let doc = self
            .nested(doc, |g| {
                for (i, op) in plan.ops.iter().enumerate() {
                    g.op(op).context(format_args!("ops[{i}]"))?;
                }
                Ok(())
            })
            .context(format_args!("making chip \"{}\"", plan.name))?;
        self.report.chips_made += 1;
        Ok(self.define(doc))
    }

    /// Run `f` on circuit `doc` as a group of its own that shares this
    /// group's library, chip names, new chips and expansion state; then
    /// lay out what it added and return the circuit.
    fn nested(
        &mut self,
        doc: CircuitDocument,
        f: impl FnOnce(&mut Group<'_>) -> Result<(), SourceError>,
    ) -> Result<CircuitDocument, SourceError> {
        let used = doc
            .nodes
            .iter()
            .map(|n| n.id)
            .chain(doc.wires.iter().map(|w| w.id))
            .collect();
        let mut sub = Group {
            doc,
            library: std::mem::take(&mut self.library),
            chips: std::mem::take(&mut self.chips),
            externals: self.externals,
            used: RefCell::new(used),
            unplaced: HashSet::new(),
            root_id: self.root_id,
            new_defs: std::mem::take(&mut self.new_defs),
            expansion: std::mem::take(&mut self.expansion),
            tests_set: Vec::new(),
            tests_removed: Vec::new(),
            report: CircuitReport::default(),
        };
        let done = f(&mut sub);
        self.library = std::mem::take(&mut sub.library);
        self.chips = std::mem::take(&mut sub.chips);
        self.new_defs = std::mem::take(&mut sub.new_defs);
        self.expansion = std::mem::take(&mut sub.expansion);
        self.report.chips_made += sub.report.chips_made;
        done?;
        sub.place_unplaced();
        source::check_port_labels(&sub.doc)?;
        Ok(sub.doc)
    }

    /// The gate-level copy of chip `source`, for `expand` with `deep`:
    /// `None` when the chip has nothing to expand at any depth.
    ///
    /// The copy is the source with its word-level parts expanded and its
    /// chips replaced by their own copies, the same way. The source is
    /// not changed: it stays the word-level reference to test the copy
    /// against. The copy's id is derived from the source's id
    /// ([`chips::gates_copy_id`]), so a chip a person made under the same
    /// name is never taken for it; its name is `<source>_gates`
    /// ([`chips::gates_copy_name`]), or `<source>_gates_2`, ... when
    /// another chip has that name.
    ///
    /// Staleness: the copy is made again from the source every time a
    /// request expands the source, and compared with the copy the
    /// workspace has (by id). When they are the same, the workspace's copy
    /// is used as it is; when they differ (the source, or a chip inside
    /// it, changed since), the copy is replaced in place, keeping its id
    /// and name, and its placements keep their wires by port. So a copy
    /// follows its source, and edits made by hand to a copy are lost the
    /// next time its source is expanded: to keep them, copy it under
    /// another name (`copy_of`).
    fn gates_copy(&mut self, source: Uuid) -> Result<Option<Uuid>, SourceError> {
        if let Some(done) = self.expansion.copies.get(&source) {
            return Ok(*done);
        }
        let Some(def) = self.library.definitions.get(&source).cloned() else {
            return err("a chip whose definition is missing cannot be expanded");
        };
        if !self.has_word_parts(source) {
            self.expansion.copies.insert(source, None);
            return Ok(None);
        }
        let copy_id = chips::gates_copy_id(self.root_id, source);
        let existing = self.library.definitions.get(&copy_id).cloned();
        let name = match &existing {
            Some(have) => have.name.clone(),
            None => {
                let base = chips::gates_copy_name(&def.name);
                let mut name = base.clone();
                let mut n = 2;
                while self.chips.contains_key(&name) || name == self.doc.name {
                    name = format!("{base}_{n}");
                    n += 1;
                }
                name
            }
        };
        // Known before its insides are expanded, so that a chip placed
        // several times below it is copied once.
        self.expansion.copies.insert(source, Some(copy_id));
        let mut doc = (*def).clone();
        doc.id = copy_id;
        doc.name = name.clone();
        doc.revision = existing.as_ref().map_or(1, |have| have.revision + 1);
        let doc = self
            .nested(doc, |g| {
                let targets = g.expandable_parts(true);
                g.expand_parts(&targets, true)
            })
            .context(format_args!("making chip \"{name}\""))?;
        match existing {
            Some(have) if have.same_content(&doc) => {}
            _ => {
                self.report.chips_made += 1;
                self.define(doc);
            }
        }
        Ok(Some(copy_id))
    }

    /// Whether chip `id` has a word-level part, itself or in a chip it
    /// places at any depth.
    fn has_word_parts(&mut self, id: Uuid) -> bool {
        if let Some(&known) = self.expansion.word_parts.get(&id) {
            return known;
        }
        // A chip inside itself is refused at the end of the request;
        // until then the inner placement counts as having nothing.
        self.expansion.word_parts.insert(id, false);
        let Some(def) = self.library.definitions.get(&id).cloned() else {
            return false;
        };
        let found = def.nodes.iter().any(|n| chips::expandable(&n.kind))
            || def
                .instanced_definitions()
                .any(|inner| self.has_word_parts(inner));
        self.expansion.word_parts.insert(id, found);
        found
    }

    /// Lay out the parts without a place as one block: the whole circuit
    /// when none of it is placed, else below the placed parts.
    fn place_unplaced(&mut self) {
        if self.unplaced.is_empty() {
            return;
        }
        let index: HashMap<Uuid, usize> = self
            .doc
            .nodes
            .iter()
            .filter(|n| self.unplaced.contains(&n.id))
            .enumerate()
            .map(|(i, n)| (n.id, i))
            .collect();
        let nodes: Vec<LayoutNode> = self
            .doc
            .nodes
            .iter()
            .filter(|n| index.contains_key(&n.id))
            .map(|n| LayoutNode::of(&n.kind))
            .collect();
        let edges: Vec<(usize, usize)> = self
            .doc
            .wires
            .iter()
            .filter_map(|w| Some((*index.get(&w.from.node)?, *index.get(&w.to.node)?)))
            .collect();
        let placed = layout::layout(&nodes, &edges);

        // Bounds of the block and of the parts that keep their place.
        let bounds = |boxes: &mut dyn Iterator<Item = (Vec2, Vec2)>| {
            boxes.fold(None, |acc: Option<(Vec2, Vec2)>, (centre, size)| {
                let (lo, hi) = (centre - size / 2.0, centre + size / 2.0);
                Some(acc.map_or((lo, hi), |(a, b)| (a.min(lo), b.max(hi))))
            })
        };
        let block = bounds(
            &mut placed
                .positions
                .iter()
                .copied()
                .zip(nodes.iter().map(|n| n.size)),
        );
        let fixed = bounds(
            &mut self
                .doc
                .nodes
                .iter()
                .filter(|n| !index.contains_key(&n.id))
                .map(|n| (n.position(), n.kind.size())),
        );
        let shift = match (block, fixed) {
            (Some((block_lo, block_hi)), Some((fixed_lo, _))) => {
                let grid = |v: f32| (v / 20.0).round() * 20.0;
                Vec2::new(
                    grid(fixed_lo.x - block_lo.x),
                    grid(fixed_lo.y - BLOCK_GAP - block_hi.y),
                )
            }
            _ => Vec2::ZERO,
        };
        for node in &mut self.doc.nodes {
            if let Some(&i) = index.get(&node.id) {
                let p = placed.positions[i] + shift;
                (node.x, node.y) = (p.x, p.y);
            }
        }
        self.report.laid_out += index.len();
        self.unplaced.clear();
    }
}
