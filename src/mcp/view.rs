//! Text views of a circuit (`docs/mcp-server.md`, "`view`").
//!
//! The outline is one line per part: how to refer to it, its kind and
//! parameters, and what drives each input pin. Generated `Split`/`Merge`
//! adapters are not listed; a pin fed through them reads as the bits it
//! gets (`a:0..4`, `{lo, hi}`), which is how the wiring was written.

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;

use serde_json::Value;
use uuid::Uuid;

use super::batch::{self, Names, names_of};
use crate::sim::{CircuitDocument, CircuitLibrary, DocNode, GateKind, WorkspaceDocument};

/// Lines of parts an outline shows unless asked for more.
pub const DEFAULT_LIMIT: usize = 200;

/// The chips of the workspace, as the library that names pins.
pub fn library_of(ws: &WorkspaceDocument) -> CircuitLibrary {
    CircuitLibrary {
        definitions: ws.definitions.clone(),
    }
}

/// `Const w=8 value=3`, `full_adder` for a chip.
pub fn describe_kind(kind: &GateKind, library: &CircuitLibrary) -> String {
    if let GateKind::SubCircuit { definition } = kind {
        return library.get(*definition).map_or_else(
            || format!("chip {definition} (missing)"),
            |d| d.name.clone(),
        );
    }
    let Ok(Value::Object(fields)) = serde_json::to_value(kind) else {
        return format!("{kind:?}");
    };
    let mut out = String::new();
    for (k, v) in &fields {
        match (k.as_str(), v) {
            ("type", Value::String(ty)) => out.insert_str(0, ty),
            (_, Value::Array(items)) if items.len() > 8 => {
                let _ = write!(out, " {k}=[{} values]", items.len());
            }
            (_, Value::String(s)) => {
                let _ = write!(out, " {k}={s}");
            }
            _ => {
                let _ = write!(out, " {k}={v}");
            }
        }
    }
    out
}

/// `fa[10]` -> `("fa", Some(10))`, so that array elements sort by index.
fn natural_key(reference: &str) -> (String, Option<u32>) {
    match reference.split_once('[') {
        Some((base, rest)) => (base.to_string(), rest.trim_end_matches(']').parse().ok()),
        None => (reference.to_string(), None),
    }
}

fn is_adapter(node: &DocNode) -> bool {
    node.label.is_none() && matches!(node.kind, GateKind::Split { .. } | GateKind::Merge { .. })
}

struct Outline<'a> {
    doc: &'a CircuitDocument,
    library: &'a CircuitLibrary,
    names: Names,
    nodes: HashMap<Uuid, &'a DocNode>,
    /// The wire into each input pin.
    drivers: HashMap<(Uuid, u8), (Uuid, u8)>,
}

impl Outline<'_> {
    /// What output pin `pin` of `node` is, as a source reference; through
    /// generated adapters.
    fn source(&self, node: Uuid, pin: u8, depth: usize) -> String {
        let Some(n) = self.nodes.get(&node) else {
            return "?".into();
        };
        if is_adapter(n) && depth < 8 {
            match &n.kind {
                GateKind::Split { lo, len, .. } => {
                    let inner = self.input(node, 0, depth + 1);
                    return if *len == 1 {
                        format!("{inner}:{lo}")
                    } else {
                        format!("{inner}:{lo}..{}", lo + len)
                    };
                }
                GateKind::Merge { widths } => {
                    let parts: Vec<String> = (0..widths.len())
                        .map(|i| self.input(node, i as u8, depth + 1))
                        .collect();
                    return format!("{{{}}}", parts.join(", "));
                }
                _ => {}
            }
        }
        let name = &self.names.reference[&node];
        let (_, outs) = self.library.pin_names(&n.kind);
        if outs.len() <= 1 {
            name.clone()
        } else {
            format!(
                "{name}.{}",
                outs.get(usize::from(pin)).map_or("?", String::as_str)
            )
        }
    }

    /// What drives input pin `pin` of `node` (`-` when nothing does).
    fn input(&self, node: Uuid, pin: u8, depth: usize) -> String {
        match self.drivers.get(&(node, pin)) {
            Some(&(from, from_pin)) => self.source(from, from_pin, depth),
            None => "-".into(),
        }
    }

    fn line(&self, node: &DocNode) -> String {
        let mut line = format!(
            "{}  {}",
            self.names.reference[&node.id],
            describe_kind(&node.kind, self.library)
        );
        let (ins, _) = self.library.pin_names(&node.kind);
        for (i, pin) in ins.iter().enumerate() {
            let _ = write!(line, "  {pin}<-{}", self.input(node.id, i as u8, 0));
        }
        line
    }
}

/// The outline of circuit `doc` of `ws`. `filter` keeps the parts whose
/// reference or kind starts with it; at most `limit` part lines.
pub fn outline(
    ws: &WorkspaceDocument,
    doc: &CircuitDocument,
    filter: Option<&str>,
    limit: usize,
) -> String {
    let library = library_of(ws);
    let o = Outline {
        doc,
        library: &library,
        names: names_of(doc, &library),
        nodes: doc.nodes.iter().map(|n| (n.id, n)).collect(),
        drivers: doc
            .wires
            .iter()
            .map(|w| ((w.to.node, w.to.pin), (w.from.node, w.from.pin)))
            .collect(),
    };
    let adapters = doc.nodes.iter().filter(|n| is_adapter(n)).count();
    let is_root = doc.id == ws.root.id;
    let mut out = String::new();
    let _ = writeln!(
        out,
        "circuit \"{}\"{}: {} parts{}, {} wires",
        if is_root { "main" } else { &doc.name },
        if is_root { " (root)" } else { " (chip)" },
        doc.nodes.len() - adapters,
        if adapters > 0 {
            format!(" + {adapters} generated adapters")
        } else {
            String::new()
        },
        doc.wires.len(),
    );

    // Ports, in pin order.
    let (ins, outs) = doc.interface();
    let port = |n: &&DocNode| {
        let w = n.kind.width().unwrap_or(1);
        let label = n.label.as_deref().unwrap_or("?");
        if w == 1 {
            label.to_string()
        } else {
            format!("{label}[{w}]")
        }
    };
    if !ins.is_empty() || !outs.is_empty() {
        let list = |ports: &[&DocNode]| ports.iter().map(port).collect::<Vec<_>>().join(" ");
        let _ = writeln!(out, "ports: in {} | out {}", list(&ins), list(&outs));
    }

    if let Some(tests) = ws.tests.get(&doc.id).filter(|t| !t.is_empty()) {
        let names: Vec<&str> = tests.iter().map(super::testing::name_of).collect();
        let _ = writeln!(out, "tests: {}", names.join(", "));
    }

    // Chips used.
    let mut chips: BTreeMap<String, usize> = BTreeMap::new();
    for n in &doc.nodes {
        if matches!(n.kind, GateKind::SubCircuit { .. }) {
            *chips.entry(describe_kind(&n.kind, &library)).or_default() += 1;
        }
    }
    if !chips.is_empty() {
        let list: Vec<String> = chips.iter().map(|(n, c)| format!("{n} x{c}")).collect();
        let _ = writeln!(out, "chips: {}", list.join(", "));
    }

    // Parts: ports first, then the rest, each group by reference (array
    // elements in index order).
    let mut parts: Vec<&DocNode> = doc.nodes.iter().filter(|n| !is_adapter(n)).collect();
    let key = |n: &&DocNode| {
        let (base, index) = natural_key(&o.names.reference[&n.id]);
        let group = match n.kind {
            GateKind::Input { .. } => 0,
            GateKind::Output { .. } | GateKind::Display { .. } => 2,
            _ => 1,
        };
        (group, base, index)
    };
    parts.sort_by_cached_key(key);
    let matches = |n: &&DocNode| {
        filter.is_none_or(|f| {
            o.names.reference[&n.id].starts_with(f)
                || describe_kind(&n.kind, &library).starts_with(f)
        })
    };
    let shown: Vec<&DocNode> = parts.into_iter().filter(matches).collect();
    let _ = writeln!(out, "parts:");
    for n in shown.iter().take(limit) {
        let _ = writeln!(out, "  {}", o.line(n));
    }
    if shown.len() > limit {
        let _ = writeln!(
            out,
            "  ... {} more parts (narrow with \"filter\", or raise \"limit\")",
            shown.len() - limit
        );
    }
    let _ = o.doc;
    out
}

/// The workspace at a glance: every circuit with its size, and the build.
pub fn status(ws: &WorkspaceDocument) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "main (root, \"{}\"): {} parts, {} wires",
        ws.root.name,
        ws.root.nodes.len(),
        ws.root.wires.len()
    );
    let mut chips: Vec<&CircuitDocument> = ws.definitions.values().map(|d| &**d).collect();
    chips.sort_by(|a, b| (&a.name, a.id).cmp(&(&b.name, b.id)));
    let mut uses: HashMap<Uuid, usize> = HashMap::new();
    for doc in std::iter::once(&ws.root).chain(chips.iter().copied()) {
        for id in doc.instanced_definitions() {
            *uses.entry(id).or_default() += 1;
        }
    }
    for chip in chips {
        let _ = writeln!(
            out,
            "chip {}: {} parts, {} wires, placed {} time(s), {} test(s)",
            chip.name,
            chip.nodes.len(),
            chip.wires.len(),
            uses.get(&chip.id).copied().unwrap_or(0),
            ws.tests.get(&chip.id).map_or(0, Vec::len)
        );
    }
    out.push_str(&diagnostics(ws));
    out
}

/// The build result, inputs nothing drives, and layout advice.
pub fn diagnostics(ws: &WorkspaceDocument) -> String {
    let mut out = String::new();
    match batch::build(ws) {
        Ok(n) => {
            let _ = writeln!(out, "build: ok ({n} parts flattened)");
        }
        Err(errors) => {
            let _ = writeln!(out, "build: failed");
            for e in errors {
                let _ = writeln!(out, "  {e}");
            }
        }
    }
    let library = library_of(ws);
    let mut docs: Vec<&CircuitDocument> = ws.definitions.values().map(|d| &**d).collect();
    docs.sort_by(|a, b| (&a.name, a.id).cmp(&(&b.name, b.id)));
    docs.insert(0, &ws.root);
    for doc in docs {
        let names = names_of(doc, &library);
        let driven: std::collections::HashSet<(Uuid, u8)> =
            doc.wires.iter().map(|w| (w.to.node, w.to.pin)).collect();
        let mut open: Vec<((String, Option<u32>), String)> = Vec::new();
        for n in &doc.nodes {
            let (ins, _) = library.pin_names(&n.kind);
            for (i, pin) in ins.iter().enumerate() {
                if !driven.contains(&(n.id, i as u8)) {
                    open.push((natural_key(&names.reference[&n.id]), pin.clone()));
                }
            }
        }
        if !open.is_empty() {
            open.sort();
            let more = open.len().saturating_sub(20);
            let open: Vec<String> = open
                .into_iter()
                .take(20)
                .map(|((base, index), pin)| match index {
                    Some(i) => format!("{base}[{i}].{pin}"),
                    None => format!("{base}.{pin}"),
                })
                .collect();
            let _ = writeln!(
                out,
                "unconnected inputs in \"{}\" (they read 0): {}{}",
                if doc.id == ws.root.id {
                    "main"
                } else {
                    &doc.name
                },
                open.join(", "),
                if more > 0 {
                    format!(" and {more} more")
                } else {
                    String::new()
                }
            );
        }
    }
    for a in batch::advisories(ws) {
        let _ = writeln!(out, "advice: {a}");
    }
    out
}

/// The groups of parts of `doc` that would make chips
/// ([`batch::groups`]), each as the references a `make_chip` takes.
pub fn groups(ws: &WorkspaceDocument, doc: &CircuitDocument) -> String {
    let library = library_of(ws);
    let names = names_of(doc, &library);
    let groups = batch::groups(doc);
    let mut out = format!(
        "\"{}\": {} parts; {} group(s) that would make chips\n",
        if doc.id == ws.root.id {
            "main"
        } else {
            &doc.name
        },
        doc.nodes.len(),
        groups.len()
    );
    if groups.is_empty() {
        out.push_str(
            "no part of it hangs together apart from the rest; split it by function instead\n",
        );
    }
    for (i, group) in groups.iter().take(12).enumerate() {
        let refs: Vec<String> = group
            .parts
            .iter()
            .map(|id| names.reference[id].clone())
            .collect();
        let refs = batch::compress_refs(refs);
        let _ = writeln!(
            out,
            "group {}: {} parts, {} nets crossing\n  {}",
            i + 1,
            group.parts.len(),
            group.crossing,
            serde_json::to_string(&refs).unwrap_or_default()
        );
    }
    if groups.len() > 12 {
        let _ = writeln!(out, "... {} smaller groups", groups.len() - 12);
    }
    out
}

/// Part counts by kind, of one circuit and flattened.
pub fn stats(ws: &WorkspaceDocument, doc: &CircuitDocument) -> String {
    let library = library_of(ws);
    let mut kinds: BTreeMap<String, usize> = BTreeMap::new();
    for n in &doc.nodes {
        let name = match &n.kind {
            GateKind::SubCircuit { .. } => format!("chip {}", describe_kind(&n.kind, &library)),
            k => k.display_name().to_string(),
        };
        *kinds.entry(name).or_default() += 1;
    }
    let mut out = format!(
        "\"{}\": {} parts, {} wires\n",
        if doc.id == ws.root.id {
            "main"
        } else {
            &doc.name
        },
        doc.nodes.len(),
        doc.wires.len()
    );
    for (kind, count) in kinds {
        let _ = writeln!(out, "  {count:>6}  {kind}");
    }
    if doc.id == ws.root.id {
        let _ = writeln!(out, "flattened: {} parts", batch::flat_parts(ws));
    }
    out
}
