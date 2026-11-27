//! `SimBridge`: maps the ECS authoring components onto a `logi_sim` v2
//! [`Design`] and drives the engine.
//!
//! ## Mapping
//!
//! The bridge owns one persistent `Design`. Every gate entity maps to one
//! node of the root definition (primitive kinds one-to-one; a subcircuit to
//! an `Instance` of a shared definition built from the
//! [`CircuitLibrary`]), and every wire entity to one connected input pin.
//! [`sync_design_system`] applies ECS changes (spawns, despawns, kind and
//! endpoint changes) to the design through its mutation API, so `NodeId`s
//! stay stable across edits and the engine can carry state over.
//!
//! ## Edits
//!
//! After a frame with changes the bridge sends the new design revision to
//! the engine ([`Engine`] on a worker thread natively, [`LocalEngine`]
//! in-frame on wasm and in tests). The engine compiles it, swaps programs and
//! keeps register/RAM state. On a compile error the old program keeps
//! running and the error is published in [`SimDiagnostics`].
//!
//! ## Definitions
//!
//! Each [`CircuitLibrary`] entry that is instanced is built once into a
//! design definition, keyed by its stable id. When a library entry changes
//! (an edit in an instance view), the bridge updates that definition in
//! place: nodes are matched by their document id, so unchanged nodes keep
//! their `NodeId`, and the engine carries state across wherever the node
//! path is unchanged. Port nodes are stateless; if the port list changes
//! they are all rebuilt, which renumbers the instance pins, and every
//! wire into or out of an instance of that definition is reconnected.
//! Definitions no longer reachable from the root are dropped.
//!
//! ## Readback
//!
//! The open view decides the probes. At the root: one probe per output net
//! of every root gate. In a subcircuit view (see [`super::view`]): one
//! probe per output net of the definition, at the instance path of the
//! open instance, so the values are those of that instance in the running
//! simulation. Changing the view changes the probe list only; it never
//! recompiles. Probe indices are written to [`GateProbes`] and
//! [`WireState::probe`]; [`project_probes_system`] copies the latest probe
//! frame into [`GateValues`] and [`WireState::value`].

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::time::Duration;

use std::sync::Arc;

use bevy::ecs::entity_disabling::Disabled;
use bevy::platform::time::Instant;
use bevy::prelude::*;
use logi_sim::compile::{CompileError, NodePath, Options};
#[cfg(not(target_arch = "wasm32"))]
use logi_sim::engine::Engine;
use logi_sim::engine::{EngineEvent, LocalEngine, MemProbe, Probe, ProbeFrame, Rate};
use logi_sim::netlist::{DefId, Design, NodeId, NodeKind, PinRef};
use uuid::Uuid;

use super::document::{CircuitDocument, CircuitLibrary, DocNode};
use super::view::{CircuitView, InstanceViewItem};
use super::visuals::EntityWorkCounter;
use super::{
    GateIdentity, GateKind, GateLabel, GateProbes, GateType, GateValues, InputDrive,
    SimulationConfig, WireEndpoints, WireState,
};

/// Which engine runs the simulation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SimRuntime {
    /// `logi_sim::engine::Engine` on a worker thread (native default).
    Threaded,
    /// `logi_sim::engine::LocalEngine` run in-frame (wasm default).
    Threadless,
}

impl SimRuntime {
    pub fn platform_default() -> Self {
        #[cfg(not(target_arch = "wasm32"))]
        return SimRuntime::Threaded;
        #[cfg(target_arch = "wasm32")]
        return SimRuntime::Threadless;
    }
}

enum Runtime {
    #[cfg(not(target_arch = "wasm32"))]
    Threaded(Engine),
    Local {
        engine: Box<LocalEngine>,
        origin: Instant,
    },
}

struct GateEntry {
    uuid: Uuid,
    kind: Option<GateKind>,
    node: Option<NodeId>,
}

struct WireEntry {
    source: (NodeId, u8),
    sink: PinRef,
}

/// Nodes (ports with their labels) and wires: what the design is built
/// from. Positions and layers are not part of it.
type StructureKey = (
    Vec<(Uuid, GateKind, Option<String>)>,
    Vec<(Uuid, u8, Uuid, u8)>,
);

fn structure_key(doc: &CircuitDocument) -> StructureKey {
    let mut nodes: Vec<_> = doc
        .nodes
        .iter()
        .filter(|n| !matches!(n.kind, GateKind::Display { .. }))
        .map(|n| {
            let port = matches!(n.kind, GateKind::Input { .. } | GateKind::Output { .. });
            (
                n.id,
                n.kind.clone(),
                if port { n.label.clone() } else { None },
            )
        })
        .collect();
    nodes.sort_by_key(|n| n.0);
    let mut wires: Vec<_> = doc
        .wires
        .iter()
        .map(|w| (w.from.node, w.from.pin, w.to.node, w.to.pin))
        .collect();
    wires.sort();
    (nodes, wires)
}

/// A library definition built into the design.
struct BuiltDef {
    def: DefId,
    key: StructureKey,
    doc: Arc<CircuitDocument>,
    /// Document node id -> design node, with the kind it was built as.
    nodes: HashMap<Uuid, (NodeId, GateKind)>,
    /// Port nodes as built, in port order.
    ports: Vec<(Uuid, GateKind)>,
}

/// Compile and load status, for the editor to show.
#[derive(Resource, Debug, Default)]
pub struct SimDiagnostics {
    /// Revision of the program the engine is running.
    pub running_revision: Option<u64>,
    /// The newest revision that failed to compile, if it is newer than the
    /// running program.
    pub compile_error: Option<CompileFailure>,
}

#[derive(Debug, Clone)]
pub struct CompileFailure {
    pub revision: u64,
    pub messages: Vec<String>,
    /// Gates on a combinational loop (root-level gates; a loop inside a
    /// subcircuit names the instance gate).
    pub loop_gates: Vec<Entity>,
    /// The nodes of each combinational loop (one entry per loop the
    /// compiler reported), by gate id and instance path. The editor
    /// highlights them in the open view.
    pub loops: Vec<Vec<LoopNode>>,
}

/// A node on a combinational loop, addressed by stable gate ids.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LoopNode {
    /// Instance gates from the root to the circuit holding the node (empty
    /// for a root gate).
    pub path: Vec<Uuid>,
    /// The gate itself, in the circuit at `path`.
    pub gate: Uuid,
}

#[derive(Resource)]
pub struct SimBridge {
    runtime: Mutex<Runtime>,
    design: Design,
    /// The design changed since it was last sent.
    dirty: bool,
    /// Gate nodes were added or removed: the probe list must be rebuilt.
    probes_dirty: bool,
    /// Revision of the last design sent to the engine.
    revision: u64,
    gates: HashMap<Entity, GateEntry>,
    node_entity: HashMap<NodeId, Entity>,
    node_by_uuid: HashMap<Uuid, NodeId>,
    wires: HashMap<Entity, WireEntry>,
    /// Definitions built from the library, by document id.
    built: HashMap<Uuid, BuiltDef>,
    def_ids: HashMap<DefId, Uuid>,
    /// Probe index by `(node, output pin)` of the probed definition (the
    /// root, or the definition of the open view).
    probe_index: HashMap<(NodeId, u8), u32>,
    /// Memory probe index by screen node of the probed definition.
    mem_probe_index: HashMap<NodeId, u32>,
    /// A subcircuit view is open.
    view_open: bool,
    /// Gate ids from the root to the open instance (live views only).
    view_path: Option<Vec<Uuid>>,
    /// The open instance as resolved at the last probe rebuild: node path
    /// and definition id.
    view_resolved: Option<(Vec<NodeId>, Uuid)>,
    /// Bumped whenever probe bindings may have changed.
    binding_generation: u64,
    running: bool,
    tick_rate: Option<f64>,
    sent_rate: Option<Rate>,
    events: Vec<EngineEvent>,
    last_event_revision: u64,
}

impl SimBridge {
    pub fn new(runtime: SimRuntime) -> Self {
        let runtime = match runtime {
            #[cfg(not(target_arch = "wasm32"))]
            // Feature `jit`: the Cranelift backend, tiered (logi-sim #117).
            SimRuntime::Threaded => Runtime::Threaded(Engine::spawn_with(
                Options::debug(),
                // The JIT and adaptive specialization (`SimOptions::spec`).
                logi_sim::sim::SimOptions::interactive(),
            )),
            #[cfg(target_arch = "wasm32")]
            SimRuntime::Threaded | SimRuntime::Threadless => Runtime::Local {
                // The wasm JIT (logi-sim feature `jit`, on for the web
                // build), tiered: each edit runs interpreted at once and
                // switches to generated wasm code when it is instantiated.
                engine: Box::new(LocalEngine::with_sim_options(
                    Options::debug(),
                    logi_sim::sim::SimOptions::interactive(),
                )),
                origin: Instant::now(),
            },
            #[cfg(not(target_arch = "wasm32"))]
            SimRuntime::Threadless => Runtime::Local {
                engine: Box::new(LocalEngine::new(Options::debug())),
                origin: Instant::now(),
            },
        };
        let mut bridge = Self {
            runtime: Mutex::new(runtime),
            design: Design::new("root"),
            dirty: false,
            probes_dirty: false,
            revision: 0,
            gates: HashMap::new(),
            node_entity: HashMap::new(),
            node_by_uuid: HashMap::new(),
            wires: HashMap::new(),
            built: HashMap::new(),
            def_ids: HashMap::new(),
            probe_index: HashMap::new(),
            mem_probe_index: HashMap::new(),
            view_open: false,
            view_path: None,
            view_resolved: None,
            binding_generation: 0,
            running: false,
            tick_rate: Some(SimulationConfig::default().tick_rate),
            sent_rate: None,
            events: Vec::new(),
            last_event_revision: 0,
        };
        bridge.apply_rate();
        bridge
    }

    fn with_runtime<R>(&self, f: impl FnOnce(&mut Runtime) -> R) -> R {
        let mut guard = self.runtime.lock().unwrap_or_else(|e| e.into_inner());
        f(&mut guard)
    }

    /// The design the engine runs (for inspection and tests).
    pub fn design(&self) -> &Design {
        &self.design
    }

    /// The node a gate entity maps to.
    pub fn node_of(&self, entity: Entity) -> Option<NodeId> {
        self.gates.get(&entity).and_then(|g| g.node)
    }

    pub fn entity_of(&self, node: NodeId) -> Option<Entity> {
        self.node_entity.get(&node).copied()
    }

    /// Revision of the last design sent to the engine.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn binding_generation(&self) -> u64 {
        self.binding_generation
    }

    // ── Design mutation (driven by `sync_design_system`) ─────────────────

    fn root(&self) -> DefId {
        self.design.root
    }

    pub fn remove_wire(&mut self, entity: Entity) {
        if let Some(entry) = self.wires.remove(&entity) {
            let root = self.root();
            if self.design.root_def().nodes.contains_key(entry.sink.0)
                && self.design.root_def().input_net(entry.sink.0, entry.sink.1)
                    == self
                        .design
                        .root_def()
                        .output_net(entry.source.0, entry.source.1)
            {
                let _ = self.design.disconnect(root, entry.sink);
            }
            self.dirty = true;
        }
    }

    pub fn remove_gate(&mut self, entity: Entity) {
        let Some(entry) = self.gates.remove(&entity) else {
            return;
        };
        self.node_by_uuid.remove(&entry.uuid);
        if let Some(node) = entry.node {
            self.node_entity.remove(&node);
            let root = self.root();
            let _ = self.design.remove_node(root, node);
            self.dirty = true;
            self.probes_dirty = true;
        }
    }

    /// Forget a gate's recorded kind so the next upsert builds it again.
    pub fn forget_gate(&mut self, entity: Entity) {
        if let Some(entry) = self.gates.get_mut(&entity) {
            entry.kind = None;
        }
    }

    /// Create or replace the node for a gate. Returns `true` when a new node
    /// was created (its wires must be reconnected).
    pub fn upsert_gate(
        &mut self,
        entity: Entity,
        uuid: Uuid,
        kind: &GateKind,
        library: &CircuitLibrary,
    ) -> bool {
        if let Some(entry) = self.gates.get(&entity)
            && entry.kind.as_ref() == Some(kind)
            && entry.uuid == uuid
        {
            return false;
        }
        self.remove_gate(entity);
        let root = self.root();
        let node = match kind {
            GateKind::SubCircuit { definition } => self
                .sync_def(
                    *definition,
                    library,
                    &mut Vec::new(),
                    &mut HashSet::new(),
                    &mut HashSet::new(),
                )
                .and_then(|def| match self.design.add_instance(root, def) {
                    Ok(n) => Some(n),
                    Err(e) => {
                        warn!("Cannot instance subcircuit {definition}: {e}");
                        None
                    }
                }),
            k => match k.to_netlist().map(|nk| self.design.add_node(root, nk)) {
                Some(Ok(n)) => Some(n),
                Some(Err(e)) => {
                    warn!("Cannot add {} gate {uuid}: {e}", k.display_name());
                    None
                }
                None => None,
            },
        };
        if let Some(n) = node {
            self.node_entity.insert(n, entity);
            self.node_by_uuid.insert(uuid, n);
            self.dirty = true;
            self.probes_dirty = true;
        }
        self.gates.insert(
            entity,
            GateEntry {
                uuid,
                kind: Some(kind.clone()),
                node,
            },
        );
        true
    }

    /// Connect a wire entity. Replaces the wire's previous connection.
    pub fn connect_wire(&mut self, entity: Entity, ep: &WireEndpoints) -> Result<u8, String> {
        self.remove_wire(entity);
        let root = self.root();
        let source = *self
            .node_by_uuid
            .get(&ep.source_gate)
            .ok_or_else(|| format!("source gate {} is not simulated", ep.source_gate))?;
        let sink = *self
            .node_by_uuid
            .get(&ep.sink_gate)
            .ok_or_else(|| format!("sink gate {} is not simulated", ep.sink_gate))?;
        self.design
            .connect_pins(root, (source, ep.source_pin), (sink, ep.sink_pin))
            .map_err(|e| e.to_string())?;
        self.dirty = true;
        self.wires.insert(
            entity,
            WireEntry {
                source: (source, ep.source_pin),
                sink: (sink, ep.sink_pin),
            },
        );
        let net = self
            .design
            .root_def()
            .output_net(source, ep.source_pin)
            .expect("connected");
        Ok(self.design.root_def().nets[net].width)
    }

    /// Bring every built definition up to date with the library. Returns
    /// the ids of definitions whose interface (ports) changed; wires to
    /// their instances in the root must be reconnected
    /// (see [`Self::reset_instance_wires`]).
    pub fn sync_library(&mut self, library: &CircuitLibrary) -> HashSet<Uuid> {
        let mut ids: Vec<Uuid> = self.built.keys().copied().collect();
        ids.sort();
        let mut done = HashSet::new();
        let mut iface_changed = HashSet::new();
        for id in ids {
            self.sync_def(id, library, &mut Vec::new(), &mut done, &mut iface_changed);
        }
        if !iface_changed.is_empty() {
            let mut parents: Vec<Uuid> = self
                .built
                .iter()
                .filter(|(_, b)| {
                    b.doc
                        .instanced_definitions()
                        .any(|d| iface_changed.contains(&d))
                })
                .map(|(id, _)| *id)
                .collect();
            parents.sort();
            for id in parents {
                if let Some(b) = self.built.remove(&id) {
                    self.rewire(&b);
                    self.built.insert(id, b);
                }
            }
        }
        iface_changed
    }

    /// Build definition `id` from the library, or update the built one in
    /// place. `stack` holds the definitions being built (recursion guard),
    /// `done` those already synced in this pass.
    fn sync_def(
        &mut self,
        id: Uuid,
        library: &CircuitLibrary,
        stack: &mut Vec<Uuid>,
        done: &mut HashSet<Uuid>,
        iface_changed: &mut HashSet<Uuid>,
    ) -> Option<DefId> {
        if done.contains(&id) {
            return self.built.get(&id).map(|b| b.def);
        }
        if stack.contains(&id) {
            warn!("Subcircuit definition {id} contains itself; it is not simulated");
            return None;
        }
        let Some(doc) = library.get(id).cloned() else {
            warn!("Subcircuit definition {id} is missing from the library");
            return self.built.get(&id).map(|b| b.def);
        };
        if let Some(b) = self.built.get(&id)
            && Arc::ptr_eq(&b.doc, &doc)
        {
            done.insert(id);
            return Some(b.def);
        }
        // Positions, layers and non-port labels are not part of the key: a
        // move in an instance view updates the document without touching
        // the design.
        let key = structure_key(&doc);
        if let Some(b) = self.built.get_mut(&id)
            && b.key == key
        {
            b.doc = doc;
            done.insert(id);
            return Some(b.def);
        }

        // Children first.
        stack.push(id);
        let mut children: HashMap<Uuid, DefId> = HashMap::new();
        let child_ids: HashSet<Uuid> = doc.instanced_definitions().collect();
        for child in child_ids {
            if let Some(def) = self.sync_def(child, library, stack, done, iface_changed) {
                children.insert(child, def);
            }
        }
        stack.pop();

        let mut b = match self.built.remove(&id) {
            Some(b) => b,
            None => {
                let def = self.design.add_definition(doc.name.clone());
                self.def_ids.insert(def, id);
                BuiltDef {
                    def,
                    key: Default::default(),
                    doc: Arc::clone(&doc),
                    nodes: HashMap::new(),
                    ports: Vec::new(),
                }
            }
        };
        let def = b.def;
        let old_signature = self.design.def(def).map(|d| d.signature()).ok();

        let wanted: HashMap<Uuid, &DocNode> = doc
            .nodes
            .iter()
            .filter(|n| !matches!(n.kind, GateKind::Display { .. }))
            .map(|n| (n.id, n))
            .collect();
        let (inputs, outputs) = doc.interface();
        let ports: Vec<(Uuid, GateKind)> = inputs
            .iter()
            .chain(outputs.iter())
            .map(|n| (n.id, n.kind.clone()))
            .collect();
        let ports_changed = b.ports != ports;

        // Remove nodes that are gone or changed kind; all ports if the port
        // list changed (ports hold no state).
        let mut stale: HashSet<Uuid> = b
            .nodes
            .iter()
            .filter(|(uuid, (_, kind))| wanted.get(*uuid).is_none_or(|n| &n.kind != kind))
            .map(|(uuid, _)| *uuid)
            .collect();
        if ports_changed {
            stale.extend(b.ports.iter().map(|(uuid, _)| *uuid));
            b.ports.clear();
        }
        let mut stale: Vec<Uuid> = stale.into_iter().collect();
        stale.sort();
        for uuid in stale {
            if let Some((node, _)) = b.nodes.remove(&uuid) {
                let _ = self.design.remove_node(def, node);
            }
        }

        if ports_changed {
            for n in inputs.iter().chain(outputs.iter()) {
                match n.kind.to_netlist().map(|k| self.design.add_node(def, k)) {
                    Some(Ok(node)) => {
                        b.nodes.insert(n.id, (node, n.kind.clone()));
                        b.ports.push((n.id, n.kind.clone()));
                    }
                    _ => warn!("Subcircuit {}: cannot build port {}", doc.name, n.id),
                }
            }
        }
        for n in inputs.iter().chain(outputs.iter()) {
            if let Some((node, _)) = b.nodes.get(&n.id) {
                let _ = self.design.set_name(def, *node, n.label.clone());
            }
        }
        for n in &doc.nodes {
            if b.nodes.contains_key(&n.id) || n.kind.is_port() {
                continue;
            }
            let node = match &n.kind {
                GateKind::SubCircuit { definition } => children.get(definition).and_then(|child| {
                    self.design
                        .add_instance(def, *child)
                        .map_err(|e| {
                            warn!("Subcircuit {}: cannot instance {definition}: {e}", doc.name)
                        })
                        .ok()
                }),
                k => k
                    .to_netlist()
                    .and_then(|nk| self.design.add_node(def, nk).ok()),
            };
            match node {
                Some(node) => {
                    b.nodes.insert(n.id, (node, n.kind.clone()));
                }
                None => warn!("Subcircuit {}: cannot build node {}", doc.name, n.id),
            }
        }

        b.doc = doc;
        b.key = key;
        self.rewire(&b);
        let new_signature = self.design.def(def).map(|d| d.signature()).ok();
        if ports_changed || old_signature != new_signature {
            iface_changed.insert(id);
        }
        self.built.insert(id, b);
        done.insert(id);
        self.dirty = true;
        self.probes_dirty = true;
        Some(def)
    }

    /// Disconnect every input pin of a built definition and connect its
    /// document's wires again.
    fn rewire(&mut self, b: &BuiltDef) {
        let Ok(d) = self.design.def(b.def) else {
            return;
        };
        let pins: Vec<PinRef> = d
            .nodes
            .iter()
            .flat_map(|(node, n)| {
                n.inputs
                    .iter()
                    .enumerate()
                    .filter(|(_, net)| net.is_some())
                    .map(move |(pin, _)| (node, pin as u8))
            })
            .collect();
        for pin in pins {
            let _ = self.design.disconnect(b.def, pin);
        }
        for w in &b.doc.wires {
            let (Some((src, _)), Some((dst, _))) =
                (b.nodes.get(&w.from.node), b.nodes.get(&w.to.node))
            else {
                continue;
            };
            if let Err(e) = self
                .design
                .connect_pins(b.def, (*src, w.from.pin), (*dst, w.to.pin))
            {
                warn!(
                    "Subcircuit {}: cannot connect wire {}: {e}",
                    b.doc.name, w.id
                );
            }
        }
        self.dirty = true;
    }

    /// The pins of a root instance were renumbered: forget and disconnect
    /// every wire into or out of it. The caller reconnects them.
    pub fn reset_instance_wires(&mut self, entity: Entity) {
        let Some(node) = self.node_of(entity) else {
            return;
        };
        self.wires
            .retain(|_, w| w.source.0 != node && w.sink.0 != node);
        let root = self.root();
        let Some(n) = self.design.root_def().nodes.get(node) else {
            return;
        };
        let mut sinks: Vec<PinRef> = (0..n.inputs.len()).map(|pin| (node, pin as u8)).collect();
        for &net in &n.outputs {
            sinks.extend(self.design.root_def().sinks(net));
        }
        for sink in sinks {
            let _ = self.design.disconnect(root, sink);
        }
        self.dirty = true;
    }

    /// Remove built definitions that the root no longer reaches.
    fn collect_unused_definitions(&mut self) {
        let mut reachable = HashSet::new();
        let mut stack = vec![self.design.root];
        while let Some(def) = stack.pop() {
            if !reachable.insert(def) {
                continue;
            }
            if let Ok(d) = self.design.def(def) {
                for n in d.nodes.values() {
                    if let NodeKind::Instance(child) = n.kind {
                        stack.push(child);
                    }
                }
            }
        }
        let unused: Vec<Uuid> = self
            .built
            .iter()
            .filter(|(_, b)| !reachable.contains(&b.def))
            .map(|(id, _)| *id)
            .collect();
        if unused.is_empty() {
            return;
        }
        let mut remaining: Vec<DefId> = Vec::new();
        for id in unused {
            if let Some(b) = self.built.remove(&id) {
                self.def_ids.remove(&b.def);
                remaining.push(b.def);
            }
        }
        // Parents first: retry until none is in use.
        while !remaining.is_empty() {
            let before = remaining.len();
            remaining.retain(|&d| self.design.remove_definition(d).is_err());
            if remaining.len() == before {
                break;
            }
        }
    }

    /// The stable gate ids of a compiled node path, as long as the design
    /// still has those nodes.
    pub fn resolve_node_path(&self, path: &NodePath) -> Option<LoopNode> {
        let mut def = self.design.root;
        let mut gates = Vec::with_capacity(path.instances.len());
        for &instance in &path.instances {
            gates.push(self.gate_uuid(def, instance)?);
            def = match self.design.def(def).ok()?.nodes.get(instance)?.kind {
                NodeKind::Instance(child) => child,
                _ => return None,
            };
        }
        Some(LoopNode {
            path: gates,
            gate: self.gate_uuid(def, path.node)?,
        })
    }

    /// The document id of node `node` of design definition `def`.
    fn gate_uuid(&self, def: DefId, node: NodeId) -> Option<Uuid> {
        if def == self.design.root {
            let entity = self.node_entity.get(&node)?;
            return self.gates.get(entity).map(|g| g.uuid);
        }
        let built = self.built.get(self.def_ids.get(&def)?)?;
        built
            .nodes
            .iter()
            .find(|(_, (n, _))| *n == node)
            .map(|(uuid, _)| *uuid)
    }

    /// The gate entity of a root gate id.
    pub fn root_gate_entity(&self, uuid: Uuid) -> Option<Entity> {
        self.node_by_uuid
            .get(&uuid)
            .and_then(|node| self.entity_of(*node))
    }

    // ── Views ────────────────────────────────────────────────────────────

    /// Follow the open view. A change rebuilds the probe list (no
    /// recompile).
    pub fn set_view(&mut self, view: &CircuitView) {
        let open = !view.is_root();
        let path = view.instance_path();
        if self.view_open != open || self.view_path != path {
            self.view_open = open;
            self.view_path = path;
            self.probes_dirty = true;
        }
    }

    /// The definition an instance node instances, by library id.
    fn instance_target(&self, def: DefId, node: NodeId) -> Option<Uuid> {
        match self.design.def(def).ok()?.nodes.get(node)?.kind {
            NodeKind::Instance(child) => self.def_ids.get(&child).copied(),
            _ => None,
        }
    }

    /// Resolve gate ids from the root to the node path of an instance and
    /// the id of its definition.
    pub fn resolve_instance_path(&self, path: &[Uuid]) -> Option<(Vec<NodeId>, Uuid)> {
        let (first, rest) = path.split_first()?;
        let mut node = *self.node_by_uuid.get(first)?;
        let mut nodes = vec![node];
        let mut def = self.instance_target(self.design.root, node)?;
        for gate in rest {
            let b = self.built.get(&def)?;
            node = b.nodes.get(gate)?.0;
            nodes.push(node);
            def = self.instance_target(b.def, node)?;
        }
        Some((nodes, def))
    }

    /// The node path of the open instance view, as probed.
    pub fn view_node_path(&self) -> Option<&[NodeId]> {
        self.view_resolved.as_ref().map(|(path, _)| path.as_slice())
    }

    /// The design node of a gate of the open view.
    fn view_node(&self, gate: Uuid) -> Option<NodeId> {
        let (_, def) = self.view_resolved.as_ref()?;
        self.built.get(def)?.nodes.get(&gate).map(|(node, _)| *node)
    }

    /// Probe index of output pin `pin` of a gate of the open view.
    pub fn view_probe(&self, gate: Uuid, pin: u8) -> Option<u32> {
        let node = self.view_node(gate)?;
        self.probe_index.get(&(node, pin)).copied()
    }

    /// Whether a wire of the open view is connected in the running
    /// definition, and the width of its net. `None` when the view is not of
    /// a live instance.
    fn view_wire(&self, ep: &WireEndpoints) -> Option<(bool, u8)> {
        let (_, def) = self.view_resolved.as_ref()?;
        let b = self.built.get(def)?;
        let d = self.design.def(b.def).ok()?;
        let Some(src) = self.view_node(ep.source_gate) else {
            return Some((false, 0));
        };
        let Some(net) = d.output_net(src, ep.source_pin) else {
            return Some((false, 0));
        };
        let width = d.nets[net].width;
        let connected = match self.view_node(ep.sink_gate) {
            Some(sink) => d.input_net(sink, ep.sink_pin) == Some(net),
            // A Display: not part of the definition, shown from its source.
            None => b
                .doc
                .node(ep.sink_gate)
                .is_some_and(|n| matches!(n.kind, GateKind::Display { .. })),
        };
        Some((connected, width))
    }

    // ── Sending revisions ────────────────────────────────────────────────

    /// Send the design to the engine if it changed. Returns whether it did.
    /// Send the design to the engine if it changed, and rebuild the probe
    /// list if the design or the view changed. Returns whether probe
    /// bindings may have changed.
    pub fn flush(&mut self) -> bool {
        if !self.dirty && !self.probes_dirty {
            return false;
        }
        let send = self.dirty;
        if send {
            self.collect_unused_definitions();
            self.dirty = false;
        }
        self.probes_dirty = false;
        self.rebuild_probes();
        self.binding_generation += 1;
        if send {
            self.revision += 1;
            let revision = self.revision;
            let design = &self.design;
            self.with_runtime(|rt| match rt {
                #[cfg(not(target_arch = "wasm32"))]
                Runtime::Threaded(engine) => engine.load(Arc::new(design.clone()), revision),
                Runtime::Local { engine, .. } => engine.load(design, revision),
            });
        }
        true
    }

    fn rebuild_probes(&mut self) {
        self.probe_index.clear();
        self.mem_probe_index.clear();
        self.view_resolved = None;
        let mut probes = Vec::new();
        let mut mem_probes = Vec::new();
        let mut screen = |index: &mut HashMap<NodeId, u32>, path: &[NodeId], node, kind| {
            if matches!(kind, &NodeKind::Screen { .. }) {
                index.insert(node, mem_probes.len() as u32);
                mem_probes.push(MemProbe {
                    path: path.to_vec(),
                    node,
                });
            }
        };
        if !self.view_open {
            let root = self.design.root_def();
            let mut nodes: Vec<NodeId> = self.node_entity.keys().copied().collect();
            nodes.sort();
            for node in nodes {
                let Some(n) = root.nodes.get(node) else {
                    continue;
                };
                screen(&mut self.mem_probe_index, &[], node, &n.kind);
                for (pin, &net) in n.outputs.iter().enumerate() {
                    self.probe_index
                        .insert((node, pin as u8), probes.len() as u32);
                    probes.push(Probe {
                        path: Vec::new(),
                        net,
                    });
                }
            }
        } else if let Some(resolved) = self
            .view_path
            .as_deref()
            .and_then(|p| self.resolve_instance_path(p))
            && let Some(d) = self
                .built
                .get(&resolved.1)
                .and_then(|b| self.design.def(b.def).ok())
        {
            let mut nodes: Vec<NodeId> = d.nodes.keys().collect();
            nodes.sort();
            for node in nodes {
                screen(
                    &mut self.mem_probe_index,
                    &resolved.0,
                    node,
                    &d.nodes[node].kind,
                );
                for (pin, &net) in d.nodes[node].outputs.iter().enumerate() {
                    self.probe_index
                        .insert((node, pin as u8), probes.len() as u32);
                    probes.push(Probe {
                        path: resolved.0.clone(),
                        net,
                    });
                }
            }
            self.view_resolved = Some(resolved);
        }
        self.with_runtime(|rt| match rt {
            #[cfg(not(target_arch = "wasm32"))]
            Runtime::Threaded(engine) => {
                engine.set_probes(probes);
                engine.set_mem_probes(mem_probes);
                // Frames read after this use the new probe layout.
                engine.sync();
            }
            Runtime::Local { engine, .. } => {
                engine.core.set_probes(probes);
                engine.core.set_mem_probes(mem_probes);
            }
        });
    }

    /// Whether any screen is probed (see [`Self::request_screens`]).
    pub fn has_screens(&self) -> bool {
        !self.mem_probe_index.is_empty()
    }

    /// Ask the engine for fresh copies of the screens' memories, once per
    /// drawn frame (the in-frame engine copies them by itself).
    pub fn request_screens(&self) {
        if !self.has_screens() {
            return;
        }
        self.with_runtime(|rt| match rt {
            #[cfg(not(target_arch = "wasm32"))]
            Runtime::Threaded(engine) => engine.request_mems(),
            Runtime::Local { .. } => {}
        });
    }

    /// Memory probe of a screen of the open view.
    pub fn view_mem_probe(&self, gate: Uuid) -> Option<u32> {
        let node = self.view_node(gate)?;
        self.mem_probe_index.get(&node).copied()
    }

    /// Probe indices for a gate's input and output pins.
    pub fn gate_probes(&self, entity: Entity) -> GateProbes {
        let Some(node) = self.node_of(entity) else {
            return GateProbes::default();
        };
        let root = self.design.root_def();
        let Some(n) = root.nodes.get(node) else {
            return GateProbes::default();
        };
        let inputs = n
            .inputs
            .iter()
            .map(|net| {
                let driver = root.nets.get((*net)?)?.driver;
                self.probe_index.get(&driver).copied()
            })
            .collect();
        let outputs = (0..n.outputs.len())
            .map(|pin| self.probe_index.get(&(node, pin as u8)).copied())
            .collect();
        GateProbes {
            inputs,
            outputs,
            memory: self.mem_probe_index.get(&node).copied(),
        }
    }

    /// Probe index for a wire's net.
    pub fn wire_probe(&self, entity: Entity) -> Option<u32> {
        let entry = self.wires.get(&entity)?;
        self.probe_index.get(&entry.source).copied()
    }

    // ── Engine control ───────────────────────────────────────────────────

    pub fn set_input(&self, entity: Entity, value: u64) {
        let Some(node) = self.node_of(entity) else {
            return;
        };
        self.with_runtime(|rt| match rt {
            #[cfg(not(target_arch = "wasm32"))]
            Runtime::Threaded(engine) => engine.set_input(node, value),
            Runtime::Local { engine, .. } => engine.core.set_input(node, value),
        });
    }

    pub fn is_running(&self) -> bool {
        self.running
    }

    pub fn set_running(&mut self, running: bool) {
        self.running = running;
        self.apply_rate();
    }

    pub fn pause(&mut self) {
        self.set_running(false);
    }

    pub fn resume(&mut self) {
        self.set_running(true);
    }

    /// `None` = unlimited.
    pub fn set_tick_rate(&mut self, tick_rate: Option<f64>) {
        self.tick_rate = tick_rate;
        self.apply_rate();
    }

    fn apply_rate(&mut self) {
        let rate = match (self.running, self.tick_rate) {
            (false, _) => Rate::Paused,
            (true, None) => Rate::Unlimited,
            (true, Some(hz)) => Rate::Hz(hz.max(0.1)),
        };
        if self.sent_rate == Some(rate) {
            return;
        }
        self.sent_rate = Some(rate);
        self.with_runtime(|rt| match rt {
            #[cfg(not(target_arch = "wasm32"))]
            Runtime::Threaded(engine) => engine.set_rate(rate),
            Runtime::Local { engine, .. } => engine.core.set_rate(rate),
        });
    }

    /// Wait until the simulation's thread has handled every command sent
    /// so far (a pause, a step), so that [`Self::tick_count`] is the tick
    /// they left it at. Nothing to wait for when it runs in the frame.
    pub fn sync(&self) {
        self.with_runtime(|rt| match rt {
            #[cfg(not(target_arch = "wasm32"))]
            Runtime::Threaded(engine) => engine.sync(),
            Runtime::Local { .. } => {}
        });
    }

    /// Queue `n` ticks, run even while paused.
    pub fn step(&self, n: u64) {
        self.with_runtime(|rt| match rt {
            #[cfg(not(target_arch = "wasm32"))]
            Runtime::Threaded(engine) => engine.step(n),
            Runtime::Local { engine, .. } => engine.core.step(n),
        });
    }

    /// Return the simulation to power-on: tick 0, every register, RAM,
    /// latch, delay and clock phase at its initial value (ROM and RAM
    /// contents from their `init`), root inputs kept at their driven
    /// values. The run/pause state and tick rate are unchanged.
    pub fn reset(&self) {
        self.with_runtime(|rt| match rt {
            #[cfg(not(target_arch = "wasm32"))]
            Runtime::Threaded(engine) => engine.reset(),
            Runtime::Local { engine, .. } => engine.core.reset(),
        });
    }

    pub fn tick_count(&self) -> u64 {
        self.with_runtime(|rt| match rt {
            #[cfg(not(target_arch = "wasm32"))]
            Runtime::Threaded(engine) => engine.tick_count(),
            Runtime::Local { engine, .. } => engine.core.tick_count(),
        })
    }

    /// Run the in-frame engine for at most `budget` (no-op when threaded).
    pub fn run_local(&self, budget: Duration) -> u64 {
        self.with_runtime(|rt| match rt {
            #[cfg(not(target_arch = "wasm32"))]
            Runtime::Threaded(_) => 0,
            Runtime::Local { engine, origin } => {
                let origin = *origin;
                engine.run_frame(budget, &move || origin.elapsed())
            }
        })
    }

    /// Read the latest probe frame.
    pub fn with_frame<R>(&self, f: impl FnOnce(&ProbeFrame) -> R) -> R {
        self.with_runtime(|rt| match rt {
            #[cfg(not(target_arch = "wasm32"))]
            Runtime::Threaded(engine) => f(&engine.frame()),
            Runtime::Local { engine, .. } => f(engine.frame()),
        })
    }

    /// Collect engine events (load results).
    pub fn poll_events(&mut self) {
        let mut new = self.with_runtime(|rt| match rt {
            #[cfg(not(target_arch = "wasm32"))]
            Runtime::Threaded(engine) => std::iter::from_fn(|| engine.try_event()).collect(),
            Runtime::Local { engine, .. } => engine.drain_events(),
        });
        for e in &new {
            let rev = match e {
                EngineEvent::Loaded { revision, .. }
                | EngineEvent::CompileFailed { revision, .. } => *revision,
            };
            self.last_event_revision = self.last_event_revision.max(rev);
        }
        self.events.append(&mut new);
    }

    pub fn take_events(&mut self) -> Vec<EngineEvent> {
        std::mem::take(&mut self.events)
    }

    /// Block until the engine has answered the last design sent (loaded it
    /// or rejected it) and published a frame after that. Native only; the
    /// in-frame engine answers immediately. Returns false on timeout.
    pub fn wait_until_current(&mut self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            self.poll_events();
            if self.last_event_revision >= self.revision {
                #[cfg(not(target_arch = "wasm32"))]
                self.with_runtime(|rt| {
                    if let Runtime::Threaded(engine) = rt {
                        engine.sync();
                    }
                });
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Systems
// ─────────────────────────────────────────────────────────────────────────────

/// Root circuit entities, also while parked behind a subcircuit view.
type RootOnly = (Without<InstanceViewItem>, Allow<Disabled>);

type ChangedRootGate = (
    Or<(Changed<GateType>, Changed<InputDrive>)>,
    Without<InstanceViewItem>,
    Allow<Disabled>,
);
type ChangedViewItem = (
    With<InstanceViewItem>,
    Or<(Changed<GateType>, Changed<WireEndpoints>)>,
);

/// Probe bindings of the open subcircuit view's entities.
#[derive(bevy::ecs::system::SystemParam)]
pub struct ViewBindings<'w, 's> {
    changed: Query<'w, 's, (), ChangedViewItem>,
    gates: Query<
        'w,
        's,
        (
            &'static GateIdentity,
            &'static GateType,
            &'static mut GateProbes,
        ),
        With<InstanceViewItem>,
    >,
    wires: Query<'w, 's, (&'static WireEndpoints, &'static mut WireState), With<InstanceViewItem>>,
}

impl ViewBindings<'_, '_> {
    /// Point the view's gates and wires at the bridge's probes. Gate input
    /// values come from the wire into each pin.
    fn bind(&mut self, bridge: &SimBridge, library: &CircuitLibrary) {
        let sources: HashMap<(Uuid, u8), (Uuid, u8)> = self
            .wires
            .iter()
            .map(|(ep, _)| ((ep.sink_gate, ep.sink_pin), (ep.source_gate, ep.source_pin)))
            .collect();
        let mut kinds: HashMap<Uuid, GateKind> = HashMap::new();
        for (id, ty, mut probes) in &mut self.gates {
            kinds.insert(id.uuid, ty.kind.clone());
            let (ins, outs) = library.pins(&ty.kind).unwrap_or_default();
            let bound = GateProbes {
                inputs: (0..ins.len() as u8)
                    .map(|pin| {
                        sources
                            .get(&(id.uuid, pin))
                            .and_then(|&(g, p)| bridge.view_probe(g, p))
                    })
                    .collect(),
                outputs: (0..outs.len() as u8)
                    .map(|pin| bridge.view_probe(id.uuid, pin))
                    .collect(),
                memory: bridge.view_mem_probe(id.uuid),
            };
            probes.set_if_neq(bound);
        }
        for (ep, mut state) in &mut self.wires {
            let (connected, width) = bridge.view_wire(ep).unwrap_or_else(|| {
                // Not a live instance: show the authored widths.
                let out = kinds
                    .get(&ep.source_gate)
                    .and_then(|k| library.pins(k))
                    .and_then(|(_, o)| o.get(usize::from(ep.source_pin)).copied());
                let inp = kinds
                    .get(&ep.sink_gate)
                    .and_then(|k| library.pins(k))
                    .and_then(|(i, _)| i.get(usize::from(ep.sink_pin)).copied());
                (out.is_some() && out == inp, out.unwrap_or(0))
            });
            let new = WireState {
                connected,
                width,
                probe: bridge.view_probe(ep.source_gate, ep.source_pin),
                ..*state
            };
            state.set_if_neq(new);
        }
    }
}

/// Apply ECS authoring changes to the bridge's design and send a new
/// revision when anything changed. Runs every frame in `PostUpdate`, after
/// the authoring commands of the frame have been applied.
///
/// Root gates and wires map to the root definition. Entities of an open
/// subcircuit view do not touch the design directly: their edits reach the
/// library through [`super::view::write_back_view_system`], and the library
/// change updates the definition here.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub fn sync_design_system(
    mut bridge: ResMut<SimBridge>,
    library: Res<CircuitLibrary>,
    view: Res<CircuitView>,
    mut removed_gates: RemovedComponents<GateType>,
    mut removed_wires: RemovedComponents<WireEndpoints>,
    changed_gates: Query<
        (
            Entity,
            &GateIdentity,
            Ref<GateType>,
            Option<Ref<InputDrive>>,
        ),
        ChangedRootGate,
    >,
    all_gates: Query<(Entity, &GateIdentity, &GateType, Option<&InputDrive>), RootOnly>,
    changed_wires: Query<(), (Changed<WireEndpoints>, Without<InstanceViewItem>)>,
    mut wires: Query<(Entity, Ref<WireEndpoints>, &mut WireState), RootOnly>,
    mut gate_probes: Query<(Entity, &mut GateProbes), RootOnly>,
    mut view_bindings: ViewBindings,
    work: Option<Res<EntityWorkCounter>>,
) {
    if view.is_changed() {
        bridge.set_view(&view);
    }
    for entity in removed_wires.read() {
        bridge.remove_wire(entity);
    }
    for entity in removed_gates.read() {
        bridge.remove_gate(entity);
    }

    let mut recreated: HashSet<Uuid> = HashSet::new();
    let add = |bridge: &mut SimBridge,
               entity: Entity,
               id: &GateIdentity,
               ty: &GateType,
               drive: Option<&InputDrive>|
     -> bool {
        let created = bridge.upsert_gate(entity, id.uuid, &ty.kind, &library);
        if created && let Some(drive) = drive {
            bridge.set_input(entity, drive.value);
        }
        created
    };

    if library.is_changed() && !library.is_added() {
        let iface_changed = bridge.sync_library(&library);
        for (entity, id, ty, drive) in &all_gates {
            let GateKind::SubCircuit { definition } = ty.kind else {
                continue;
            };
            if bridge.node_of(entity).is_none() {
                // Its definition may be buildable now.
                bridge.forget_gate(entity);
                if add(&mut bridge, entity, id, ty, drive) {
                    recreated.insert(id.uuid);
                }
            } else if iface_changed.contains(&definition) {
                bridge.reset_instance_wires(entity);
                recreated.insert(id.uuid);
            }
        }
    }
    for (entity, id, ty, drive) in &changed_gates {
        if add(&mut bridge, entity, id, &ty, drive.as_deref()) {
            recreated.insert(id.uuid);
        } else if let Some(drive) = drive
            && drive.is_changed()
        {
            bridge.set_input(entity, drive.value);
        }
    }

    if !recreated.is_empty() || !changed_wires.is_empty() || bridge.dirty {
        for (entity, ep, mut state) in &mut wires {
            let touched = ep.is_changed()
                || recreated.contains(&ep.source_gate)
                || recreated.contains(&ep.sink_gate)
                || !state.connected;
            if !touched {
                continue;
            }
            EntityWorkCounter::add(&work, 1);
            let (connected, width) = match bridge.connect_wire(entity, &ep) {
                Ok(w) => (true, w),
                Err(e) => {
                    if state.connected || ep.is_added() || ep.is_changed() {
                        warn!("Wire not simulated: {e}");
                    }
                    (false, 0)
                }
            };
            if state.connected != connected || state.width != width {
                state.connected = connected;
                state.width = width;
            }
        }
    }

    let rebound = bridge.flush();
    if bridge.view_open {
        if rebound || !view_bindings.changed.is_empty() {
            EntityWorkCounter::add(
                &work,
                view_bindings.gates.iter().count() + view_bindings.wires.iter().count(),
            );
            view_bindings.bind(&bridge, &library);
        }
    } else if rebound {
        EntityWorkCounter::add(&work, gate_probes.iter().count() + wires.iter().count());
        for (entity, mut probes) in &mut gate_probes {
            probes.set_if_neq(bridge.gate_probes(entity));
        }
        for (entity, _, mut state) in &mut wires {
            let probe = bridge.wire_probe(entity);
            if state.probe != probe {
                state.probe = probe;
            }
        }
    }
}

/// The most gate names a loop message lists.
const LOOP_NAMES_SHOWN: usize = 8;

/// `Kind "label"`, or the kind name for an unlabelled gate.
fn gate_description(kind: &GateKind, label: Option<&str>) -> String {
    match label.map(str::trim).filter(|l| !l.is_empty()) {
        Some(label) => format!("{} \"{label}\"", kind.display_name()),
        None => kind.display_name().to_string(),
    }
}

/// Names a loop node for the editor: `cell1 / Not "inv"` (instance gates
/// by label, the gate by kind and label).
fn loop_node_name(
    node: &LoopNode,
    bridge: &SimBridge,
    library: &CircuitLibrary,
    root_gates: &Query<(&GateLabel, &GateType), Allow<Disabled>>,
) -> String {
    // The circuit holding the current gate: `None` is the root.
    let mut scope: Option<Uuid> = None;
    let mut parts = Vec::with_capacity(node.path.len() + 1);
    for (i, &gate) in node
        .path
        .iter()
        .chain(std::iter::once(&node.gate))
        .enumerate()
    {
        let found: Option<(GateKind, Option<String>)> = match scope {
            None => bridge
                .root_gate_entity(gate)
                .and_then(|e| root_gates.get(e).ok())
                .map(|(label, ty)| (ty.kind.clone(), Some(label.value.clone()))),
            Some(def) => library
                .get(def)
                .and_then(|d| d.node(gate))
                .map(|n| (n.kind.clone(), n.label.clone())),
        };
        let Some((kind, label)) = found else {
            parts.push("?".to_string());
            break;
        };
        let leaf = i == node.path.len();
        parts.push(if leaf {
            gate_description(&kind, label.as_deref())
        } else {
            label
                .filter(|l| !l.trim().is_empty())
                .unwrap_or_else(|| kind.display_name().to_string())
        });
        if let GateKind::SubCircuit { definition } = kind {
            scope = Some(definition);
        }
    }
    parts.join(" / ")
}

/// The editor message for a combinational loop through the named gates.
pub fn loop_message(names: &[String]) -> String {
    let count = names.len();
    let mut list = names
        .iter()
        .take(LOOP_NAMES_SHOWN)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    if count > LOOP_NAMES_SHOWN {
        list.push_str(&format!(" and {} more", count - LOOP_NAMES_SHOWN));
    }
    let gates = if count == 1 { "gate" } else { "gates" };
    format!(
        "Combinational loop through {count} {gates}: {list}. A feedback loop needs a state \
         element: put a Delay (or a Register, D latch, SR latch or RAM) on the loop."
    )
}

/// Collect load results into [`SimDiagnostics`].
pub fn collect_engine_events_system(
    mut bridge: ResMut<SimBridge>,
    mut diagnostics: ResMut<SimDiagnostics>,
    library: Res<CircuitLibrary>,
    root_gates: Query<(&GateLabel, &GateType), Allow<Disabled>>,
) {
    bridge.poll_events();
    for event in bridge.take_events() {
        match event {
            EngineEvent::Loaded { revision, .. } => {
                diagnostics.running_revision = Some(revision);
                if diagnostics
                    .compile_error
                    .as_ref()
                    .is_some_and(|e| e.revision <= revision)
                {
                    diagnostics.compile_error = None;
                }
            }
            EngineEvent::CompileFailed { revision, errors } => {
                let mut loop_gates = Vec::new();
                let mut loops = Vec::new();
                let mut messages = Vec::new();
                for error in &errors {
                    let nodes = match error {
                        CompileError::CombinationalLoop { nodes } => nodes,
                        CompileError::Invalid(v) => {
                            messages.push(format!("Invalid circuit: {v:?}"));
                            continue;
                        }
                    };
                    for path in nodes {
                        let root_node = path.instances.first().copied().unwrap_or(path.node);
                        if let Some(e) = bridge.entity_of(root_node)
                            && !loop_gates.contains(&e)
                        {
                            loop_gates.push(e);
                        }
                    }
                    let mut resolved: Vec<LoopNode> = Vec::new();
                    for node in nodes.iter().filter_map(|p| bridge.resolve_node_path(p)) {
                        if !resolved.contains(&node) {
                            resolved.push(node);
                        }
                    }
                    let mut names: Vec<String> = resolved
                        .iter()
                        .map(|n| loop_node_name(n, &bridge, &library, &root_gates))
                        .collect();
                    if names.is_empty() {
                        // The design changed before the error arrived.
                        names = vec![format!("{} node(s)", nodes.len())];
                    }
                    names.sort();
                    messages.push(loop_message(&names));
                    loops.push(resolved);
                }
                warn!("Revision {revision} did not compile: {errors:?}");
                diagnostics.compile_error = Some(CompileFailure {
                    revision,
                    messages,
                    loop_gates,
                    loops,
                });
            }
        }
    }
}

/// Ask the engine for this frame's copies of the screens' memories.
pub fn request_screens_system(bridge: Res<SimBridge>) {
    bridge.request_screens();
}

/// Run the in-frame engine (threadless runtime only).
pub fn drive_local_engine_system(bridge: Res<SimBridge>) {
    bridge.run_local(Duration::from_millis(8));
}

/// Copy the latest probe frame into [`GateValues`] and [`WireState`].
pub fn project_probes_system(
    bridge: Res<SimBridge>,
    mut last: Local<Option<(u64, u64, u64)>>,
    mut gates: Query<(&GateProbes, &mut GateValues)>,
    mut wires: Query<&mut WireState>,
    work: Option<Res<EntityWorkCounter>>,
) {
    let generation = bridge.binding_generation();
    bridge.with_frame(|frame| {
        let key = (frame.revision, frame.tick, generation);
        if *last == Some(key) {
            return;
        }
        *last = Some(key);
        EntityWorkCounter::add(&work, gates.iter().len() + wires.iter().len());
        let read = |p: &Option<u32>| {
            p.and_then(|i| frame.values.get(i as usize).copied())
                .unwrap_or(0)
        };
        for (probes, mut values) in &mut gates {
            let inputs = probes.inputs.iter().map(read);
            let outputs = probes.outputs.iter().map(read);
            if !values.inputs.iter().copied().eq(inputs.clone())
                || !values.outputs.iter().copied().eq(outputs.clone())
            {
                values.inputs = inputs.collect();
                values.outputs = outputs.collect();
            }
        }
        for mut state in &mut wires {
            let v = read(&state.probe);
            if state.value != v {
                state.value = v;
            }
        }
    });
}

/// Forward tick-rate changes to the engine.
pub fn update_tickrate_system(sim_config: Res<SimulationConfig>, mut bridge: ResMut<SimBridge>) {
    if !sim_config.is_changed() {
        return;
    }
    let rate = if sim_config.unlimited_tick_rate {
        None
    } else {
        Some(sim_config.tick_rate)
    };
    bridge.set_tick_rate(rate);
}
