use uuid::Uuid;
use crate::{ComponentId, GateType, PinId, TickError};

pub trait Simulatable {
    fn id(&self) -> ComponentId;
    fn get_type(&self) -> GateType; // Or a more generic component type
    fn set_input(&mut self, pin: PinId, value: Option<bool>);
    // Consider a method to get all outputs if more suitable
    fn get_output(&self, pin: PinId) -> Option<bool>;
    fn get_inputs(&self) -> Vec<Option<bool>>;
    // Unified method name for compute/tick
    fn run(&mut self) -> Result<(), TickError>;
    // is_dirty/mark_dirty might be relevant for change propagation
    // fn is_dirty(&self) -> bool;
    // fn mark_dirty(&mut self, dirty: bool);
    fn expected_inputs(&self) -> usize;
    // Add a method to get all outputs, useful for circuits
    fn get_all_outputs(&self) -> Vec<Option<bool>>;

    // For downcasting Box<dyn Simulatable> to concrete types if needed,
    // especially for Clone or specific getters not in Simulatable.
    fn as_any(&self) -> &dyn std::any::Any;
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any;
    // To make LogicComponent clonable, you'd add:
    // fn clone_box(&self) -> Box<dyn Simulatable>;
}
