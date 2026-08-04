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

use bevy::log::{info, trace};
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
    wire_bit: u8,
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
    // ── Internal per-frame snapshot (opaque to callers) ─────────────────────
    /// Current packed state of each logical wire, keyed by game-side UUID.
    wire_values: HashMap<Uuid, u64>,

    /// Current output-pin state for each node, keyed by `(node_uuid, pin_index)`.
    node_outputs: HashMap<(Uuid, u8), bool>,

    /// Current input-pin state for each node, keyed by `(node_uuid, pin_index)`.
    node_inputs: HashMap<(Uuid, u8), bool>,

    /// Whether the simulation worker is currently running.
    is_running: bool,

    /// Optional debug output for boundary mapping issues.
    trace_mapping: bool,

    // ── Internal reverse index (WireId → game-space entries) ──────────────────
    /// Maps a sim-internal `WireId` to all the game-space node/pin pairs that
    /// are connected to that wire.  Built by [`Self::rebuild_index`].
    wire_reverse: HashMap<WireId, Vec<WirePinEntry>>,
}

impl SimCache {
    /// Enable or disable trace output for bridge/cache translation boundaries.
    pub fn set_trace_mapping(&mut self, enabled: bool) {
        self.trace_mapping = enabled;
    }

    /// Whether the simulation runner is currently running.
    pub fn is_running(&self) -> bool {
        self.is_running
    }

    /// Return the cached per-node input bits as a compact bitmask.
    pub fn node_input_bits(&self, node_id: Uuid) -> u64 {
        let mut bits = 0_u64;
        for bit in 0..64 {
            if self
                .node_inputs
                .get(&(node_id, bit as u8))
                .copied()
                .unwrap_or(false)
            {
                bits |= 1_u64 << bit;
            }
        }
        bits
    }

    /// Return the cached per-node output bits as a compact bitmask.
    pub fn node_output_bits(&self, node_id: Uuid) -> u64 {
        let mut bits = 0_u64;
        for bit in 0..64 {
            if self
                .node_outputs
                .get(&(node_id, bit as u8))
                .copied()
                .unwrap_or(false)
            {
                bits |= 1_u64 << bit;
            }
        }
        bits
    }

    /// Return a cached wire value.
    pub fn wire_value(&self, wire_id: Uuid) -> bool {
        self.wire_bits(wire_id) != 0
    }

    pub fn wire_bits(&self, wire_id: Uuid) -> u64 {
        self.wire_values.get(&wire_id).copied().unwrap_or(0)
    }

    pub(crate) fn set_running(&mut self, running: bool) {
        self.is_running = running;
    }

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
            let Some(segments) = bridge.wire_segments(wire.id) else {
                if self.trace_mapping {
                    trace!("SimCache: missing cached segments for wire {:?}", wire.id);
                }
                continue;
            };

            for (wire_bit, segment) in segments.iter().enumerate() {
                let wire_bit = u8::try_from(wire_bit).unwrap_or(63).min(63);
                let entries = self.wire_reverse.entry(segment.wire_id).or_default();
                entries.push(WirePinEntry {
                    node_uuid: wire.source_node,
                    pin: segment.source_pin,
                    direction: PinDirection::Output,
                    wire_uuid: wire.id,
                    wire_bit,
                });
                entries.push(WirePinEntry {
                    node_uuid: wire.sink_node,
                    pin: segment.sink_pin,
                    direction: PinDirection::Input,
                    wire_uuid: wire.id,
                    wire_bit,
                });
            }
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
        self.wire_values.clear();
        self.node_outputs.clear();
        self.node_inputs.clear();

        let mut missing = 0_u64;
        let mut mapped = 0_u64;
        let mut multi_pin_wires = 0_u64;

        bridge.snapshot_wire_states(&mut |wire_id, val| {
            if let Some(entries) = self.wire_reverse.get(&wire_id) {
                mapped += 1;
                if entries.len() > 1 {
                    multi_pin_wires += 1;
                }
                for entry in entries {
                    if val {
                        *self.wire_values.entry(entry.wire_uuid).or_default() |=
                            1_u64 << entry.wire_bit;
                    }
                    match entry.direction {
                        PinDirection::Output => {
                            self.node_outputs.insert((entry.node_uuid, entry.pin), val);
                        }
                        PinDirection::Input => {
                            self.node_inputs.insert((entry.node_uuid, entry.pin), val);
                        }
                    }
                }
            } else if self.trace_mapping {
                missing += 1;
            }
        });

        if self.trace_mapping && (missing > 0 || multi_pin_wires > 0) {
            info!(
                "SimCache trace: snapshot wires mapped {mapped}, unmapped {missing}, multi-pin wires {multi_pin_wires}"
            );
        }
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

    /// Pull latest simulation state and lifecycle events from the bridge in one
    /// call. This keeps callers from depending on bridge internals.
    pub(crate) fn sync_with_bridge(
        &mut self,
        bridge: &SimBridge,
        sim_config: &mut SimulationConfig,
    ) {
        self.set_running(bridge.is_running());
        self.update_from_snapshot(bridge);
        let events = bridge.drain_events();
        self.apply_events(&events, sim_config);
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
