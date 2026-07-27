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
    #[default]
    Naive,
    NaiveOptimized,
    Flat,
    FlatOptimized,
    EventDriven,
    EventDrivenOptimized,
    Simd,
    SimdOptimized,
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
    reverse_wire_map: std::collections::HashMap<WireId, WireId>,
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
            if let Some(&raw_wire) = self.reverse_wire_map.get(&mapped_wire) {
                f(raw_wire, value);
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
        let (opt_circuit, node_map, _) =
            crate::compiler::dead_code::cull_dead_nodes_with_map(circuit);

        let mut wire_map = std::collections::HashMap::new();
        let mut reverse_wire_map = std::collections::HashMap::new();

        // cull_dead_nodes_with_map preserves wires that are connected. But it creates a new circuit with new WireIds.
        // We need to map raw wire IDs to opt wire IDs.
        // Let's match them based on source node and pin.
        // For each wire in opt_circuit, find its source node and pin. Map back to raw source node, and find raw wire.
        let mut reverse_node_map = std::collections::HashMap::new();
        for (raw_node, opt_node) in &node_map {
            reverse_node_map.insert(*opt_node, *raw_node);
        }

        for (opt_wire_id, opt_wire) in &opt_circuit.wires {
            let opt_source_node = opt_wire.source.node;
            if let Some(&raw_source_node) = reverse_node_map.get(&opt_source_node) {
                if let Some(raw_node_ref) = circuit.nodes.get(raw_source_node) {
                    if let Some(raw_out_pin) =
                        raw_node_ref.outputs.get(opt_wire.source.pin as usize)
                    {
                        if let Some(&raw_wire_id) = raw_out_pin.wires.first() {
                            wire_map.insert(raw_wire_id, opt_wire_id);
                            reverse_wire_map.insert(opt_wire_id, raw_wire_id);
                        }
                    }
                }
            }
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

            let mut tick_rate = Some(10.0);
            let mut tick_duration = std::time::Duration::from_secs_f64(1.0 / 10.0);
            let mut next_tick_time = std::time::Instant::now();

            // Perform initial wire state sync
            if let Ok(mut lock) = shared.wire_states.write() {
                runner.for_each_wire(&mut |wire, val| {
                    lock.insert(wire, val);
                });
            }

            loop {
                // Process all queued commands from main thread
                while let Ok(cmd) = rx_cmd.try_recv() {
                    match cmd {
                        SimCommand::SetRunner(rt) => {
                            current_runner_type = rt;
                            needs_rebuild = true;
                        }
                        SimCommand::SetTickRate(rate) => {
                            tick_rate = rate;
                            if let Some(r) = rate {
                                tick_duration =
                                    std::time::Duration::from_secs_f64(1.0 / r.max(0.001));
                            }
                            next_tick_time = std::time::Instant::now();
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
                            if let Ok(mut lock) = shared.wire_states.write() {
                                runner.for_each_wire(&mut |wire, val| {
                                    lock.insert(wire, val);
                                });
                            }
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
                    let mut should_tick = false;

                    if let Some(_) = tick_rate {
                        let now = std::time::Instant::now();
                        if now >= next_tick_time {
                            if now > next_tick_time + tick_duration * 2 {
                                log::warn!("Simulation running too slowly! Skipping ticks.");
                                next_tick_time = now;
                            }
                            next_tick_time += tick_duration;
                            should_tick = true;
                        } else {
                            let remaining = next_tick_time.saturating_duration_since(now);
                            if remaining > std::time::Duration::from_millis(2) {
                                std::thread::sleep(std::time::Duration::from_millis(1));
                            } else {
                                std::thread::yield_now();
                            }
                        }
                    } else {
                        should_tick = true;
                        std::thread::yield_now(); // Yield to prevent completely starving other threads in unlimited mode
                    }

                    if should_tick {
                        if needs_rebuild {
                            runner = build_runner(&circuit, current_runner_type);
                            needs_rebuild = false;
                        }
                        runner.tick();
                        let _tc = shared.tick_count.fetch_add(1, Ordering::Relaxed) + 1;
                        if let Ok(mut lock) = shared.wire_states.write() {
                            runner.for_each_wire(&mut |wire, val| {
                                lock.insert(wire, val);
                            });
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
        if let Ok(mut lock) = self.shared_state.wire_states.write() {
            runner.for_each_wire(&mut |wire, val| {
                lock.insert(wire, val);
            });
        }
    }
}

impl Drop for SimulationEngine {
    fn drop(&mut self) {
        self.stop_worker();
    }
}
