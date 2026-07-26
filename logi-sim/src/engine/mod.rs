pub mod atomic_state;

pub use atomic_state::SharedState;

use crate::circuit::{Circuit, NodeId, WireId};
use crate::runner::{NaiveRunner, Runner};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{Sender, channel};
use std::thread::{JoinHandle, spawn};

/// Messages sent to the simulation worker thread.
pub enum SimCommand {
    SetInput { node: NodeId, pin: u8, value: bool },
    Pause,
    Resume,
    Step,
    Stop,
}

/// Top-level owned Simulation Engine.
///
/// Manages circuit layout, runtime state, and an optional background thread that continuously
/// simulates the circuit logic as fast as possible while updating a shared atomic state.
#[cfg_attr(
    feature = "bevy",
    derive(bevy::prelude::Component, bevy::prelude::Resource)
)]
pub struct SimulationEngine {
    pub circuit: Circuit,
    pub shared_state: Arc<SharedState>,
    command_tx: Option<Sender<SimCommand>>,
    worker_handle: Option<JoinHandle<()>>,
}

impl SimulationEngine {
    pub fn new(circuit: Circuit) -> Self {
        Self {
            circuit,
            shared_state: Arc::new(SharedState::new()),
            command_tx: None,
            worker_handle: None,
        }
    }

    /// Spawns a background thread that continuously ticks the simulation as fast as possible.
    pub fn start_worker(&mut self) {
        if self.worker_handle.is_some() {
            return; // Worker thread already running
        }

        let (tx, rx) = channel::<SimCommand>();
        self.command_tx = Some(tx);
        self.shared_state.is_running.store(true, Ordering::Relaxed);

        let shared = Arc::clone(&self.shared_state);
        let circuit_clone = self.circuit.clone();

        let handle = spawn(move || {
            let circuit = circuit_clone;
            let mut runner = NaiveRunner::new(&circuit);
            let mut running = true;

            // Perform initial wire state sync
            if let Ok(mut lock) = shared.wire_states.write() {
                runner.for_each_wire(&mut |wire, val| {
                    lock.insert(wire, val);
                });
            }

            loop {
                // Process all queued commands from main/Bevy thread
                while let Ok(cmd) = rx.try_recv() {
                    match cmd {
                        SimCommand::SetInput { node, pin, value } => {
                            runner.set_input(node, pin, value);
                        }
                        SimCommand::Pause => {
                            running = false;
                            shared.is_running.store(false, Ordering::Relaxed);
                        }
                        SimCommand::Resume => {
                            running = true;
                            shared.is_running.store(true, Ordering::Relaxed);
                        }
                        SimCommand::Step => {
                            runner.tick();
                            shared.tick_count.fetch_add(1, Ordering::Relaxed);
                            if let Ok(mut lock) = shared.wire_states.write() {
                                runner.for_each_wire(&mut |wire, val| {
                                    lock.insert(wire, val);
                                });
                            }
                        }
                        SimCommand::Stop => {
                            shared.is_running.store(false, Ordering::Relaxed);
                            return;
                        }
                    }
                }

                if running {
                    runner.tick();
                    shared.tick_count.fetch_add(1, Ordering::Relaxed);
                    if let Ok(mut lock) = shared.wire_states.write() {
                        runner.for_each_wire(&mut |wire, val| {
                            lock.insert(wire, val);
                        });
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

    /// Atomically read the output state of a node's pin.
    pub fn get_output(&self, node: NodeId, pin: u8) -> bool {
        let node_ref = &self.circuit.nodes[node];
        if (pin as usize) < node_ref.inputs.len() {
            if let Some(wire) = node_ref.inputs[pin as usize].wire {
                return self.shared_state.get_wire(wire);
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
