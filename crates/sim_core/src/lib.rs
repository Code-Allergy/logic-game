use ahash::AHashMap;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[cfg(test)]
mod tests;

pub mod circuits;
pub mod gates;

pub type ComponentId = Uuid;
pub type GateId = ComponentId;
pub type CircuitId = ComponentId;
pub type PinId = usize;

#[derive(Debug, Copy, Clone, PartialEq)]
pub enum TickError {
    CycleDetected,
    InputGateMismatch,
    OutputGateMismatch,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum LogicComponent {
    Gate(Gate),
    Circuit(Circuit),
}

// Generic wrapper for a logic component
impl LogicComponent {
    pub fn id(&self) -> ComponentId {
        match self {
            LogicComponent::Gate(gate) => gate.id.gate_id,
            LogicComponent::Circuit(circuit) => circuit.id,
        }
    }

    pub fn get_type(&self) -> GateType {
        match self {
            LogicComponent::Gate(gate) => gate.gate_type,
            LogicComponent::Circuit(_) => GateType::Circuit,
        }
    }

    pub fn set_input(&mut self, pin: PinId, value: Option<bool>) {
        match self {
            LogicComponent::Gate(gate) => {
                if pin < gate.inputs.len() {
                    gate.inputs[pin] = value;
                }
            }
            LogicComponent::Circuit(circuit) => {
                if let Some(gate_id) = circuit.input_gates.get(&pin) {
                    if let Some(gate) = circuit.components.get_mut(gate_id) {
                        gate.set_input(0, value);
                    }
                }
            }
        }
    }

    pub fn get_input(&self, pin: PinId) -> Option<bool> {
        match self {
            LogicComponent::Gate(gate) => gate.inputs.get(pin).cloned().flatten(),
            LogicComponent::Circuit(circuit) => {
                if let Some(gate_id) = circuit.input_gates.get(&pin) {
                    if let Some(gate) = circuit.components.get(gate_id) {
                        return gate.get_input(0);
                    }
                }
                None
            }
        }
    }

    pub fn get_output(&self, pin: PinId) -> Option<bool> {
        match self {
            LogicComponent::Gate(gate) => gate.output,
            LogicComponent::Circuit(circuit) => {
                if let Some(gate_id) = circuit.output_gates.get(&pin) {
                    if let Some(gate) = circuit.components.get(gate_id) {
                        return gate.get_output(0);
                    }
                }
                None
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum GateType {
    // Special gates
    Input,     // Input gate, constant value
    InputPin,  // Input port, value is set externally via a connection
    Output,    // Output gate, value is read internally
    OutputPin, // Output port, value is read externally via a connection

    // Oscillator
    Oscillator,

    // Circuit gate (collection of gates)
    Circuit,

    // Binary gates
    Or,
    And,

    // Unary gates
    Not,
    Identity,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Circuit {
    pub id: Uuid,
    pub components: AHashMap<ComponentId, LogicComponent>,
    pub connections: Vec<Connection>,

    /* Topological sort for tick() and execute() */
    topological_order: Option<Vec<ComponentId>>,
    topological_dirty: bool,

    /* Port Config */
    input_gates: AHashMap<PinId, GateId>,
    output_gates: AHashMap<PinId, GateId>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Gate {
    pub id: PortId,
    pub gate_type: GateType,
    pub gate_data: GateData,
    pub inputs: Vec<Option<bool>>,
    pub output: Option<bool>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum GateData {
    Oscillator { frequency: usize, tick: usize },
    None,
}

#[derive(Copy, Clone, Debug, Serialize, Deserialize)]
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
