//! ECS authoring model for circuits.
//!
//! These components are the live, UUID-addressed source of truth for the open
//! circuit. Each gate entity maps to exactly one node of the root definition
//! of the bridge's `Design`, and each wire entity to one connected input pin
//! (see [`super::bridge`]). Documents ([`super::CircuitDocument`]) are
//! produced from these components only on demand (save, subcircuit view).

use std::collections::HashMap;
use std::sync::Arc;

use bevy::ecs::entity_disabling::Disabled;
use bevy::ecs::query::QueryFilter;
use bevy::ecs::system::SystemParam;
use bevy::prelude::*;
use uuid::Uuid;

use super::GateKind;
use super::document::{
    CircuitDocument, CircuitLibrary, DEFAULT_LAYER_UUID, DocNode, DocWire, Incoming, PinAddr,
    WorkspaceDocument,
};
use super::view::{CircuitView, InstanceViewItem};

// ─────────────────────────────────────────────────────────────────────────────
// Resources
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Resource, Debug, Clone, Reflect)]
#[reflect(Resource)]
pub struct ActiveCircuit {
    pub uuid: Uuid,
    pub name: String,
    /// A built-in demo: edits are refused until it is copied to the
    /// player's workspace (inputs can still be driven).
    pub read_only: bool,
}

impl Default for ActiveCircuit {
    fn default() -> Self {
        Self {
            uuid: Uuid::new_v4(),
            name: "workspace".into(),
            read_only: false,
        }
    }
}

/// Message shown when an edit of a read-only demo is refused.
pub const READ_ONLY_MESSAGE: &str =
    "This demo is read-only. Use \"Copy to my workspace\" to make an editable copy.";

impl ActiveCircuit {
    /// Refuse an edit of a read-only circuit (returns whether it was refused).
    pub(crate) fn refuse_edit(&self, feedback: &mut AuthoringFeedback) -> bool {
        if self.read_only {
            feedback.refuse(READ_ONLY_MESSAGE.into());
        }
        self.read_only
    }
}

#[derive(Resource, Debug, Default, Reflect)]
#[reflect(Resource)]
pub struct WorkspaceRevision {
    pub current: u64,
    pub saved: u64,
    /// The highest revision handed out. Undo moves `current` back to an
    /// earlier revision; the next edit still gets a revision never used
    /// before, so it cannot pass for the saved one.
    issued: u64,
}

impl WorkspaceRevision {
    pub fn mark_dirty(&mut self) {
        self.issued = self.issued.max(self.current).wrapping_add(1);
        self.current = self.issued;
    }
    /// Go back (or forward) to `revision`, one handed out earlier for the
    /// circuit as it is now (undo and redo).
    pub fn restore(&mut self, revision: u64) {
        self.issued = self.issued.max(self.current);
        self.current = revision;
    }
    pub fn mark_saved(&mut self) {
        self.saved = self.current;
    }
    /// The circuit as it is now is saved nowhere (a workspace opened from
    /// an import, or a copy that could not be saved). `current` does not
    /// move: opening is not an edit, so there is nothing to undo, and no
    /// undo makes the workspace count as saved.
    pub fn mark_never_saved(&mut self) {
        self.saved = u64::MAX;
    }
    pub fn is_dirty(&self) -> bool {
        self.current != self.saved
    }
}

/// The stored tests of the open workspace's circuits. The ECS holds only
/// parts and wires, so the tests a workspace was loaded with are kept
/// here and written back when it is saved ([`CircuitReader::workspace`]).
#[derive(Resource, Debug, Clone, Default)]
pub struct WorkspaceTests(pub super::document::CircuitTests);

/// The revision and content hash of the root circuit as it was loaded, so
/// that a saved root (placed elsewhere as a chip) carries a revision that
/// grows when it is edited: [`CircuitReader::root_document`] gives the
/// loaded revision while the content is unchanged, the next one after.
#[derive(Resource, Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct LoadedRoot {
    pub revision: u64,
    pub hash: u64,
}

/// A newer (or diverged) saved version of chips this workspace already
/// has, found when placing a chip: the editor asks the player whether to
/// refresh them ([`RefreshDefinitions`]) or keep the workspace's copies.
#[derive(Debug, Clone, PartialEq)]
pub struct ChipUpdate {
    /// Everything that was offered (sent back with [`RefreshDefinitions`]).
    pub definitions: HashMap<Uuid, Arc<CircuitDocument>>,
    /// The definitions that differ: id, name, revision here, revision
    /// offered, and how they compare.
    pub changed: Vec<ChipChange>,
}

/// One definition of a [`ChipUpdate`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChipChange {
    pub id: Uuid,
    pub name: String,
    pub have: u64,
    pub offered: u64,
    pub how: Incoming,
}

/// Chip updates waiting for the player's answer (newest last).
#[derive(Resource, Debug, Default)]
pub struct PendingChipUpdates {
    pub updates: Vec<ChipUpdate>,
}

/// The last authoring command that was refused, for the editor to show.
#[derive(Resource, Debug, Default)]
pub struct AuthoringFeedback {
    pub message: Option<String>,
}

impl AuthoringFeedback {
    pub(crate) fn refuse(&mut self, message: String) {
        warn!("{message}");
        self.message = Some(message);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Components
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Component, Debug, Clone, Copy, Reflect)]
#[reflect(Component)]
pub struct CircuitMember {
    pub circuit_uuid: Uuid,
}

#[derive(Component, Debug, Clone, Copy, Reflect)]
#[reflect(Component)]
pub struct CircuitLayer {
    pub layer_uuid: Uuid,
    pub order: i32,
}

impl Default for CircuitLayer {
    fn default() -> Self {
        Self {
            layer_uuid: DEFAULT_LAYER_UUID,
            order: 0,
        }
    }
}

#[derive(Component, Debug, Clone, Copy, Reflect)]
#[reflect(Component)]
pub struct GateIdentity {
    pub uuid: Uuid,
}

#[derive(Component, Debug, Clone, Reflect)]
#[reflect(Component)]
pub struct GateLabel {
    pub value: String,
}

/// What the gate is. Changing it replaces the gate's simulator node.
#[derive(Component, Debug, Clone, PartialEq, Reflect)]
#[reflect(Component)]
pub struct GateType {
    pub kind: GateKind,
}

#[derive(Component, Debug, Clone, Default, Reflect)]
#[reflect(Component)]
pub struct PersistentGateData {
    #[reflect(ignore)]
    pub custom_properties: HashMap<String, String>,
}

/// The value the editor drives onto an input gate.
#[derive(Component, Debug, Clone, Copy, Default, PartialEq, Eq, Reflect)]
#[reflect(Component)]
pub struct InputDrive {
    pub value: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Reflect)]
pub enum DisplayRadix {
    Binary,
    Decimal,
    #[default]
    Hexadecimal,
}

/// Presentation of a display gate's value.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq, Reflect)]
#[reflect(Component)]
pub struct DisplayConfig {
    pub radix: DisplayRadix,
    pub leading_zeroes: bool,
}

impl Default for DisplayConfig {
    fn default() -> Self {
        Self {
            radix: DisplayRadix::Hexadecimal,
            leading_zeroes: true,
        }
    }
}

/// Simulated pin values of a gate, from engine probes (one word per pin).
#[derive(Component, Debug, Clone, Default, PartialEq, Eq, Reflect)]
#[reflect(Component)]
pub struct GateValues {
    pub inputs: Vec<u64>,
    pub outputs: Vec<u64>,
}

/// Probe indices feeding [`GateValues`], set by the bridge after each edit.
#[derive(Component, Debug, Clone, Default, PartialEq, Eq, Reflect)]
#[reflect(Component)]
pub struct GateProbes {
    pub inputs: Vec<Option<u32>>,
    pub outputs: Vec<Option<u32>>,
    /// A screen's memory probe: its index in `ProbeFrame::mems`.
    pub memory: Option<u32>,
}

#[derive(Component, Debug, Clone, Copy, Reflect)]
#[reflect(Component)]
pub struct WireIdentity {
    pub uuid: Uuid,
}

#[derive(Component, Debug, Clone, Copy, PartialEq, Eq, Reflect)]
#[reflect(Component)]
pub struct WireEndpoints {
    pub source_gate: Uuid,
    pub source_pin: u8,
    pub sink_gate: Uuid,
    pub sink_pin: u8,
}

/// Simulation-side state of a wire.
#[derive(Component, Debug, Clone, Copy, Default, PartialEq, Eq, Reflect)]
#[reflect(Component)]
pub struct WireState {
    /// Current value of the wire's net.
    pub value: u64,
    /// Width of the driving pin (0 while unresolved).
    pub width: u8,
    /// Whether the simulator accepted the connection.
    pub connected: bool,
    /// Probe index of the driving net.
    pub probe: Option<u32>,
}

/// Marker for gate entities that are currently selected for dragging.
#[derive(Component, Reflect)]
#[reflect(Component)]
pub struct Selected {
    pub offset: Vec2,
}

// ─────────────────────────────────────────────────────────────────────────────
// Spawning
// ─────────────────────────────────────────────────────────────────────────────

/// Spawn the authoring entity for a document node. Visuals are attached by
/// the view plugin when it sees the new [`GateType`].
pub fn spawn_gate(commands: &mut Commands, node: &DocNode, circuit_uuid: Uuid) -> Entity {
    let label = node.label.clone().unwrap_or_default();
    let mut entity = commands.spawn((
        Name::new(format!("{} {}", node.kind.display_name(), label)),
        Transform::from_xyz(node.x, node.y, 0.0),
        Visibility::Visible,
        GateIdentity { uuid: node.id },
        GateLabel { value: label },
        GateType {
            kind: node.kind.clone(),
        },
        CircuitMember { circuit_uuid },
        CircuitLayer {
            layer_uuid: node.layer,
            order: node.layer_order,
        },
        PersistentGateData {
            custom_properties: node.properties.clone(),
        },
        GateValues::default(),
        GateProbes::default(),
    ));
    match node.kind {
        GateKind::Input { .. } => {
            entity.insert(InputDrive::default());
        }
        GateKind::Display { .. } => {
            entity.insert(DisplayConfig::default());
        }
        _ => {}
    }
    entity.id()
}

pub fn spawn_wire(commands: &mut Commands, wire: &DocWire, circuit_uuid: Uuid) -> Entity {
    commands
        .spawn((
            Name::new(format!("Wire {}", wire.id)),
            WireIdentity { uuid: wire.id },
            WireEndpoints {
                source_gate: wire.from.node,
                source_pin: wire.from.pin,
                sink_gate: wire.to.node,
                sink_pin: wire.to.pin,
            },
            WireState::default(),
            CircuitMember { circuit_uuid },
            CircuitLayer {
                layer_uuid: wire.layer,
                order: wire.layer_order,
            },
        ))
        .id()
}

/// Spawn every node and wire of `doc`.
pub fn spawn_document(commands: &mut Commands, doc: &CircuitDocument, circuit_uuid: Uuid) {
    for node in &doc.nodes {
        spawn_gate(commands, node, circuit_uuid);
    }
    for wire in &doc.wires {
        spawn_wire(commands, wire, circuit_uuid);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Reading the circuit back as a document (on demand only)
// ─────────────────────────────────────────────────────────────────────────────

type GateDocQuery = (
    &'static GateIdentity,
    &'static GateType,
    &'static GateLabel,
    &'static Transform,
    &'static CircuitLayer,
    &'static PersistentGateData,
);
type WireDocQuery = (
    &'static WireIdentity,
    &'static WireEndpoints,
    &'static CircuitLayer,
);

/// Gate and wire entities read as a document. With the default filter
/// (`F = ()`) these are the entities of the open view: the root circuit's
/// while it is open, the view items while a subcircuit view is open (the
/// root's are parked with [`Disabled`]).
#[derive(SystemParam)]
pub struct DocumentQueries<'w, 's, F: QueryFilter + 'static = ()> {
    gates: Query<'w, 's, GateDocQuery, F>,
    wires: Query<'w, 's, WireDocQuery, F>,
}

impl<F: QueryFilter + 'static> DocumentQueries<'_, '_, F> {
    pub fn document(&self, id: Uuid, name: String) -> CircuitDocument {
        let mut nodes: Vec<DocNode> = self
            .gates
            .iter()
            .map(|(id, ty, label, transform, layer, data)| DocNode {
                id: id.uuid,
                kind: ty.kind.clone(),
                label: (!label.value.trim().is_empty()).then(|| label.value.clone()),
                x: transform.translation.x,
                y: transform.translation.y,
                layer: layer.layer_uuid,
                layer_order: layer.order,
                properties: data.custom_properties.clone(),
            })
            .collect();
        nodes.sort_by_key(|n| n.id);
        let mut wires: Vec<DocWire> = self
            .wires
            .iter()
            .map(|(id, ep, layer)| DocWire {
                id: id.uuid,
                from: PinAddr {
                    node: ep.source_gate,
                    pin: ep.source_pin,
                },
                to: PinAddr {
                    node: ep.sink_gate,
                    pin: ep.sink_pin,
                },
                layer: layer.layer_uuid,
                layer_order: layer.order,
            })
            .collect();
        wires.sort_by_key(|w| w.id);
        CircuitDocument {
            id,
            name,
            revision: 0,
            nodes,
            wires,
        }
    }
}

/// The root circuit's entities, whether or not they are parked.
pub type RootItems = (Without<InstanceViewItem>, Allow<Disabled>);

/// Reads circuits as documents. Used for save, view switches and tests;
/// never per frame.
#[derive(SystemParam)]
pub struct CircuitReader<'w, 's> {
    pub active: Res<'w, ActiveCircuit>,
    loaded: Option<Res<'w, LoadedRoot>>,
    tests: Option<Res<'w, WorkspaceTests>>,
    pub library: Res<'w, CircuitLibrary>,
    pub view: Res<'w, CircuitView>,
    open: DocumentQueries<'w, 's>,
    root: DocumentQueries<'w, 's, RootItems>,
}

impl CircuitReader<'_, '_> {
    /// The open circuit: the root, or the definition of the open view.
    pub fn document(&self) -> CircuitDocument {
        match self.view.frames().last() {
            Some(frame) => self.open.document(frame.definition, frame.name.clone()),
            None => self.root_document(),
        }
    }

    /// The root circuit, also while a subcircuit view is open. Its
    /// `revision` is the loaded one while its content is unchanged, the
    /// next one after an edit (see [`LoadedRoot`]).
    pub fn root_document(&self) -> CircuitDocument {
        let mut doc = self
            .root
            .document(self.active.uuid, self.active.name.clone());
        if let Some(loaded) = self.loaded.as_deref() {
            doc.revision = if doc.content_hash() == loaded.hash {
                loaded.revision
            } else {
                loaded.revision + 1
            };
        }
        doc
    }

    /// The root circuit plus every definition it instances. Edits made in an
    /// open view are already in the library.
    pub fn workspace(&self) -> WorkspaceDocument {
        let mut root = self.root_document();
        let definitions = self.library.reachable_from(&root);
        if definitions.contains_key(&root.id) {
            // Cannot happen through the editor (recursion is refused), but
            // keep ids unique if it does.
            root.id = Uuid::new_v4();
        }
        let mut workspace = WorkspaceDocument {
            root,
            definitions,
            tests: self
                .tests
                .as_deref()
                .map(|t| t.0.clone())
                .unwrap_or_default(),
        };
        workspace.tests = workspace.kept_tests();
        workspace
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Commands (messages) and their handlers
// ─────────────────────────────────────────────────────────────────────────────

/// Place a part; its centre goes to the nearest point of the snap grid
/// ([`SNAP`](super::layout::SNAP)).
#[derive(Message, Debug, Clone)]
pub struct CreateGate {
    pub kind: GateKind,
    pub position: Vec2,
    pub label: Option<String>,
}

#[derive(Message, Debug, Clone, Copy, PartialEq, Eq)]
pub struct CreateWire {
    pub source_gate: Uuid,
    pub source_pin: u8,
    pub sink_gate: Uuid,
    pub sink_pin: u8,
}

#[derive(Message, Debug, Clone, Copy)]
pub struct DeleteGate {
    pub entity: Entity,
}

#[derive(Message, Debug, Clone, Copy)]
pub struct DeleteWire {
    pub entity: Entity,
}

/// Drive an input gate (the value is masked to its width).
#[derive(Message, Debug, Clone, Copy)]
pub struct SetInputValue {
    pub entity: Entity,
    pub value: u64,
}

/// Change a gate's kind or parameters (width, clock period, ...).
#[derive(Message, Debug, Clone)]
pub struct SetGateKind {
    pub entity: Entity,
    pub kind: GateKind,
}

/// Add definitions to the [`CircuitLibrary`] (e.g. before placing a
/// subcircuit from the catalog).
#[derive(Message, Debug, Clone)]
pub struct RegisterDefinitions {
    pub definitions: HashMap<Uuid, Arc<CircuitDocument>>,
}

impl RegisterDefinitions {
    /// Everything needed to place workspace `ws` as a chip: its
    /// definitions plus its root, keyed by the root's id. Returns the
    /// registration and the chip's kind.
    pub fn chip(ws: &WorkspaceDocument) -> (Self, GateKind) {
        let mut definitions = ws.definitions.clone();
        definitions.insert(ws.root.id, Arc::new(ws.root.clone()));
        (
            Self { definitions },
            GateKind::SubCircuit {
                definition: ws.root.id,
            },
        )
    }
}

#[derive(Message)]
pub struct GateLabelChanged {
    pub gate_uuid: Uuid,
    pub label: String,
}

#[derive(Message)]
pub struct DisplayConfigChanged {
    pub gate_uuid: Uuid,
    pub radix: DisplayRadix,
    pub leading_zeroes: bool,
}

#[derive(Message)]
pub struct GateMoved {
    pub gate_uuid: Uuid,
    pub position: Vec2,
}

#[derive(Message)]
pub struct EntityLayerChanged {
    pub entity_uuid: Uuid,
    pub layer_uuid: Uuid,
    pub order: i32,
}

/// Replace the open circuit with a workspace.
#[derive(Message, Clone)]
pub struct LoadWorkspaceEvent {
    pub name: String,
    pub workspace: Arc<WorkspaceDocument>,
    /// Open it read-only (a built-in demo).
    pub read_only: bool,
    /// It is not saved anywhere yet (an imported netlist, a copy that could
    /// not be written): it counts as unsaved changes from the start.
    pub unsaved: bool,
}

#[derive(Message)]
pub struct SaveWorkspaceEvent {
    pub name: String,
}

/// Generate a unique `"{base} {n}"` label for a new port gate.
fn generate_port_label<'a>(labels: impl Iterator<Item = &'a str>, base: &str) -> String {
    let prefix = format!("{base} ");
    let max = labels
        .filter_map(|l| {
            l.strip_prefix(&prefix)
                .and_then(|s| s.parse::<usize>().ok())
        })
        .max()
        .unwrap_or(0);
    format!("{base} {}", max + 1)
}

fn title_case(name: &str) -> String {
    let mut out = String::new();
    for (i, c) in name.chars().enumerate() {
        if i == 0 {
            out.push(c);
        } else {
            out.extend(c.to_lowercase());
        }
    }
    out
}

#[allow(clippy::too_many_arguments)]
pub fn apply_create_gate_system(
    mut commands: Commands,
    mut events: MessageReader<CreateGate>,
    active: Res<ActiveCircuit>,
    view: Res<CircuitView>,
    mut revision: ResMut<WorkspaceRevision>,
    mut feedback: ResMut<AuthoringFeedback>,
    library: Res<CircuitLibrary>,
    labels: Query<&GateLabel>,
) {
    let mut taken: Vec<String> = Vec::new();
    for event in events.read() {
        if active.refuse_edit(&mut feedback) {
            continue;
        }
        if let Err(reason) = event.kind.check() {
            feedback.refuse(format!(
                "Cannot place {}: {reason}",
                event.kind.display_name()
            ));
            continue;
        }
        if let GateKind::SubCircuit { definition } = event.kind {
            if library.get(definition).is_none() {
                feedback.refuse(format!(
                    "Cannot place subcircuit: unknown definition {definition}"
                ));
                continue;
            }
            // The open circuit and every circuit around it in the view.
            let refused = view
                .circuit_chain(active.uuid)
                .into_iter()
                .rev()
                .find_map(|into| library.recursion_error(definition, into, &active.name));
            if let Some(message) = refused {
                feedback.refuse(message);
                continue;
            }
        }
        // Placed by hand: onto the snap grid, like a moved part.
        let at = super::layout::snap_to_grid(event.position);
        let mut node = DocNode::new(event.kind.clone(), at);
        node.label = event.label.clone().filter(|l| !l.trim().is_empty());
        if node.label.is_none() && event.kind.is_port() {
            let base = title_case(event.kind.display_name());
            let label = generate_port_label(
                labels
                    .iter()
                    .map(|l| l.value.as_str())
                    .chain(taken.iter().map(String::as_str)),
                &base,
            );
            taken.push(label.clone());
            node.label = Some(label);
        }
        let circuit = view.definition().unwrap_or(active.uuid);
        let entity = spawn_gate(&mut commands, &node, circuit);
        if !view.is_root() {
            commands.entity(entity).insert(InstanceViewItem);
        }
        feedback.message = None;
        revision.mark_dirty();
    }
}

#[allow(clippy::too_many_arguments)]
pub fn apply_create_wire_system(
    mut commands: Commands,
    mut events: MessageReader<CreateWire>,
    active: Res<ActiveCircuit>,
    view: Res<CircuitView>,
    library: Res<CircuitLibrary>,
    mut revision: ResMut<WorkspaceRevision>,
    mut feedback: ResMut<AuthoringFeedback>,
    gates: Query<(&GateIdentity, &GateType, Option<&GateLabel>)>,
    wires: Query<(Entity, &WireEndpoints)>,
) {
    for event in events.read() {
        if active.refuse_edit(&mut feedback) {
            continue;
        }
        let find = |uuid: Uuid| {
            gates
                .iter()
                .find(|(id, ..)| id.uuid == uuid)
                .map(|(_, t, _)| t)
        };
        let (Some(source), Some(sink)) = (find(event.source_gate), find(event.sink_gate)) else {
            feedback.refuse("Cannot create wire: gate not found".into());
            continue;
        };
        let (Some((_, source_outs)), Some((sink_ins, _))) =
            (library.pins(&source.kind), library.pins(&sink.kind))
        else {
            feedback.refuse("Cannot create wire: unknown gate pins".into());
            continue;
        };
        let (Some(&src_w), Some(&sink_w)) = (
            source_outs.get(usize::from(event.source_pin)),
            sink_ins.get(usize::from(event.sink_pin)),
        ) else {
            feedback.refuse(format!(
                "Cannot create wire: {} has no output pin {} or {} has no input pin {}",
                source.kind.display_name(),
                event.source_pin,
                sink.kind.display_name(),
                event.sink_pin
            ));
            continue;
        };
        if src_w != sink_w {
            feedback.refuse(format!(
                "Cannot create wire: source pin is {src_w} bit(s) wide but sink pin is {sink_w}"
            ));
            continue;
        }
        // One driver per input pin: a new wire replaces the old one (and
        // says so, unless it is the same wire again).
        let mut replaced = false;
        for (entity, ep) in &wires {
            if ep.sink_gate == event.sink_gate && ep.sink_pin == event.sink_pin {
                commands.entity(entity).despawn();
                replaced |=
                    (ep.source_gate, ep.source_pin) != (event.source_gate, event.source_pin);
            }
        }
        let wire = DocWire::new(
            event.source_gate,
            event.source_pin,
            event.sink_gate,
            event.sink_pin,
        );
        let circuit = view.definition().unwrap_or(active.uuid);
        let entity = spawn_wire(&mut commands, &wire, circuit);
        if !view.is_root() {
            commands.entity(entity).insert(InstanceViewItem);
        }
        feedback.message = replaced.then(|| {
            let label = gates
                .iter()
                .find(|(id, ..)| id.uuid == event.sink_gate)
                .and_then(|(.., label)| label);
            replaced_wire_message(&sink.kind, label, event.sink_pin, sink_ins.len())
        });
        revision.mark_dirty();
    }
}

/// The toast when a new wire takes over an input pin: the sink by its
/// label (else its kind), and the pin when it has more than one.
pub fn replaced_wire_message(
    kind: &GateKind,
    label: Option<&GateLabel>,
    pin: u8,
    inputs: usize,
) -> String {
    let name = label
        .map(|l| l.value.trim())
        .filter(|l| !l.is_empty())
        .map_or_else(
            || crate::editor::parts::kind_title(kind).to_owned(),
            str::to_owned,
        );
    if inputs > 1 {
        let pin_name = kind
            .input_pin_names()
            .get(usize::from(pin))
            .map_or_else(|| format!("input {pin}"), |p| p.to_uppercase());
        format!("Replaced the wire into {name} ({pin_name})")
    } else {
        format!("Replaced the wire into {name}")
    }
}

/// Delete gates (with their wires) and wires.
#[allow(clippy::too_many_arguments)]
pub fn apply_delete_system(
    mut commands: Commands,
    mut gate_events: MessageReader<DeleteGate>,
    mut wire_events: MessageReader<DeleteWire>,
    mut revision: ResMut<WorkspaceRevision>,
    active: Res<ActiveCircuit>,
    mut feedback: ResMut<AuthoringFeedback>,
    gates: Query<&GateIdentity>,
    wires: Query<(Entity, &WireEndpoints)>,
) {
    if (!gate_events.is_empty() || !wire_events.is_empty()) && active.refuse_edit(&mut feedback) {
        gate_events.clear();
        wire_events.clear();
        return;
    }
    for event in wire_events.read() {
        if let Ok((entity, _)) = wires.get(event.entity) {
            commands.entity(entity).despawn();
            revision.mark_dirty();
        }
    }
    for event in gate_events.read() {
        let Ok(identity) = gates.get(event.entity) else {
            continue;
        };
        for (wire, ep) in &wires {
            if ep.source_gate == identity.uuid || ep.sink_gate == identity.uuid {
                commands.entity(wire).despawn();
            }
        }
        commands.entity(event.entity).despawn();
        revision.mark_dirty();
    }
}

pub fn apply_set_input_system(
    mut events: MessageReader<SetInputValue>,
    // The root's inputs are parked (disabled) while a chip's view is open;
    // a key bound to one, or the MCP server, still drives them.
    mut inputs: Query<(&GateType, &mut InputDrive, Has<InstanceViewItem>), Allow<Disabled>>,
    mut feedback: ResMut<AuthoringFeedback>,
) {
    for event in events.read() {
        let Ok((ty, mut drive, in_view)) = inputs.get_mut(event.entity) else {
            continue;
        };
        if in_view {
            feedback.refuse(
                "Inputs inside a subcircuit are driven by the parent circuit; change them there."
                    .into(),
            );
            continue;
        }
        let w = ty.kind.width().unwrap_or(64);
        drive.set_if_neq(InputDrive {
            value: event.value & logi_sim::netlist::mask(w),
        });
    }
}

pub fn apply_set_kind_system(
    mut commands: Commands,
    mut events: MessageReader<SetGateKind>,
    mut gates: Query<(&mut GateType, Option<&InputDrive>, Option<&DisplayConfig>)>,
    mut revision: ResMut<WorkspaceRevision>,
    mut feedback: ResMut<AuthoringFeedback>,
    active: Res<ActiveCircuit>,
) {
    for event in events.read() {
        if active.refuse_edit(&mut feedback) {
            continue;
        }
        let Ok((mut ty, drive, display)) = gates.get_mut(event.entity) else {
            continue;
        };
        if let Err(reason) = event.kind.check() {
            feedback.refuse(format!(
                "Invalid {} settings: {reason}",
                event.kind.display_name()
            ));
            continue;
        }
        if ty.kind == event.kind {
            continue;
        }
        ty.kind = event.kind.clone();
        if event.kind.is_input() && drive.is_none() {
            commands.entity(event.entity).insert(InputDrive::default());
        }
        if matches!(event.kind, GateKind::Display { .. }) && display.is_none() {
            commands
                .entity(event.entity)
                .insert(DisplayConfig::default());
        }
        revision.mark_dirty();
    }
}

/// Register definitions sent by the editor (placing from the catalog or a
/// saved workspace). Definitions are identified by id, and a merge never
/// replaces one with different content (see [`CircuitLibrary::merge`]):
/// new ids are added; when the offered copy of a definition the library
/// has is newer (or both changed), a [`ChipUpdate`] asks the player
/// whether to refresh; an older offered copy is ignored, with a note.
/// Registrations that would make a definition contain itself are refused.
pub fn apply_register_definitions_system(
    mut events: MessageReader<RegisterDefinitions>,
    mut library: ResMut<CircuitLibrary>,
    mut feedback: ResMut<AuthoringFeedback>,
    mut pending: ResMut<PendingChipUpdates>,
) {
    for event in events.read() {
        let mut merged = CircuitLibrary {
            definitions: library.definitions.clone(),
        };
        let report = merged.merge(&event.definitions);
        if !report.added.is_empty() {
            if let Some(cycle) = merged.find_cycle() {
                let name = merged
                    .get(cycle)
                    .map_or_else(String::new, |d| d.name.clone());
                feedback.refuse(format!(
                    "Cannot add subcircuit definitions: \"{name}\" would contain itself."
                ));
                continue;
            }
            library.merge(&event.definitions);
        }
        let changes: Vec<ChipChange> = report
            .conflicts
            .iter()
            .map(|&(id, how)| {
                let have = library.get(id).map_or(0, |d| d.revision);
                let offered = &event.definitions[&id];
                ChipChange {
                    id,
                    name: offered.name.clone(),
                    have,
                    offered: offered.revision,
                    how,
                }
            })
            .collect();
        let (older, changed): (Vec<ChipChange>, Vec<ChipChange>) =
            changes.into_iter().partition(|c| c.how == Incoming::Older);
        if !older.is_empty() && changed.is_empty() {
            let names: Vec<String> = older.iter().map(|c| format!("\"{}\"", c.name)).collect();
            feedback.message = Some(format!(
                "Kept this workspace's {}: it is newer than the saved one.",
                names.join(", ")
            ));
        }
        if changed.is_empty() {
            continue;
        }
        let update = ChipUpdate {
            definitions: event.definitions.clone(),
            changed,
        };
        // Asked again for the same versions: one question is enough.
        pending.updates.retain(|u| u.changed != update.changed);
        pending.updates.push(update);
    }
}

/// Take the offered definitions of a [`ChipUpdate`]: every definition in
/// `definitions` replaces the library's (all instances change at once;
/// wires to ports that are gone are dropped and reported). One undo
/// step.
#[derive(Message, Debug, Clone)]
pub struct RefreshDefinitions {
    pub definitions: HashMap<Uuid, Arc<CircuitDocument>>,
}

/// Apply [`RefreshDefinitions`]: refused in a read-only demo, while a
/// subcircuit view is open, or when it would make a definition contain
/// itself.
pub fn apply_refresh_definitions_system(
    mut commands: Commands,
    mut events: MessageReader<RefreshDefinitions>,
    active: Res<ActiveCircuit>,
    view: Res<CircuitView>,
    mut writer: super::view::DefinitionWriter,
    mut revision: ResMut<WorkspaceRevision>,
    mut feedback: ResMut<AuthoringFeedback>,
) {
    for event in events.read() {
        if active.refuse_edit(&mut feedback) {
            continue;
        }
        if !view.is_root() {
            feedback.refuse("Go back to the root circuit to refresh chips.".into());
            continue;
        }
        let mut ids: Vec<Uuid> = event
            .definitions
            .iter()
            .filter(|(id, def)| {
                writer
                    .library
                    .get(**id)
                    .is_none_or(|have| !have.same_content(def))
            })
            .map(|(id, _)| *id)
            .collect();
        if ids.is_empty() {
            continue;
        }
        ids.sort();
        let mut merged = CircuitLibrary {
            definitions: writer.library.definitions.clone(),
        };
        for id in &ids {
            merged.replace(Arc::clone(&event.definitions[id]));
        }
        if let Some(cycle) = merged.find_cycle() {
            let name = merged
                .get(cycle)
                .map_or_else(String::new, |d| d.name.clone());
            feedback.refuse(format!("Cannot refresh: \"{name}\" would contain itself."));
            continue;
        }
        let mut messages = Vec::new();
        for id in &ids {
            let def = Arc::clone(&event.definitions[id]);
            if let Some(message) = writer.replace(def, &mut commands) {
                messages.push(message);
            }
        }
        let names: Vec<String> = ids
            .iter()
            .map(|id| format!("\"{}\"", event.definitions[id].name))
            .collect();
        let mut message = format!("Refreshed {}.", names.join(", "));
        for m in messages {
            message.push(' ');
            message.push_str(&m);
        }
        feedback.message = Some(message);
        revision.mark_dirty();
    }
}

/// Apply label, display, move and layer commands.
#[allow(clippy::too_many_arguments)]
pub fn apply_authoring_messages_system(
    mut labels: MessageReader<GateLabelChanged>,
    mut displays: MessageReader<DisplayConfigChanged>,
    mut moves: MessageReader<GateMoved>,
    mut layers: MessageReader<EntityLayerChanged>,
    mut revision: ResMut<WorkspaceRevision>,
    active: Res<ActiveCircuit>,
    mut feedback: ResMut<AuthoringFeedback>,
    mut gate_labels: Query<(&GateIdentity, &mut GateLabel)>,
    mut display_configs: Query<(&GateIdentity, &mut DisplayConfig)>,
    mut gate_transforms: Query<(&GateIdentity, &mut Transform)>,
    mut gate_layers: Query<(&GateIdentity, &mut CircuitLayer)>,
    mut wire_layers: Query<(&WireIdentity, &mut CircuitLayer), Without<GateIdentity>>,
) {
    // Display settings are presentation, allowed in a read-only demo.
    if (!labels.is_empty() || !moves.is_empty() || !layers.is_empty())
        && active.refuse_edit(&mut feedback)
    {
        labels.clear();
        moves.clear();
        layers.clear();
    }
    for command in labels.read() {
        if command.label.trim().is_empty() {
            continue;
        }
        for (identity, mut label) in &mut gate_labels {
            if identity.uuid == command.gate_uuid {
                label.value = command.label.trim().to_owned();
                revision.mark_dirty();
            }
        }
    }
    for command in displays.read() {
        for (identity, mut config) in &mut display_configs {
            if identity.uuid == command.gate_uuid {
                config.radix = command.radix;
                config.leading_zeroes = command.leading_zeroes;
                revision.mark_dirty();
            }
        }
    }
    for command in moves.read() {
        for (identity, mut transform) in &mut gate_transforms {
            if identity.uuid == command.gate_uuid {
                transform.translation.x = command.position.x;
                transform.translation.y = command.position.y;
                revision.mark_dirty();
            }
        }
    }
    for command in layers.read() {
        for (identity, mut layer) in &mut gate_layers {
            if identity.uuid == command.entity_uuid {
                layer.layer_uuid = command.layer_uuid;
                layer.order = command.order;
                revision.mark_dirty();
            }
        }
        for (identity, mut layer) in &mut wire_layers {
            if identity.uuid == command.entity_uuid {
                layer.layer_uuid = command.layer_uuid;
                layer.order = command.order;
                revision.mark_dirty();
            }
        }
    }
}

type NameSource = Or<(Changed<GateType>, Changed<GateLabel>)>;

/// Keep `Name` in sync with the label and kind.
pub fn sync_gate_names_system(mut gates: Query<(&GateType, &GateLabel, &mut Name), NameSource>) {
    for (ty, label, mut name) in &mut gates {
        name.set(format!("{} {}", ty.kind.display_name(), label.value));
    }
}

/// Capture direct inspector edits that bypass command messages.
pub fn track_authoring_component_changes_system(
    mut revision: ResMut<WorkspaceRevision>,
    transforms: Query<Ref<Transform>, (With<GateIdentity>, Changed<Transform>)>,
    labels: Query<Ref<GateLabel>, Changed<GateLabel>>,
    kinds: Query<Ref<GateType>, Changed<GateType>>,
    layers: Query<Ref<CircuitLayer>, Changed<CircuitLayer>>,
) {
    let changed = transforms.iter().any(|v| !v.is_added())
        || labels.iter().any(|v| !v.is_added())
        || kinds.iter().any(|v| !v.is_added())
        || layers.iter().any(|v| !v.is_added());
    if changed {
        revision.mark_dirty();
    }
}

/// Replace the open circuit with a workspace: despawn every gate and wire,
/// merge the definitions into the library, spawn the new root. The bridge
/// then sees the removals and additions and edits its `Design` to match.
#[allow(clippy::too_many_arguments)]
pub fn load_workspace_system(
    mut commands: Commands,
    mut events: MessageReader<LoadWorkspaceEvent>,
    mut active: ResMut<ActiveCircuit>,
    mut library: ResMut<CircuitLibrary>,
    mut revision: ResMut<WorkspaceRevision>,
    mut view: ResMut<CircuitView>,
    mut loaded: ResMut<LoadedRoot>,
    mut pending: ResMut<PendingChipUpdates>,
    mut tests: ResMut<WorkspaceTests>,
    gates: Query<Entity, (With<GateIdentity>, Allow<Disabled>)>,
    wires: Query<Entity, (With<WireIdentity>, Allow<Disabled>)>,
) {
    let Some(event) = events.read().last() else {
        return;
    };
    tests.0 = event.workspace.tests.clone();
    // Close any subcircuit view; parked root entities go too.
    if !view.is_root() {
        view.reset();
    }
    for entity in gates.iter().chain(wires.iter()) {
        commands.entity(entity).despawn();
    }
    // The file's definitions are taken as they are: the open circuit is
    // replaced, so no instance of another version of them is left. (A
    // merge would keep the library's copies.)
    for (id, def) in &event.workspace.definitions {
        if library.get(*id).is_none_or(|have| **have != **def) {
            library.replace(Arc::clone(def));
        }
    }
    pending.updates.clear();
    let root = &event.workspace.root;
    let mut named = root.clone();
    named.name = event.name.clone();
    *loaded = LoadedRoot {
        revision: root.revision,
        hash: named.content_hash(),
    };
    *active = ActiveCircuit {
        uuid: root.id,
        name: event.name.clone(),
        read_only: event.read_only,
    };
    spawn_document(&mut commands, root, root.id);
    *revision = WorkspaceRevision::default();
    if event.unsaved && !event.read_only {
        revision.mark_never_saved();
    }
}
