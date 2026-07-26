use crate::circuit::{NodeId, WireId};

/// Core contract for logic simulation runners.
///
/// Keeps a minimal API surface so lower-level execution models (SIMD, JIT, Event-driven)
/// do not need to carry high-level wire map overhead during execution.
pub trait Runner {
    fn set_input(&mut self, node: NodeId, pin: u8, value: bool);
    fn tick(&mut self);
    fn output(&self, node: NodeId, pin: u8) -> bool;
}

/// Optional extension trait for runners that support detailed wire-level state inspection
/// and debugging (e.g. for editor wire visualization).
pub trait InspectableRunner: Runner {
    fn wire_value(&self, wire: WireId) -> bool;
    fn for_each_wire(&self, f: &mut dyn FnMut(WireId, bool));
}


