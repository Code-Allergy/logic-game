//! `SimBridge` – The authoritative integration layer between the Bevy editor
//! and the `logi-sim` background simulation worker.
//!
//! ## Responsibilities
//!
//! - **Thread lifecycle**: Spawn, pause, resume, step, stop, and restart the
//!   `logi-sim` worker thread.
//! - **ID mapping**: Maintain bi-directional maps between stable game-side
//!   [`Uuid`]s and the runtime [`NodeId`]/[`WireId`] keys used by `logi-sim`.
//! - **Dynamic mutation**: Add/remove nodes and wires while the simulation is
//!   running without requiring a full restart.
//! - **Circuit switching**: Load a new [`GameCircuit`] (e.g. on file load or
//!   sub-circuit navigation) – tears down the old worker, rebuilds the sim
//!   circuit from the model, and restarts.
//! - **Event draining**: Provide [`SimBridge::drain_events`] so Bevy systems
//!   can read [`SimEvent`]s produced by the worker on each frame.

use std::collections::HashMap;
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use uuid::Uuid;

use super::service::{DirectSimService, SimExecutionService, SimRuntime, ThreadedSimService};
use logi_sim::circuit::Circuit;
use logi_sim::engine::{RunnerType, SimEvent};
use logi_sim::prelude::{InputRef, NodeId, OutputRef, WireId};

use super::model::{GameCircuit, GameNode, GameWire};

/// Represents a pending `AddNode` command awaiting a `NodeId` response from the worker.
struct PendingNode {
    game_uuid: Uuid,
    response_rx: Receiver<(u64, NodeId)>,
}

/// Represents a pending `AddWire` command awaiting a `WireId` response from the worker.
struct PendingWire {
    game_uuid: Uuid,
    response_rx: Receiver<(u64, WireId)>,
    source_pin: u8,
    sink_pin: u8,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct WireSegment {
    pub wire_id: WireId,
    pub source_pin: u8,
    pub sink_pin: u8,
}

// ─────────────────────────────────────────────────────────────────────────────
// SimBridge
// ─────────────────────────────────────────────────────────────────────────────

/// Bevy `Resource` that owns the simulation bridge layer and the bidirectional
/// `Uuid ↔ NodeId / WireId` mapping tables.
///
/// All game systems interact with the simulator exclusively through this
/// resource – they never touch `logi-sim` types directly.
///
/// # Sync safety
/// The bridge wraps the execution service in an `Arc`, while each service
/// implementation handles its own internal synchronization.
#[derive(Resource)]
pub struct SimBridge {
    // ── Execution service (threaded or direct) ────────────────────────────
    service: Arc<dyn SimExecutionService>,

    // ── Pending async responses (also !Sync, also wrapped) ─────────────────
    pending_nodes: Mutex<Vec<PendingNode>>,
    pending_wires: Mutex<Vec<PendingWire>>,

    // ── Bi-directional ID maps (HashMap is Send+Sync) ──────────────────────
    node_by_uuid: HashMap<Uuid, NodeId>,
    uuid_by_node: HashMap<NodeId, Uuid>,
    wire_by_uuid: HashMap<Uuid, Vec<WireSegment>>,
    uuid_by_wire: HashMap<WireId, Uuid>,

    runtime: SimRuntime,

    // ── Metadata ───────────────────────────────────────────────────────────
    next_temp_id: u64,
    pub is_running: bool,
}

impl SimBridge {
    // ─────────────────────────────────────────────────────────────────────
    // Construction
    // ─────────────────────────────────────────────────────────────────────

    /// Build a `SimBridge` from a [`GameCircuit`] using the platform default
    /// runtime (threaded on native targets, threadless on wasm).
    pub fn from_circuit(game_circuit: &GameCircuit) -> Self {
        #[cfg(not(target_arch = "wasm32"))]
        let runtime = SimRuntime::Threaded;
        #[cfg(target_arch = "wasm32")]
        let runtime = SimRuntime::Threadless;
        Self::from_circuit_with_runtime(game_circuit, runtime)
    }

    pub fn from_circuit_threaded(game_circuit: &GameCircuit) -> Self {
        Self::from_circuit_with_runtime(game_circuit, SimRuntime::Threaded)
    }

    pub fn from_circuit_threadless(game_circuit: &GameCircuit) -> Self {
        Self::from_circuit_with_runtime(game_circuit, SimRuntime::Threadless)
    }

    pub fn from_circuit_with_runtime(game_circuit: &GameCircuit, runtime: SimRuntime) -> Self {
        let (sim_circuit, node_by_uuid, wire_by_uuid) = Self::build_sim_circuit(game_circuit);
        let uuid_by_node: HashMap<NodeId, Uuid> =
            node_by_uuid.iter().map(|(u, n)| (*n, *u)).collect();
        let mut uuid_by_wire = HashMap::new();
        for (game_wire_id, segments) in &wire_by_uuid {
            for segment in segments {
                uuid_by_wire.insert(segment.wire_id, *game_wire_id);
            }
        }

        let service: Arc<dyn SimExecutionService> = match runtime {
            SimRuntime::Threaded => Arc::new(ThreadedSimService::new(sim_circuit)),
            SimRuntime::Threadless => Arc::new(DirectSimService::new(sim_circuit)),
        };

        Self {
            service,
            pending_nodes: Mutex::new(Vec::new()),
            pending_wires: Mutex::new(Vec::new()),
            node_by_uuid,
            uuid_by_node,
            wire_by_uuid,
            uuid_by_wire,
            runtime,
            next_temp_id: 0,
            is_running: true,
        }
    }

    pub fn runtime(&self) -> SimRuntime {
        self.runtime
    }

    /// Whether this bridge is currently stepping/ticking.
    #[inline]
    pub fn is_running(&self) -> bool {
        self.is_running
    }

    // ─────────────────────────────────────────────────────────────────────
    // Circuit building helpers
    // ─────────────────────────────────────────────────────────────────────

    fn build_sim_circuit(
        game_circuit: &GameCircuit,
    ) -> (
        Circuit,
        HashMap<Uuid, NodeId>,
        HashMap<Uuid, Vec<WireSegment>>,
    ) {
        Self::build_sim_circuit_internal(game_circuit, true)
    }

    fn build_sim_circuit_internal(
        game_circuit: &GameCircuit,
        include_displays: bool,
    ) -> (
        Circuit,
        HashMap<Uuid, NodeId>,
        HashMap<Uuid, Vec<WireSegment>>,
    ) {
        let mut circuit = Circuit::new();
        let mut node_by_uuid: HashMap<Uuid, NodeId> = HashMap::new();

        let mut sorted_nodes: Vec<_> = game_circuit.nodes.values().collect();
        sorted_nodes.sort_by(|a, b| {
            let a_label = Self::input_output_display_name(a);
            let b_label = Self::input_output_display_name(b);
            a_label.cmp(&b_label).then_with(|| a.id.cmp(&b.id))
        });

        for node in sorted_nodes {
            if matches!(
                node.kind,
                super::model::GameNodeKind::Input
                    | super::model::GameNodeKind::Input8
                    | super::model::GameNodeKind::Input64
                    | super::model::GameNodeKind::Output
                    | super::model::GameNodeKind::Output8
                    | super::model::GameNodeKind::Output64
                    | super::model::GameNodeKind::Display { .. }
            ) && (node.label.as_deref().is_none_or(str::is_empty))
            {
                warn!(
                    "IO node {:?} ({:?}) has no label. This can make diagnostics ambiguous during vector wiring.",
                    node.id,
                    node.kind
                );
            }

            let sim_id = if let super::model::GameNodeKind::SubCircuit {
                circuit: sub_game_circuit,
            } = &node.kind
            {
                let (sub_sim_circuit, _, _) =
                    Self::build_sim_circuit_internal(sub_game_circuit, false);
                circuit.add_subcircuit(sub_sim_circuit)
            } else if let Some(kind) = node.kind.to_sim_kind() {
                if !include_displays
                    && matches!(node.kind, super::model::GameNodeKind::Display { .. })
                {
                    continue;
                }
                circuit.add_node(kind, node.kind.input_count(), node.kind.output_count())
            } else {
                continue;
            };

            node_by_uuid.insert(node.id, sim_id);
        }

        let mut wire_by_uuid: HashMap<Uuid, Vec<WireSegment>> = HashMap::new();
        for wire in game_circuit.wires.values() {
            let Some(&src_node) = node_by_uuid.get(&wire.source_node) else {
                continue;
            };
            let Some(&snk_node) = node_by_uuid.get(&wire.sink_node) else {
                continue;
            };
            let src_outputs = circuit.nodes[src_node].outputs.len();
            let snk_inputs = circuit.nodes[snk_node].inputs.len();
            let mut segments = Vec::new();
            let source_node_label = game_circuit
                .nodes
                .get(&wire.source_node)
                .map_or_else(
                    || Self::anonymous_node_desc(wire.source_node),
                    |node| Self::describe_named_node(node),
                );
            let sink_node_label = game_circuit
                .nodes
                .get(&wire.sink_node)
                .map_or_else(
                    || Self::anonymous_node_desc(wire.sink_node),
                    |node| Self::describe_named_node(node),
                );
            let source_kind = game_circuit
                .nodes
                .get(&wire.source_node)
                .map(|node| format!("{:?}", node.kind));
            let sink_kind = game_circuit
                .nodes
                .get(&wire.sink_node)
                .map(|node| format!("{:?}", node.kind));
            let source_width = Self::width_from_game_node(
                game_circuit,
                wire.source_node,
                true, /* is_source */
            );
            let sink_width = Self::width_from_game_node(
                game_circuit,
                wire.sink_node,
                false, /* is_source */
            );

            let source_width_u8 = source_width.min(u8::MAX as usize) as u8;
            let sink_width_u8 = sink_width.min(u8::MAX as usize) as u8;
            if !wire.kind.fits_between(source_width_u8, sink_width_u8) {
                warn!(
                    "Invalid wire {:?}: kind {} from {source_node_label}({}) -> {sink_node_label}({}) ; src width {source_width}, sink width {sink_width}, but bit width {}",
                    wire.id,
                    wire.kind.display_name(),
                    source_kind.as_deref().unwrap_or("unknown"),
                    sink_kind.as_deref().unwrap_or("unknown"),
                    wire.kind.bit_width()
                );
                continue;
            }

            let wire_width = match wire.kind {
                super::model::WireKind::Scalar => 1usize,
                super::model::WireKind::Vector { bits } => bits.max(1).into(),
            };

            for bit in 0..wire_width {
                let source_pin = match wire.source_pin.checked_add(bit as u8) {
                    Some(pin) => pin as usize,
                    None => {
                        warn!(
                            "Skipping vector wire {:?}: source pin overflow at offset {}",
                            wire.id, bit
                        );
                        break;
                    }
                };
                let sink_pin = match wire.sink_pin.checked_add(bit as u8) {
                    Some(pin) => pin as usize,
                    None => {
                        warn!(
                            "Skipping vector wire {:?}: sink pin overflow at offset {}",
                            wire.id, bit
                        );
                        break;
                    }
                };
                if source_pin >= src_outputs {
                    warn!(
                        "Skipping vector wire {:?}: source node {source_node_label} ({}) has {} output pins, attempted {} (bit {})",
                        wire.id,
                        source_kind.as_deref().unwrap_or("unknown"),
                        src_outputs,
                        source_pin,
                        bit
                    );
                    continue;
                }
                if sink_pin >= snk_inputs {
                    warn!(
                        "Skipping vector wire {:?}: sink node {sink_node_label} ({}) has {} input pins, attempted {} (bit {})",
                        wire.id,
                        sink_kind.as_deref().unwrap_or("unknown"),
                        snk_inputs,
                        sink_pin,
                        bit
                    );
                    continue;
                }

                let wire_id =
                    circuit.connect((src_node, source_pin as u8), (snk_node, sink_pin as u8));
                segments.push(WireSegment {
                    wire_id,
                    source_pin: source_pin as u8,
                    sink_pin: sink_pin as u8,
                });
            }

            if !segments.is_empty() {
                wire_by_uuid.insert(wire.id, segments);
            }
        }

        (circuit, node_by_uuid, wire_by_uuid)
    }

    fn anonymous_node_desc(node_id: Uuid) -> String {
        format!("<unlabeled {}>", node_id)
    }

    fn describe_named_node(node: &GameNode) -> String {
        node.label.clone().unwrap_or_else(|| {
            format!(
                "{}-{}",
                Self::input_output_display_name(node),
                node.id.as_simple()
            )
        })
    }

    fn width_from_game_node(
        game_circuit: &GameCircuit,
        node_id: Uuid,
        is_source: bool,
    ) -> usize {
        game_circuit
            .nodes
            .get(&node_id)
            .map(|node| {
                if is_source {
                    node.kind.output_count()
                } else {
                    node.kind.input_count()
                }
            })
            .unwrap_or(0)
    }

    fn input_output_display_name(node: &GameNode) -> String {
        if let Some(label) = &node.label {
            return label.clone();
        }

        let prefix = match node.kind {
            super::model::GameNodeKind::Input => "Input",
            super::model::GameNodeKind::Input8 => "Input8",
            super::model::GameNodeKind::Input64 => "Input64",
            super::model::GameNodeKind::Output => "Output",
            super::model::GameNodeKind::Output8 => "Output8",
            super::model::GameNodeKind::Output64 => "Output64",
            _ => "Node",
        };
        format!("{prefix}-{}", node.id)
    }

    // ─────────────────────────────────────────────────────────────────────
    // Circuit switching / reload
    // ─────────────────────────────────────────────────────────────────────

    /// Replace the current circuit. Stops the old worker, rebuilds from
    /// `game_circuit`, and restarts.
    pub fn load_circuit(&mut self, game_circuit: &GameCircuit) {
        self.service.stop();

        self.node_by_uuid.clear();
        self.uuid_by_node.clear();
        self.wire_by_uuid.clear();
        self.uuid_by_wire.clear();
        self.pending_nodes.lock().unwrap().clear();
        self.pending_wires.lock().unwrap().clear();

        let (sim_circuit, node_by_uuid, wire_by_uuid) = Self::build_sim_circuit(game_circuit);
        self.node_by_uuid = node_by_uuid;
        self.uuid_by_node = self.node_by_uuid.iter().map(|(u, n)| (*n, *u)).collect();
        self.wire_by_uuid = wire_by_uuid;
        self.uuid_by_wire.clear();
        for (game_wire_id, segments) in &self.wire_by_uuid {
            for segment in segments {
                self.uuid_by_wire.insert(segment.wire_id, *game_wire_id);
            }
        }

        self.service = match self.runtime {
            SimRuntime::Threaded => Arc::new(ThreadedSimService::new(sim_circuit)),
            SimRuntime::Threadless => Arc::new(DirectSimService::new(sim_circuit)),
        };
        self.is_running = true;
    }

    // ─────────────────────────────────────────────────────────────────────
    // Dynamic node / wire mutation
    // ─────────────────────────────────────────────────────────────────────

    /// Add a new node to the running simulation. The [`NodeId`] mapping is
    /// resolved asynchronously via [`flush_pending`].
    pub fn add_node(&mut self, node: &GameNode) -> Uuid {
        let Some(kind) = node.kind.to_sim_kind() else {
            return node.id;
        };
        let inputs = node.kind.input_count();
        let outputs = node.kind.output_count();

        let temp_id = self.next_temp_id;
        self.next_temp_id += 1;

        let resp_rx = self.service.add_node(temp_id, kind, inputs, outputs);
        self.pending_nodes.lock().unwrap().push(PendingNode {
            game_uuid: node.id,
            response_rx: resp_rx,
        });

        node.id
    }

    /// Add a wire between two already-registered nodes.
    pub fn add_wire(&mut self, wire: &GameWire) -> Option<Uuid> {
        let &src_node = self.node_by_uuid.get(&wire.source_node)?;
        let &snk_node = self.node_by_uuid.get(&wire.sink_node)?;
        let mut bit_index = 0_usize;
        let wire_width = match wire.kind {
            super::model::WireKind::Scalar => 1usize,
            super::model::WireKind::Vector { bits } => bits.max(1).into(),
        };

        for bit in 0..wire_width {
            let source_pin = match wire.source_pin.checked_add(bit as u8) {
                Some(pin) => pin as usize,
                None => {
                    warn!(
                        "Skipping vector wire {:?}: source pin overflow at offset {}",
                        wire.id, bit
                    );
                    break;
                }
            };
            let sink_pin = match wire.sink_pin.checked_add(bit as u8) {
                Some(pin) => pin as usize,
                None => {
                    warn!(
                        "Skipping vector wire {:?}: sink pin overflow at offset {}",
                        wire.id, bit
                    );
                    break;
                }
            };
            let temp_id = self.next_temp_id;
            self.next_temp_id += 1;

            let resp_rx = self.service.add_wire(
                temp_id,
                OutputRef {
                    node: src_node,
                    pin: source_pin as u8,
                },
                InputRef {
                    node: snk_node,
                    pin: sink_pin as u8,
                },
            );
            self.pending_wires.lock().unwrap().push(PendingWire {
                game_uuid: wire.id,
                response_rx: resp_rx,
                source_pin: source_pin as u8,
                sink_pin: sink_pin as u8,
            });
            bit_index += 1;
        }

        if bit_index == 0 { None } else { Some(wire.id) }
    }

    /// Remove a node (and its wires) by game UUID.
    pub fn remove_node(&mut self, uuid: Uuid) {
        if let Some(node_id) = self.node_by_uuid.remove(&uuid) {
            self.uuid_by_node.remove(&node_id);
            self.service.remove_node(node_id);
        }
    }

    /// Remove a wire by game UUID.
    pub fn remove_wire(&mut self, uuid: Uuid) {
        if let Some(segments) = self.wire_by_uuid.remove(&uuid) {
            for segment in segments {
                self.uuid_by_wire.remove(&segment.wire_id);
                self.service.remove_wire(segment.wire_id);
            }
        }
    }

    // ─────────────────────────────────────────────────────────────────────
    // Input control
    // ─────────────────────────────────────────────────────────────────────

    pub fn set_input(&self, node_uuid: Uuid, pin: u8, value: bool) {
        if let Some(&node_id) = self.node_by_uuid.get(&node_uuid) {
            info!(
                "Setting input for node {:?} pin {} to {}",
                node_id, pin, value
            );
            self.service.set_input(node_id, pin, value);
        }
    }

    // ─────────────────────────────────────────────────────────────────────
    // Simulation control
    // ─────────────────────────────────────────────────────────────────────

    pub fn pause(&mut self) {
        self.is_running = false;
        self.service.pause();
    }

    pub fn resume(&mut self) {
        self.is_running = true;
        self.service.resume();
    }

    pub fn step(&self) {
        self.service.tick();
    }

    /// Run multiple simulation ticks.
    pub fn step_many(&self, ticks: u64) {
        self.service.step_many(ticks);
    }

    pub fn stop(&mut self) {
        self.is_running = false;
        self.service.stop();
    }

    pub fn set_tick_rate(&self, tick_rate: Option<f64>) {
        self.service.set_tick_rate(tick_rate);
    }

    pub fn set_runner_type(&mut self, runner_type: RunnerType) {
        self.service.set_runner_type(runner_type);
    }

    // ─────────────────────────────────────────────────────────────────────
    // Per-frame atomic reads
    // ─────────────────────────────────────────────────────────────────────

    /// Read the total number of completed simulation ticks.
    ///
    /// This is a single [`AtomicU64::load`] (lock-free, no IPC) and is cheap
    /// to call every frame.  Wire / pin state should be read from
    /// [`crate::sim::SimCache`] instead.
    pub fn tick_count(&self) -> u64 {
        self.service.tick_count()
    }

    /// Snapshot the current state of every wire with a single `RwLock::read()`.
    ///
    /// Calls `f(wire_id, value)` for each wire in the shared state.  This is
    /// the intended way for [`crate::sim::SimCache`] to refresh itself once per
    /// Bevy frame — one lock acquisition regardless of how many simulation ticks
    /// ran since the last frame.
    pub fn snapshot_wire_states(&self, f: &mut impl FnMut(WireId, bool)) {
        self.service.snapshot_wire_states(f);
    }

    // ─────────────────────────────────────────────────────────────────────
    // Per-frame update (called from PreUpdate by bridge_flush_system)
    // ─────────────────────────────────────────────────────────────────────

    /// Drain pending `AddNode`/`AddWire` responses and populate the ID maps.
    pub fn flush_pending(&mut self) {
        let mut resolved_nodes: Vec<(Uuid, NodeId)> = Vec::new();
        self.pending_nodes
            .lock()
            .unwrap()
            .retain(|p| match p.response_rx.try_recv() {
                Ok((_temp_id, node_id)) => {
                    resolved_nodes.push((p.game_uuid, node_id));
                    false
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => true,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => false,
            });
        for (uuid, node_id) in resolved_nodes {
            self.node_by_uuid.insert(uuid, node_id);
            self.uuid_by_node.insert(node_id, uuid);
        }

        let mut resolved_wires: Vec<(Uuid, WireId, u8, u8)> = Vec::new();
        self.pending_wires
            .lock()
            .unwrap()
            .retain(|p| match p.response_rx.try_recv() {
                Ok((_temp_id, wire_id)) => {
                    resolved_wires.push((p.game_uuid, wire_id, p.source_pin, p.sink_pin));
                    false
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => true,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => false,
            });
        for (uuid, wire_id, source_pin, sink_pin) in resolved_wires {
            self.wire_by_uuid
                .entry(uuid)
                .or_default()
                .push(WireSegment {
                    wire_id,
                    source_pin,
                    sink_pin,
                });
            self.uuid_by_wire.insert(wire_id, uuid);
        }
    }

    /// Collect all `SimEvent`s produced by the worker since the last call.
    pub fn drain_events(&self) -> Vec<SimEvent> {
        self.service.drain_events()
    }

    // ─────────────────────────────────────────────────────────────────────
    // ID lookup helpers
    // ─────────────────────────────────────────────────────────────────────

    pub fn node_id(&self, uuid: Uuid) -> Option<NodeId> {
        self.node_by_uuid.get(&uuid).copied()
    }

    pub fn node_uuid(&self, id: NodeId) -> Option<Uuid> {
        self.uuid_by_node.get(&id).copied()
    }

    pub fn wire_id(&self, uuid: Uuid) -> Option<WireId> {
        self.wire_by_uuid
            .get(&uuid)
            .and_then(|segments| segments.first())
            .map(|segment| segment.wire_id)
    }

    pub(crate) fn wire_segments(&self, uuid: Uuid) -> Option<&[WireSegment]> {
        self.wire_by_uuid
            .get(&uuid)
            .map(|segments| segments.as_slice())
    }

    pub fn wire_uuid(&self, id: WireId) -> Option<Uuid> {
        self.uuid_by_wire.get(&id).copied()
    }

    pub fn node_ready(&self, uuid: Uuid) -> bool {
        self.node_by_uuid.contains_key(&uuid)
    }

    pub fn wire_ready(&self, uuid: Uuid) -> bool {
        self.wire_by_uuid.contains_key(&uuid)
    }
}

impl Drop for SimBridge {
    fn drop(&mut self) {
        self.service.stop();
    }
}
