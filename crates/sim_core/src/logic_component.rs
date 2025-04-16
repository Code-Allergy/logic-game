use crate::{Circuit, CircuitId, ComponentId, Gate, GateId, GateType, PinId};
use std::any::Any;

pub trait LogicComponent: std::fmt::Debug + Send + Sync {
    fn id(&self) -> ComponentId;
    fn get_type(&self) -> GateType;

    fn get_input(&self, pin: PinId) -> Option<bool>;
    fn get_inputs(&self) -> Vec<Option<bool>>;

    fn get_output(&self, pin: PinId) -> Option<bool>;
    fn get_outputs(&self) -> Vec<Option<bool>>;

    fn set_input(&mut self, pin: PinId, value: Option<bool>);
    fn set_inputs(&mut self, values: Vec<Option<bool>>);
    fn set_output(&mut self, pin: PinId, value: Option<bool>);
    fn set_outputs(&mut self, values: Vec<Option<bool>>);

    // Downcasting
    fn as_any(&self) -> &dyn Any;
    fn as_any_mut(&mut self) -> &mut dyn Any;
}

impl LogicComponent for Circuit {
    fn id(&self) -> CircuitId {
        self.id
    }
    fn get_type(&self) -> GateType {
        GateType::Circuit
    }

    fn get_input(&self, pin: PinId) -> Option<bool> {
        if let Some(gate_id) = self.input_gates.get(&pin) {
            if let Some(gate) = self.components.get(gate_id) {
                return gate.get_input(0);
            }
        }
        None
    }
    fn get_inputs(&self) -> Vec<Option<bool>> {
        let mut pins: Vec<(PinId, Option<bool>)> = self
            .input_gates
            .iter()
            .filter_map(|(&pin, gate_id)| {
                if let Some(component) = self.components.get(gate_id) {
                    if component.get_type() == GateType::InputPin {
                        return Some((pin, component.get_input(0)));
                    }
                }
                None
            })
            .collect();

        // Sort by pin number if needed
        pins.sort_by_key(|&(pin, _)| pin);

        // Extract just the values
        pins.into_iter().map(|(_, value)| value).collect()
    }

    fn get_output(&self, pin: PinId) -> Option<bool> {
        if let Some(gate_id) = self.output_gates.get(&pin) {
            if let Some(gate) = self.components.get(gate_id) {
                return gate.get_input(0);
            }
        }
        None
    }
    fn get_outputs(&self) -> Vec<Option<bool>> {
        let mut pins: Vec<(PinId, Option<bool>)> = self
            .output_gates
            .iter()
            .filter_map(|(&pin, gate_id)| {
                if let Some(component) = self.components.get(gate_id) {
                    if component.get_type() == GateType::Output {
                        return Some((pin, component.get_input(0)));
                    }
                }
                None
            })
            .collect();

        // Sort by pin number
        pins.sort_by_key(|&(pin, _)| pin);

        // Extract just the values
        pins.into_iter().map(|(_, value)| value).collect()
    }

    fn set_input(&mut self, pin: PinId, value: Option<bool>) {
        if let Some(gate_id) = self.input_gates.get(&pin) {
            if let Some(gate) = self.components.get_mut(gate_id) {
                gate.set_input(0, value);
            }
        }
    }
    fn set_inputs(&mut self, values: Vec<Option<bool>>) {
        if values.len() != self.input_gates.len() {
            panic!("Number of values does not match number of input gates");
        }

        for (pin, value) in self.input_gates.iter().zip(values) {
            if let Some(gate) = self.components.get_mut(pin.1) {
                gate.set_input(0, value);
            }
        }
    }

    fn set_output(&mut self, pin: PinId, value: Option<bool>) {
        if let Some(gate_id) = self.output_gates.get(&pin) {
            if let Some(gate) = self.components.get_mut(gate_id) {
                gate.set_input(0, value);
            }
        }
    }
    fn set_outputs(&mut self, values: Vec<Option<bool>>) {
        if values.len() != self.output_gates.len() {
            panic!("Number of values does not match number of output gates");
        }

        for (pin, value) in self.output_gates.iter().zip(values) {
            if let Some(gate) = self.components.get_mut(pin.1) {
                gate.set_input(0, value);
            }
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

impl LogicComponent for Gate {
    fn id(&self) -> GateId {
        self.id.gate_id
    }
    fn get_type(&self) -> GateType {
        self.gate_type
    }

    fn get_input(&self, pin: PinId) -> Option<bool> {
        self.inputs.get(pin).copied().flatten()
    }
    fn get_inputs(&self) -> Vec<Option<bool>> {
        self.inputs.clone()
    }

    fn get_output(&self, pin: PinId) -> Option<bool> {
        if pin == 0 {
            self.output
        } else {
            todo!("hit: Multi output pin support")
        }
    }
    fn get_outputs(&self) -> Vec<Option<bool>> {
        vec![self.output]
    }

    fn set_input(&mut self, pin: PinId, value: Option<bool>) {
        if pin < self.inputs.len() {
            self.inputs[pin] = value;
        }
    }
    fn set_inputs(&mut self, values: Vec<Option<bool>>) {
        self.inputs = values;
    }

    fn set_output(&mut self, pin: PinId, value: Option<bool>) {
        if pin == 0 {
            self.output = value;
        } else {
            todo!("hit: Multi output pin support")
        }
    }
    fn set_outputs(&mut self, values: Vec<Option<bool>>) {
        if values.len() == 1 {
            self.output = values[0];
        } else {
            todo!("hit: Multi output pin support")
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}
