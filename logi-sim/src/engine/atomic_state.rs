use crate::circuit::{NodeId, WireId};
use slotmap::SecondaryMap;
use std::collections::HashMap;
use std::sync::RwLock;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Thread-safe shared simulation state snapshot.
/// Allows background simulation runner thread to update state while main/Bevy threads
/// query wire/pin states atomically and with minimal lock overhead.
pub struct SharedState {
    pub tick_count: AtomicU64,
    pub is_running: AtomicBool,
    pub wire_states: RwLock<SecondaryMap<WireId, bool>>,
    pub driven_inputs: RwLock<HashMap<NodeId, bool>>,
}

impl SharedState {
    pub fn new() -> Self {
        Self {
            tick_count: AtomicU64::new(0),
            is_running: AtomicBool::new(false),
            wire_states: RwLock::new(SecondaryMap::new()),
            driven_inputs: RwLock::new(HashMap::new()),
        }
    }

    /// Atomically reads the value of a specific wire.
    pub fn get_wire(&self, wire: WireId) -> bool {
        if let Ok(lock) = self.wire_states.read() {
            lock.get(wire).copied().unwrap_or(false)
        } else {
            false
        }
    }

    /// Reads total completed simulation tick count.
    pub fn tick_count(&self) -> u64 {
        self.tick_count.load(Ordering::Relaxed)
    }

    /// Checks if the worker thread simulation is currently active.
    pub fn is_running(&self) -> bool {
        self.is_running.load(Ordering::Relaxed)
    }

    /// Atomically reads the value of a specific driven input node.
    pub fn get_driven_input(&self, node: NodeId) -> bool {
        if let Ok(lock) = self.driven_inputs.read() {
            lock.get(&node).copied().unwrap_or(false)
        } else {
            false
        }
    }
}

impl Default for SharedState {
    fn default() -> Self {
        Self::new()
    }
}
