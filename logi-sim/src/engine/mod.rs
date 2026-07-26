pub mod atomic_state;

pub use atomic_state::SharedState;

use crate::circuit::{Circuit, InputRef, NodeId, NodeKind, OutputRef, WireId};
use crate::runner::{InspectableRunner, NaiveRunner, Runner};

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread::{JoinHandle, spawn};

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
    SetTickRate(f64),
}

/// Events emitted from the simulation worker thread.
#[derive(Debug, Clone)]
pub enum SimEvent {
    WireStateChanged { wire: WireId, value: bool },
    TickStarted { tick_count: u64 },
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
    command_tx: Option<Sender<SimCommand>>,
    /// Internal event receiver when the caller hasn't provided an external Sender.
    event_rx: Option<Receiver<SimEvent>>,
    /// External event sender provided by the caller (e.g. SimBridge).
    external_event_tx: Option<Sender<SimEvent>>,
    worker_handle: Option<JoinHandle<()>>,
}

impl SimulationEngine {
    /// Create an engine whose event `Receiver` is held internally.
    /// Callers can take the `Receiver` via [`take_event_receiver`].
    pub fn new(circuit: Circuit) -> Self {
        Self {
            circuit,
            shared_state: Arc::new(SharedState::new()),
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

        let handle = spawn(move || {
            let mut runner = NaiveRunner::new(&circuit);
            let mut running = true;

            let mut tick_rate = 10.0;
            let mut tick_duration = std::time::Duration::from_secs_f64(1.0 / tick_rate);
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
                        SimCommand::SetTickRate(rate) => {
                            tick_rate = rate.max(0.001);
                            tick_duration = std::time::Duration::from_secs_f64(1.0 / tick_rate);
                        }
                        SimCommand::SetInput { node, pin, value } => {
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
                            runner = NaiveRunner::new(&circuit);
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
                            runner = NaiveRunner::new(&circuit);
                            let _ = response_tx.send((temp_id, wire_id));
                        }
                        SimCommand::RemoveNode { node } => {
                            circuit.nodes.remove(node);
                            runner = NaiveRunner::new(&circuit);
                        }
                        SimCommand::RemoveWire { wire } => {
                            circuit.wires.remove(wire);
                            runner = NaiveRunner::new(&circuit);
                        }
                        SimCommand::RebuildCircuit {
                            circuit: new_circuit,
                            response_tx,
                        } => {
                            circuit = new_circuit;
                            runner = NaiveRunner::new(&circuit);
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
                    let now = std::time::Instant::now();
                    if now >= next_tick_time {
                        if now > next_tick_time + tick_duration * 2 {
                            log::warn!("Simulation running too slowly! Skipping ticks.");
                            next_tick_time = now;
                        }

                        let _ = tx_evt.send(SimEvent::TickStarted {
                            tick_count: shared.tick_count.load(Ordering::Relaxed),
                        });
                        runner.tick();
                        let _tc = shared.tick_count.fetch_add(1, Ordering::Relaxed) + 1;
                        if let Ok(mut lock) = shared.wire_states.write() {
                            runner.for_each_wire(&mut |wire, val| {
                                lock.insert(wire, val);
                            });
                        }

                        next_tick_time += tick_duration;
                    } else {
                        let remaining = next_tick_time.saturating_duration_since(now);
                        if remaining > std::time::Duration::from_millis(2) {
                            std::thread::sleep(std::time::Duration::from_millis(1));
                        } else {
                            std::thread::yield_now();
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

    /// Set the simulation tick rate (Hz) for the worker thread.
    pub fn set_tick_rate(&self, tick_rate: f64) {
        if let Some(tx) = &self.command_tx {
            let _ = tx.send(SimCommand::SetTickRate(tick_rate));
        }
    }

    /// Step simulation synchronously on current thread (when worker thread is not running).
    pub fn step_sync(&mut self) {
        let mut runner = NaiveRunner::new(&self.circuit);
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
