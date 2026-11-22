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
//! they stay on the same port; wires to a removed port are deleted.

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
fn spawn_view(commands: &mut Commands, doc: &CircuitDocument) {
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PinRemap {
    pub inputs: Vec<Option<u8>>,
    pub outputs: Vec<Option<u8>>,
}

impl PinRemap {
    /// `None` when every existing pin keeps its index (ports appended at
    /// the end, or no port change).
    pub(crate) fn between(old: &CircuitDocument, new: &CircuitDocument) -> Option<Self> {
        let (oi, oo) = old.interface();
        let (ni, no) = new.interface();
        let map = |o: &[&DocNode], n: &[&DocNode]| -> Vec<Option<u8>> {
            o.iter()
                .map(|x| n.iter().position(|y| y.id == x.id).map(|p| p as u8))
                .collect()
        };
        let remap = Self {
            inputs: map(&oi, &ni),
            outputs: map(&oo, &no),
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

    /// Renumber the wires of `doc` to instances of `def`. Returns the
    /// updated document if anything changed.
    fn apply(&self, def: Uuid, doc: &CircuitDocument) -> Option<CircuitDocument> {
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
        out.wires.retain_mut(|w| {
            if instances.contains(&w.from.node) {
                match self.output(w.from.pin) {
                    Some(p) => w.from.pin = p,
                    None => return false,
                }
            }
            if instances.contains(&w.to.node) {
                match self.input(w.to.pin) {
                    Some(p) => w.to.pin = p,
                    None => return false,
                }
            }
            true
        });
        (out != *doc).then_some(out)
    }
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
    /// Returns whether it did.
    fn write_back(&mut self, view: &CircuitView, commands: &mut Commands) -> bool {
        let Some(frame) = view.frames.last() else {
            return false;
        };
        let def = frame.definition;
        let doc = self.items.document(def, frame.name.clone());
        let old = self.library.get(def).cloned();
        if old.as_ref().is_some_and(|have| same_document(have, &doc)) {
            return false;
        }
        let remap = old.as_ref().and_then(|old| PinRemap::between(old, &doc));
        self.library.definitions.insert(def, Arc::new(doc));
        if let Some(remap) = remap {
            self.renumber_instance_wires(def, &remap, commands);
        }
        true
    }

    fn renumber_instance_wires(&mut self, def: Uuid, remap: &PinRemap, commands: &mut Commands) {
        let mut parents: Vec<(Uuid, CircuitDocument)> = self
            .library
            .definitions
            .iter()
            .filter_map(|(id, d)| remap.apply(def, d).map(|d| (*id, d)))
            .collect();
        parents.sort_by_key(|(id, _)| *id);
        for (id, d) in parents {
            self.library.definitions.insert(id, Arc::new(d));
        }
        let instances: Vec<Uuid> = self
            .root_gates
            .iter()
            .filter(|(_, ty)| ty.kind == GateKind::SubCircuit { definition: def })
            .map(|(id, _)| id.uuid)
            .collect();
        if instances.is_empty() {
            return;
        }
        for (entity, mut ep) in &mut self.root_wires {
            let mut new = *ep;
            if instances.contains(&ep.source_gate) {
                match remap.output(ep.source_pin) {
                    Some(p) => new.source_pin = p,
                    None => {
                        commands.entity(entity).despawn();
                        continue;
                    }
                }
            }
            if instances.contains(&ep.sink_gate) {
                match remap.input(ep.sink_pin) {
                    Some(p) => new.sink_pin = p,
                    None => {
                        commands.entity(entity).despawn();
                        continue;
                    }
                }
            }
            ep.set_if_neq(new);
        }
    }
}

/// Equal up to node and wire order.
pub(crate) fn same_document(a: &CircuitDocument, b: &CircuitDocument) -> bool {
    if a.id != b.id
        || a.name != b.name
        || a.nodes.len() != b.nodes.len()
        || a.wires.len() != b.wires.len()
    {
        return false;
    }
    let mut an: Vec<_> = a.nodes.iter().collect();
    let mut bn: Vec<_> = b.nodes.iter().collect();
    an.sort_by_key(|n| n.id);
    bn.sort_by_key(|n| n.id);
    let mut aw: Vec<_> = a.wires.iter().collect();
    let mut bw: Vec<_> = b.wires.iter().collect();
    aw.sort_by_key(|w| w.id);
    bw.sort_by_key(|w| w.id);
    an == bn && aw == bw
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
        writer.write_back(&view, &mut commands);
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
) {
    let removed = removed_gates.read().count() + removed_wires.read().count() > 0;
    // A view switch this frame already wrote the old view back; the new
    // view was spawned from the library.
    if view.is_root() || view.is_changed() || active.is_changed() {
        return;
    }
    if removed || !changed_gates.is_empty() || !changed_wires.is_empty() {
        writer.write_back(&view, &mut commands);
    }
}
