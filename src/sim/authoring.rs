//! ECS authoring model for circuits.
//!
//! These components are the live, UUID-addressed source of truth for the open
//! circuit. Each gate entity maps to exactly one node of the root definition
//! of the bridge's `Design`, and each wire entity to one connected input pin
//! (see [`super::bridge`]). Documents ([`super::CircuitDocument`]) are
//! produced from these components only on demand (save, subcircuit view).

use std::collections::HashMap;
use std::sync::Arc;

use bevy::ecs::system::SystemParam;
use bevy::prelude::*;
use uuid::Uuid;

use super::document::{
    CircuitDocument, CircuitLibrary, DEFAULT_LAYER_UUID, DocNode, DocWire, PinAddr,
    WorkspaceDocument,
};
use super::GateKind;

// ─────────────────────────────────────────────────────────────────────────────
// Resources
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Resource, Debug, Clone, Reflect)]
#[reflect(Resource)]
pub struct ActiveCircuit {
    pub uuid: Uuid,
    pub name: String,
}

impl Default for ActiveCircuit {
    fn default() -> Self {
        Self { uuid: Uuid::new_v4(), name: "workspace".into() }
    }
}

#[derive(Resource, Debug, Default, Reflect)]
#[reflect(Resource)]
pub struct WorkspaceRevision {
    pub current: u64,
    pub saved: u64,
}

impl WorkspaceRevision {
    pub fn mark_dirty(&mut self) {
        self.current = self.current.wrapping_add(1);
    }
    pub fn mark_saved(&mut self) {
        self.saved = self.current;
    }
    pub fn is_dirty(&self) -> bool {
        self.current != self.saved
    }
}

/// The last authoring command that was refused, for the editor to show.
#[derive(Resource, Debug, Default)]
pub struct AuthoringFeedback {
    pub message: Option<String>,
}

impl AuthoringFeedback {
    fn refuse(&mut self, message: String) {
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
        Self { layer_uuid: DEFAULT_LAYER_UUID, order: 0 }
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
#[derive(Component, Debug, Clone, Copy, Reflect)]
#[reflect(Component)]
pub struct DisplayConfig {
    pub radix: DisplayRadix,
    pub leading_zeroes: bool,
}

impl Default for DisplayConfig {
    fn default() -> Self {
        Self { radix: DisplayRadix::Hexadecimal, leading_zeroes: true }
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
        GateType { kind: node.kind.clone() },
        CircuitMember { circuit_uuid },
        CircuitLayer { layer_uuid: node.layer, order: node.layer_order },
        PersistentGateData { custom_properties: node.properties.clone() },
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
            CircuitLayer { layer_uuid: wire.layer, order: wire.layer_order },
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
type WireDocQuery = (&'static WireIdentity, &'static WireEndpoints, &'static CircuitLayer);

/// Reads the open circuit's authoring components as a document. Used for
/// save and subcircuit navigation; never per frame.
#[derive(SystemParam)]
pub struct CircuitReader<'w, 's> {
    pub active: Res<'w, ActiveCircuit>,
    pub library: Res<'w, CircuitLibrary>,
    gates: Query<'w, 's, GateDocQuery>,
    wires: Query<'w, 's, WireDocQuery>,
}

impl CircuitReader<'_, '_> {
    pub fn document(&self) -> CircuitDocument {
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
                from: PinAddr { node: ep.source_gate, pin: ep.source_pin },
                to: PinAddr { node: ep.sink_gate, pin: ep.sink_pin },
                layer: layer.layer_uuid,
                layer_order: layer.order,
            })
            .collect();
        wires.sort_by_key(|w| w.id);
        CircuitDocument { id: self.active.uuid, name: self.active.name.clone(), nodes, wires }
    }

    /// The open circuit plus every definition it instances.
    pub fn workspace(&self) -> WorkspaceDocument {
        let mut root = self.document();
        let definitions = self.library.reachable_from(&root);
        if definitions.contains_key(&root.id) {
            // The circuit instances a definition that shares its id (it was
            // opened from the catalog and placed into itself): keep ids unique.
            root.id = Uuid::new_v4();
        }
        WorkspaceDocument { root, definitions }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Commands (messages) and their handlers
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Message, Debug, Clone)]
pub struct CreateGate {
    pub kind: GateKind,
    pub position: Vec2,
    pub label: Option<String>,
}

#[derive(Message, Debug, Clone, Copy)]
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
}

#[derive(Message)]
pub struct SaveWorkspaceEvent {
    pub name: String,
}

/// Generate a unique `"{base} {n}"` label for a new port gate.
fn generate_port_label<'a>(labels: impl Iterator<Item = &'a str>, base: &str) -> String {
    let prefix = format!("{base} ");
    let max = labels
        .filter_map(|l| l.strip_prefix(&prefix).and_then(|s| s.parse::<usize>().ok()))
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

pub fn apply_create_gate_system(
    mut commands: Commands,
    mut events: MessageReader<CreateGate>,
    active: Res<ActiveCircuit>,
    mut revision: ResMut<WorkspaceRevision>,
    mut feedback: ResMut<AuthoringFeedback>,
    library: Res<CircuitLibrary>,
    labels: Query<&GateLabel>,
) {
    let mut taken: Vec<String> = Vec::new();
    for event in events.read() {
        if let Err(reason) = event.kind.check() {
            feedback.refuse(format!("Cannot place {}: {reason}", event.kind.display_name()));
            continue;
        }
        if let GateKind::SubCircuit { definition } = event.kind
            && library.get(definition).is_none()
        {
            feedback.refuse(format!("Cannot place subcircuit: unknown definition {definition}"));
            continue;
        }
        let mut node = DocNode::new(event.kind.clone(), event.position);
        node.label = event.label.clone().filter(|l| !l.trim().is_empty());
        if node.label.is_none() && event.kind.is_port() {
            let base = title_case(event.kind.display_name());
            let label = generate_port_label(
                labels.iter().map(|l| l.value.as_str()).chain(taken.iter().map(String::as_str)),
                &base,
            );
            taken.push(label.clone());
            node.label = Some(label);
        }
        spawn_gate(&mut commands, &node, active.uuid);
        revision.mark_dirty();
    }
}

#[allow(clippy::too_many_arguments)]
pub fn apply_create_wire_system(
    mut commands: Commands,
    mut events: MessageReader<CreateWire>,
    active: Res<ActiveCircuit>,
    library: Res<CircuitLibrary>,
    mut revision: ResMut<WorkspaceRevision>,
    mut feedback: ResMut<AuthoringFeedback>,
    gates: Query<(&GateIdentity, &GateType)>,
    wires: Query<(Entity, &WireEndpoints)>,
) {
    for event in events.read() {
        let find = |uuid: Uuid| gates.iter().find(|(id, _)| id.uuid == uuid).map(|(_, t)| t);
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
        // One driver per input pin: a new wire replaces the old one.
        for (entity, ep) in &wires {
            if ep.sink_gate == event.sink_gate && ep.sink_pin == event.sink_pin {
                commands.entity(entity).despawn();
            }
        }
        let wire = DocWire::new(event.source_gate, event.source_pin, event.sink_gate, event.sink_pin);
        spawn_wire(&mut commands, &wire, active.uuid);
        feedback.message = None;
        revision.mark_dirty();
    }
}

/// Delete gates (with their wires) and wires.
pub fn apply_delete_system(
    mut commands: Commands,
    mut gate_events: MessageReader<DeleteGate>,
    mut wire_events: MessageReader<DeleteWire>,
    mut revision: ResMut<WorkspaceRevision>,
    gates: Query<&GateIdentity>,
    wires: Query<(Entity, &WireEndpoints)>,
) {
    for event in wire_events.read() {
        if let Ok((entity, _)) = wires.get(event.entity) {
            commands.entity(entity).despawn();
            revision.mark_dirty();
        }
    }
    for event in gate_events.read() {
        let Ok(identity) = gates.get(event.entity) else { continue };
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
    mut inputs: Query<(&GateType, &mut InputDrive)>,
) {
    for event in events.read() {
        let Ok((ty, mut drive)) = inputs.get_mut(event.entity) else { continue };
        let w = ty.kind.width().unwrap_or(64);
        drive.set_if_neq(InputDrive { value: event.value & logi_sim::netlist::mask(w) });
    }
}

pub fn apply_set_kind_system(
    mut commands: Commands,
    mut events: MessageReader<SetGateKind>,
    mut gates: Query<(&mut GateType, Option<&InputDrive>, Option<&DisplayConfig>)>,
    mut revision: ResMut<WorkspaceRevision>,
    mut feedback: ResMut<AuthoringFeedback>,
) {
    for event in events.read() {
        let Ok((mut ty, drive, display)) = gates.get_mut(event.entity) else { continue };
        if let Err(reason) = event.kind.check() {
            feedback.refuse(format!("Invalid {} settings: {reason}", event.kind.display_name()));
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
            commands.entity(event.entity).insert(DisplayConfig::default());
        }
        revision.mark_dirty();
    }
}

/// Register definitions sent by the editor.
pub fn apply_register_definitions_system(
    mut events: MessageReader<RegisterDefinitions>,
    mut library: ResMut<CircuitLibrary>,
) {
    for event in events.read() {
        // Only touch the resource (and trigger a rebuild) on a real change.
        let changed = event.definitions.iter().any(|(id, def)| {
            library.definitions.get(id).is_none_or(|have| **have != **def)
        });
        if changed {
            library.merge(&event.definitions);
        }
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
    mut gate_labels: Query<(&GateIdentity, &mut GateLabel)>,
    mut display_configs: Query<(&GateIdentity, &mut DisplayConfig)>,
    mut gate_transforms: Query<(&GateIdentity, &mut Transform)>,
    mut gate_layers: Query<(&GateIdentity, &mut CircuitLayer)>,
    mut wire_layers: Query<(&WireIdentity, &mut CircuitLayer), Without<GateIdentity>>,
) {
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
pub fn sync_gate_names_system(
    mut gates: Query<(&GateType, &GateLabel, &mut Name), NameSource>,
) {
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
    gates: Query<Entity, With<GateIdentity>>,
    wires: Query<Entity, With<WireIdentity>>,
) {
    let Some(event) = events.read().last() else { return };
    for entity in gates.iter().chain(wires.iter()) {
        commands.entity(entity).despawn();
    }
    if event.workspace.definitions.iter().any(|(id, def)| {
            library.definitions.get(id).is_none_or(|have| **have != **def)
        })
    {
        library.merge(&event.workspace.definitions);
    }
    let root = &event.workspace.root;
    *active = ActiveCircuit { uuid: root.id, name: event.name.clone() };
    spawn_document(&mut commands, root, root.id);
    *revision = WorkspaceRevision::default();
}
