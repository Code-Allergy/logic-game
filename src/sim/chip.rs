//! Make chip: a selection of parts becomes a new subcircuit definition and
//! is replaced by one instance of it, wired the same way.
//!
//! ## The algorithm ([`plan_chip`])
//!
//! - The chip takes the selected parts, except `Input`, `Output` and
//!   `Display` parts: those are the circuit's own ports and readouts, so
//!   they stay outside (inside a definition an `Input` would become a port
//!   nobody drives, and a `Display` would no longer be seen).
//! - Wires between two parts that go into the chip go with them.
//! - Every wire that crosses the selection boundary becomes a port. Wires
//!   that come in from the same outside output pin share one `Input`
//!   port; wires that leave from the same inside output pin share one
//!   `Output` port (one net, one port). Port widths are the driving pin's.
//! - Ports are named after the source of the net: an input port after the
//!   outside part driving it, an output port after the inside part driving
//!   it, or else the labelled part it drives outside (an `Output` "Sum"
//!   gives a port "Sum"). A part with several outputs adds the pin name
//!   (`"alu.cout"`); unlabelled sources fall back to the sink's pin name or
//!   `in` / `out`. Names are made unique with ` 2`, ` 3`, ...
//! - The definition's parts keep their layout, centred on the origin, with
//!   the input ports in a column on the left and the outputs on the right.
//! - In the circuit, the chip's parts and their wires are removed, one
//!   instance is placed at the centre of the selection, and each crossing
//!   wire is redrawn to the instance pin of its port.
//!
//! Instances are simulated as if flattened, so the circuit behaves the
//! same before and after (the tests check it against the reference
//! simulator).
//!
//! In the editor, [`MakeChip`] does it for the open circuit (the root, or
//! a definition opened in a view, where the new chip nests inside it). The
//! new definition goes into the workspace's library (saved with it), and
//! the whole change is one undo step.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use bevy::prelude::*;
use uuid::Uuid;

use super::document::{CircuitDocument, CircuitLibrary, DocNode, DocWire, PinAddr};
use super::view::{CircuitView, InstanceViewItem};
use super::{
    ActiveCircuit, AuthoringFeedback, DocumentQueries, GateIdentity, GateKind, MultiSelection,
    SelectedGate, WireIdentity, WorkspaceRevision, spawn_gate, spawn_wire,
};

/// Distance from the chip's parts to its port columns.
const PORT_MARGIN: f32 = 120.0;
/// Vertical distance between ports in a column.
const PORT_PITCH: f32 = 100.0;
/// Grid the chip's instance and ports are placed on.
const GRID: f32 = 10.0;

fn snap(v: Vec2) -> Vec2 {
    (v / GRID).round() * GRID
}

/// Make a chip of the parts `gates` (by id) of the open circuit, named
/// `name`.
#[derive(Message, Debug, Clone)]
pub struct MakeChip {
    pub gates: Vec<Uuid>,
    pub name: String,
}

/// What [`plan_chip`] decided: the new definition and how the circuit
/// changes.
#[derive(Debug, Clone)]
pub struct ChipPlan {
    /// The new definition.
    pub definition: CircuitDocument,
    /// The instance of it that replaces the parts.
    pub instance: DocNode,
    /// Parts that move into the chip (removed from the circuit).
    pub moved: Vec<Uuid>,
    /// Selected ports and readouts that stay outside.
    pub kept: Vec<Uuid>,
    /// Wires removed from the circuit (inside the chip, or crossing it).
    pub removed_wires: Vec<Uuid>,
    /// Wires added to the circuit, to and from the instance.
    pub new_wires: Vec<DocWire>,
}

impl ChipPlan {
    /// `circuit` with the plan applied.
    pub fn apply(&self, circuit: &CircuitDocument) -> CircuitDocument {
        let mut out = circuit.clone();
        out.nodes.retain(|n| !self.moved.contains(&n.id));
        out.nodes.push(self.instance.clone());
        out.wires.retain(|w| !self.removed_wires.contains(&w.id));
        out.wires.extend(self.new_wires.iter().cloned());
        out
    }
}

/// Whether a part stays outside a chip made of a selection containing it.
pub fn stays_outside(kind: &GateKind) -> bool {
    kind.is_port()
}

/// `base`, or `base 2`, `base 3`, ... : the first not in `taken`.
fn unique(base: &str, taken: &HashSet<String>) -> String {
    if !taken.contains(base) {
        return base.to_owned();
    }
    (2..)
        .map(|i| format!("{base} {i}"))
        .find(|n| !taken.contains(n))
        .expect("an unused name")
}

/// The most input (or output) ports [`plan_chip`] gives a chip: what a
/// wire's pin index (`u8`) can name.
pub const MAX_CHIP_PINS: usize = 256;

/// Plan making a chip of `selection` in `circuit` (see the module docs).
pub fn plan_chip(
    circuit: &CircuitDocument,
    selection: &HashSet<Uuid>,
    name: &str,
    library: &CircuitLibrary,
) -> Result<ChipPlan, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("A chip needs a name.".into());
    }
    let nodes: HashMap<Uuid, &DocNode> = circuit.nodes.iter().map(|n| (n.id, n)).collect();
    let mut inner: Vec<&DocNode> = Vec::new();
    let mut kept = Vec::new();
    for n in &circuit.nodes {
        if !selection.contains(&n.id) {
            continue;
        }
        if stays_outside(&n.kind) {
            kept.push(n.id);
        } else {
            inner.push(n);
        }
    }
    if inner.is_empty() {
        return Err(
            "Select at least one part that is not an input, output or display to make a chip."
                .into(),
        );
    }
    let inside: HashSet<Uuid> = inner.iter().map(|n| n.id).collect();

    let label = |id: Uuid| {
        nodes
            .get(&id)
            .and_then(|n| n.label.as_deref())
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_owned)
    };
    let names = |id: Uuid| {
        nodes
            .get(&id)
            .map(|n| library.pin_names(&n.kind))
            .unwrap_or_default()
    };
    // Name of a net after its driving pin: the part's label, with the pin
    // name when the part has several outputs.
    let source_name = |pin: PinAddr| {
        let label = label(pin.node)?;
        let (_, outs) = names(pin.node);
        Some(if outs.len() > 1 {
            let pin_name = outs
                .get(usize::from(pin.pin))
                .cloned()
                .unwrap_or_else(|| pin.pin.to_string());
            format!("{label}.{pin_name}")
        } else {
            label
        })
    };
    let width = |pin: PinAddr| {
        nodes
            .get(&pin.node)
            .and_then(|n| library.pins(&n.kind))
            .and_then(|(_, outs)| outs.get(usize::from(pin.pin)).copied())
            .unwrap_or(1)
    };
    let y_of = |id: Uuid| nodes.get(&id).map_or(0.0, |n| n.y);

    // Group the crossing wires by their driving pin.
    let mut internal: Vec<&DocWire> = Vec::new();
    let mut incoming: Vec<(PinAddr, Vec<&DocWire>)> = Vec::new();
    let mut outgoing: Vec<(PinAddr, Vec<&DocWire>)> = Vec::new();
    let mut removed_wires = Vec::new();
    for w in &circuit.wires {
        let (from_in, to_in) = (inside.contains(&w.from.node), inside.contains(&w.to.node));
        let groups = match (from_in, to_in) {
            (true, true) => {
                internal.push(w);
                removed_wires.push(w.id);
                continue;
            }
            (false, true) => &mut incoming,
            (true, false) => &mut outgoing,
            (false, false) => continue,
        };
        removed_wires.push(w.id);
        match groups.iter_mut().find(|(pin, _)| *pin == w.from) {
            Some((_, ws)) => ws.push(w),
            None => groups.push((w.from, vec![w])),
        }
    }
    // Ports top to bottom in the order of what they connect to.
    let order = |groups: &mut Vec<(PinAddr, Vec<&DocWire>)>, inner_end: fn(&DocWire) -> Uuid| {
        groups.sort_by(|a, b| {
            let ya =
                a.1.iter()
                    .map(|w| y_of(inner_end(w)))
                    .fold(f32::MIN, f32::max);
            let yb =
                b.1.iter()
                    .map(|w| y_of(inner_end(w)))
                    .fold(f32::MIN, f32::max);
            yb.total_cmp(&ya)
                .then(a.0.node.cmp(&b.0.node))
                .then(a.0.pin.cmp(&b.0.pin))
        });
    };
    order(&mut incoming, |w| w.to.node);
    order(&mut outgoing, |w| w.to.node);
    // A wire names a pin with a `u8`: a chip with more ports on a side
    // could not be wired back in (pin 256 would land on pin 0).
    for (side, count) in [("inputs", incoming.len()), ("outputs", outgoing.len())] {
        if count > MAX_CHIP_PINS {
            return Err(format!(
                "This selection would make a chip with {count} {side}; a chip has at most \
                 {MAX_CHIP_PINS} on a side. Select fewer parts, or parts with fewer \
                 connections to the rest."
            ));
        }
    }

    // Layout: the parts' bounding box, centred on the origin.
    let bounds = inner
        .iter()
        .map(|n| Rect::from_center_size(n.position(), n.kind.size()))
        .reduce(|a, b| a.union(b))
        .expect("at least one part");
    let centre = snap(bounds.center());

    let mut def = CircuitDocument::new(name);
    def.revision = 1;
    for n in &inner {
        let mut n = (*n).clone();
        n.x -= centre.x;
        n.y -= centre.y;
        def.nodes.push(n);
    }
    for w in &internal {
        def.wires.push((*w).clone());
    }

    let mut taken: HashSet<String> = HashSet::new();
    let column = |count: usize| {
        let top = (count.saturating_sub(1)) as f32 * PORT_PITCH * 0.5;
        move |i: usize| top - i as f32 * PORT_PITCH
    };
    // Input ports, wired inside to every sink of their net.
    let left = snap(Vec2::new(bounds.min.x - centre.x - PORT_MARGIN, 0.0)).x;
    let ys = column(incoming.len());
    let mut in_ports: Vec<(Uuid, PinAddr, Vec<&DocWire>)> = Vec::new();
    for (i, (from, ws)) in incoming.iter().enumerate() {
        let base = source_name(*from)
            .or_else(|| {
                let (ins, _) = names(ws[0].to.node);
                ins.get(usize::from(ws[0].to.pin)).cloned()
            })
            .unwrap_or_else(|| "in".into());
        let port_name = unique(&base, &taken);
        taken.insert(port_name.clone());
        let port = DocNode::labeled(
            GateKind::Input { w: width(*from) },
            Vec2::new(left, ys(i)),
            port_name,
        );
        for w in ws {
            def.connect(port.id, 0, w.to.node, w.to.pin);
        }
        in_ports.push((port.id, *from, ws.clone()));
        def.nodes.push(port);
    }
    // Output ports, each driven by its inside pin.
    let right = snap(Vec2::new(bounds.max.x - centre.x + PORT_MARGIN, 0.0)).x;
    let ys = column(outgoing.len());
    let mut out_ports: Vec<(Uuid, Vec<&DocWire>)> = Vec::new();
    for (i, (from, ws)) in outgoing.iter().enumerate() {
        let base = source_name(*from)
            .or_else(|| ws.iter().find_map(|w| label(w.to.node)))
            .unwrap_or_else(|| "out".into());
        let port_name = unique(&base, &taken);
        taken.insert(port_name.clone());
        let port = DocNode::labeled(
            GateKind::Output { w: width(*from) },
            Vec2::new(right, ys(i)),
            port_name,
        );
        def.connect(from.node, from.pin, port.id, 0);
        out_ports.push((port.id, ws.clone()));
        def.nodes.push(port);
    }

    // The instance and its wires, on the pins of the ports' order.
    let (ins, outs) = def.interface();
    let pin_of = |ports: &[&DocNode], id: Uuid| {
        let i = ports.iter().position(|p| p.id == id).expect("a port");
        u8::try_from(i).expect("checked: at most MAX_CHIP_PINS ports on a side")
    };
    let mut instance = DocNode::labeled(
        GateKind::SubCircuit { definition: def.id },
        centre,
        name.to_owned(),
    );
    if let Some(first) = inner.first() {
        instance.layer = first.layer;
    }
    let mut new_wires = Vec::new();
    for (port, from, ws) in &in_ports {
        let pin = pin_of(&ins, *port);
        // One wire per net into the instance.
        let mut wire = DocWire::new(from.node, from.pin, instance.id, pin);
        wire.layer = ws[0].layer;
        wire.layer_order = ws[0].layer_order;
        new_wires.push(wire);
    }
    for (port, ws) in &out_ports {
        let pin = pin_of(&outs, *port);
        for w in ws {
            let mut wire = DocWire::new(instance.id, pin, w.to.node, w.to.pin);
            wire.layer = w.layer;
            wire.layer_order = w.layer_order;
            new_wires.push(wire);
        }
    }
    Ok(ChipPlan {
        definition: def,
        instance,
        moved: inner.iter().map(|n| n.id).collect(),
        kept,
        removed_wires,
        new_wires,
    })
}

/// Apply [`MakeChip`] to the open circuit (see the module docs). Refused
/// in a read-only demo. The new definition is added to the library and
/// the selection becomes the instance.
#[allow(clippy::too_many_arguments)]
pub fn apply_make_chip_system(
    mut commands: Commands,
    mut events: MessageReader<MakeChip>,
    active: Res<ActiveCircuit>,
    view: Res<CircuitView>,
    mut library: ResMut<CircuitLibrary>,
    mut revision: ResMut<WorkspaceRevision>,
    mut feedback: ResMut<AuthoringFeedback>,
    open: DocumentQueries,
    gates: Query<(Entity, &GateIdentity)>,
    wires: Query<(Entity, &WireIdentity)>,
    selected: Option<ResMut<SelectedGate>>,
    multi: Option<ResMut<MultiSelection>>,
) {
    // One chip per frame: the circuit read below is the one before it.
    let Some(event) = events.read().last() else {
        return;
    };
    if active.refuse_edit(&mut feedback) {
        return;
    }
    let circuit_id = view.definition().unwrap_or(active.uuid);
    let circuit = open.document(circuit_id, String::new());
    let selection: HashSet<Uuid> = event.gates.iter().copied().collect();
    let plan = match plan_chip(&circuit, &selection, &event.name, &library) {
        Ok(plan) => plan,
        Err(message) => {
            feedback.refuse(format!("Cannot make a chip: {message}"));
            return;
        }
    };
    let moved: HashSet<Uuid> = plan.moved.iter().copied().collect();
    let removed: HashSet<Uuid> = plan.removed_wires.iter().copied().collect();
    for (entity, id) in &gates {
        if moved.contains(&id.uuid) {
            commands.entity(entity).despawn();
        }
    }
    for (entity, id) in &wires {
        if removed.contains(&id.uuid) {
            commands.entity(entity).despawn();
        }
    }
    library.replace(Arc::new(plan.definition.clone()));
    let in_view = !view.is_root();
    let instance = spawn_gate(&mut commands, &plan.instance, circuit_id);
    if in_view {
        commands.entity(instance).insert(InstanceViewItem);
    }
    for wire in &plan.new_wires {
        let e = spawn_wire(&mut commands, wire, circuit_id);
        if in_view {
            commands.entity(e).insert(InstanceViewItem);
        }
    }
    if let Some(mut selected) = selected {
        selected.0 = Some(instance);
    }
    if let Some(mut multi) = multi {
        multi.clear();
    }
    let ports = {
        let (i, o) = plan.definition.interface();
        i.len() + o.len()
    };
    let mut message = format!(
        "Made chip \"{}\": {} part(s), {} port(s).",
        plan.definition.name,
        plan.moved.len(),
        ports
    );
    if !plan.kept.is_empty() {
        message.push_str(&format!(
            " {} input/output/display part(s) stay outside.",
            plan.kept.len()
        ));
    }
    feedback.message = Some(message);
    revision.mark_dirty();
}
