//! Stable simulation execution service abstraction for Bevy.
//!
//! The editor layer should depend on this trait rather than directly managing
//! worker threads, command channels, or engine internals.

use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};

use logi_sim::circuit::{Circuit, InputRef, NodeId, NodeKind, OutputRef, WireId};
use logi_sim::engine::{RunnerType, SimCommand, SimEvent, SimulationEngine};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SimRuntime {
    Threaded,
    Threadless,
}

/// Abstract interface used by Bevy systems and bridge logic.
pub trait SimExecutionService: Send + Sync {
    /// Runtime variant used by this service.
    fn runtime(&self) -> SimRuntime;

    /// Set an input wire on a simulation node.
    fn set_input(&self, node: NodeId, pin: u8, value: bool);

    /// Set whether the simulator should be running.
    fn pause(&self);
    fn resume(&self);

    /// Step the simulation by one tick.
    fn step(&self);

    /// Backwards-compatible tick alias used by higher-level callers.
    fn tick(&self) {
        self.step();
    }

    /// Step the simulation by `ticks` batches.
    fn step_many(&self, ticks: u64);

    /// Update the simulation tick rate (`None` for unlimited).
    fn set_tick_rate(&self, tick_rate: Option<f64>);

    /// Replace active execution runner.
    fn set_runner_type(&self, runner_type: RunnerType);

    /// Stop worker/task, if present.
    fn stop(&self);

    /// Add a node into the live circuit.
    ///
    /// The returned channel contains `(temp_id, node_id)` once the command is
    /// applied. Threaded implementations use the worker response channel,
    /// threadless implementations resolve synchronously.
    fn add_node(
        &self,
        temp_id: u64,
        kind: NodeKind,
        inputs: usize,
        outputs: usize,
    ) -> Receiver<(u64, NodeId)>;

    /// Add a wire into the live circuit.
    fn add_wire(&self, temp_id: u64, source: OutputRef, sink: InputRef) -> Receiver<(u64, WireId)>;

    /// Remove a node from the live circuit.
    fn remove_node(&self, node: NodeId);

    /// Remove a wire from the live circuit.
    fn remove_wire(&self, wire: WireId);

    /// Rebuild the full live circuit.
    fn rebuild_circuit(&self, circuit: Circuit) -> Receiver<()>;

    /// Snapshot all wire values.
    fn snapshot_wire_states(&self, f: &mut dyn FnMut(WireId, bool));

    /// Drain worker/system events since the last call.
    fn drain_events(&self) -> Vec<SimEvent>;

    /// Read current running tick count.
    fn tick_count(&self) -> u64;

    /// Read current running state.
    fn is_running(&self) -> bool;
}

/// Thread-backed service: simulation continues on an engine-owned background worker.
pub struct ThreadedSimService {
    engine: Mutex<SimulationEngine>,
    event_rx: Mutex<Receiver<SimEvent>>,
}

impl ThreadedSimService {
    pub fn new(circuit: Circuit) -> Self {
        let (event_tx, event_rx) = mpsc::channel::<SimEvent>();
        let mut engine = SimulationEngine::new_with_event_channel(circuit, event_tx);
        engine.start_worker();
        Self {
            engine: Mutex::new(engine),
            event_rx: Mutex::new(event_rx),
        }
    }

    fn with_engine<R>(&self, f: impl FnOnce(&mut SimulationEngine) -> R) -> R {
        match self.engine.lock() {
            Ok(mut engine) => f(&mut engine),
            Err(poisoned) => {
                let mut engine = poisoned.into_inner();
                f(&mut engine)
            }
        }
    }

    fn dispatch_command(&self, command: SimCommand) {
        self.with_engine(|engine| {
            if let Some(tx) = &engine.command_sender() {
                let _ = tx.send(command);
                return;
            }

            match command {
                SimCommand::SetInput { node, pin, value } => {
                    engine.set_input(node, pin, value);
                }
                SimCommand::SetTickRate(rate) => engine.set_tick_rate(rate),
                SimCommand::SetRunner(runner_type) => engine.set_runner(runner_type),
                SimCommand::Pause => engine.pause(),
                SimCommand::Resume => engine.resume(),
                SimCommand::Step => engine.step(),
                SimCommand::Stop => engine.stop_worker(),
                _ => {
                    // Threaded fallback path only occurs if `start_worker` failed
                    // to initialize command routing; this keeps behavior consistent in
                    // tests and ensures build integrity.
                    match command {
                        SimCommand::RebuildCircuit {
                            circuit,
                            response_tx,
                        } => {
                            engine.rebuild_circuit(circuit);
                            let _ = response_tx.send(());
                        }
                        SimCommand::AddNode {
                            temp_id,
                            kind,
                            inputs,
                            outputs,
                            response_tx,
                        } => {
                            let node = engine.add_node(kind, inputs, outputs);
                            let _ = response_tx.send((temp_id, node));
                        }
                        SimCommand::AddWire {
                            temp_id,
                            source,
                            sink,
                            response_tx,
                        } => {
                            let wire = engine.add_wire(source, sink);
                            let _ = response_tx.send((temp_id, wire));
                        }
                        SimCommand::RemoveNode { node } => engine.remove_node(node),
                        SimCommand::RemoveWire { wire } => engine.remove_wire(wire),
                        _ => {}
                    }
                }
            }
        });
    }
}

impl SimExecutionService for ThreadedSimService {
    fn runtime(&self) -> SimRuntime {
        SimRuntime::Threaded
    }

    fn set_input(&self, node: NodeId, pin: u8, value: bool) {
        self.dispatch_command(SimCommand::SetInput { node, pin, value })
    }

    fn pause(&self) {
        self.dispatch_command(SimCommand::Pause)
    }

    fn resume(&self) {
        self.dispatch_command(SimCommand::Resume)
    }

    fn step(&self) {
        self.dispatch_command(SimCommand::Step)
    }

    fn step_many(&self, ticks: u64) {
        for _ in 0..ticks {
            self.step();
        }
    }

    fn set_tick_rate(&self, tick_rate: Option<f64>) {
        self.dispatch_command(SimCommand::SetTickRate(tick_rate))
    }

    fn set_runner_type(&self, runner_type: RunnerType) {
        self.dispatch_command(SimCommand::SetRunner(runner_type))
    }

    fn stop(&self) {
        self.dispatch_command(SimCommand::Stop)
    }

    fn add_node(
        &self,
        temp_id: u64,
        kind: NodeKind,
        inputs: usize,
        outputs: usize,
    ) -> Receiver<(u64, NodeId)> {
        let (tx, rx) = mpsc::channel();
        self.dispatch_command(SimCommand::AddNode {
            temp_id,
            kind,
            inputs,
            outputs,
            response_tx: tx,
        });
        rx
    }

    fn add_wire(&self, temp_id: u64, source: OutputRef, sink: InputRef) -> Receiver<(u64, WireId)> {
        let (tx, rx) = mpsc::channel();
        self.dispatch_command(SimCommand::AddWire {
            temp_id,
            source,
            sink,
            response_tx: tx,
        });
        rx
    }

    fn remove_node(&self, node: NodeId) {
        self.dispatch_command(SimCommand::RemoveNode { node })
    }

    fn remove_wire(&self, wire: WireId) {
        self.dispatch_command(SimCommand::RemoveWire { wire })
    }

    fn rebuild_circuit(&self, circuit: Circuit) -> Receiver<()> {
        let (tx, rx) = mpsc::channel();
        self.dispatch_command(SimCommand::RebuildCircuit {
            circuit,
            response_tx: tx,
        });
        rx
    }

    fn snapshot_wire_states(&self, f: &mut dyn FnMut(WireId, bool)) {
        if let Ok(engine) = self.engine.lock() {
            if let Ok(states) = engine.shared_state.wire_states.read() {
                for (wire, val) in states.iter() {
                    f(wire, *val);
                }
            }
        }
    }

    fn drain_events(&self) -> Vec<SimEvent> {
        let mut events = Vec::new();
        let mut event_rx = match self.event_rx.lock() {
            Ok(rx) => rx,
            Err(poisoned) => poisoned.into_inner(),
        };
        while let Ok(evt) = event_rx.try_recv() {
            events.push(evt);
        }
        events
    }

    fn tick_count(&self) -> u64 {
        self.with_engine(|engine| engine.tick_count())
    }

    fn is_running(&self) -> bool {
        self.with_engine(|engine| {
            engine
                .shared_state
                .is_running
                .load(std::sync::atomic::Ordering::Relaxed)
        })
    }
}

/// Threadless/direct service: no background thread, all work happens in-frame.
pub struct DirectSimService {
    engine: Mutex<SimulationEngine>,
}

impl DirectSimService {
    pub fn new(circuit: Circuit) -> Self {
        let mut engine = SimulationEngine::new(circuit);
        // The direct mode owns stepping on the main thread.
        engine.resume();
        Self {
            engine: Mutex::new(engine),
        }
    }

    fn with_engine<R>(&self, f: impl FnOnce(&mut SimulationEngine) -> R) -> R {
        match self.engine.lock() {
            Ok(mut engine) => f(&mut engine),
            Err(poisoned) => {
                let mut engine = poisoned.into_inner();
                f(&mut engine)
            }
        }
    }
}

impl SimExecutionService for DirectSimService {
    fn runtime(&self) -> SimRuntime {
        SimRuntime::Threadless
    }

    fn set_input(&self, node: NodeId, pin: u8, value: bool) {
        self.with_engine(|engine| engine.set_input(node, pin, value));
    }

    fn pause(&self) {
        self.with_engine(|engine| engine.pause());
    }

    fn resume(&self) {
        self.with_engine(|engine| engine.resume());
    }

    fn step(&self) {
        self.with_engine(|engine| engine.step());
    }

    fn step_many(&self, ticks: u64) {
        self.with_engine(|engine| {
            engine.step_many(ticks);
        });
    }

    fn set_tick_rate(&self, tick_rate: Option<f64>) {
        self.with_engine(|engine| engine.set_tick_rate(tick_rate));
    }

    fn set_runner_type(&self, runner_type: RunnerType) {
        self.with_engine(|engine| engine.set_runner(runner_type));
    }

    fn stop(&self) {
        self.with_engine(|engine| engine.stop_worker());
    }

    fn add_node(
        &self,
        temp_id: u64,
        kind: NodeKind,
        inputs: usize,
        outputs: usize,
    ) -> Receiver<(u64, NodeId)> {
        let (tx, rx) = mpsc::channel();
        self.with_engine(|engine| {
            let node = engine.add_node(kind, inputs, outputs);
            let _ = tx.send((temp_id, node));
        });
        rx
    }

    fn add_wire(&self, temp_id: u64, source: OutputRef, sink: InputRef) -> Receiver<(u64, WireId)> {
        let (tx, rx) = mpsc::channel();
        self.with_engine(|engine| {
            let wire = engine.add_wire(source, sink);
            let _ = tx.send((temp_id, wire));
        });
        rx
    }

    fn remove_node(&self, node: NodeId) {
        self.with_engine(|engine| engine.remove_node(node));
    }

    fn remove_wire(&self, wire: WireId) {
        self.with_engine(|engine| engine.remove_wire(wire));
    }

    fn rebuild_circuit(&self, circuit: Circuit) -> Receiver<()> {
        let (tx, rx) = mpsc::channel();
        self.with_engine(|engine| {
            engine.rebuild_circuit(circuit);
            let _ = tx.send(());
        });
        rx
    }

    fn snapshot_wire_states(&self, f: &mut dyn FnMut(WireId, bool)) {
        if let Ok(engine) = self.engine.lock() {
            if let Ok(states) = engine.shared_state.wire_states.read() {
                for (wire, val) in states.iter() {
                    f(wire, *val);
                }
            }
        }
    }

    fn drain_events(&self) -> Vec<SimEvent> {
        Vec::new()
    }

    fn tick_count(&self) -> u64 {
        self.with_engine(|engine| engine.tick_count())
    }

    fn is_running(&self) -> bool {
        self.with_engine(|engine| {
            engine
                .shared_state
                .is_running
                .load(std::sync::atomic::Ordering::Relaxed)
        })
    }
}

pub fn make_threaded_service(circuit: Circuit) -> Arc<dyn SimExecutionService> {
    Arc::new(ThreadedSimService::new(circuit))
}

pub fn make_threadless_service(circuit: Circuit) -> Arc<dyn SimExecutionService> {
    Arc::new(DirectSimService::new(circuit))
}
