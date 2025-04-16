use ahash::AHashMap;
use uuid::Uuid;

#[cfg(test)]
mod tests;

pub mod circuits;
pub mod gates;
mod logic_component;
pub use logic_component::*;

#[derive(Debug, Copy, Clone, PartialEq)]
pub enum TickError {
    CycleDetected,
}

#[derive(Debug)]
pub struct Circuit {
    pub id: Uuid,
    // pub gate_indices: HashMap<GateId, usize>,
    // pub gates: Vec<Box<dyn LogicComponent>>,
    pub components: AHashMap<ComponentId, Box<dyn LogicComponent>>,
    pub connections: Vec<Connection>,
    topological_order: Option<Vec<ComponentId>>,
    topological_dirty: bool,
    input_gates: AHashMap<PinId, GateId>,
    output_gates: AHashMap<PinId, GateId>,
}

pub trait CircuitTemplate {
    fn template() -> Circuit;
}

pub trait Port {
    fn port_id(&self) -> usize;
}

pub type ComponentId = Uuid;
pub type GateId = ComponentId;
pub type CircuitId = ComponentId;
pub type PinId = usize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PortId {
    pub circuit_id: Uuid,
    pub gate_id: GateId,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum GateType {
    // Special gates
    Input,     // Input gate, constant value
    InputPin,  // Input port, value is set externally via a connection
    Output,    // Output gate, value is read internally
    OutputPin, // Output port, value is read externally via a connection

    // Circuit gate (collection of gates)
    Circuit,

    // Binary gates
    Or,
    And,

    // Unary gates
    Not,
    Identity,
}

#[derive(Debug)]
pub struct Gate {
    pub id: PortId,
    pub gate_type: GateType,
    pub inputs: Vec<Option<bool>>,
    pub output: Option<bool>,
}

#[derive(Debug)]
pub struct Connection {
    pub source: ComponentId,
    pub target: ComponentId,
    pub target_input: PinId,
    pub source_output: PinId,
}
