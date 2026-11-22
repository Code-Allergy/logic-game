//! Undo and redo of edits to an editable workspace.
//!
//! ## Snapshots
//!
//! The history keeps whole snapshots of the workspace: the root circuit as
//! a document plus the [`CircuitLibrary`]'s definitions (shared `Arc`s, so
//! a snapshot costs one copy of the root document and a map of pointers).
//! It does not hook the individual edits: every edit, whichever system
//! makes it, bumps [`WorkspaceRevision`], and [`edit_history_system`],
//! running after all the authoring systems of the frame (and after a
//! subcircuit view has written its edits back to the library), compares
//! the revision with the one of its *baseline*, the snapshot of the
//! circuit as it stood after the last step. When it moved, the circuit is
//! read again; if it differs, the baseline goes onto the undo stack and the
//! new snapshot becomes the baseline.
//!
//! ## One gesture, one step
//!
//! While a mouse button is held (dragging a part, a width slider) or the
//! editor says an edit is in progress ([`UndoHistory::hold`], set while a
//! text field such as a label has the keyboard), nothing is committed: the
//! whole gesture becomes one step when it ends. A delete is one message
//! that removes a part with its wires, so it is one step too.
//!
//! ## Restoring
//!
//! Undo restores the previous snapshot by difference, in one frame: gates
//! and wires that are not in it are despawned, missing ones spawned, the
//! rest updated in place (only what differs, so change detection sees
//! just the real changes). Unchanged gates keep their entities and their
//! simulator nodes; the bridge applies the whole restore as one
//! structural edit, and register and memory state carries over as for any
//! edit. Definitions that differ are put back into the library.
//!
//! In a subcircuit view the view stays open when it still exists and the
//! root needs no new entities (the usual case: the undone edit was made in
//! the view); otherwise the editor returns to the root circuit first.
//!
//! [`WorkspaceRevision`] follows: undoing to the saved circuit makes it
//! clean again, any other state is unsaved.
//!
//! The history is per session and never saved; loading a workspace or a
//! demo clears it. Read-only demos have none.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use bevy::ecs::entity_disabling::Disabled;
use bevy::ecs::system::SystemParam;
use bevy::prelude::*;
use uuid::Uuid;

use super::document::{CircuitDocument, CircuitLibrary};
use super::view::{CircuitView, InstanceViewItem, spawn_view};
use super::{
    ActiveCircuit, CircuitLayer, DisplayConfig, DocumentQueries, GateIdentity, GateKind, GateLabel,
    GateType, InputDrive, PersistentGateData, RootItems, SelectedGate, WireEndpoints, WireIdentity,
    WorkspaceRevision, spawn_gate, spawn_wire,
};

/// Steps kept for undo; older ones are dropped.
pub const HISTORY_LIMIT: usize = 100;

/// Undo or redo one step (from the keyboard or the HUD buttons).
#[derive(Message, Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryCommand {
    Undo,
    Redo,
}

/// The circuit at one point of the history.
#[derive(Debug, Clone)]
pub(crate) struct Snapshot {
    /// The root circuit (its name left empty: renaming is not an edit).
    root: CircuitDocument,
    definitions: HashMap<Uuid, Arc<CircuitDocument>>,
    /// The [`WorkspaceRevision`] the circuit had.
    revision: u64,
}

impl Snapshot {
    pub(crate) fn same_circuit(&self, other: &Snapshot) -> bool {
        self.root == other.root
            && self.definitions.len() == other.definitions.len()
            && self.definitions.iter().all(|(id, def)| {
                other
                    .definitions
                    .get(id)
                    .is_some_and(|o| Arc::ptr_eq(o, def) || **o == **def)
            })
    }
}

/// Undo and redo stacks of the open workspace.
#[derive(Resource, Debug, Default)]
pub struct UndoHistory {
    undo: VecDeque<Snapshot>,
    redo: Vec<Snapshot>,
    /// The circuit after the last step (`None` until first seen, and for a
    /// read-only demo).
    baseline: Option<Snapshot>,
    /// An edit is in progress (a text field has the keyboard): hold the
    /// step until it ends. Set by the editor every frame.
    pub hold: bool,
}

impl UndoHistory {
    /// Whether there is something to undo: a step, or an edit not yet
    /// committed.
    pub fn can_undo(&self, revision: &WorkspaceRevision) -> bool {
        !self.undo.is_empty()
            || self
                .baseline
                .as_ref()
                .is_some_and(|b| b.revision != revision.current)
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Steps that can be undone.
    pub fn undo_len(&self) -> usize {
        self.undo.len()
    }

    /// Steps that can be redone.
    pub fn redo_len(&self) -> usize {
        self.redo.len()
    }

    pub fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
        self.baseline = None;
    }

    /// Record `now` as the next step if the circuit changed since the
    /// baseline. If it did not (an edit undone by hand, a change that is
    /// not part of the circuit), the revision goes back to the baseline's,
    /// so the circuit counts as saved again when it was.
    fn commit(&mut self, now: Snapshot, revision: &mut WorkspaceRevision) {
        let Some(base) = self.baseline.as_mut() else {
            self.baseline = Some(now);
            return;
        };
        if base.same_circuit(&now) {
            if revision.current == revision.saved {
                base.revision = revision.current;
            } else {
                revision.restore(base.revision);
            }
            return;
        }
        let previous = std::mem::replace(base, now);
        self.undo.push_back(previous);
        while self.undo.len() > HISTORY_LIMIT {
            self.undo.pop_front();
        }
        self.redo.clear();
    }

    /// Move one step back (`Undo`) or forward. Returns the snapshot to
    /// restore, or `None` when there is no such step.
    fn step(&mut self, command: HistoryCommand) -> Option<&Snapshot> {
        let base = self.baseline.take()?;
        let target = match command {
            HistoryCommand::Undo => self.undo.pop_back(),
            HistoryCommand::Redo => self.redo.pop(),
        };
        let Some(target) = target else {
            self.baseline = Some(base);
            return None;
        };
        match command {
            HistoryCommand::Undo => self.redo.push(base),
            HistoryCommand::Redo => self.undo.push_back(base),
        }
        self.baseline = Some(target);
        self.baseline.as_ref()
    }
}

type RootGateQuery = (
    Entity,
    &'static GateIdentity,
    &'static mut GateType,
    &'static mut GateLabel,
    &'static mut Transform,
    &'static mut CircuitLayer,
    &'static mut PersistentGateData,
    Has<InputDrive>,
    Has<DisplayConfig>,
);
type RootWireQuery = (
    Entity,
    &'static WireIdentity,
    &'static mut WireEndpoints,
    &'static mut CircuitLayer,
);
type RootWireFilter = (
    Without<GateIdentity>,
    Without<InstanceViewItem>,
    Allow<Disabled>,
);
type ViewItemFilter = (
    With<InstanceViewItem>,
    Or<(With<GateIdentity>, With<WireIdentity>)>,
);

/// The root circuit's gate and wire entities (parked or not) for a
/// restore, and the open view's.
#[derive(SystemParam)]
pub struct RestoreQueries<'w, 's> {
    gates: Query<'w, 's, RootGateQuery, RootItems>,
    wires: Query<'w, 's, RootWireQuery, RootWireFilter>,
    view_items: Query<'w, 's, Entity, ViewItemFilter>,
}

/// Everything [`edit_history_system`] reads and writes to snapshot and
/// restore the circuit.
#[derive(SystemParam)]
pub struct HistoryWorld<'w, 's> {
    commands: Commands<'w, 's>,
    active: Res<'w, ActiveCircuit>,
    view: ResMut<'w, CircuitView>,
    library: ResMut<'w, CircuitLibrary>,
    selected: Option<ResMut<'w, SelectedGate>>,
    items: ParamSet<'w, 's, (DocumentQueries<'w, 's, RootItems>, RestoreQueries<'w, 's>)>,
}

impl HistoryWorld<'_, '_> {
    pub(crate) fn snapshot(&mut self, revision: u64) -> Snapshot {
        Snapshot {
            root: self.items.p0().document(self.active.uuid, String::new()),
            definitions: self.library.definitions.clone(),
            revision,
        }
    }

    /// Make the open circuit `target`, in one frame.
    pub(crate) fn restore(&mut self, target: &Snapshot) {
        let library_before = self.library.definitions.clone();
        let changed_defs: Vec<Uuid> = target
            .definitions
            .iter()
            .filter(|&(id, def)| {
                library_before
                    .get(id)
                    .is_none_or(|have| !Arc::ptr_eq(have, def) && **have != **def)
            })
            .map(|(id, _)| *id)
            .collect();
        if !changed_defs.is_empty() {
            for id in &changed_defs {
                self.library
                    .definitions
                    .insert(*id, Arc::clone(&target.definitions[id]));
            }
        }

        let root = &target.root;
        let mut queries = self.items.p1();
        let gate_ids: HashSet<Uuid> = queries.gates.iter().map(|g| g.1.uuid).collect();
        let wire_ids: HashSet<Uuid> = queries.wires.iter().map(|w| w.1.uuid).collect();
        let spawns_root = root.nodes.iter().any(|n| !gate_ids.contains(&n.id))
            || root.wires.iter().any(|w| !wire_ids.contains(&w.id));

        // The open view, if any: keep it when it still exists and the root
        // needs no new entities (they would have to be parked); else go
        // back to the root circuit.
        if !self.view.is_root() {
            let keep = !spawns_root && view_exists(&self.view, root, &self.library);
            if keep {
                let def = self.view.definition().expect("a view is open");
                if changed_defs.contains(&def) {
                    for entity in &queries.view_items {
                        self.commands.entity(entity).despawn();
                    }
                    let mut doc = (*self.library.definitions[&def]).clone();
                    doc.id = def;
                    spawn_view(&mut self.commands, &doc);
                }
            } else {
                for entity in &queries.view_items {
                    self.commands.entity(entity).despawn();
                }
                for entity in queries
                    .gates
                    .iter()
                    .map(|g| g.0)
                    .chain(queries.wires.iter().map(|w| w.0))
                {
                    self.commands
                        .entity(entity)
                        .remove_recursive::<Children, Disabled>();
                }
                self.view.reset();
                if let Some(selected) = self.selected.as_mut() {
                    selected.0 = None;
                }
            }
        }

        let nodes: HashMap<Uuid, _> = root.nodes.iter().map(|n| (n.id, n)).collect();
        for (entity, id, mut ty, mut label, mut transform, mut layer, mut data, drive, display) in
            &mut queries.gates
        {
            let Some(node) = nodes.get(&id.uuid) else {
                self.commands.entity(entity).despawn();
                continue;
            };
            if ty.kind != node.kind {
                ty.kind = node.kind.clone();
                if node.kind.is_input() && !drive {
                    self.commands.entity(entity).insert(InputDrive::default());
                }
                if matches!(node.kind, GateKind::Display { .. }) && !display {
                    self.commands
                        .entity(entity)
                        .insert(DisplayConfig::default());
                }
            }
            let text = node.label.as_deref().unwrap_or_default();
            let have = if label.value.trim().is_empty() {
                ""
            } else {
                label.value.as_str()
            };
            if have != text {
                label.value = text.to_owned();
            }
            if transform.translation.x != node.x || transform.translation.y != node.y {
                transform.translation.x = node.x;
                transform.translation.y = node.y;
            }
            set_layer(&mut layer, node.layer, node.layer_order);
            if data.custom_properties != node.properties {
                data.custom_properties = node.properties.clone();
            }
        }
        let wires: HashMap<Uuid, _> = root.wires.iter().map(|w| (w.id, w)).collect();
        for (entity, id, mut ep, mut layer) in &mut queries.wires {
            let Some(wire) = wires.get(&id.uuid) else {
                self.commands.entity(entity).despawn();
                continue;
            };
            ep.set_if_neq(WireEndpoints {
                source_gate: wire.from.node,
                source_pin: wire.from.pin,
                sink_gate: wire.to.node,
                sink_pin: wire.to.pin,
            });
            set_layer(&mut layer, wire.layer, wire.layer_order);
        }
        // New entities are spawned at the root, so (from the check above)
        // only when the root circuit is open.
        for node in root.nodes.iter().filter(|n| !gate_ids.contains(&n.id)) {
            spawn_gate(&mut self.commands, node, root.id);
        }
        for wire in root.wires.iter().filter(|w| !wire_ids.contains(&w.id)) {
            spawn_wire(&mut self.commands, wire, root.id);
        }
    }
}

fn set_layer(layer: &mut Mut<CircuitLayer>, layer_uuid: Uuid, order: i32) {
    if layer.layer_uuid != layer_uuid || layer.order != order {
        **layer = CircuitLayer { layer_uuid, order };
    }
}

/// Whether every level of `view` still exists in the restored circuit:
/// each opened instance gate is in the circuit one level up and still
/// instances the frame's definition.
fn view_exists(view: &CircuitView, root: &CircuitDocument, library: &CircuitLibrary) -> bool {
    let mut parent: Option<&CircuitDocument> = Some(root);
    for frame in view.frames() {
        let Some(def) = library.get(frame.definition) else {
            return false;
        };
        if let Some(gate) = frame.gate {
            let instanced = parent.is_some_and(|p| {
                p.nodes.iter().any(|n| {
                    n.id == gate
                        && n.kind
                            == GateKind::SubCircuit {
                                definition: frame.definition,
                            }
                })
            });
            if !instanced {
                return false;
            }
        }
        parent = Some(def);
    }
    true
}

/// Record edits as undo steps and apply [`HistoryCommand`]s. Runs after
/// the authoring systems (see the module docs).
pub fn edit_history_system(
    mut history: ResMut<UndoHistory>,
    mut commands: MessageReader<HistoryCommand>,
    mut revision: ResMut<WorkspaceRevision>,
    mouse: Option<Res<ButtonInput<MouseButton>>>,
    mut world: HistoryWorld,
) {
    // Only a load replaces the active circuit: a new history.
    if world.active.is_changed() {
        history.clear();
    }
    if world.active.read_only {
        commands.clear();
        return;
    }
    if history.baseline.is_none() {
        let mut baseline = world.snapshot(revision.current);
        if revision.current != 0 {
            // Edited before the history first ran (the empty workspace of
            // a new session, edited in its first frame): it started empty.
            baseline.root.nodes.clear();
            baseline.root.wires.clear();
            baseline.revision = 0;
        }
        history.baseline = Some(baseline);
    }
    let requested: Vec<HistoryCommand> = commands.read().copied().collect();
    let pending = history
        .baseline
        .as_ref()
        .is_some_and(|b| b.revision != revision.current);
    let gesture = history.hold || mouse.is_some_and(|m| m.get_pressed().next().is_some());
    // An undo first ends the edit in progress, so that edit is what it
    // undoes.
    if pending && (!gesture || !requested.is_empty()) {
        let now = world.snapshot(revision.current);
        history.commit(now, &mut revision);
    }
    if requested.is_empty() {
        return;
    }
    let mut target: Option<Snapshot> = None;
    for command in requested {
        if let Some(snapshot) = history.step(command) {
            target = Some(snapshot.clone());
        }
    }
    if let Some(target) = target {
        world.restore(&target);
        revision.restore(target.revision);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(n: usize) -> CircuitDocument {
        let mut d = CircuitDocument::new("t");
        for _ in 0..n {
            d.add(super::super::DocNode::new(
                GateKind::Not { w: 1 },
                Vec2::ZERO,
            ));
        }
        d
    }

    fn snap(root: &CircuitDocument, revision: u64) -> Snapshot {
        Snapshot {
            root: root.clone(),
            definitions: HashMap::new(),
            revision,
        }
    }

    #[test]
    fn the_history_is_bounded() {
        let mut history = UndoHistory::default();
        let mut revision = WorkspaceRevision::default();
        let mut d = doc(0);
        history.commit(snap(&d, 0), &mut revision);
        for _ in 0..HISTORY_LIMIT + 20 {
            d.add(super::super::DocNode::new(
                GateKind::Not { w: 1 },
                Vec2::ZERO,
            ));
            revision.mark_dirty();
            history.commit(snap(&d, revision.current), &mut revision);
        }
        assert_eq!(history.undo_len(), HISTORY_LIMIT);
    }

    #[test]
    fn an_unchanged_circuit_is_no_step_and_stays_saved() {
        let mut history = UndoHistory::default();
        let mut revision = WorkspaceRevision::default();
        let d = doc(1);
        history.commit(snap(&d, 0), &mut revision);
        revision.mark_dirty();
        assert!(revision.is_dirty());
        history.commit(snap(&d, revision.current), &mut revision);
        assert_eq!(history.undo_len(), 0);
        assert!(!revision.is_dirty(), "nothing changed: still saved");
    }

    #[test]
    fn a_restored_revision_is_never_handed_out_again() {
        let mut revision = WorkspaceRevision::default();
        revision.mark_dirty();
        revision.mark_saved();
        revision.restore(0);
        assert!(revision.is_dirty());
        revision.mark_dirty();
        assert!(revision.is_dirty(), "a new edit is not the saved circuit");
    }
}
