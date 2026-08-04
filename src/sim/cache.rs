//! `SimCache` – Per-frame snapshot of simulation state populated from [`SimEvent`]s.
//!
//! ## Why this exists
//!
//! The old approach called `bridge.wire_value()` / `bridge.node_output()` /
//! `bridge.node_input()` from every rendering system on every frame.  Each of
//! those calls acquires an `RwLock` against the background worker thread, which
//! is expensive and couples rendering tightly to IPC timing.
//!
//! `SimCache` solves this by draining all [`SimEvent`]s **once** in `PreUpdate`
//! (inside [`crate::sim::systems::bridge_flush_system`]) and projecting them
//! into a plain `HashMap` snapshot.  All `Update` systems then read from the
//! cache with zero cross-thread overhead.
//!
//! ## Reverse index
//!
//! The simulator emits [`SimEvent::WireStateChanged`] keyed by [`WireId`] (the
//! sim-internal key).  To update `node_outputs` and `node_inputs` we need to
//! know which node/pin each wire touches.  [`SimCache::rebuild_index`] builds
//! that reverse lookup from the game-side [`GameCircuit`] + [`SimBridge`] ID
//! maps whenever the circuit changes.

use std::collections::HashMap;

use bevy::log::trace;
use bevy::prelude::*;
use uuid::Uuid;

use logi_sim::engine::SimEvent;
use logi_sim::prelude::WireId;

use crate::sim::SimulationConfig;

use super::bridge::SimBridge;
use super::model::GameCircuit;

// ─────────────────────────────────────────────────────────────────────────────
// Reverse-index entry
// ─────────────────────────────────────────────────────────────────────────────

/// Which end of a wire a particular node/pin sits on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PinDirection {
    /// This node drives the wire (source / output pin).
    Output,
    /// This node is driven by the wire (sink / input pin).
    Input,
}

/// One side of a wire in game-space.
#[derive(Debug, Clone)]
struct WirePinEntry {
    node_uuid: Uuid,
    pin: u8,
    direction: PinDirection,
    /// UUID of the wire itself (for `wire_values` updates).
    wire_uuid: Uuid,
}

// ─────────────────────────────────────────────────────────────────────────────
// SimCache
// ─────────────────────────────────────────────────────────────────────────────

/// Bevy resource that caches the last-known simulation state in game-UUID space.
///
/// Updated once per frame in `PreUpdate` by draining `SimEvent`s from the
/// bridge.  All `Update` systems read from this resource instead of making
/// cross-thread calls into [`SimBridge`].
#[derive(Resource, Default)]
pub struct SimCache {
    // ── Public per-frame snapshot ─────────────────────────────────────────────
    /// Current boolean state of each wire, keyed by game-side wire UUID.
    pub wire_values: HashMap<Uuid, bool>,

    /// Current output-pin state for each node, keyed by `(node_uuid, pin_index)`.
    pub node_outputs: HashMap<(Uuid, u8), bool>,

    /// Current input-pin state for each node, keyed by `(node_uuid, pin_index)`.
    pub node_inputs: HashMap<(Uuid, u8), bool>,

    /// Whether the simulation worker is currently running.
    pub is_running: bool,

    // ── Internal reverse index (WireId → game-space entries) ──────────────────
    /// Maps a sim-internal `WireId` to all the game-space node/pin pairs that
    /// are connected to that wire.  Built by [`Self::rebuild_index`].
    wire_reverse: HashMap<WireId, Vec<WirePinEntry>>,
}

impl SimCache {
    // ─────────────────────────────────────────────────────────────────────────
    // Index management
    // ─────────────────────────────────────────────────────────────────────────

    /// Rebuild the `WireId → WirePinEntry` reverse index from the current
    /// [`GameCircuit`] and the [`SimBridge`] ID tables.
    ///
    /// Call this whenever the circuit changes (detected via Bevy's
    /// `is_changed()` on [`GameCircuit`]).
    pub fn rebuild_index(&mut self, circuit: &GameCircuit, bridge: &SimBridge) {
        self.wire_reverse.clear();

        for wire in circuit.wires.values() {
            let Some(wire_id) = bridge.wire_id(wire.id) else {
                continue; // wire not yet registered in the sim
            };

            let entries = self.wire_reverse.entry(wire_id).or_default();

            // Source side → output pin
            entries.push(WirePinEntry {
                node_uuid: wire.source_node,
                pin: wire.source_pin,
                direction: PinDirection::Output,
                wire_uuid: wire.id,
            });

            // Sink side → input pin
            entries.push(WirePinEntry {
                node_uuid: wire.sink_node,
                pin: wire.sink_pin,
                direction: PinDirection::Input,
                wire_uuid: wire.id,
            });
        }
    }

    // ─────────────────────────────────────────────────────────────────────
    // Snapshot (once per frame)
    // ─────────────────────────────────────────────────────────────────────

    /// Refresh the cache from the simulator's shared state with a **single
    /// `RwLock::read()`** per frame.
    ///
    /// This is O(N wires) regardless of how many simulation ticks ran since the
    /// last Bevy frame.  At high or unlimited tick rates this is far cheaper
    /// than accumulating one `WireStateChanged` event per wire transition.
    pub fn update_from_snapshot(&mut self, bridge: &SimBridge) {
        bridge.snapshot_wire_states(&mut |wire_id, val| {
            if let Some(entries) = self.wire_reverse.get(&wire_id) {
                for entry in entries {
                    self.wire_values.insert(entry.wire_uuid, val);
                    match entry.direction {
                        PinDirection::Output => {
                            self.node_outputs.insert((entry.node_uuid, entry.pin), val);
                        }
                        PinDirection::Input => {
                            self.node_inputs.insert((entry.node_uuid, entry.pin), val);
                        }
                    }
                }
            }
        });
    }

    // ─────────────────────────────────────────────────────────────────────
    // Event application (lifecycle events only)
    // ─────────────────────────────────────────────────────────────────────

    /// Apply lifecycle [`SimEvent`]s (pause/resume/stop and manual step).
    ///
    /// Wire state is handled by [`Self::update_from_snapshot`] instead.
    pub fn apply_events(&mut self, events: &[SimEvent], sim_config: &mut SimulationConfig) {
        for event in events {
            match event {
                SimEvent::RunnerSet(runner_type) => {
                    sim_config.current_runner = *runner_type;
                }
                SimEvent::StatusChanged { is_running } => {
                    self.is_running = *is_running;
                }
                SimEvent::TickCompleted { .. } => {
                    trace!("Sim Step completed");
                    // Currently informational only; consumed so the channel
                    // doesn't grow. Callers that care about step completion
                    // can extend this branch.
                }
            }
        }
    }

    /// Clear all cached state (e.g. on circuit reload).
    pub fn clear(&mut self) {
        self.wire_values.clear();
        self.node_outputs.clear();
        self.node_inputs.clear();
        self.wire_reverse.clear();
        self.is_running = false;
    }
}
