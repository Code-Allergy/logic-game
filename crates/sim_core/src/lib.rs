use ahash::AHashMap;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[cfg(test)]
mod tests;

pub mod circuits;
pub mod gates;
mod logic_component;
pub use logic_component::*;

pub type ComponentId = Uuid;
pub type GateId = ComponentId;
pub type CircuitId = ComponentId;
pub type PinId = usize;

#[derive(Debug, Copy, Clone, PartialEq)]
pub enum TickError {
    CycleDetected,
}

// #[derive(Serialize, Deserialize)]
// #[serde(tag = "type")]
pub enum Component {
    Gate(Gate),
    Circuit(Circuit),
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
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
pub struct Circuit {
    pub id: Uuid,
    pub components: AHashMap<ComponentId, Box<dyn LogicComponent>>,
    pub connections: Vec<Connection>,

    /* Topological sort for tick() and execute() */
    topological_order: Option<Vec<ComponentId>>,
    topological_dirty: bool,

    /* Port Config */
    input_gates: AHashMap<PinId, GateId>,
    output_gates: AHashMap<PinId, GateId>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Gate {
    pub id: PortId,
    pub gate_type: GateType,
    pub inputs: Vec<Option<bool>>,
    pub output: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Connection {
    pub source: ComponentId,
    pub target: ComponentId,
    pub target_input: PinId,
    pub source_output: PinId,
}

pub trait CircuitTemplate {
    fn template() -> Circuit;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PortId {
    pub circuit_id: Uuid,
    pub gate_id: GateId,
}
