use crate::circuit::{NodeId, WireId};

/// Core contract for logic simulation runners.
///
/// Keeps a minimal API surface so lower-level execution models (SIMD, JIT, Event-driven)
/// do not need to carry high-level wire map overhead during execution.
pub trait Runner: Send {
    fn set_input(&mut self, node: NodeId, pin: u8, value: bool);
    fn tick(&mut self);
    fn output(&self, node: NodeId, pin: u8) -> bool;

    /// Returns the number of ticks that have elapsed since the runner was created.
    ///
    /// This is incremented once per `tick()` call and is the canonical source of
    /// simulation time used to drive time-based nodes (e.g. `NodeKind::Clock`).
    /// Because it only depends on how many ticks have been processed, stepping a
    /// runner deterministically (e.g. in tests) reproduces identical results.
    fn current_tick(&self) -> u64;
}

/// Optional extension trait for runners that support detailed wire-level state inspection
/// and debugging (e.g. for editor wire visualization).
pub trait InspectableRunner: Runner + Send {
    fn wire_value(&self, wire: WireId) -> bool;
    fn for_each_wire(&self, f: &mut dyn FnMut(WireId, bool));

    /// Snapshot all execution wires in representation order. Runners compiled
    /// from the same logical layout can use this to retain feedback state.
    fn execution_state(&self) -> Vec<bool>;

    /// Restore an execution snapshot, returning false when layouts differ.
    fn restore_execution_state(&mut self, state: &[bool]) -> bool;

    /// Restore a wire through its stable circuit identity where available.
    fn restore_wire_value(&mut self, wire: WireId, value: bool);

    /// Preserve clock phase across representation changes.
    fn set_current_tick(&mut self, tick: u64);
}
