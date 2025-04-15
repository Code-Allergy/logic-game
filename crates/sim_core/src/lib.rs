use std::collections::HashMap;
use uuid::Uuid;

#[cfg(test)]
mod tests;

pub mod circuits;
pub mod gates;

#[derive(Debug, Copy, Clone, PartialEq)]
pub enum TickError {
    CycleDetected,
}

#[derive(Debug)]
pub struct Circuit {
    pub id: Uuid,
    // pub gate_indices: HashMap<GateId, usize>,
    // pub gates: Vec<Box<dyn LogicComponent>>,
    pub components: HashMap<ComponentId, Box<dyn LogicComponent>>,
    pub connections: Vec<Connection>,
    topological_order: Option<Vec<ComponentId>>,
    topological_dirty: bool,
    input_gates: HashMap<PinId, GateId>,
    output_gates: HashMap<PinId, GateId>,
}

pub trait LogicComponent: std::fmt::Debug + Send + Sync {
    fn id(&self) -> Uuid;

    fn get_type(&self) -> GateType;

    fn get_input(&self, pin: PinId) -> Option<bool>;
    fn get_inputs(&self) -> Vec<Option<bool>>;

    fn get_output(&self, pin: PinId) -> Option<bool>;
    fn get_outputs(&self) -> Vec<Option<bool>>;

    fn set_input(&mut self, pin: PinId, value: Option<bool>);
    fn set_inputs(&mut self, values: Vec<Option<bool>>);
    fn set_output(&mut self, pin: PinId, value: Option<bool>);
    fn set_outputs(&mut self, values: Vec<Option<bool>>);
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
}

pub trait GateTrait {
    fn add_input(&mut self, initial: Option<bool>) -> GateId;
    fn add_input_pin(&mut self, initial: Option<bool>, pin: usize) -> GateId;
    fn add_output(&mut self) -> GateId;
    fn add_output_pin(&mut self, pin: usize) -> GateId;

    fn add_identity(&mut self) -> GateId;
    fn add_not(&mut self) -> GateId;
    fn add_or(&mut self) -> GateId;
    fn add_and(&mut self) -> GateId;

    // fn set_input(&mut self, pin: usize, value: bool);
    // fn read_output(&self, pin: usize) -> Option<bool>;
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
                    if component.get_type() == GateType::OutputPin {
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
}
