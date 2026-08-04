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
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use uuid::Uuid;

use logi_sim::circuit::Circuit;
use logi_sim::engine::{RunnerType, SimCommand, SimEvent, SimulationEngine};
use logi_sim::prelude::{InputRef, NodeId, OutputRef, SharedState, WireId};

use super::model::{GameCircuit, GameNode, GameWire};

// ─────────────────────────────────────────────────────────────────────────────
// Inner mutable state (wrapped in Mutex so SimBridge is Sync)
//
// `SimulationEngine` contains `Option<Receiver<SimEvent>>` which is `!Sync`.
// By wrapping the engine + command channel in a `Mutex` we get a type that is
// both `Send` (Receiver<T>: Send if T: Send) and `Sync` (Mutex<T>: Sync if T: Send).
// ─────────────────────────────────────────────────────────────────────────────

struct SimInner {
    engine: SimulationEngine,
    command_tx: Option<Sender<SimCommand>>,
    event_rx: Option<Receiver<SimEvent>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SimRuntime {
    Threaded,
    Threadless,
}

/// Represents a pending `AddNode` command awaiting a `NodeId` response from the worker.
struct PendingNode {
    game_uuid: Uuid,
    response_rx: Receiver<(u64, NodeId)>,
}

/// Represents a pending `AddWire` command awaiting a `WireId` response from the worker.
struct PendingWire {
    game_uuid: Uuid,
    response_rx: Receiver<(u64, WireId)>,
}

// ─────────────────────────────────────────────────────────────────────────────
// SimBridge
// ─────────────────────────────────────────────────────────────────────────────

/// Bevy `Resource` that owns the simulation worker thread and the bidirectional
/// `Uuid ↔ NodeId / WireId` mapping tables.
///
/// All game systems interact with the simulator exclusively through this
/// resource – they never touch `logi-sim` types directly.
///
/// # Sync safety
/// The engine's internal `Receiver` is `!Sync`. We solve this by wrapping the
/// entire engine + channels in a `Mutex<SimInner>`. Access to the inner state
/// happens exclusively from Bevy's main thread via `ResMut<SimBridge>`.
#[derive(Resource)]
pub struct SimBridge {
    // ── Mutex-wrapped engine (makes the whole struct Sync) ─────────────────
    inner: Mutex<SimInner>,

    // ── Pending async responses (also !Sync, also wrapped) ─────────────────
    pending_nodes: Mutex<Vec<PendingNode>>,
    pending_wires: Mutex<Vec<PendingWire>>,

    // ── Bi-directional ID maps (HashMap is Send+Sync) ──────────────────────
    node_by_uuid: HashMap<Uuid, NodeId>,
    uuid_by_node: HashMap<NodeId, Uuid>,
    wire_by_uuid: HashMap<Uuid, WireId>,
    uuid_by_wire: HashMap<WireId, Uuid>,

    // ── Shared atomic state (Arc<RwLock<...>>, Send+Sync) ──────────────────
    shared_state: Arc<SharedState>,
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
        let uuid_by_wire: HashMap<WireId, Uuid> =
            wire_by_uuid.iter().map(|(u, w)| (*w, *u)).collect();

        let mut engine = match runtime {
            SimRuntime::Threaded => {
                let (event_tx, _event_rx) = mpsc::channel::<SimEvent>();
                let mut engine = SimulationEngine::new_with_event_channel(sim_circuit, event_tx);
                engine.start_worker();
                engine
            }
            SimRuntime::Threadless => {
                let engine = SimulationEngine::new(sim_circuit);
                engine
            }
        };

        let (command_tx, event_rx) = match runtime {
            SimRuntime::Threaded => (engine.command_sender(), engine.take_event_receiver()),
            SimRuntime::Threadless => (None, None),
        };
        let shared_state = Arc::clone(&engine.shared_state);

        Self {
            inner: Mutex::new(SimInner {
                engine,
                command_tx: command_tx.clone(),
                event_rx,
            }),
            pending_nodes: Mutex::new(Vec::new()),
            pending_wires: Mutex::new(Vec::new()),
            node_by_uuid,
            uuid_by_node,
            wire_by_uuid,
            uuid_by_wire,
            shared_state,
            runtime,
            next_temp_id: 0,
            is_running: true,
        }
    }

    // ─────────────────────────────────────────────────────────────────────
    // Private helpers
    // ─────────────────────────────────────────────────────────────────────

    /// Lock the inner state. Panics only if the mutex is poisoned (i.e. a
    /// previous panic occurred inside a lock – which should never happen in
    /// normal operation).
    fn inner(&self) -> std::sync::MutexGuard<'_, SimInner> {
        self.inner.lock().expect("SimBridge inner mutex poisoned")
    }

    pub fn runtime(&self) -> SimRuntime {
        self.runtime
    }

    fn send_cmd(&self, cmd: SimCommand) {
        let mut inner = self.inner();
        if let Some(tx) = &inner.command_tx {
            let _ = tx.send(cmd);
            return;
        }

        match cmd {
            SimCommand::SetRunner(runner_type) => {
                inner.engine.set_runner(runner_type);
            }
            SimCommand::SetInput { node, pin, value } => inner.engine.set_input(node, pin, value),
            SimCommand::SetTickRate(tick_rate) => inner.engine.set_tick_rate(tick_rate),
            SimCommand::AddNode {
                temp_id,
                kind,
                inputs,
                outputs,
                response_tx,
            } => {
                let node_id = inner.engine.add_node(kind, inputs, outputs);
                let _ = response_tx.send((temp_id, node_id));
            }
            SimCommand::AddWire {
                temp_id,
                source,
                sink,
                response_tx,
            } => {
                let wire_id = inner.engine.add_wire(source, sink);
                let _ = response_tx.send((temp_id, wire_id));
            }
            SimCommand::RemoveNode { node } => inner.engine.remove_node(node),
            SimCommand::RemoveWire { wire } => inner.engine.remove_wire(wire),
            SimCommand::RebuildCircuit {
                circuit,
                response_tx,
            } => {
                inner.engine.rebuild_circuit(circuit);
                let _ = response_tx.send(());
            }
            SimCommand::Pause => inner.engine.pause(),
            SimCommand::Resume => inner.engine.resume(),
            SimCommand::Step => inner.engine.step(),
            SimCommand::Stop => inner.engine.stop_worker(),
        }
    }

    // ─────────────────────────────────────────────────────────────────────
    // Circuit building helpers
    // ─────────────────────────────────────────────────────────────────────

    fn build_sim_circuit(
        game_circuit: &GameCircuit,
    ) -> (Circuit, HashMap<Uuid, NodeId>, HashMap<Uuid, WireId>) {
        let mut circuit = Circuit::new();
        let mut node_by_uuid: HashMap<Uuid, NodeId> = HashMap::new();

        let mut sorted_nodes: Vec<_> = game_circuit.nodes.values().collect();
        sorted_nodes.sort_by(|a, b| a.label.cmp(&b.label).then_with(|| a.id.cmp(&b.id)));

        for node in sorted_nodes {
            match &node.kind {
                super::model::GameNodeKind::Input | super::model::GameNodeKind::Output => {
                    assert!(
                        node.label.is_some(),
                        "Input and Output nodes must have a label set for mapping"
                    );
                }
                _ => {}
            }

            let sim_id = if let super::model::GameNodeKind::SubCircuit {
                circuit: sub_game_circuit,
            } = &node.kind
            {
                let (sub_sim_circuit, _, _) = Self::build_sim_circuit(sub_game_circuit);
                circuit.add_subcircuit(sub_sim_circuit)
            } else if let Some(kind) = node.kind.to_sim_kind() {
                circuit.add_node(kind, node.kind.input_count(), node.kind.output_count())
            } else {
                continue;
            };

            node_by_uuid.insert(node.id, sim_id);
        }

        let mut wire_by_uuid: HashMap<Uuid, WireId> = HashMap::new();
        for wire in game_circuit.wires.values() {
            let Some(&src_node) = node_by_uuid.get(&wire.source_node) else {
                continue;
            };
            let Some(&snk_node) = node_by_uuid.get(&wire.sink_node) else {
                continue;
            };
            let wire_id = circuit.connect((src_node, wire.source_pin), (snk_node, wire.sink_pin));
            wire_by_uuid.insert(wire.id, wire_id);
        }

        (circuit, node_by_uuid, wire_by_uuid)
    }

    // ─────────────────────────────────────────────────────────────────────
    // Circuit switching / reload
    // ─────────────────────────────────────────────────────────────────────

    /// Replace the current circuit. Stops the old worker, rebuilds from
    /// `game_circuit`, and restarts.
    pub fn load_circuit(&mut self, game_circuit: &GameCircuit) {
        {
            let mut inner = self.inner();
            inner.engine.stop_worker();
        }

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
        self.uuid_by_wire = self.wire_by_uuid.iter().map(|(u, w)| (*w, *u)).collect();

        match self.runtime {
            SimRuntime::Threaded => {
                let (event_tx, event_rx) = mpsc::channel::<SimEvent>();
                let mut engine = SimulationEngine::new_with_event_channel(sim_circuit, event_tx);
                engine.start_worker();
                let command_tx = engine
                    .command_sender()
                    .expect("worker should be running after start_worker");
                self.shared_state = Arc::clone(&engine.shared_state);
                *self.inner() = SimInner {
                    engine,
                    command_tx: Some(command_tx),
                    event_rx: Some(event_rx),
                };
            }
            SimRuntime::Threadless => {
                let engine = SimulationEngine::new(sim_circuit);
                self.shared_state = Arc::clone(&engine.shared_state);
                *self.inner() = SimInner {
                    engine,
                    command_tx: None,
                    event_rx: None,
                };
            }
        }
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

        let (resp_tx, resp_rx) = mpsc::channel::<(u64, NodeId)>();
        self.send_cmd(SimCommand::AddNode {
            temp_id,
            kind,
            inputs,
            outputs,
            response_tx: resp_tx,
        });
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

        let temp_id = self.next_temp_id;
        self.next_temp_id += 1;

        let (resp_tx, resp_rx) = mpsc::channel::<(u64, WireId)>();
        self.send_cmd(SimCommand::AddWire {
            temp_id,
            source: OutputRef {
                node: src_node,
                pin: wire.source_pin,
            },
            sink: InputRef {
                node: snk_node,
                pin: wire.sink_pin,
            },
            response_tx: resp_tx,
        });
        self.pending_wires.lock().unwrap().push(PendingWire {
            game_uuid: wire.id,
            response_rx: resp_rx,
        });

        Some(wire.id)
    }

    /// Remove a node (and its wires) by game UUID.
    pub fn remove_node(&mut self, uuid: Uuid) {
        if let Some(node_id) = self.node_by_uuid.remove(&uuid) {
            self.uuid_by_node.remove(&node_id);
            self.send_cmd(SimCommand::RemoveNode { node: node_id });
        }
    }

    /// Remove a wire by game UUID.
    pub fn remove_wire(&mut self, uuid: Uuid) {
        if let Some(wire_id) = self.wire_by_uuid.remove(&uuid) {
            self.uuid_by_wire.remove(&wire_id);
            self.send_cmd(SimCommand::RemoveWire { wire: wire_id });
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
            self.send_cmd(SimCommand::SetInput {
                node: node_id,
                pin,
                value,
            });
        }
    }

    // ─────────────────────────────────────────────────────────────────────
    // Simulation control
    // ─────────────────────────────────────────────────────────────────────

    pub fn pause(&mut self) {
        self.is_running = false;
        self.send_cmd(SimCommand::Pause);
    }

    pub fn resume(&mut self) {
        self.is_running = true;
        self.send_cmd(SimCommand::Resume);
    }

    pub fn step(&self) {
        self.send_cmd(SimCommand::Step);
    }

    /// Run multiple simulation ticks synchronously in threadless mode, or enqueue
    /// each tick as a command in threaded mode.
    pub fn step_many(&self, ticks: u64) {
        if self.runtime() == SimRuntime::Threadless {
            let mut inner = self.inner();
            for _ in 0..ticks {
                inner.engine.step();
            }
            return;
        }

        for _ in 0..ticks {
            self.send_cmd(SimCommand::Step);
        }
    }

    pub fn stop(&mut self) {
        self.is_running = false;
        self.inner().engine.stop_worker();
    }

    pub fn set_tick_rate(&self, tick_rate: Option<f64>) {
        self.send_cmd(SimCommand::SetTickRate(tick_rate));
    }

    pub fn set_runner_type(&mut self, runner_type: RunnerType) {
        self.inner().engine.set_runner(runner_type);
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
        self.shared_state.tick_count()
    }

    /// Snapshot the current state of every wire with a single `RwLock::read()`.
    ///
    /// Calls `f(wire_id, value)` for each wire in the shared state.  This is
    /// the intended way for [`crate::sim::SimCache`] to refresh itself once per
    /// Bevy frame — one lock acquisition regardless of how many simulation ticks
    /// ran since the last frame.
    pub fn snapshot_wire_states(&self, f: &mut impl FnMut(WireId, bool)) {
        if let Ok(lock) = self.shared_state.wire_states.read() {
            for (wire_id, &val) in lock.iter() {
                f(wire_id, val);
            }
        }
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

        let mut resolved_wires: Vec<(Uuid, WireId)> = Vec::new();
        self.pending_wires
            .lock()
            .unwrap()
            .retain(|p| match p.response_rx.try_recv() {
                Ok((_temp_id, wire_id)) => {
                    resolved_wires.push((p.game_uuid, wire_id));
                    false
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => true,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => false,
            });
        for (uuid, wire_id) in resolved_wires {
            self.wire_by_uuid.insert(uuid, wire_id);
            self.uuid_by_wire.insert(wire_id, uuid);
        }
    }

    /// Collect all `SimEvent`s produced by the worker since the last call.
    pub fn drain_events(&self) -> Vec<SimEvent> {
        let mut events = Vec::new();
        let inner = self.inner();
        let Some(event_rx) = &inner.event_rx else {
            return events;
        };
        while let Ok(evt) = event_rx.try_recv() {
            events.push(evt);
        }
        events
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
        self.wire_by_uuid.get(&uuid).copied()
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
        if let Ok(mut inner) = self.inner.lock() {
            inner.engine.stop_worker();
        }
    }
}
