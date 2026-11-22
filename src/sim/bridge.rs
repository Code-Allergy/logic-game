//! `SimBridge`: maps the ECS authoring components onto a `logi_sim` v2
//! [`Design`] and drives the v2 engine.
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
//! ## Readback
//!
//! The bridge registers one probe per output net of every gate in the open
//! circuit. Probe indices are written to [`GateProbes`] and
//! [`WireState::probe`]; [`project_probes_system`] copies the latest probe
//! frame into [`GateValues`] and [`WireState::value`].

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::time::Duration;

use bevy::platform::time::Instant;
use bevy::prelude::*;
use logi_sim::compile::{CompileError, Options};
#[cfg(not(target_arch = "wasm32"))]
use logi_sim::engine_v2::Engine;
use logi_sim::engine_v2::{EngineEvent, LocalEngine, Probe, ProbeFrame, Rate};
use logi_sim::netlist::{DefId, Design, NodeId, PinRef};
use uuid::Uuid;

use super::document::CircuitLibrary;
use super::{
    GateIdentity, GateKind, GateProbes, GateType, GateValues, InputDrive, SimulationConfig,
    WireEndpoints, WireState,
};

/// Which engine runs the simulation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SimRuntime {
    /// `logi_sim::engine_v2::Engine` on a worker thread (native default).
    Threaded,
    /// `logi_sim::engine_v2::LocalEngine` run in-frame (wasm default).
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
    Local { engine: Box<LocalEngine>, origin: Instant },
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
    defs: HashMap<Uuid, DefId>,
    probe_index: HashMap<(NodeId, u8), u32>,
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
            SimRuntime::Threaded => Runtime::Threaded(Engine::spawn(Options::debug())),
            #[cfg(target_arch = "wasm32")]
            SimRuntime::Threaded => Runtime::Local {
                engine: Box::new(LocalEngine::new(Options::debug())),
                origin: Instant::now(),
            },
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
            defs: HashMap::new(),
            probe_index: HashMap::new(),
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
                    == self.design.root_def().output_net(entry.source.0, entry.source.1)
            {
                let _ = self.design.disconnect(root, entry.sink);
            }
            self.dirty = true;
        }
    }

    pub fn remove_gate(&mut self, entity: Entity) {
        let Some(entry) = self.gates.remove(&entity) else { return };
        self.node_by_uuid.remove(&entry.uuid);
        if let Some(node) = entry.node {
            self.node_entity.remove(&node);
            let root = self.root();
            let _ = self.design.remove_node(root, node);
            self.dirty = true;
            self.probes_dirty = true;
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
            GateKind::SubCircuit { definition } => {
                let mut visiting = HashSet::new();
                self.ensure_definition(*definition, library, &mut visiting)
                    .and_then(|def| match self.design.add_instance(root, def) {
                        Ok(n) => Some(n),
                        Err(e) => {
                            warn!("Cannot instance subcircuit {definition}: {e}");
                            None
                        }
                    })
            }
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
        self.gates.insert(entity, GateEntry { uuid, kind: Some(kind.clone()), node });
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
        self.wires.insert(entity, WireEntry { source: (source, ep.source_pin), sink: (sink, ep.sink_pin) });
        let net = self.design.root_def().output_net(source, ep.source_pin).expect("connected");
        Ok(self.design.root_def().nets[net].width)
    }

    /// Drop every definition built from the library; returns the gates whose
    /// instance nodes were removed (they must be re-added).
    pub fn invalidate_definitions(&mut self) -> Vec<Entity> {
        let subcircuits: Vec<Entity> = self
            .gates
            .iter()
            .filter(|(_, g)| matches!(g.kind, Some(GateKind::SubCircuit { .. })))
            .map(|(e, _)| *e)
            .collect();
        for &e in &subcircuits {
            if let Some(entry) = self.gates.get(&e) {
                let uuid = entry.uuid;
                self.remove_gate(e);
                self.gates.insert(e, GateEntry { uuid, kind: None, node: None });
            }
        }
        // Remove definitions; parents first, so retry until none is in use.
        let mut remaining: Vec<DefId> = self.defs.drain().map(|(_, d)| d).collect();
        while !remaining.is_empty() {
            let before = remaining.len();
            remaining.retain(|&d| self.design.remove_definition(d).is_err());
            if remaining.len() == before {
                break;
            }
        }
        self.dirty = true;
        subcircuits
    }

    fn ensure_definition(
        &mut self,
        id: Uuid,
        library: &CircuitLibrary,
        visiting: &mut HashSet<Uuid>,
    ) -> Option<DefId> {
        if let Some(&def) = self.defs.get(&id) {
            return Some(def);
        }
        let Some(doc) = library.get(id).cloned() else {
            warn!("Subcircuit definition {id} is missing from the library");
            return None;
        };
        if !visiting.insert(id) {
            warn!("Subcircuit definition {} ({id}) contains itself", doc.name);
            return None;
        }
        let def = self.design.add_definition(doc.name.clone());
        let mut nodes: HashMap<Uuid, NodeId> = HashMap::new();
        let (inputs, outputs) = doc.interface();
        let ports: Vec<_> = inputs.iter().chain(outputs.iter()).map(|n| n.id).collect();
        for n in inputs.iter().chain(outputs.iter()) {
            if let Some(k) = n.kind.to_netlist()
                && let Ok(node) = self.design.add_node(def, k)
            {
                let _ = self.design.set_name(def, node, n.label.clone());
                nodes.insert(n.id, node);
            }
        }
        for n in &doc.nodes {
            if ports.contains(&n.id) || matches!(n.kind, GateKind::Display { .. }) {
                continue;
            }
            let node = match &n.kind {
                GateKind::SubCircuit { definition } => self
                    .ensure_definition(*definition, library, visiting)
                    .and_then(|child| self.design.add_instance(def, child).ok()),
                k => k.to_netlist().and_then(|nk| self.design.add_node(def, nk).ok()),
            };
            match node {
                Some(node) => {
                    nodes.insert(n.id, node);
                }
                None => warn!("Subcircuit {}: cannot build node {}", doc.name, n.id),
            }
        }
        for w in &doc.wires {
            let (Some(&src), Some(&dst)) = (nodes.get(&w.from.node), nodes.get(&w.to.node)) else {
                continue;
            };
            if let Err(e) = self.design.connect_pins(def, (src, w.from.pin), (dst, w.to.pin)) {
                warn!("Subcircuit {}: cannot connect wire {}: {e}", doc.name, w.id);
            }
        }
        visiting.remove(&id);
        self.defs.insert(id, def);
        Some(def)
    }

    // ── Sending revisions ────────────────────────────────────────────────

    /// Send the design to the engine if it changed. Returns whether it did.
    pub fn flush(&mut self) -> bool {
        if !self.dirty {
            return false;
        }
        self.dirty = false;
        if self.probes_dirty {
            self.probes_dirty = false;
            self.rebuild_probes();
        }
        self.revision += 1;
        self.binding_generation += 1;
        let revision = self.revision;
        let design = &self.design;
        self.with_runtime(|rt| match rt {
            #[cfg(not(target_arch = "wasm32"))]
            Runtime::Threaded(engine) => engine.load(std::sync::Arc::new(design.clone()), revision),
            Runtime::Local { engine, .. } => engine.load(design, revision),
        });
        true
    }

    fn rebuild_probes(&mut self) {
        self.probe_index.clear();
        let mut probes = Vec::new();
        let root = self.design.root_def();
        let mut nodes: Vec<NodeId> = self.node_entity.keys().copied().collect();
        nodes.sort();
        for node in nodes {
            let Some(n) = root.nodes.get(node) else { continue };
            for (pin, &net) in n.outputs.iter().enumerate() {
                self.probe_index.insert((node, pin as u8), probes.len() as u32);
                probes.push(Probe { path: Vec::new(), net });
            }
        }
        self.with_runtime(|rt| match rt {
            #[cfg(not(target_arch = "wasm32"))]
            Runtime::Threaded(engine) => {
                engine.set_probes(probes);
                // Frames read after this use the new probe layout.
                engine.sync();
            }
            Runtime::Local { engine, .. } => engine.core.set_probes(probes),
        });
    }

    /// Probe indices for a gate's input and output pins.
    pub fn gate_probes(&self, entity: Entity) -> GateProbes {
        let Some(node) = self.node_of(entity) else { return GateProbes::default() };
        let root = self.design.root_def();
        let Some(n) = root.nodes.get(node) else { return GateProbes::default() };
        let inputs = n
            .inputs
            .iter()
            .map(|net| {
                let driver = root.nets.get((*net)?)?.driver;
                self.probe_index.get(&driver).copied()
            })
            .collect();
        let outputs =
            (0..n.outputs.len()).map(|pin| self.probe_index.get(&(node, pin as u8)).copied()).collect();
        GateProbes { inputs, outputs }
    }

    /// Probe index for a wire's net.
    pub fn wire_probe(&self, entity: Entity) -> Option<u32> {
        let entry = self.wires.get(&entity)?;
        self.probe_index.get(&entry.source).copied()
    }

    // ── Engine control ───────────────────────────────────────────────────

    pub fn set_input(&self, entity: Entity, value: u64) {
        let Some(node) = self.node_of(entity) else { return };
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

    /// Queue `n` ticks, run even while paused.
    pub fn step(&self, n: u64) {
        self.with_runtime(|rt| match rt {
            #[cfg(not(target_arch = "wasm32"))]
            Runtime::Threaded(engine) => engine.step(n),
            Runtime::Local { engine, .. } => engine.core.step(n),
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
                EngineEvent::Loaded { revision, .. } | EngineEvent::CompileFailed { revision, .. } => {
                    *revision
                }
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

/// Apply ECS authoring changes to the bridge's design and send a new
/// revision when anything changed. Runs every frame in `PostUpdate`, after
/// the authoring commands of the frame have been applied.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub fn sync_design_system(
    mut bridge: ResMut<SimBridge>,
    library: Res<CircuitLibrary>,
    mut removed_gates: RemovedComponents<GateType>,
    mut removed_wires: RemovedComponents<WireEndpoints>,
    changed_gates: Query<
        (Entity, &GateIdentity, Ref<GateType>, Option<Ref<InputDrive>>),
        Or<(Changed<GateType>, Changed<InputDrive>)>,
    >,
    all_gates: Query<(Entity, &GateIdentity, &GateType, Option<&InputDrive>)>,
    changed_wires: Query<(), Changed<WireEndpoints>>,
    mut wires: Query<(Entity, Ref<WireEndpoints>, &mut WireState)>,
    mut gate_probes: Query<(Entity, &mut GateProbes)>,
) {
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
        for entity in bridge.invalidate_definitions() {
            if let Ok((e, id, ty, drive)) = all_gates.get(entity)
                && add(&mut bridge, e, id, ty, drive)
            {
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

    if bridge.flush() {
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

/// Collect load results into [`SimDiagnostics`].
pub fn collect_engine_events_system(
    mut bridge: ResMut<SimBridge>,
    mut diagnostics: ResMut<SimDiagnostics>,
) {
    bridge.poll_events();
    for event in bridge.take_events() {
        match event {
            EngineEvent::Loaded { revision, .. } => {
                diagnostics.running_revision = Some(revision);
                if diagnostics.compile_error.as_ref().is_some_and(|e| e.revision <= revision) {
                    diagnostics.compile_error = None;
                }
            }
            EngineEvent::CompileFailed { revision, errors } => {
                let mut loop_gates = Vec::new();
                for error in &errors {
                    if let CompileError::CombinationalLoop { nodes } = error {
                        for path in nodes {
                            let root_node = path.instances.first().copied().unwrap_or(path.node);
                            if let Some(e) = bridge.entity_of(root_node)
                                && !loop_gates.contains(&e)
                            {
                                loop_gates.push(e);
                            }
                        }
                    }
                }
                let messages = errors
                    .iter()
                    .map(|e| match e {
                        CompileError::CombinationalLoop { nodes } => format!(
                            "Combinational loop through {} gate(s). Break it with a Delay or Register.",
                            nodes.len()
                        ),
                        CompileError::Invalid(v) => format!("Invalid circuit: {v:?}"),
                    })
                    .collect();
                warn!("Revision {revision} did not compile: {errors:?}");
                diagnostics.compile_error = Some(CompileFailure { revision, messages, loop_gates });
            }
        }
    }
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
) {
    let generation = bridge.binding_generation();
    bridge.with_frame(|frame| {
        let key = (frame.revision, frame.tick, generation);
        if *last == Some(key) {
            return;
        }
        *last = Some(key);
        let read = |p: &Option<u32>| p.and_then(|i| frame.values.get(i as usize).copied()).unwrap_or(0);
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
    let rate = if sim_config.unlimited_tick_rate { None } else { Some(sim_config.tick_rate) };
    bridge.set_tick_rate(rate);
}
