//! A workspace as a `logi_sim` design, without the app: for tests,
//! benchmarks and tools that simulate a circuit directly with
//! [`logi_sim::sim::Sim`].
//!
//! It builds the same design the editor's [`SimBridge`](super::SimBridge)
//! builds from the same documents: one definition per circuit document,
//! ports in [`CircuitDocument::interface`] order (inputs and outputs sorted
//! by label), `Display` nodes as outputs of the root and left out of
//! definitions, and every wire as a pin-to-pin connection. Root ports are
//! named by their labels, so `Sim::get("Out")` reads the root output
//! labelled `Out`.

use std::collections::HashMap;

use logi_sim::netlist::{DefId, Design, NodeId};
use uuid::Uuid;

use super::{CircuitDocument, GateKind, WorkspaceDocument};

/// Where the parts of a workspace are in its design: for reading a
/// part's nets and state in a simulation (`crate::mcp::rig`).
#[derive(Debug, Clone, Default)]
pub struct DesignMap {
    /// Circuit id -> its definition (the root included).
    pub defs: HashMap<Uuid, DefId>,
    /// Circuit id -> part id -> node. A `Display` inside a chip has no
    /// node (it is not simulated there).
    pub nodes: HashMap<Uuid, HashMap<Uuid, NodeId>>,
}

/// The design of `workspace`, its root circuit as the design's root.
pub fn design_of(workspace: &WorkspaceDocument) -> Result<Design, String> {
    design_map_of(workspace).map(|(design, _)| design)
}

/// [`design_of`], with the map from parts to nodes.
pub fn design_map_of(workspace: &WorkspaceDocument) -> Result<(Design, DesignMap), String> {
    let mut design = Design::new(workspace.root.name.clone());
    let root = design.root;
    let mut built = DesignMap::default();
    let mut stack = Vec::new();
    fill(
        &mut design,
        root,
        &workspace.root,
        true,
        workspace,
        &mut built,
        &mut stack,
    )?;
    built.defs.insert(workspace.root.id, root);
    Ok((design, built))
}

fn definition(
    design: &mut Design,
    id: Uuid,
    workspace: &WorkspaceDocument,
    built: &mut DesignMap,
    stack: &mut Vec<Uuid>,
) -> Result<DefId, String> {
    if let Some(&def) = built.defs.get(&id) {
        return Ok(def);
    }
    if stack.contains(&id) {
        return Err(format!("definition {id} contains itself"));
    }
    let doc = workspace
        .definitions
        .get(&id)
        .ok_or_else(|| format!("definition {id} is missing"))?
        .clone();
    stack.push(id);
    let def = design.add_definition(doc.name.clone());
    fill(design, def, &doc, false, workspace, built, stack)?;
    stack.pop();
    built.defs.insert(id, def);
    Ok(def)
}

fn fill(
    design: &mut Design,
    def: DefId,
    doc: &CircuitDocument,
    is_root: bool,
    workspace: &WorkspaceDocument,
    built: &mut DesignMap,
    stack: &mut Vec<Uuid>,
) -> Result<(), String> {
    let what = |e: &dyn std::fmt::Display| format!("{}: {e}", doc.name);
    let mut nodes: HashMap<Uuid, NodeId> = HashMap::new();
    // Ports first, in port order, so instance pins line up.
    let (inputs, outputs) = doc.interface();
    for n in inputs.into_iter().chain(outputs) {
        let kind = n.kind.to_netlist().ok_or_else(|| what(&"bad port"))?;
        let node = design.add_node(def, kind).map_err(|e| what(&e))?;
        design
            .set_name(def, node, n.label.clone())
            .map_err(|e| what(&e))?;
        nodes.insert(n.id, node);
    }
    for n in &doc.nodes {
        if nodes.contains_key(&n.id) {
            continue;
        }
        let node = match &n.kind {
            GateKind::Display { .. } if !is_root => continue,
            GateKind::SubCircuit { definition: child } => {
                let child = definition(design, *child, workspace, built, stack)?;
                design.add_instance(def, child).map_err(|e| what(&e))?
            }
            kind => {
                let nk = kind
                    .to_netlist()
                    .ok_or_else(|| what(&format!("{} is not simulated", kind.display_name())))?;
                let node = design.add_node(def, nk).map_err(|e| what(&e))?;
                if matches!(kind, GateKind::Display { .. }) {
                    design
                        .set_name(def, node, n.label.clone())
                        .map_err(|e| what(&e))?;
                }
                node
            }
        };
        nodes.insert(n.id, node);
    }
    for w in &doc.wires {
        let (Some(&src), Some(&dst)) = (nodes.get(&w.from.node), nodes.get(&w.to.node)) else {
            continue;
        };
        design
            .connect_pins(def, (src, w.from.pin), (dst, w.to.pin))
            .map_err(|e| what(&e))?;
    }
    built.nodes.insert(doc.id, nodes);
    Ok(())
}
