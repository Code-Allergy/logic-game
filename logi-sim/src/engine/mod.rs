pub mod atomic_state;

pub use atomic_state::SharedState;

use crate::circuit::{Circuit, InputRef, NodeId, NodeKind, OutputRef, WireId};
use crate::runner::{InspectableRunner, NaiveRunner};

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread::{JoinHandle, spawn};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RunnerType {
    Naive,
    NaiveOptimized,
    Flat,
    #[default]
    FlatOptimized,
    EventDriven,
    EventDrivenOptimized,
    Simd,
    SimdOptimized,
}

impl RunnerType {
    pub const ALL: &'static [RunnerType] = &[
        RunnerType::Naive,
        RunnerType::NaiveOptimized,
        RunnerType::Flat,
        RunnerType::FlatOptimized,
        RunnerType::EventDriven,
        RunnerType::EventDrivenOptimized,
        RunnerType::Simd,
        RunnerType::SimdOptimized,
    ];
}

impl std::fmt::Display for RunnerType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            RunnerType::Naive => "Naive",
            RunnerType::NaiveOptimized => "Naive (Optimized)",
            RunnerType::Flat => "Flat",
            RunnerType::FlatOptimized => "Flat (Optimized)",
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
    command_tx: Option<Sender<SimCommand>>,
    /// Internal event receiver when the caller hasn't provided an external Sender.
    event_rx: Option<Receiver<SimEvent>>,
    /// External event sender provided by the caller (e.g. SimBridge).
    external_event_tx: Option<Sender<SimEvent>>,
    worker_handle: Option<JoinHandle<()>>,
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
}

fn build_runner(circuit: &Circuit, runner_type: RunnerType) -> Box<dyn InspectableRunner> {
    use crate::circuit::{BitPackedCircuit, EventDrivenCircuit, FlattenedCircuit};
    use crate::runner::{EventDrivenRunner, FlatRunner, SimdRunner};

    let is_optimized = match runner_type {
        RunnerType::NaiveOptimized
        | RunnerType::FlatOptimized
        | RunnerType::EventDrivenOptimized
        | RunnerType::SimdOptimized => true,
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
            RunnerType::NaiveOptimized => Box::new(NaiveRunner::new(opt_circuit)),
            RunnerType::FlatOptimized => {
                let flat = FlattenedCircuit::compile(&opt_circuit);
                Box::new(FlatRunner::new(flat))
            }
            RunnerType::EventDrivenOptimized => {
                let evt = EventDrivenCircuit::compile(&opt_circuit);
                Box::new(EventDrivenRunner::new(evt))
            }
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
            RunnerType::Naive => Box::new(NaiveRunner::new(circuit.clone())),
            RunnerType::Flat => {
                let flat = FlattenedCircuit::compile(circuit);
                Box::new(FlatRunner::new(flat))
            }
            RunnerType::EventDriven => {
                let evt = EventDrivenCircuit::compile(circuit);
                Box::new(EventDrivenRunner::new(evt))
            }
            RunnerType::Simd => {
                let bp = BitPackedCircuit::compile(circuit);
                Box::new(SimdRunner::new(bp))
            }
            _ => unreachable!(),
        }
    }
}

impl SimulationEngine {
    /// Create an engine whose event `Receiver` is held internally.
    /// Callers can take the `Receiver` via [`take_event_receiver`].
    pub fn new(circuit: Circuit) -> Self {
        Self {
            circuit,
            shared_state: Arc::new(SharedState::new()),
            runner_type: RunnerType::default(),
            command_tx: None,
            event_rx: None,
            external_event_tx: None,
            worker_handle: None,
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
            command_tx: None,
            event_rx: None,
            external_event_tx: Some(event_tx),
            worker_handle: None,
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

    /// Spawns a background thread that continuously ticks the simulation as fast as possible.
    pub fn start_worker(&mut self) {
        if self.worker_handle.is_some() {
            return; // Worker thread already running
        }

        let (tx_cmd, rx_cmd) = channel::<SimCommand>();
        // If the caller provided an external Sender use that; otherwise create
        // an internal channel and stash the Receiver.
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
                while let Ok(cmd) = rx_cmd.try_recv() {
                    match cmd {
                        SimCommand::SetRunner(rt) => {
                            current_runner_type = rt;
                            needs_rebuild = true;
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
                            let wire_id =
                                circuit.connect((source.node, source.pin), (sink.node, sink.pin));
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
                            let due = (now.saturating_duration_since(next_tick_time).as_nanos()
                                / duration_ns)
                                + 1;
                            ticks_to_run = due.min(MAX_RATE_CATCH_UP_TICKS as u128) as u32;
                            next_tick_time += tick_duration * ticks_to_run;
                        } else {
                            let remaining = next_tick_time.saturating_duration_since(now);
                            if remaining > SCHEDULER_SPIN_THRESHOLD {
                                std::thread::sleep(remaining - SCHEDULER_SPIN_THRESHOLD);
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
                        ticks_since_snapshot = ticks_since_snapshot.saturating_add(ticks_to_run);

                        if tick_rate.is_none() {
                            // Deliberately timer- and syscall-free. This is the
                            // only synchronization point in unlimited mode.
                            publish_wire_states(&shared, runner.as_ref());
                            ticks_since_snapshot = 0;
                        } else {
                            let now = std::time::Instant::now();
                            if now.duration_since(last_snapshot_time) >= MAX_SNAPSHOT_INTERVAL {
                                publish_wire_states(&shared, runner.as_ref());
                                ticks_since_snapshot = 0;
                                last_snapshot_time = now;
                            }
                        }
                    }
                } else {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
            }
        });

        self.worker_handle = Some(handle);
    }

    /// Stops the background worker thread if active.
    pub fn set_runner(&mut self, runner_type: RunnerType) {
        self.runner_type = runner_type;
        if let Some(tx) = &self.command_tx {
            let _ = tx.send(SimCommand::SetRunner(runner_type));
        }
    }

    pub fn stop_worker(&mut self) {
        if let Some(tx) = self.command_tx.take() {
            let _ = tx.send(SimCommand::Stop);
        }
        if let Some(handle) = self.worker_handle.take() {
            let _ = handle.join();
        }
    }

    /// Set an input pin value. Thread-safe when worker is active.
    pub fn set_input(&mut self, node: NodeId, pin: u8, value: bool) {
        if let Some(tx) = &self.command_tx {
            let _ = tx.send(SimCommand::SetInput { node, pin, value });
        }
    }

    /// Atomically read the output state of a node's output pin.
    pub fn get_output(&self, node: NodeId, pin: u8) -> bool {
        if let Some(node_ref) = self.circuit.nodes.get(node) {
            if (pin as usize) < node_ref.outputs.len() {
                if let Some(wire) = node_ref.outputs[pin as usize].wires.first() {
                    return self.shared_state.get_wire(*wire);
                }
            }
        }
        false
    }

    /// Atomically read the input state of a node's input pin.
    pub fn get_input(&self, node: NodeId, pin: u8) -> bool {
        if let Some(node_ref) = self.circuit.nodes.get(node) {
            if (pin as usize) < node_ref.inputs.len() {
                if let Some(wire) = node_ref.inputs[pin as usize].wire {
                    return self.shared_state.get_wire(wire);
                }
            }
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
        }
    }

    /// Resume simulation worker.
    pub fn resume(&self) {
        if let Some(tx) = &self.command_tx {
            let _ = tx.send(SimCommand::Resume);
        }
    }

    /// Step simulation worker by 1 tick manually.
    pub fn step(&self) {
        if let Some(tx) = &self.command_tx {
            let _ = tx.send(SimCommand::Step);
        }
    }

    /// Set the simulation tick rate (Hz) for the worker thread, or None for unlimited.
    pub fn set_tick_rate(&self, tick_rate: Option<f64>) {
        if let Some(tx) = &self.command_tx {
            let _ = tx.send(SimCommand::SetTickRate(tick_rate));
        }
    }

    /// Step simulation synchronously on current thread (when worker thread is not running).
    pub fn step_sync(&mut self) {
        let mut runner = build_runner(&self.circuit, self.runner_type);
        runner.tick();
        self.shared_state.tick_count.fetch_add(1, Ordering::Relaxed);
        publish_wire_states(&self.shared_state, runner.as_ref());
    }
}

impl Drop for SimulationEngine {
    fn drop(&mut self) {
        self.stop_worker();
    }
}
