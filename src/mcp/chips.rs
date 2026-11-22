//! Chips made by the batch editor: a chip of a set of parts
//! (`make_chip`), and the gate-level form of a word-level part
//! (`expand`). See `docs/mcp-server.md`, "`edit`".
//!
//! ## Gate-level chips
//!
//! [`gate_chip`] builds, for one word-level kind, a circuit of 1-bit
//! gates that does the same: an 8-bit `Add` becomes a ripple-carry adder
//! of `Xor`, `And` and `Or` gates, an 8-bit `Register` eight 1-bit
//! registers. It is `logi_sim::testkit::bitblast` applied to a design of
//! that one part between ports named after its pins, read back as a
//! document and laid out. The chip is an ordinary, editable definition.

use std::collections::{HashMap, HashSet};

use logi_sim::netlist::{Design, NodeId};
use logi_sim::testkit::bitblast::bitblast;
use uuid::Uuid;

use crate::sim::layout::layout_circuit;
use crate::sim::source::item_id;
use crate::sim::{CircuitDocument, CircuitLibrary, DocNode, DocWire, GateKind};

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

/// The gate-level chip of `kind`, with id `id`: ports labelled with the
/// kind's pin names, gates labelled `g0`, `g1`, ...
pub fn gate_chip(kind: &GateKind, id: Uuid) -> Result<CircuitDocument, String> {
    if !expandable(kind) {
        return Err(format!(
            "a {} has no gate-level form (it is a gate, a memory or wiring already)",
            kind.display_name()
        ));
    }
    let nk = kind
        .to_netlist()
        .ok_or("this part is not simulated on its own")?;
    nk.check_params()?;
    let (ins, outs) = nk.pins().ok_or("this part has no pins of its own")?;
    let library = CircuitLibrary::default();
    let (in_names, out_names) = library.pin_names(kind);

    // The part alone between ports named after its pins.
    let name = gate_chip_name(kind);
    let mut design = Design::new(name.clone());
    let root = design.root;
    {
        let mut b = design.builder(root);
        let inputs: Vec<_> = ins
            .iter()
            .zip(&in_names)
            .map(|(w, n)| Some(b.input(n.clone(), *w)))
            .collect();
        let node = b.node(nk, &inputs);
        for (k, n) in out_names.iter().enumerate().take(outs.len()) {
            let net = b.out(node, k as u8);
            b.output(n.clone(), net);
        }
    }
    let flat = bitblast(&design);
    let def = flat.root_def();

    // Keep what an output depends on.
    let mut live: HashSet<NodeId> = HashSet::new();
    let mut stack: Vec<NodeId> = def.outputs.clone();
    stack.extend(def.inputs.iter().copied());
    while let Some(n) = stack.pop() {
        if !live.insert(n) {
            continue;
        }
        for net in def.nodes[n].inputs.iter().flatten() {
            stack.push(def.nets[*net].driver.0);
        }
    }

    let mut doc = CircuitDocument::new(name);
    doc.id = id;
    doc.revision = 1;
    let mut ids: HashMap<NodeId, Uuid> = HashMap::new();
    let mut gates = 0;
    for (n, node) in &def.nodes {
        if !live.contains(&n) {
            continue;
        }
        let kind = GateKind::from_netlist(&node.kind)
            .ok_or_else(|| format!("no editor part for {:?}", node.kind))?;
        let label = if kind.is_port() {
            def.name_of(n).map(str::to_string)
        } else if matches!(kind, GateKind::Split { .. } | GateKind::Merge { .. }) {
            // Wiring: left unlabelled, so views read through it.
            None
        } else {
            gates += 1;
            Some(format!("g{}", gates - 1))
        };
        let node_id = item_id(id, &format!("node/{}", ids.len()));
        ids.insert(n, node_id);
        doc.nodes.push(DocNode {
            id: node_id,
            label,
            ..DocNode::new(kind, Default::default())
        });
    }
    for (n, node) in &def.nodes {
        let Some(&to) = ids.get(&n) else { continue };
        for (pin, net) in node.inputs.iter().enumerate() {
            let Some(net) = net else { continue };
            let (driver, from_pin) = def.nets[*net].driver;
            let Some(&from) = ids.get(&driver) else {
                continue;
            };
            doc.wires.push(DocWire {
                id: item_id(id, &format!("wire/{}", doc.wires.len())),
                ..DocWire::new(from, from_pin, to, pin as u8)
            });
        }
    }
    layout_circuit(&mut doc);
    Ok(doc)
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
