pub mod atomic_state;

pub use atomic_state::SharedState;

use crate::circuit::{Circuit, InputRef, NodeId, NodeKind, OutputRef, WireId};
use crate::runner::{InspectableRunner, NaiveRunner};

use std::cell::RefCell;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{Receiver, Sender, channel};
#[cfg(not(target_arch = "wasm32"))]
use std::thread::{JoinHandle, spawn};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RunnerType {
    Naive,
    NaiveOptimized,
    Flat,
    #[default]
    FlatOptimized,
    ThreadedFlat,
    EventDriven,
    EventDrivenOptimized,
    Simd,
    SimdOptimized,
    ThreadedFlatOptimized,
}

impl RunnerType {
    /// All runner types exposed on the current compilation target.
    #[cfg(not(target_arch = "wasm32"))]
    pub const ALL: &'static [RunnerType] = &[
        RunnerType::Naive,
        RunnerType::NaiveOptimized,
        RunnerType::Flat,
        RunnerType::FlatOptimized,
        RunnerType::ThreadedFlat,
        RunnerType::EventDriven,
        RunnerType::EventDrivenOptimized,
        RunnerType::Simd,
        RunnerType::SimdOptimized,
        RunnerType::ThreadedFlatOptimized,
    ];

    #[cfg(target_arch = "wasm32")]
    pub const ALL: &'static [RunnerType] = &[
        RunnerType::Naive,
        RunnerType::NaiveOptimized,
        RunnerType::Flat,
        RunnerType::FlatOptimized,
        RunnerType::EventDriven,
        RunnerType::EventDrivenOptimized,
    ];
}

impl std::fmt::Display for RunnerType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            RunnerType::Naive => "Naive",
            RunnerType::NaiveOptimized => "Naive (Optimized)",
            RunnerType::Flat => "Flat",
            RunnerType::FlatOptimized => "Flat (Optimized)",
            RunnerType::ThreadedFlat => "Threaded Flat",
            RunnerType::ThreadedFlatOptimized => "Threaded Flat (Optimized)",
            RunnerType::EventDriven => "Event Driven",
            RunnerType::EventDrivenOptimized => "Event Driven (Optimized)",
            RunnerType::Simd => "SIMD",
            RunnerType::SimdOptimized => "SIMD (Optimized)",
        };
        write!(f, "{}", name)
    }
}

/// Commands sent to the simulation worker thread.
pub enum SimCommand {
    SetInput {
        node: NodeId,
        pin: u8,
        value: bool,
    },
    AddNode {
        temp_id: u64,
        kind: NodeKind,
        inputs: usize,
        outputs: usize,
        response_tx: Sender<(u64, NodeId)>,
    },
    AddWire {
        temp_id: u64,
        source: OutputRef,
        sink: InputRef,
        response_tx: Sender<(u64, WireId)>,
    },
    RemoveNode {
        node: NodeId,
    },
    RemoveWire {
        wire: WireId,
    },
    RebuildCircuit {
        circuit: Circuit,
        response_tx: Sender<()>,
    },
    Pause,
    Resume,
    Step,
    Stop,
    SetTickRate(Option<f64>),
    SetRunner(RunnerType),
}

/// Events emitted from the simulation worker thread.
#[derive(Debug, Clone)]
pub enum SimEvent {
    TickCompleted { tick_count: u64 },
    StatusChanged { is_running: bool },
    RunnerSet(RunnerType),
}

/// Top-level owned Simulation Engine.
///
/// Manages circuit layout, runtime state, and an optional background thread that continuously
/// simulates the circuit logic as fast as possible while updating a shared atomic state.
pub struct SimulationEngine {
    pub circuit: Circuit,
    pub shared_state: Arc<SharedState>,
    runner_type: RunnerType,
    sync_runner: RefCell<Option<SyncRunnerState>>,
    command_tx: Option<Sender<SimCommand>>,
    /// Internal event receiver when the caller hasn't provided an external Sender.
    event_rx: Option<Receiver<SimEvent>>,
    /// External event sender provided by the caller (e.g. SimBridge).
    external_event_tx: Option<Sender<SimEvent>>,
    #[cfg(not(target_arch = "wasm32"))]
    worker_handle: Option<JoinHandle<()>>,
    #[cfg(target_arch = "wasm32")]
    worker_handle: (),
}

struct SyncRunnerState {
    circuit: Circuit,
    runner_type: RunnerType,
    runner: Box<dyn InspectableRunner>,
    needs_rebuild: bool,
}

impl SyncRunnerState {
    fn new(circuit: Circuit, runner_type: RunnerType) -> Self {
        let runner = build_runner(&circuit, runner_type);
        Self {
            circuit,
            runner_type,
            runner,
            needs_rebuild: false,
        }
    }

    fn set_runner_type(&mut self, runner_type: RunnerType) {
        let compatible_layout =
            is_optimized_runner(self.runner_type) == is_optimized_runner(runner_type);
        let tick = self.runner.current_tick();
        let execution_state = self.runner.execution_state();
        let mut observable_state = Vec::new();
        self.runner.for_each_wire(&mut |wire, value| {
            observable_state.push((wire, value));
        });

        self.runner_type = runner_type;
        let mut replacement = build_runner(&self.circuit, self.runner_type);
        if !compatible_layout || !replacement.restore_execution_state(&execution_state) {
            for (wire, value) in observable_state {
                replacement.restore_wire_value(wire, value);
            }
        }
        replacement.set_current_tick(tick);
        self.runner = replacement;
        self.needs_rebuild = false;
    }

    fn ensure_runner(&mut self) {
        if self.needs_rebuild {
            self.runner = build_runner(&self.circuit, self.runner_type);
            self.needs_rebuild = false;
        }
    }
}

struct MappedRunner {
    inner: Box<dyn InspectableRunner>,
    node_map: std::collections::HashMap<NodeId, NodeId>,
    wire_map: std::collections::HashMap<WireId, WireId>,
    reverse_wire_map: std::collections::HashMap<WireId, Vec<WireId>>,
}

/// Unlimited simulation can run many logical ticks between rendered frames.
/// Publish observable wire state in batches so snapshotting does not cost more
/// than the simulation itself.
/// Work quantum for unlimited mode. Commands and observable wire state are
/// synchronized between batches, keeping latency bounded without putting any
/// clock reads or scheduler syscalls in the hot loop.
const UNLIMITED_BATCH_TICKS: u32 = 4_096;
const MAX_RATE_CATCH_UP_TICKS: u32 = 4_096;
const SCHEDULER_SPIN_THRESHOLD: std::time::Duration = std::time::Duration::from_micros(200);
const SLOW_SIMULATION_LAG: std::time::Duration = std::time::Duration::from_millis(100);
const SLOW_WARNING_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);
const MAX_SNAPSHOT_INTERVAL: std::time::Duration = std::time::Duration::from_millis(4);

fn publish_wire_states(shared: &SharedState, runner: &dyn InspectableRunner) {
    if let Ok(mut lock) = shared.wire_states.write() {
        runner.for_each_wire(&mut |wire, val| {
            lock.insert(wire, val);
        });
    }
}

impl crate::runner::Runner for MappedRunner {
    fn set_input(&mut self, node: NodeId, pin: u8, value: bool) {
        if let Some(&mapped_node) = self.node_map.get(&node) {
            self.inner.set_input(mapped_node, pin, value);
        }
    }

    fn tick(&mut self) {
        self.inner.tick();
    }

    fn output(&self, node: NodeId, pin: u8) -> bool {
        if let Some(&mapped_node) = self.node_map.get(&node) {
            self.inner.output(mapped_node, pin)
        } else {
            false
        }
    }

    fn current_tick(&self) -> u64 {
        self.inner.current_tick()
    }
}

impl InspectableRunner for MappedRunner {
    fn wire_value(&self, wire: WireId) -> bool {
        if let Some(&mapped_wire) = self.wire_map.get(&wire) {
            self.inner.wire_value(mapped_wire)
        } else {
            false
        }
    }

    fn for_each_wire(&self, f: &mut dyn FnMut(WireId, bool)) {
        self.inner.for_each_wire(&mut |mapped_wire, value| {
            if let Some(raw_wires) = self.reverse_wire_map.get(&mapped_wire) {
                for &raw_wire in raw_wires {
                    f(raw_wire, value);
                }
            }
        });
    }

    fn execution_state(&self) -> Vec<bool> {
        self.inner.execution_state()
    }

    fn restore_execution_state(&mut self, state: &[bool]) -> bool {
        self.inner.restore_execution_state(state)
    }

    fn restore_wire_value(&mut self, wire: WireId, value: bool) {
        if let Some(&mapped_wire) = self.wire_map.get(&wire) {
            self.inner.restore_wire_value(mapped_wire, value);
        }
    }

    fn set_current_tick(&mut self, tick: u64) {
        self.inner.set_current_tick(tick);
    }
}

fn build_runner(circuit: &Circuit, runner_type: RunnerType) -> Box<dyn InspectableRunner> {
    validate_runner_support(runner_type);

    use crate::circuit::{BitPackedCircuit, EventDrivenCircuit, FlattenedCircuit};
    #[cfg(target_arch = "wasm32")]
    use crate::runner::{EventDrivenRunner, FlatRunner};
    #[cfg(not(target_arch = "wasm32"))]
    use crate::runner::{EventDrivenRunner, FlatRunner, SimdRunner, ThreadedFlatRunner};

    let is_optimized = match runner_type {
        RunnerType::NaiveOptimized
        | RunnerType::FlatOptimized
        | RunnerType::EventDrivenOptimized => true,
        #[cfg(not(target_arch = "wasm32"))]
        RunnerType::ThreadedFlatOptimized | RunnerType::SimdOptimized => true,
        _ => false,
    };

    if is_optimized {
        let (opt_circuit, node_map, wire_map) =
            crate::compiler::Compiler::optimize_with_maps(circuit);

        let mut reverse_wire_map = std::collections::HashMap::new();

        for (&raw_wire_id, &opt_wire_id) in &wire_map {
            reverse_wire_map
                .entry(opt_wire_id)
                .or_insert_with(Vec::new)
                .push(raw_wire_id);
        }

        let inner: Box<dyn InspectableRunner> = match runner_type {
            RunnerType::NaiveOptimized if contains_nested_state(&opt_circuit) => {
                Box::new(FlatRunner::new(FlattenedCircuit::compile(&opt_circuit)))
            }
            RunnerType::NaiveOptimized => Box::new(NaiveRunner::new(opt_circuit)),
            RunnerType::FlatOptimized => {
                let flat = FlattenedCircuit::compile(&opt_circuit);
                Box::new(FlatRunner::new(flat))
            }
            #[cfg(not(target_arch = "wasm32"))]
            RunnerType::ThreadedFlatOptimized => {
                let flat = FlattenedCircuit::compile(&opt_circuit);
                Box::new(ThreadedFlatRunner::new(flat))
            }
            RunnerType::EventDrivenOptimized => {
                let evt = EventDrivenCircuit::compile(&opt_circuit);
                Box::new(EventDrivenRunner::new(evt))
            }
            #[cfg(not(target_arch = "wasm32"))]
            RunnerType::SimdOptimized => {
                let bp = BitPackedCircuit::compile(&opt_circuit);
                Box::new(SimdRunner::new(bp))
            }
            _ => unreachable!(),
        };

        Box::new(MappedRunner {
            inner,
            node_map,
            wire_map,
            reverse_wire_map,
        })
    } else {
        match runner_type {
            RunnerType::Naive if contains_nested_state(circuit) => {
                Box::new(FlatRunner::new(FlattenedCircuit::compile(circuit)))
            }
            RunnerType::Naive => Box::new(NaiveRunner::new(circuit.clone())),
            RunnerType::Flat => {
                let flat = FlattenedCircuit::compile(circuit);
                Box::new(FlatRunner::new(flat))
            }
            #[cfg(not(target_arch = "wasm32"))]
            RunnerType::ThreadedFlat => {
                let flat = FlattenedCircuit::compile(circuit);
                Box::new(ThreadedFlatRunner::new(flat))
            }
            RunnerType::EventDriven => {
                let evt = EventDrivenCircuit::compile(circuit);
                Box::new(EventDrivenRunner::new(evt))
            }
            #[cfg(not(target_arch = "wasm32"))]
            RunnerType::Simd => {
                let bp = BitPackedCircuit::compile(circuit);
                Box::new(SimdRunner::new(bp))
            }
            _ => unreachable!(),
        }
    }
}

/// The reference Naive runner currently evaluates nested circuits with a fresh
/// child runner per tick. Use its scalar flattened fallback when that would
/// discard sequential state.
fn contains_nested_state(circuit: &Circuit) -> bool {
    circuit.nodes.values().any(|node| match &node.kind {
        NodeKind::SubCircuit(child) => {
            child
                .nodes
                .values()
                .any(|nested| matches!(nested.kind, NodeKind::Counter8 | NodeKind::Counter64))
                || contains_nested_state(child)
        }
        _ => false,
    })
}

impl SimulationEngine {
    /// Create an engine whose event `Receiver` is held internally.
    /// Callers can take the `Receiver` via [`take_event_receiver`].
    pub fn new(circuit: Circuit) -> Self {
        Self {
            circuit,
            shared_state: Arc::new(SharedState::new()),
            runner_type: RunnerType::default(),
            sync_runner: RefCell::new(None),
            command_tx: None,
            event_rx: None,
            external_event_tx: None,
            #[cfg(not(target_arch = "wasm32"))]
            worker_handle: None,
            #[cfg(target_arch = "wasm32")]
            worker_handle: (),
        }
    }

    /// Create an engine that forwards all [`SimEvent`]s to the given `Sender`.
    ///
    /// This is the preferred constructor when the caller (e.g. [`SimBridge`])
    /// owns the matching `Receiver` and wants to poll it independently.
    pub fn new_with_event_channel(circuit: Circuit, event_tx: Sender<SimEvent>) -> Self {
        Self {
            circuit,
            shared_state: Arc::new(SharedState::new()),
            runner_type: RunnerType::default(),
            sync_runner: RefCell::new(None),
            command_tx: None,
            event_rx: None,
            external_event_tx: Some(event_tx),
            #[cfg(not(target_arch = "wasm32"))]
            worker_handle: None,
            #[cfg(target_arch = "wasm32")]
            worker_handle: (),
        }
    }

    /// Take the internal event `Receiver` (only valid when using [`new`]).
    pub fn take_event_receiver(&mut self) -> Option<Receiver<SimEvent>> {
        self.event_rx.take()
    }

    /// Returns the command sender channel to send commands to the worker thread.
    pub fn command_sender(&self) -> Option<Sender<SimCommand>> {
        self.command_tx.clone()
    }

    fn sync_state(&self) -> std::cell::RefMut<'_, SyncRunnerState> {
        let mut slot = self.sync_runner.borrow_mut();
        if slot.is_none() {
            slot.replace(SyncRunnerState::new(self.circuit.clone(), self.runner_type));
        }

        std::cell::RefMut::map(slot, |state| {
            state.as_mut().expect("sync state initialized above")
        })
    }

    fn refresh_sync_circuit_from_main(&self) {
        let mut slot = self.sync_runner.borrow_mut();
        if let Some(state) = slot.as_mut() {
            state.circuit = self.circuit.clone();
            state.needs_rebuild = true;
        }
    }

    fn apply_command_sync(&self, cmd: SimCommand) {
        let event_sender = self.external_event_tx.as_ref().cloned();
        let shared = Arc::clone(&self.shared_state);

        match cmd {
            SimCommand::SetRunner(rt) => {
                let mut state = self.sync_state();
                state.set_runner_type(rt);
                publish_wire_states(&shared, state.runner.as_ref());
                Self::send_event_or_ignore(event_sender.as_ref(), SimEvent::RunnerSet(rt));
            }
            SimCommand::SetTickRate(rate) => {
                let _ = rate;
            }
            SimCommand::SetInput { node, pin, value } => {
                let mut state = self.sync_state();
                if state.needs_rebuild {
                    state.ensure_runner();
                }
                state.runner.set_input(node, pin, value);
            }
            SimCommand::AddNode {
                temp_id: _,
                kind: _,
                inputs: _,
                outputs: _,
                response_tx: _,
            } => {}
            SimCommand::AddWire {
                temp_id: _,
                source: _,
                sink: _,
                response_tx: _,
            } => {}
            SimCommand::RemoveNode { node: _ } => {}
            SimCommand::RemoveWire { wire: _ } => {}
            SimCommand::RebuildCircuit {
                circuit: _,
                response_tx: _,
            } => {}
            SimCommand::Pause => {
                self.shared_state.is_running.store(false, Ordering::Relaxed);
                Self::send_event_or_ignore(
                    event_sender.as_ref(),
                    SimEvent::StatusChanged { is_running: false },
                );
            }
            SimCommand::Resume => {
                self.shared_state.is_running.store(true, Ordering::Relaxed);
                Self::send_event_or_ignore(
                    event_sender.as_ref(),
                    SimEvent::StatusChanged { is_running: true },
                );
            }
            SimCommand::Step => {
                let mut state = self.sync_state();
                if state.needs_rebuild {
                    state.ensure_runner();
                    state.needs_rebuild = false;
                }
                state.runner.tick();
                shared.tick_count.fetch_add(1, Ordering::Relaxed);
                publish_wire_states(&shared, state.runner.as_ref());
                Self::send_event_or_ignore(
                    event_sender.as_ref(),
                    SimEvent::TickCompleted {
                        tick_count: shared.tick_count.load(Ordering::Relaxed),
                    },
                );
            }
            SimCommand::Stop => {
                self.shared_state.is_running.store(false, Ordering::Relaxed);
                Self::send_event_or_ignore(
                    event_sender.as_ref(),
                    SimEvent::StatusChanged { is_running: false },
                );
            }
        }
    }

    fn send_event_or_ignore(sender: Option<&Sender<SimEvent>>, event: SimEvent) {
        if let Some(tx) = sender {
            let _ = tx.send(event);
        }
    }

    /// Runs a fixed number of ticks synchronously on the current thread.
    ///
    /// This is the primary direct mode API for threadless hosts.
    pub fn step_many(&mut self, ticks: u64) {
        for _ in 0..ticks {
            if self.command_tx.is_none() {
                self.apply_command_sync(SimCommand::Step);
            } else if let Some(tx) = &self.command_tx {
                let _ = tx.send(SimCommand::Step);
            }
        }
    }

    /// Adds a node in threadless mode, mutating local circuit state and tagging the
    /// runnable for rebuild on the next tick.
    ///
    /// In worker mode this is still supported for API parity but should not be
    /// used for high-frequency edits.
    pub fn add_node(&mut self, kind: NodeKind, inputs: usize, outputs: usize) -> NodeId {
        let node = self.circuit.add_node(kind, inputs, outputs);
        self.refresh_sync_circuit_from_main();
        node
    }

    /// Adds a wire in threadless mode, mutating local circuit state and tagging
    /// the runnable for rebuild on the next tick.
    pub fn add_wire(&mut self, source: OutputRef, sink: InputRef) -> WireId {
        let wire = self
            .circuit
            .connect((source.node, source.pin), (sink.node, sink.pin));
        self.refresh_sync_circuit_from_main();
        wire
    }

    /// Removes a node from the threadless circuit state.
    pub fn remove_node(&mut self, node: NodeId) {
        self.circuit.nodes.remove(node);
        self.refresh_sync_circuit_from_main();
    }

    /// Removes a wire from the threadless circuit state.
    pub fn remove_wire(&mut self, wire: WireId) {
        self.circuit.wires.remove(wire);
        self.refresh_sync_circuit_from_main();
    }

    /// Replaces the simulation circuit in threadless mode.
    pub fn rebuild_circuit(&mut self, circuit: Circuit) {
        self.circuit = circuit;
        self.refresh_sync_circuit_from_main();
    }

    /// Spawns a background thread that continuously ticks the simulation as fast as possible.
    pub fn start_worker(&mut self) {
        #[cfg(not(target_arch = "wasm32"))]
        if self.worker_handle.is_some() {
            return; // Worker thread already running
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let (tx_cmd, rx_cmd) = channel::<SimCommand>();
            let tx_evt = if let Some(ext_tx) = self.external_event_tx.take() {
                ext_tx
            } else {
                let (tx, rx) = channel::<SimEvent>();
                self.event_rx = Some(rx);
                tx
            };
            self.command_tx = Some(tx_cmd);
            self.shared_state.is_running.store(true, Ordering::Relaxed);

            let shared = Arc::clone(&self.shared_state);
            let mut circuit = self.circuit.clone();
            let mut current_runner_type = self.runner_type;
            let handle = spawn(move || {
                let mut runner = build_runner(&circuit, current_runner_type);
                let mut running = true;
                let mut needs_rebuild = false;
                let mut ticks_since_snapshot = 0_u32;
                let mut pending_cmd = None;

                let mut tick_rate = Some(10.0);
                let mut tick_duration = std::time::Duration::from_secs_f64(1.0 / 10.0);
                let mut next_tick_time = std::time::Instant::now();
                let mut last_snapshot_time = std::time::Instant::now();
                let mut last_slow_warning = None;

                // Perform initial wire state sync
                publish_wire_states(&shared, runner.as_ref());

                let _ = tx_evt.send(SimEvent::RunnerSet(current_runner_type));

                loop {
                    // Process all queued commands from main thread
                    loop {
                        let cmd = pending_cmd.take().or_else(|| rx_cmd.try_recv().ok());
                        let Some(cmd) = cmd else {
                            break;
                        };
                        match cmd {
                            SimCommand::SetRunner(rt) => {
                                let compatible_layout = is_optimized_runner(current_runner_type)
                                    == is_optimized_runner(rt);
                                let tick = runner.current_tick();
                                let execution_state = runner.execution_state();
                                let mut observable_state = Vec::new();
                                runner.for_each_wire(&mut |wire, value| {
                                    observable_state.push((wire, value));
                                });

                                current_runner_type = rt;
                                let mut replacement = build_runner(&circuit, current_runner_type);
                                if !compatible_layout
                                    || !replacement.restore_execution_state(&execution_state)
                                {
                                    for (wire, value) in observable_state {
                                        replacement.restore_wire_value(wire, value);
                                    }
                                }
                                replacement.set_current_tick(tick);
                                runner = replacement;
                                needs_rebuild = false;
                                publish_wire_states(&shared, runner.as_ref());
                                let _ = tx_evt.send(SimEvent::RunnerSet(current_runner_type));
                            }
                            SimCommand::SetTickRate(rate) => {
                                tick_rate = rate;
                                if let Some(r) = rate {
                                    tick_duration = std::time::Duration::from_secs_f64(
                                        1.0 / r.clamp(0.001, 1_000_000_000.0),
                                    );
                                }
                                next_tick_time = std::time::Instant::now();
                                last_snapshot_time = next_tick_time;
                            }
                            SimCommand::SetInput { node, pin, value } => {
                                if needs_rebuild {
                                    runner = build_runner(&circuit, current_runner_type);
                                    needs_rebuild = false;
                                }
                                runner.set_input(node, pin, value);
                            }
                            SimCommand::AddNode {
                                temp_id,
                                kind,
                                inputs,
                                outputs,
                                response_tx,
                            } => {
                                let node_id = circuit.add_node(kind, inputs, outputs);
                                needs_rebuild = true;
                                let _ = response_tx.send((temp_id, node_id));
                            }
                            SimCommand::AddWire {
                                temp_id,
                                source,
                                sink,
                                response_tx,
                            } => {
                                let wire_id = circuit
                                    .connect((source.node, source.pin), (sink.node, sink.pin));
                                needs_rebuild = true;
                                let _ = response_tx.send((temp_id, wire_id));
                            }
                            SimCommand::RemoveNode { node } => {
                                circuit.nodes.remove(node);
                                needs_rebuild = true;
                            }
                            SimCommand::RemoveWire { wire } => {
                                circuit.wires.remove(wire);
                                needs_rebuild = true;
                            }
                            SimCommand::RebuildCircuit {
                                circuit: new_circuit,
                                response_tx,
                            } => {
                                circuit = new_circuit;
                                needs_rebuild = true;
                                let _ = response_tx.send(());
                            }
                            SimCommand::Pause => {
                                running = false;
                                shared.is_running.store(false, Ordering::Relaxed);
                                let _ = tx_evt.send(SimEvent::StatusChanged { is_running: false });
                            }
                            SimCommand::Resume => {
                                running = true;
                                next_tick_time = std::time::Instant::now();
                                shared.is_running.store(true, Ordering::Relaxed);
                                let _ = tx_evt.send(SimEvent::StatusChanged { is_running: true });
                            }
                            SimCommand::Step => {
                                if needs_rebuild {
                                    runner = build_runner(&circuit, current_runner_type);
                                    needs_rebuild = false;
                                }
                                runner.tick();
                                let tc = shared.tick_count.fetch_add(1, Ordering::Relaxed) + 1;
                                publish_wire_states(&shared, runner.as_ref());
                                ticks_since_snapshot = 0;
                                last_snapshot_time = std::time::Instant::now();
                                let _ = tx_evt.send(SimEvent::TickCompleted { tick_count: tc });
                            }
                            SimCommand::Stop => {
                                shared.is_running.store(false, Ordering::Relaxed);
                                let _ = tx_evt.send(SimEvent::StatusChanged { is_running: false });
                                return;
                            }
                        }
                    }

                    if running {
                        let mut ticks_to_run = 0_u32;

                        if let Some(rate) = tick_rate {
                            let now = std::time::Instant::now();
                            if now >= next_tick_time {
                                let lag = now.saturating_duration_since(next_tick_time);
                                if lag >= SLOW_SIMULATION_LAG {
                                    let should_warn = last_slow_warning.is_none_or(|last| {
                                        now.duration_since(last) >= SLOW_WARNING_INTERVAL
                                    });
                                    if should_warn {
                                        log::warn!(
                                            "Simulation cannot sustain requested {rate:.0} Hz; dropping {:.1} ms of backlog",
                                            lag.as_secs_f64() * 1_000.0
                                        );
                                        last_slow_warning = Some(now);
                                    }
                                    next_tick_time = now;
                                }

                                let duration_ns = tick_duration.as_nanos().max(1);
                                let due =
                                    (now.saturating_duration_since(next_tick_time).as_nanos()
                                        / duration_ns)
                                        + 1;
                                ticks_to_run = due.min(MAX_RATE_CATCH_UP_TICKS as u128) as u32;
                                next_tick_time += tick_duration * ticks_to_run;
                            } else {
                                let remaining = next_tick_time.saturating_duration_since(now);
                                if remaining > SCHEDULER_SPIN_THRESHOLD {
                                    match rx_cmd.recv_timeout(remaining - SCHEDULER_SPIN_THRESHOLD)
                                    {
                                        Ok(cmd) => pending_cmd = Some(cmd),
                                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                                        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                                            return;
                                        }
                                    }
                                } else {
                                    std::hint::spin_loop();
                                }
                            }
                        } else {
                            ticks_to_run = UNLIMITED_BATCH_TICKS;
                        }

                        if ticks_to_run > 0 {
                            if needs_rebuild {
                                runner = build_runner(&circuit, current_runner_type);
                                needs_rebuild = false;
                            }
                            for _ in 0..ticks_to_run {
                                runner.tick();
                            }
                            shared
                                .tick_count
                                .fetch_add(ticks_to_run as u64, Ordering::Relaxed);
                            ticks_since_snapshot =
                                ticks_since_snapshot.saturating_add(ticks_to_run);

                            if tick_rate.is_none() {
                                // Deliberately timer- and syscall-free. This is the
                                // only synchronization point in unlimited mode.
                                publish_wire_states(&shared, runner.as_ref());
                                ticks_since_snapshot = 0;
                            } else {
                                let now = std::time::Instant::now();
                                // Slow rate-limited simulations must publish every
                                // tick. Otherwise an immediately due 0.1 Hz tick
                                // can remain invisible until the next tick ten
                                // seconds later. At high rates, retain the bounded
                                // publication interval to amortize observation.
                                if tick_duration >= MAX_SNAPSHOT_INTERVAL
                                    || now.duration_since(last_snapshot_time)
                                        >= MAX_SNAPSHOT_INTERVAL
                                {
                                    publish_wire_states(&shared, runner.as_ref());
                                    ticks_since_snapshot = 0;
                                    last_snapshot_time = now;
                                }
                            }
                        }
                    } else {
                        match rx_cmd.recv() {
                            Ok(cmd) => pending_cmd = Some(cmd),
                            Err(_) => return,
                        }
                    }
                }
            });

            self.worker_handle = Some(handle);
        }

        #[cfg(target_arch = "wasm32")]
        {
            // Background workers are not available on wasm targets; threadless mode
            // advances state through direct calls into `step`/`tick_sync` instead.
        }
    }

    /// Stops the background worker thread if active.
    pub fn set_runner(&mut self, runner_type: RunnerType) {
        validate_runner_support(runner_type);
        self.runner_type = runner_type;
        if let Some(tx) = &self.command_tx {
            let _ = tx.send(SimCommand::SetRunner(runner_type));
            return;
        }
        self.apply_command_sync(SimCommand::SetRunner(runner_type));
    }

    pub fn stop_worker(&mut self) {
        #[cfg(not(target_arch = "wasm32"))]
        {
            if let Some(tx) = self.command_tx.take() {
                let _ = tx.send(SimCommand::Stop);
            }
            if let Some(handle) = self.worker_handle.take() {
                let _ = handle.join();
            }
        }
        self.shared_state.is_running.store(false, Ordering::Relaxed);
    }

    /// Set an input pin value. Thread-safe when worker is active.
    pub fn set_input(&mut self, node: NodeId, pin: u8, value: bool) {
        if let Some(tx) = &self.command_tx {
            let _ = tx.send(SimCommand::SetInput { node, pin, value });
            return;
        }
        self.apply_command_sync(SimCommand::SetInput { node, pin, value });
    }

    /// Atomically read the output state of a node's output pin.
    pub fn get_output(&self, node: NodeId, pin: u8) -> bool {
        if let Some(node_ref) = self.circuit.nodes.get(node)
            && (pin as usize) < node_ref.outputs.len()
            && let Some(wire) = node_ref.outputs[pin as usize].wires.first()
        {
            return self.shared_state.get_wire(*wire);
        }
        false
    }

    /// Atomically read the input state of a node's input pin.
    pub fn get_input(&self, node: NodeId, pin: u8) -> bool {
        if let Some(node_ref) = self.circuit.nodes.get(node)
            && (pin as usize) < node_ref.inputs.len()
            && let Some(wire) = node_ref.inputs[pin as usize].wire
        {
            return self.shared_state.get_wire(wire);
        }
        false
    }

    /// Atomically read state of a wire.
    pub fn get_wire(&self, wire: WireId) -> bool {
        self.shared_state.get_wire(wire)
    }

    /// Read total ticks completed.
    pub fn tick_count(&self) -> u64 {
        self.shared_state.tick_count()
    }

    /// Pause simulation worker.
    pub fn pause(&self) {
        if let Some(tx) = &self.command_tx {
            let _ = tx.send(SimCommand::Pause);
            return;
        }
        self.apply_command_sync(SimCommand::Pause);
    }

    /// Resume simulation worker.
    pub fn resume(&self) {
        if let Some(tx) = &self.command_tx {
            let _ = tx.send(SimCommand::Resume);
            return;
        }
        self.apply_command_sync(SimCommand::Resume);
    }

    /// Toggle simulation running state when using a direct (threadless) runtime.
    /// Returns the current shared-running value for compatibility checks.
    pub fn is_running(&self) -> bool {
        self.shared_state.is_running.load(Ordering::Relaxed)
    }

    /// Step simulation worker by 1 tick manually.
    pub fn step(&self) {
        if let Some(tx) = &self.command_tx {
            let _ = tx.send(SimCommand::Step);
            return;
        }
        self.apply_command_sync(SimCommand::Step);
    }

    /// Set the simulation tick rate (Hz) for the worker thread, or None for unlimited.
    pub fn set_tick_rate(&self, tick_rate: Option<f64>) {
        if let Some(tx) = &self.command_tx {
            let _ = tx.send(SimCommand::SetTickRate(tick_rate));
            return;
        }
        self.apply_command_sync(SimCommand::SetTickRate(tick_rate));
    }

    /// Step simulation synchronously on current thread (when worker thread is not running).
    pub fn step_sync(&mut self) {
        self.apply_command_sync(SimCommand::Step);
    }
}

#[cfg(target_arch = "wasm32")]
fn validate_runner_support(runner_type: RunnerType) {
    if matches!(
        runner_type,
        RunnerType::ThreadedFlat
            | RunnerType::ThreadedFlatOptimized
            | RunnerType::Simd
            | RunnerType::SimdOptimized
    ) {
        panic!(
            "Runner `{runner_type}` is not supported on wasm32 targets. Use `Naive`, `Flat`, or `EventDriven` variants."
        );
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn validate_runner_support(_runner_type: RunnerType) {}

#[cfg(target_arch = "wasm32")]
fn is_optimized_runner(runner: RunnerType) -> bool {
    matches!(
        runner,
        RunnerType::NaiveOptimized | RunnerType::FlatOptimized | RunnerType::EventDrivenOptimized
    )
}

#[cfg(not(target_arch = "wasm32"))]
fn is_optimized_runner(runner: RunnerType) -> bool {
    matches!(
        runner,
        RunnerType::NaiveOptimized
            | RunnerType::FlatOptimized
            | RunnerType::ThreadedFlatOptimized
            | RunnerType::SimdOptimized
            | RunnerType::EventDrivenOptimized
    )
}

impl Drop for SimulationEngine {
    fn drop(&mut self) {
        self.stop_worker();
    }
}
