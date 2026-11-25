//! Instance views: opening a subcircuit shows its definition with live
//! values from the running root simulation.
//!
//! ## Model
//!
//! [`CircuitView`] is a stack of [`ViewFrame`]s. Empty means the root
//! circuit is open. Each frame names the instance gate that was opened (by
//! its stable gate id, in the circuit of the frame below) and the
//! definition it instances. The gate ids from the root down form the
//! *instance path*; the bridge resolves it to `logi_sim` node ids and probes
//! the definition's nets at that path (`Probe { path, net }`), so the
//! values shown are those of that instance in the running simulation. No
//! second simulation is started.
//!
//! ## Entities
//!
//! - Opening a view from the root *parks* the root's gate and wire entities:
//!   they get Bevy's [`Disabled`] marker (with their children), so every
//!   default query skips them, and the bridge keeps their simulator nodes.
//!   Closing back to the root removes the marker; nothing is respawned, so
//!   no state is lost.
//! - The open definition is spawned from the [`CircuitLibrary`] as ordinary
//!   authoring entities marked [`InstanceViewItem`]. Opening a nested
//!   instance despawns them and spawns the inner definition.
//!
//! ## Editing in a view
//!
//! Edits in an instance view edit the shared *definition*:
//! [`write_back_view_system`] writes the view's document back to the
//! library, the bridge updates that definition in the design in place (node
//! ids of unchanged nodes are kept), and every instance changes at once.
//! The engine carries state across where node paths still match. Inputs
//! inside a view are driven by the parent circuit and cannot be toggled.
//!
//! Instance pins are numbered by port order (port label, then id). When an
//! edit adds, removes or renames ports, wires to instances of the
//! definition (in the root and in other definitions) are renumbered so
//! they stay on the same port: followed by the port's node id, or else by
//! its name (a port deleted and added again with the same label and
//! width keeps its wires). Wires to a port that is gone are deleted, and
//! the editor says which ports lost wires.
//!
//! Each write-back bumps the definition's `revision` (see
//! [`CircuitDocument`]), so a saved copy can tell it is older or newer.

use bevy::ecs::entity_disabling::Disabled;
use bevy::ecs::system::SystemParam;
use bevy::prelude::*;
use std::sync::Arc;
use uuid::Uuid;

use super::document::{CircuitDocument, CircuitLibrary, DocNode};
use super::{ActiveCircuit, GateKind};
use super::{
    AuthoringFeedback, CircuitLayer, DocumentQueries, GateIdentity, GateLabel, GateType,
    PersistentGateData, SelectedGate, WireEndpoints, WireIdentity, spawn_gate, spawn_wire,
};

/// Marks the gate and wire entities of an open instance or definition view.
#[derive(Component, Debug, Clone, Copy, Default, Reflect)]
#[reflect(Component)]
pub struct InstanceViewItem;

/// One level of an open subcircuit view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewFrame {
    /// The instance gate that was opened, in the circuit one level up.
    /// `None` when a definition was opened without an instance (from the
    /// definition table, when nothing instances it): its values are not
    /// live.
    pub gate: Option<Uuid>,
    /// The definition shown.
    pub definition: Uuid,
    /// Definition name, for breadcrumbs.
    pub name: String,
}

/// Which circuit is open: the root (no frames) or a subcircuit view.
#[derive(Resource, Debug, Default, Clone)]
pub struct CircuitView {
    frames: Vec<ViewFrame>,
}

impl CircuitView {
    pub fn frames(&self) -> &[ViewFrame] {
        &self.frames
    }

    pub fn is_root(&self) -> bool {
        self.frames.is_empty()
    }

    /// The definition being shown, if a subcircuit view is open.
    pub fn definition(&self) -> Option<Uuid> {
        self.frames.last().map(|f| f.definition)
    }

    /// Gate ids from the root to the open instance, or `None` at the root
    /// or when the view is not of a live instance.
    pub fn instance_path(&self) -> Option<Vec<Uuid>> {
        if self.frames.is_empty() {
            return None;
        }
        self.frames.iter().map(|f| f.gate).collect()
    }

    /// Whether the open view shows live values of an instance.
    pub fn is_live(&self) -> bool {
        self.is_root() || self.instance_path().is_some()
    }

    /// `root / outer / inner` names.
    pub fn breadcrumb(&self, root_name: &str) -> String {
        std::iter::once(root_name)
            .chain(self.frames.iter().map(|f| f.name.as_str()))
            .collect::<Vec<_>>()
            .join(" / ")
    }

    /// Ids of the circuits from the root (`root`) to the open one.
    pub fn circuit_chain(&self, root: Uuid) -> Vec<Uuid> {
        std::iter::once(root)
            .chain(self.frames.iter().map(|f| f.definition))
            .collect()
    }

    pub(crate) fn reset(&mut self) {
        self.frames.clear();
    }
}

/// Open the instance view of a subcircuit gate in the open circuit.
#[derive(Message, Debug, Clone, Copy)]
pub struct OpenInstanceView {
    pub entity: Entity,
}

/// Open a definition from the definition table. The view is of the first
/// instance of it found from the root (live), or of the bare definition
/// when nothing instances it (not live).
#[derive(Message, Debug, Clone, Copy)]
pub struct OpenDefinitionView {
    pub definition: Uuid,
}

/// Leave the open subcircuit view: one level up, or back to the root.
#[derive(Message, Debug, Clone, Copy, Default)]
pub struct CloseView {
    pub to_root: bool,
}

/// Gates and wires of the root circuit, parked or not.
type RootItem = (
    Or<(With<GateIdentity>, With<WireIdentity>)>,
    Without<InstanceViewItem>,
    Allow<Disabled>,
);
type ViewItem = (
    Or<(With<GateIdentity>, With<WireIdentity>)>,
    With<InstanceViewItem>,
);
type RootGate = (Without<InstanceViewItem>, Allow<Disabled>);

/// The first instance path from the root to `target`: gate ids, with the
/// definition and name of each hop.
fn find_instance(
    root_gates: &[(Uuid, GateKind)],
    library: &CircuitLibrary,
    target: Uuid,
) -> Option<Vec<ViewFrame>> {
    fn search(
        nodes: Vec<(Uuid, Uuid)>,
        library: &CircuitLibrary,
        target: Uuid,
        depth: usize,
    ) -> Option<Vec<ViewFrame>> {
        if depth > 64 {
            return None;
        }
        for (gate, def) in nodes {
            let Some(doc) = library.get(def) else {
                continue;
            };
            let frame = ViewFrame {
                gate: Some(gate),
                definition: def,
                name: doc.name.clone(),
            };
            if def == target {
                return Some(vec![frame]);
            }
            let mut inner: Vec<(Uuid, Uuid)> = doc
                .nodes
                .iter()
                .filter_map(|n| match n.kind {
                    GateKind::SubCircuit { definition } => Some((n.id, definition)),
                    _ => None,
                })
                .collect();
            inner.sort();
            if let Some(mut rest) = search(inner, library, target, depth + 1) {
                rest.insert(0, frame);
                return Some(rest);
            }
        }
        None
    }
    let mut nodes: Vec<(Uuid, Uuid)> = root_gates
        .iter()
        .filter_map(|(id, kind)| match kind {
            GateKind::SubCircuit { definition } => Some((*id, *definition)),
            _ => None,
        })
        .collect();
    nodes.sort();
    search(nodes, library, target, 0)
}

/// Spawn a definition's gates and wires as view items.
pub(crate) fn spawn_view(commands: &mut Commands, doc: &CircuitDocument) {
    for node in &doc.nodes {
        let e = spawn_gate(commands, node, doc.id);
        commands.entity(e).insert(InstanceViewItem);
    }
    for wire in &doc.wires {
        let e = spawn_wire(commands, wire, doc.id);
        commands.entity(e).insert(InstanceViewItem);
    }
}

/// How the pins of a definition's instances move after an edit: old pin
/// index to new pin index, `None` for a removed port.
///
/// A port is followed by its node id (a relabelled port keeps its wires);
/// a port whose node is gone reconnects to a new port of the same name and
/// width that no other old port took, so a port deleted and re-created, or
/// a definition refreshed from a saved copy, keeps its wires where it can.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PinRemap {
    pub inputs: Vec<Option<u8>>,
    pub outputs: Vec<Option<u8>>,
    /// Names of the old ports (for reporting dropped wires).
    input_names: Vec<String>,
    output_names: Vec<String>,
}

impl PinRemap {
    /// `None` when every existing pin keeps its index (ports appended at
    /// the end, or no port change).
    pub(crate) fn between(old: &CircuitDocument, new: &CircuitDocument) -> Option<Self> {
        let (oi, oo) = old.interface();
        let (ni, no) = new.interface();
        let map = |o: &[&DocNode], n: &[&DocNode]| -> Vec<Option<u8>> {
            let mut out: Vec<Option<u8>> = o
                .iter()
                .map(|x| n.iter().position(|y| y.id == x.id).map(|p| p as u8))
                .collect();
            let mut taken: Vec<bool> = vec![false; n.len()];
            for p in out.iter().flatten() {
                taken[usize::from(*p)] = true;
            }
            for (i, x) in o.iter().enumerate() {
                if out[i].is_some() {
                    continue;
                }
                let Some(label) = x.label.as_deref() else {
                    continue;
                };
                let found = n.iter().enumerate().position(|(j, y)| {
                    !taken[j]
                        && y.label.as_deref() == Some(label)
                        && y.kind.width() == x.kind.width()
                });
                if let Some(j) = found {
                    taken[j] = true;
                    out[i] = Some(j as u8);
                }
            }
            out
        };
        let name = |n: &&DocNode| n.label.clone().unwrap_or_else(|| "?".into());
        let remap = Self {
            inputs: map(&oi, &ni),
            outputs: map(&oo, &no),
            input_names: oi.iter().map(name).collect(),
            output_names: oo.iter().map(name).collect(),
        };
        let identity = |v: &[Option<u8>]| v.iter().enumerate().all(|(i, p)| *p == Some(i as u8));
        (!identity(&remap.inputs) || !identity(&remap.outputs)).then_some(remap)
    }

    fn input(&self, pin: u8) -> Option<u8> {
        self.inputs.get(usize::from(pin)).copied().flatten()
    }

    fn output(&self, pin: u8) -> Option<u8> {
        self.outputs.get(usize::from(pin)).copied().flatten()
    }

    fn input_name(&self, pin: u8) -> String {
        self.input_names
            .get(usize::from(pin))
            .cloned()
            .unwrap_or_else(|| format!("in {pin}"))
    }

    fn output_name(&self, pin: u8) -> String {
        self.output_names
            .get(usize::from(pin))
            .cloned()
            .unwrap_or_else(|| format!("out {pin}"))
    }

    /// The port a wire to instances of `def` loses, if it loses one.
    fn dropped(&self, instances: &[Uuid], source: (Uuid, u8), sink: (Uuid, u8)) -> Option<String> {
        if instances.contains(&source.0) && self.output(source.1).is_none() {
            return Some(self.output_name(source.1));
        }
        if instances.contains(&sink.0) && self.input(sink.1).is_none() {
            return Some(self.input_name(sink.1));
        }
        None
    }

    /// Renumber the wires of `doc` to instances of `def`. Returns the
    /// updated document if anything changed, and the ports of the wires
    /// it dropped.
    pub(crate) fn apply(
        &self,
        def: Uuid,
        doc: &CircuitDocument,
    ) -> Option<(CircuitDocument, Vec<String>)> {
        let instances: Vec<Uuid> = doc
            .nodes
            .iter()
            .filter(|n| n.kind == GateKind::SubCircuit { definition: def })
            .map(|n| n.id)
            .collect();
        if instances.is_empty() {
            return None;
        }
        let mut out = doc.clone();
        let mut dropped = Vec::new();
        out.wires.retain_mut(|w| {
            if let Some(port) =
                self.dropped(&instances, (w.from.node, w.from.pin), (w.to.node, w.to.pin))
            {
                dropped.push(port);
                return false;
            }
            if instances.contains(&w.from.node) {
                w.from.pin = self.output(w.from.pin).unwrap_or(w.from.pin);
            }
            if instances.contains(&w.to.node) {
                w.to.pin = self.input(w.to.pin).unwrap_or(w.to.pin);
            }
            true
        });
        if out.wires == doc.wires {
            return None;
        }
        out.revision = doc.revision + 1;
        Some((out, dropped))
    }
}

/// Wires dropped because the ports they were on are gone, for the player:
/// `None` when there are none.
pub(crate) fn dropped_message(definition: &str, ports: &[String]) -> Option<String> {
    if ports.is_empty() {
        return None;
    }
    let mut names: Vec<&str> = ports.iter().map(String::as_str).collect();
    names.sort_unstable();
    names.dedup();
    let names: Vec<String> = names.iter().map(|n| format!("\"{n}\"")).collect();
    Some(format!(
        "\"{definition}\" changed its ports: removed {} wire(s) to {} (no port of that name \
         is left).",
        ports.len(),
        names.join(", ")
    ))
}

/// Writes the open view back to its definition in the library, keeping
/// wires to its instances on the same ports.
#[derive(SystemParam)]
pub struct DefinitionWriter<'w, 's> {
    pub library: ResMut<'w, CircuitLibrary>,
    items: DocumentQueries<'w, 's, With<InstanceViewItem>>,
    root_gates: Query<'w, 's, (&'static GateIdentity, &'static GateType), RootGate>,
    root_wires: Query<'w, 's, (Entity, &'static mut WireEndpoints), RootGate>,
}

impl DefinitionWriter<'_, '_> {
    /// Write the open view's document to the library if it differs.
    /// Returns a message for the player when wires were dropped (ports
    /// removed).
    fn write_back(&mut self, view: &CircuitView, commands: &mut Commands) -> Option<String> {
        let frame = view.frames.last()?;
        let def = frame.definition;
        let mut doc = self.items.document(def, frame.name.clone());
        let old = self.library.get(def).cloned();
        if old.as_ref().is_some_and(|have| same_document(have, &doc)) {
            return None;
        }
        doc.revision = old.as_ref().map_or(1, |o| o.revision + 1);
        self.replace(Arc::new(doc), commands)
    }

    /// Replace a definition in the library, renumbering the wires to its
    /// instances (in the root circuit and in other definitions) so they
    /// stay on the same ports. Returns a message for the player when
    /// wires were dropped.
    pub fn replace(
        &mut self,
        doc: Arc<CircuitDocument>,
        commands: &mut Commands,
    ) -> Option<String> {
        let def = doc.id;
        let name = doc.name.clone();
        let remap = self
            .library
            .get(def)
            .and_then(|old| PinRemap::between(old, &doc));
        self.library.replace(doc);
        let dropped = match remap {
            Some(remap) => self.renumber_instance_wires(def, &remap, commands),
            None => Vec::new(),
        };
        dropped_message(&name, &dropped)
    }

    fn renumber_instance_wires(
        &mut self,
        def: Uuid,
        remap: &PinRemap,
        commands: &mut Commands,
    ) -> Vec<String> {
        let mut dropped = Vec::new();
        let mut parents: Vec<(Uuid, CircuitDocument)> = self
            .library
            .definitions
            .iter()
            .filter_map(|(id, d)| {
                remap.apply(def, d).map(|(d, lost)| {
                    dropped.extend(lost);
                    (*id, d)
                })
            })
            .collect();
        parents.sort_by_key(|(id, _)| *id);
        for (_, d) in parents {
            self.library.replace(Arc::new(d));
        }
        let instances: Vec<Uuid> = self
            .root_gates
            .iter()
            .filter(|(_, ty)| ty.kind == GateKind::SubCircuit { definition: def })
            .map(|(id, _)| id.uuid)
            .collect();
        if instances.is_empty() {
            return dropped;
        }
        for (entity, mut ep) in &mut self.root_wires {
            if let Some(port) = remap.dropped(
                &instances,
                (ep.source_gate, ep.source_pin),
                (ep.sink_gate, ep.sink_pin),
            ) {
                dropped.push(port);
                commands.entity(entity).despawn();
                continue;
            }
            let mut new = *ep;
            if instances.contains(&ep.source_gate) {
                new.source_pin = remap.output(ep.source_pin).unwrap_or(ep.source_pin);
            }
            if instances.contains(&ep.sink_gate) {
                new.sink_pin = remap.input(ep.sink_pin).unwrap_or(ep.sink_pin);
            }
            ep.set_if_neq(new);
        }
        dropped
    }
}

/// Equal up to node and wire order (and the revision).
pub(crate) fn same_document(a: &CircuitDocument, b: &CircuitDocument) -> bool {
    a.same_content(b)
}

/// Handle [`OpenInstanceView`], [`OpenDefinitionView`] and [`CloseView`].
#[allow(clippy::too_many_arguments)]
pub fn view_navigation_system(
    mut commands: Commands,
    mut open: MessageReader<OpenInstanceView>,
    mut open_definition: MessageReader<OpenDefinitionView>,
    mut close: MessageReader<CloseView>,
    mut view: ResMut<CircuitView>,
    mut writer: DefinitionWriter,
    mut feedback: ResMut<AuthoringFeedback>,
    selected: Option<ResMut<SelectedGate>>,
    gates: Query<(&GateIdentity, &GateType)>,
    root_items: Query<Entity, RootItem>,
    view_items: Query<Entity, ViewItem>,
) {
    let mut target = view.frames.clone();
    let mut requested = false;
    if let Some(event) = close.read().last() {
        requested = true;
        if event.to_root {
            target.clear();
        } else {
            target.pop();
        }
    }
    if let Some(event) = open.read().last() {
        requested = true;
        match gates.get(event.entity) {
            Ok((
                id,
                GateType {
                    kind: GateKind::SubCircuit { definition },
                },
            )) => match writer.library.get(*definition) {
                Some(doc) => target.push(ViewFrame {
                    gate: Some(id.uuid),
                    definition: *definition,
                    name: doc.name.clone(),
                }),
                None => feedback.refuse(format!(
                    "Cannot open subcircuit: definition {definition} is missing"
                )),
            },
            _ => feedback.refuse("Only a subcircuit gate can be opened".into()),
        }
    }
    if let Some(event) = open_definition.read().last() {
        requested = true;
        let roots: Vec<(Uuid, GateKind)> = writer
            .root_gates
            .iter()
            .map(|(id, ty)| (id.uuid, ty.kind.clone()))
            .collect();
        match writer.library.get(event.definition) {
            None => feedback.refuse(format!(
                "Cannot open definition {}: it is not in the definition table",
                event.definition
            )),
            Some(doc) => {
                target =
                    find_instance(&roots, &writer.library, event.definition).unwrap_or_else(|| {
                        vec![ViewFrame {
                            gate: None,
                            definition: event.definition,
                            name: doc.name.clone(),
                        }]
                    });
            }
        }
    }
    if !requested || target == view.frames {
        return;
    }

    // Leave the current view.
    if view.is_root() {
        for entity in &root_items {
            commands
                .entity(entity)
                .insert_recursive::<Children>(Disabled);
        }
    } else {
        if let Some(message) = writer.write_back(&view, &mut commands) {
            feedback.refuse(message);
        }
        for entity in &view_items {
            commands.entity(entity).despawn();
        }
    }
    // Enter the target.
    match target.last() {
        None => {
            for entity in &root_items {
                commands
                    .entity(entity)
                    .remove_recursive::<Children, Disabled>();
            }
        }
        Some(frame) => {
            let mut doc = writer
                .library
                .get(frame.definition)
                .map(|d| (**d).clone())
                .unwrap_or_else(|| CircuitDocument {
                    id: frame.definition,
                    ..CircuitDocument::new(frame.name.clone())
                });
            doc.id = frame.definition;
            spawn_view(&mut commands, &doc);
        }
    }
    view.frames = target;
    if let Some(mut selected) = selected {
        selected.0 = None;
    }
}

type ChangedViewGate = (
    With<InstanceViewItem>,
    Or<(
        Changed<GateType>,
        Changed<GateLabel>,
        Changed<Transform>,
        Changed<CircuitLayer>,
        Changed<PersistentGateData>,
    )>,
    With<GateIdentity>,
);
type ChangedViewWire = (
    With<InstanceViewItem>,
    Or<(Changed<WireEndpoints>, Changed<CircuitLayer>)>,
);

/// While a subcircuit view is open, write edits made in it back to the
/// shared definition in the library (the bridge then updates every
/// instance). Runs after the authoring commands of the frame.
#[allow(clippy::too_many_arguments)]
pub fn write_back_view_system(
    mut commands: Commands,
    view: Res<CircuitView>,
    mut writer: DefinitionWriter,
    changed_gates: Query<(), ChangedViewGate>,
    changed_wires: Query<(), ChangedViewWire>,
    mut removed_gates: RemovedComponents<GateIdentity>,
    mut removed_wires: RemovedComponents<WireIdentity>,
    active: Res<ActiveCircuit>,
    mut feedback: ResMut<AuthoringFeedback>,
) {
    let removed = removed_gates.read().count() + removed_wires.read().count() > 0;
    // A view switch this frame already wrote the old view back; the new
    // view was spawned from the library.
    if view.is_root() || view.is_changed() || active.is_changed() {
        return;
    }
    if (removed || !changed_gates.is_empty() || !changed_wires.is_empty())
        && let Some(message) = writer.write_back(&view, &mut commands)
    {
        feedback.refuse(message);
    }
}
