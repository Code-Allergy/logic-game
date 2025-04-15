use uuid::Uuid;

use crate::circuits::PortId;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum GateType {
    // Special gates
    Input,     // Input gate, constant value
    InputPin,  // Input port, value is set externally via a connection
    Output,    // Output gate, value is read internally
    OutputPin, // Output port, value is read externally via a connection

    // Binary gates
    Or,
    And,

    // Unary gates
    Not,
    Identity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GateId(pub usize);

#[derive(Debug)]
pub struct Gate {
    pub id: PortId,
    pub gate_type: GateType,
    pub inputs: Vec<Option<bool>>,
    pub output: Option<bool>,
}

#[derive(Debug)]
pub struct Connection {
    pub source: usize,
    pub target: usize,
    pub target_input: usize, // ID of the pin on the target gate
}

impl Connection {
    pub fn new(source: usize, target: usize, target_input: usize) -> Self {
        Connection {
            source,
            target,
            target_input,
        }
    }
}

impl Gate {
    pub fn new(
        circuit_id: Uuid,
        gate_id: GateId,
        gate_type: GateType,
        inputs: Vec<Option<bool>>,
        output: Option<bool>,
    ) -> Self {
        Gate {
            id: PortId {
                circuit_id,
                gate_id,
            },
            gate_type,
            inputs,
            output,
        }
    }

    pub fn new_input(circuit_id: Uuid, id: GateId, initial_value: Option<bool>) -> Self {
        Gate::new(circuit_id, id, GateType::Input, vec![initial_value], None)
    }

    pub fn new_output(circuit_id: Uuid, id: GateId) -> Self {
        Gate::new(circuit_id, id, GateType::Output, vec![None], None)
    }

    pub fn new_or(circuit_id: Uuid, id: GateId) -> Self {
        Gate::new(circuit_id, id, GateType::Or, vec![None, None], None)
    }

    pub fn new_and(circuit_id: Uuid, id: GateId) -> Self {
        Gate::new(circuit_id, id, GateType::And, vec![None, None], None)
    }

    pub fn new_not(circuit_id: Uuid, id: GateId) -> Self {
        Gate::new(circuit_id, id, GateType::Not, vec![None], None)
    }

    pub fn new_identity(circuit_id: Uuid, id: GateId) -> Self {
        Gate::new(circuit_id, id, GateType::Identity, vec![None], None)
    }
}

pub fn compute_gate_logic(gate_type: GateType, inputs: Vec<bool>) -> bool {
    debug_assert!(!inputs.is_empty(), "Inputs should not be empty");

    match gate_type {
        GateType::And => {
            debug_assert_eq!(
                inputs.len(),
                2,
                "AND gate requires exactly 2 inputs, found: {}",
                inputs.len()
            );
            inputs.into_iter().all(|value| value)
        }
        GateType::Or => {
            debug_assert_eq!(
                inputs.len(),
                2,
                "OR gate requires exactly 2 inputs, found: {}",
                inputs.len()
            );
            inputs.into_iter().any(|value| value)
        }
        GateType::Identity => {
            debug_assert_eq!(
                inputs.len(),
                1,
                "Identity gate requires exactly 1 input, found: {}",
                inputs.len()
            );
            inputs[0]
        }
        GateType::Not => {
            debug_assert_eq!(
                inputs.len(),
                1,
                "NOT gate requires exactly 1 input, found: {}",
                inputs.len()
            );
            !inputs[0]
        }
        GateType::Input => {
            debug_assert_eq!(
                inputs.len(),
                1,
                "Input gate requires exactly 1 input, found: {}",
                inputs.len()
            );
            inputs[0]
        }
        GateType::Output => {
            debug_assert_eq!(inputs.len(), 1, "Output gate requires exactly 1 input");
            inputs[0]
        }
        GateType::InputPin => {
            todo!("Input port logic not implemented yet")
        }
        GateType::OutputPin => {
            todo!("Output port logic not implemented yet")
        }
    }
}
