use crate::gates::proto::AnyGate;
use ahash::AHashMap;
use serde::{Deserialize, Serialize};
use std::fmt::Debug;
use uuid::Uuid;

pub mod circuits;
pub mod gates;
mod traits;

pub use traits::Simulatable;

/// Unique identifier for a component in the simulation.
/// Either a gate ID or a circuit ID.
pub type ComponentId = Uuid;
/// Unique identifier for a gate in the simulation.
pub type GateId = ComponentId;
/// Unique identifier for a circuit in the simulation.
pub type CircuitId = ComponentId;
/// Identifier for a pin in the simulation, unique to the particular circuit.
pub type PinId = usize;

#[derive(Debug, Copy, Clone, PartialEq)]
pub enum TickError {
    CycleDetected,
    InputGateMismatch,
    OutputGateMismatch,
}

// LogicComponent is now a struct holding a trait object.
// Derives for Clone, Serialize, Deserialize are removed for now as they are complex for Box<dyn Simulatable>.
// #[derive(Clone, Debug, Serialize, Deserialize)]
// #[serde(tag = "type")]
pub struct LogicComponent {
    pub component: Box<dyn Simulatable>,
}

// Manual Debug implementation for LogicComponent
impl Debug for LogicComponent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LogicComponent")
            .field("id", &self.component.id()) // Assuming Simulatable has id()
            .field("type", &self.component.get_type()) // Assuming Simulatable has get_type()
            .finish()
    }
}

// To make LogicComponent Cloneable, Simulatable trait would need a `clone_box(&self) -> Box<dyn Simulatable>` method,
// and each implementor (AnyGate, Circuit) would need to implement it.
// Example:
// pub trait Simulatable {
//     /* ... other methods ... */
//     fn clone_box(&self) -> Box<dyn Simulatable>;
// }
// impl Clone for LogicComponent {
//     fn clone(&self) -> Self {
//         LogicComponent { component: self.component.clone_box() }
//     }
// }
// Similar considerations apply for Serialize/Deserialize using libraries like `typetag`.

impl LogicComponent {
    pub fn new(component: Box<dyn Simulatable>) -> Self {
        Self { component }
    }

    // Delegate all Simulatable methods to the contained component.
    pub fn id(&self) -> ComponentId {
        self.component.id()
    }

    pub fn get_type(&self) -> GateType {
        self.component.get_type()
    }

    pub fn set_input(&mut self, pin: PinId, value: Option<bool>) {
        self.component.set_input(pin, value)
    }

    pub fn get_output(&self, pin: PinId) -> Option<bool> {
        self.component.get_output(pin)
    }

    pub fn get_inputs(&self) -> Vec<Option<bool>> {
        self.component.get_inputs()
    }

    pub fn run(&mut self) -> Result<(), TickError> {
        self.component.run()
    }

    pub fn expected_inputs(&self) -> usize {
        self.component.expected_inputs()
    }

    pub fn get_all_outputs(&self) -> Vec<Option<bool>> {
        self.component.get_all_outputs()
    }

    // Methods like get_input that were specific to LogicComponent::Gate(AnyGate::Input(_))
    // are removed. If such specific access is needed, it would require downcasting
    // component.as_any().downcast_ref::<AnyGate>() and then matching on the AnyGate variant.
    // Or, the functionality might be better exposed via the Simulatable trait if generally applicable.
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum GateType {
    // Special gates
    Input, // Input gate, constant value
    // InputArray,  // Input gate, array of values
    InputPin, // Input port, value is set externally via a connection
    Output,   // Output gate, value is read internally
    // OutputArray, // Output gate, array of values
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

// #[derive(Debug, Clone, Serialize, Deserialize)] // Clone, Serialize, Deserialize removed due to LogicComponent holding Box<dyn Simulatable>
#[derive(Debug)] // Keeping Debug for now
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
