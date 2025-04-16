use uuid::Uuid;

use crate::{ComponentId, Connection, Gate, GateType, PinId, PortId};

impl Connection {
    pub fn new(source: ComponentId, target: ComponentId, target_input: PinId) -> Self {
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
        gate_type: GateType,
        inputs: Vec<Option<bool>>,
        output: Option<bool>,
    ) -> Self {
        Gate {
            id: PortId {
                circuit_id,
                gate_id: Uuid::new_v4(),
            },
            gate_type,
            inputs,
            output,
        }
    }

    pub fn new_input(circuit_id: Uuid, initial_value: Option<bool>) -> Self {
        Gate::new(circuit_id, GateType::Input, vec![initial_value], None)
    }

    pub fn new_output(circuit_id: Uuid) -> Self {
        Gate::new(circuit_id, GateType::Output, vec![None], None)
    }

    pub fn new_or(circuit_id: Uuid) -> Self {
        Gate::new(circuit_id, GateType::Or, vec![None, None], None)
    }

    pub fn new_and(circuit_id: Uuid) -> Self {
        Gate::new(circuit_id, GateType::And, vec![None, None], None)
    }

    pub fn new_not(circuit_id: Uuid) -> Self {
        Gate::new(circuit_id, GateType::Not, vec![None], None)
    }

    pub fn new_identity(circuit_id: Uuid) -> Self {
        Gate::new(circuit_id, GateType::Identity, vec![None], None)
    }
}

pub fn compute_gate_logic(gate_type: GateType, inputs: &Vec<bool>) -> bool {
    debug_assert!(!inputs.is_empty(), "Inputs should not be empty");

    match gate_type {
        GateType::And => {
            debug_assert_eq!(
                inputs.len(),
                2,
                "AND gate requires exactly 2 inputs, found: {}",
                inputs.len()
            );
            inputs.into_iter().all(|value| *value)
        }
        GateType::Or => {
            debug_assert_eq!(
                inputs.len(),
                2,
                "OR gate requires exactly 2 inputs, found: {}",
                inputs.len()
            );
            inputs.into_iter().any(|value| *value)
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
        GateType::Circuit => panic!(
            "GateType::Circuit should not be computed, use Circuit::execute() or Circuit::tick() to get the result"
        ),
    }
}
