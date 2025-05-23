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
            GateType::InputPin => AnyGate::InputPin(GenericGate::<InputPinLogic>::new(circuit_id)),
            GateType::Output => AnyGate::Output(GenericGate::<OutputLogic>::new(circuit_id)),
            GateType::OutputPin => AnyGate::OutputPin(GenericGate::<OutputPinLogic>::new(circuit_id)),
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
}

macro_rules! impl_any_gate_methods {
    ($($variant:ident),*) => {
        impl AnyGate {
            pub fn get_id(&self) -> PortId {
                match self {
                    $(AnyGate::$variant(gate) => gate.id,)*
                }
            }

            pub fn set_input(&mut self, pin: PinId, value: Option<bool>) {
                match self {
                    $(AnyGate::$variant(gate) => gate.set_input(pin, value),)*
                }
            }

            pub fn get_inputs(&self) -> Vec<Option<bool>> {
                match self {
                    $(AnyGate::$variant(gate) => gate.inputs.clone(),)*
                }
            }

            pub fn get_output(&self) -> Option<bool> {
                match self {
                    $(AnyGate::$variant(gate) => gate.get_output(),)*
                }
            }

            pub fn expected_inputs(&self) -> usize {
                match self {
                    $(AnyGate::$variant(gate) => gate.expected_inputs(),)*
                }
            }

            pub fn compute(&mut self) {
                debug_assert_eq!(self.get_inputs().len(), self.expected_inputs());
                match self {
                    $(AnyGate::$variant(gate) => gate.compute(),)*
                }
            }
        }
    };
}

impl_any_gate_methods!(
    Input,
    InputPin,
    Output,
    OutputPin,
    Identity,
    Or,
    Not,
    And,
    Oscillator
);

use crate::Simulatable;
use crate::TickError;

impl Simulatable for AnyGate {
    fn id(&self) -> Uuid {
        self.get_id().gate_id
    }

    fn get_type(&self) -> GateType {
        self.get_gate_type()
    }

    fn set_input(&mut self, pin: PinId, value: Option<bool>) {
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

    fn get_output(&self, _pin: PinId) -> Option<bool> {
        // AnyGate typically has a single output, pin is ignored.
        // If specific pin output is needed, this match needs to be more granular
        // or GenericGate needs a get_output_by_pin method.
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

    fn get_inputs(&self) -> Vec<Option<bool>> {
        match self {
            AnyGate::Input(gate) => gate.inputs.clone(),
            AnyGate::InputPin(gate) => gate.inputs.clone(),
            AnyGate::Output(gate) => gate.inputs.clone(),
            AnyGate::OutputPin(gate) => gate.inputs.clone(),
            AnyGate::Identity(gate) => gate.inputs.clone(),
            AnyGate::Or(gate) => gate.inputs.clone(),
            AnyGate::Not(gate) => gate.inputs.clone(),
            AnyGate::And(gate) => gate.inputs.clone(),
            AnyGate::Oscillator(gate) => gate.inputs.clone(),
        }
    }

    fn run(&mut self) -> Result<(), TickError> {
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
        Ok(())
    }

    fn expected_inputs(&self) -> usize {
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

    fn get_all_outputs(&self) -> Vec<Option<bool>> {
        // For AnyGate, which represents single-output gates,
        // we return a vec containing the single output.
        vec![self.get_output()] // get_output for AnyGate takes no arguments.
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    // If Simulatable had clone_box:
    // fn clone_box(&self) -> Box<dyn Simulatable> {
    //     Box::new(self.clone()) // Requires AnyGate to be Clone
    // }
}
