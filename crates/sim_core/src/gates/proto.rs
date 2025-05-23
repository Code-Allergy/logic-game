use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{GateType, PinId, PortId};

use super::{
    AndLogic, GenericGate, IdentityLogic, InputLogic, InputPinLogic, NotLogic, OrLogic,
    OscillatorLogic, OutputLogic, OutputPinLogic,
};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum AnyGate {
    Input(GenericGate<InputLogic>),
    InputPin(GenericGate<InputPinLogic>),
    Output(GenericGate<OutputLogic>),
    OutputPin(GenericGate<OutputPinLogic>),
    Identity(GenericGate<IdentityLogic>),
    Or(GenericGate<OrLogic>),
    Not(GenericGate<NotLogic>),
    And(GenericGate<AndLogic>),
    Oscillator(GenericGate<OscillatorLogic>),
}

impl AnyGate {
    pub fn new(gate_type: GateType, circuit_id: Uuid) -> Self {
        match gate_type {
            GateType::And => AnyGate::And(GenericGate::<AndLogic>::new(circuit_id)),
            GateType::Or => AnyGate::Or(GenericGate::<OrLogic>::new(circuit_id)),
            GateType::Not => AnyGate::Not(GenericGate::<NotLogic>::new(circuit_id)),
            GateType::Identity => AnyGate::Identity(GenericGate::<IdentityLogic>::new(circuit_id)),
            GateType::Input => AnyGate::Input(GenericGate::<InputLogic>::new(circuit_id)),
            GateType::Output => AnyGate::Output(GenericGate::<OutputLogic>::new(circuit_id)),
            GateType::Oscillator => {
                AnyGate::Oscillator(GenericGate::<OscillatorLogic>::new(circuit_id))
            }
            GateType::Circuit => {
                panic!("Use Circuit::new() to make a sub‐circuit");
            }
            _ => {
                unimplemented!(
                    "Gate was attempted to be created that is not yet supported in AnyGate::new"
                );
            }
        }
    }

    pub fn new_input(circuit_id: Uuid, initial_value: bool) -> Self {
        AnyGate::Input(GenericGate::<InputLogic>::with_initial(
            circuit_id,
            initial_value,
        ))
    }

    pub fn new_input_pin(circuit_id: Uuid) -> Self {
        AnyGate::InputPin(GenericGate::<InputPinLogic>::new(circuit_id))
    }

    pub fn new_output(circuit_id: Uuid) -> Self {
        AnyGate::Output(GenericGate::<OutputLogic>::new(circuit_id))
    }

    pub fn new_identity(circuit_id: Uuid) -> Self {
        AnyGate::Identity(GenericGate::<IdentityLogic>::new(circuit_id))
    }

    pub fn new_oscillator(circuit_id: Uuid, frequency: usize) -> Self {
        AnyGate::Oscillator(GenericGate::<OscillatorLogic>::with_frequency(
            circuit_id, frequency,
        ))
    }

    pub fn new_and(circuit_id: Uuid) -> Self {
        AnyGate::And(GenericGate::<AndLogic>::new(circuit_id))
    }

    pub fn new_or(circuit_id: Uuid) -> Self {
        AnyGate::Or(GenericGate::<OrLogic>::new(circuit_id))
    }

    pub fn new_not(circuit_id: Uuid) -> Self {
        AnyGate::Not(GenericGate::<NotLogic>::new(circuit_id))
    }

    pub fn get_gate_type(&self) -> GateType {
        match self {
            AnyGate::Input(_) => GateType::Input,
            AnyGate::InputPin(_) => GateType::InputPin,
            AnyGate::Output(_) => GateType::Output,
            AnyGate::OutputPin(_) => GateType::OutputPin,
            AnyGate::Identity(_) => GateType::Identity,
            AnyGate::Or(_) => GateType::Or,
            AnyGate::Not(_) => GateType::Not,
            AnyGate::And(_) => GateType::And,
            AnyGate::Oscillator(_) => GateType::Oscillator,
        }
    }

    pub fn get_id(&self) -> PortId {
        match self {
            AnyGate::Input(gate) => gate.id,
            AnyGate::InputPin(gate) => gate.id,
            AnyGate::Output(gate) => gate.id,
            AnyGate::OutputPin(gate) => gate.id,
            AnyGate::Identity(gate) => gate.id,
            AnyGate::Or(gate) => gate.id,
            AnyGate::Not(gate) => gate.id,
            AnyGate::And(gate) => gate.id,
            AnyGate::Oscillator(gate) => gate.id,
        }
    }

    pub fn set_input(&mut self, pin: PinId, value: Option<bool>) {
        match self {
            AnyGate::Input(gate) => gate.set_input(pin, value),
            AnyGate::InputPin(gate) => gate.set_input(pin, value),
            AnyGate::Output(gate) => gate.set_input(pin, value),
            AnyGate::OutputPin(gate) => gate.set_input(pin, value),
            AnyGate::Identity(gate) => gate.set_input(pin, value),
            AnyGate::Or(gate) => gate.set_input(pin, value),
            AnyGate::Not(gate) => gate.set_input(pin, value),
            AnyGate::And(gate) => gate.set_input(pin, value),
            AnyGate::Oscillator(gate) => gate.set_input(pin, value),
        }
    }

    pub fn get_inputs(&self) -> &[Option<bool>] { // Changed return type
        match self {
            AnyGate::Input(gate) => &gate.inputs[..], // Return slice
            AnyGate::InputPin(gate) => &gate.inputs[..],
            AnyGate::Output(gate) => &gate.inputs[..],
            AnyGate::OutputPin(gate) => &gate.inputs[..],
            AnyGate::Identity(gate) => &gate.inputs[..],
            AnyGate::Or(gate) => &gate.inputs[..],
            AnyGate::Not(gate) => &gate.inputs[..],
            AnyGate::And(gate) => &gate.inputs[..],
            AnyGate::Oscillator(gate) => &gate.inputs[..],
        }
    }

    pub fn get_output(&self) -> Option<bool> {
        match self {
            AnyGate::Input(gate) => gate.get_output(),
            AnyGate::InputPin(gate) => gate.get_output(),
            AnyGate::Output(gate) => gate.get_output(),
            AnyGate::OutputPin(gate) => gate.get_output(),
            AnyGate::Identity(gate) => gate.get_output(),
            AnyGate::Or(gate) => gate.get_output(),
            AnyGate::Not(gate) => gate.get_output(),
            AnyGate::And(gate) => gate.get_output(),
            AnyGate::Oscillator(gate) => gate.get_output(),
        }
    }

    pub fn expected_inputs(&self) -> usize {
        match self {
            AnyGate::Input(gate) => gate.expected_inputs(),
            AnyGate::InputPin(gate) => gate.expected_inputs(),
            AnyGate::Output(gate) => gate.expected_inputs(),
            AnyGate::OutputPin(gate) => gate.expected_inputs(),
            AnyGate::Identity(gate) => gate.expected_inputs(),
            AnyGate::Or(gate) => gate.expected_inputs(),
            AnyGate::Not(gate) => gate.expected_inputs(),
            AnyGate::And(gate) => gate.expected_inputs(),
            AnyGate::Oscillator(gate) => gate.expected_inputs(),
        }
    }

    pub fn compute(&mut self) {
        debug_assert_eq!(self.get_inputs().len(), self.expected_inputs());
        match self {
            AnyGate::Input(gate) => gate.compute(),
            AnyGate::InputPin(gate) => gate.compute(),
            AnyGate::Output(gate) => gate.compute(),
            AnyGate::OutputPin(gate) => gate.compute(),
            AnyGate::Identity(gate) => gate.compute(),
            AnyGate::Or(gate) => gate.compute(),
            AnyGate::Not(gate) => gate.compute(),
            AnyGate::And(gate) => gate.compute(),
            AnyGate::Oscillator(gate) => gate.compute(),
        }
    }
}
