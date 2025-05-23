pub mod builtin;

// Removed unused import: use std::collections::HashMap;
use ahash::AHashMap;
use uuid::Uuid;

use crate::{
    AnyGate, Circuit, CircuitId, ComponentId, Connection, Gate, GateId, LogicComponent, PinId,
    PortId, TickError,
};

// Enum to hold output for propagation, avoiding Vec allocation for single-output gates (Step 4)
enum PropagationOutput {
    Single(Option<bool>),
    Multiple(Vec<Option<bool>>),
}

impl PortId {
    pub fn new(circuit_id: CircuitId, gate_id: GateId) -> Self {
        PortId {
            circuit_id,
            gate_id,
        }
    }
}

impl Circuit {
    pub fn new() -> Self {
        Circuit {
            id: Uuid::new_v4(),
            components: AHashMap::new(),
            // gates: Vec::new(),
            connections: Vec::new(),
            topological_order: None,
            topological_dirty: true,
            input_gates: AHashMap::new(),
            output_gates: AHashMap::new(),
            // Initialize pin caching fields (Step 3 of overall plan)
            sorted_input_pin_ids: None,
            sorted_output_pin_ids: None,
            pins_dirty: true,
            // Initialize static_pins (This was missed in a previous diff application)
            static_pins: None,
        }
    }

    pub fn id(&self) -> CircuitId {
        self.id
    }

    pub fn add_component(&mut self, component: LogicComponent) {
        let component_id = match &component {
            LogicComponent::Gate(gate) => gate.get_id().gate_id,
            LogicComponent::Circuit(circuit) => circuit.id,
        };
        self.components.insert(component_id, component);
    }

    pub fn connect(
        &mut self,
        source: &Gate,
        destination: &Gate,
        target_input: usize,
        source_output: PinId,
    ) {
        self.connections.push(Connection::new(
            source.id.gate_id,
            destination.id.gate_id,
            target_input,
            source_output,
        ));
        self.topological_dirty = true; 
    }

    pub fn add_connection(
        &mut self,
        source: ComponentId,
        target: ComponentId,
        target_input: PinId,
        source_output: PinId,
    ) {
        self.connections
            .push(Connection::new(source, target, target_input, source_output));
        self.topological_dirty = true; 
    }

    pub fn get_topological_order(&mut self) -> Option<Vec<Uuid>> {
        if self.topological_dirty {
            self.topological_order = self.topological_sort();
            self.topological_dirty = false;
        }

        self.topological_order.clone()
    }

    pub fn topological_sort(&self) -> Option<Vec<ComponentId>> {
        use std::collections::VecDeque;
        let mut in_degree = AHashMap::new(); 
        for id in self.components.keys() {
            in_degree.insert(*id, 0);
        }
        for connection in self.connections.iter() {
            if let Some(degree) = in_degree.get_mut(&connection.target) {
                *degree += 1;
            }
        }
        let mut queue: VecDeque<ComponentId> = in_degree
            .iter()
            .filter(|(_, deg)| **deg == 0)
            .map(|(&id, _)| id)
            .collect();

        let mut sorted = Vec::new();
        while let Some(id) = queue.pop_front() {
            sorted.push(id);
            for connection in self.connections.iter().filter(|c| c.source == id) {
                if let Some(degree) = in_degree.get_mut(&connection.target) {
                    *degree -= 1;
                    if *degree == 0 {
                        queue.push_back(connection.target);
                    }
                }
            }
        }

        if sorted.len() == self.components.len() {
            Some(sorted)
        } else {
            None
        }
    }

    pub fn set_input(&mut self, pin: PinId, value: Option<bool>) {
        if let Some(gate_id) = self.input_gates.get(&pin) {
            if let Some(gate) = self.components.get_mut(gate_id) {
                match gate {
                    LogicComponent::Gate(gate) => {
                        gate.set_input(0, value);
                    }
                    LogicComponent::Circuit(circuit) => {
                        circuit.set_input(pin, value);
                    }
                }
            }
        }
    }

    pub fn get_output(&self, pin: PinId) -> Option<bool> {
        if let Some(gate_id) = self.output_gates.get(&pin) {
            if let Some(gate) = self.components.get(gate_id) {
                match gate {
                    LogicComponent::Gate(gate) => return gate.get_output(),
                    LogicComponent::Circuit(circuit) => {
                        return circuit.get_output(pin);
                    }
                }
            }
        }
        None
    }

    pub fn tick(&mut self) -> Result<(), TickError> {
        let order = match self.get_topological_order() {
            Some(o) => o,
            None => return Err(TickError::CycleDetected),
        };

        for gate_id in order.iter() {
            let component = match self.components.get_mut(gate_id) {
                Some(g) => g,
                Option::None => continue,
            };

            let propagation_data = match component {
                LogicComponent::Gate(gate) => {
                    let inputs = gate.get_inputs(); 
                    if inputs.iter().any(|i| i.is_none()) {
                        panic!("Gate {:?} has uninitialized inputs: {:?}", gate_id, inputs);
                    }
                    gate.compute();
                    PropagationOutput::Single(gate.get_output())
                }
                LogicComponent::Circuit(circuit) => {
                    circuit.tick()?;
                    let circuit_outputs_for_propagation = circuit.get_outputs();
                    let outputs_for_internal_pins = circuit_outputs_for_propagation.clone(); 
                    if outputs_for_internal_pins.len() != circuit.output_gates.len() {
                        return Err(TickError::OutputGateMismatch);
                    }
                    for (pin, value) in self.output_gates.iter().zip(outputs_for_internal_pins) {
                        if let Some(LogicComponent::Gate(output_representing_gate)) = self.components.get_mut(pin.1) {
                            output_representing_gate.set_input(0, value);
                        }
                    }
                    PropagationOutput::Multiple(circuit_outputs_for_propagation)
                }
            };

            for connection in self.connections.iter().filter(|c| c.source == *gate_id) {
                let output_value = match propagation_data {
                    PropagationOutput::Single(out_opt) => {
                        debug_assert_eq!(connection.source_output, 0, "Gate outputs assumed to be on pin 0");
                        out_opt
                    }
                    PropagationOutput::Multiple(ref outs_vec) => {
                        outs_vec.get(connection.source_output).copied().unwrap_or(None)
                    }
                };
                if let Some(target_gate) = self.components.get_mut(&connection.target) {
                    match target_gate {
                        LogicComponent::Gate(gate) => {
                            gate.set_input(connection.target_input, output_value);
                        }
                        LogicComponent::Circuit(circuit) => {
                            circuit.set_input(connection.target_input, output_value);
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn get_inputs(&mut self) -> Vec<Option<bool>> {
        if self.pins_dirty || self.sorted_input_pin_ids.is_none() {
            let mut sorted_pins_vec: Vec<(PinId, GateId)> = self
                .input_gates
                .iter()
                .map(|(&pin, &id)| (pin, id))
                .collect();
            sorted_pins_vec.sort_by_key(|&(pin, _)| pin);
            self.sorted_input_pin_ids = Some(sorted_pins_vec.into_iter().map(|(pin, _)| pin).collect());
            self.pins_dirty = false;
        }
        self.sorted_input_pin_ids
            .as_ref()
            .unwrap()
            .iter()
            .map(|&pin_id| {
                let gate_id = self.input_gates.get(&pin_id).unwrap();
                if let Some(LogicComponent::Gate(AnyGate::Input(gate))) =
                    self.components.get(gate_id)
                {
                    gate.get_input(0)
                } else { None }
            })
            .collect()
    }

    fn get_outputs(&mut self) -> Vec<Option<bool>> {
        if self.pins_dirty || self.sorted_output_pin_ids.is_none() {
            let mut sorted_pins_vec: Vec<(PinId, GateId)> = self
                .output_gates
                .iter()
                .map(|(&pin, &id)| (pin, id))
                .collect();
            sorted_pins_vec.sort_by_key(|&(pin, _)| pin);
            self.sorted_output_pin_ids = Some(sorted_pins_vec.into_iter().map(|(pin, _)| pin).collect());
            self.pins_dirty = false;
        }
        self.sorted_output_pin_ids
            .as_ref()
            .unwrap()
            .iter()
            .map(|&pin_id| {
                let gate_id = self.output_gates.get(&pin_id).unwrap();
                if let Some(LogicComponent::Gate(AnyGate::Output(gate))) =
                    self.components.get(gate_id)
                {
                    gate.output 
                } else { None }
            })
            .collect()
    }
}

impl Circuit {
    pub fn get_gate_count(&self) -> usize {
        self.components.values().fold(0, |acc, component| {
            acc + match component {
                LogicComponent::Gate(_) => 1,
                LogicComponent::Circuit(circuit) => circuit.get_gate_count(),
            }
        })
    }
    
    pub fn add_input(&mut self, initial: bool) -> GateId {
        let gate = AnyGate::new_input(self.id, initial);
        let gate_id = gate.get_id().gate_id;
        self.components.insert(gate_id, LogicComponent::Gate(gate));
        gate_id
    }

    pub fn add_input_pin(&mut self, pin: usize) -> GateId {
        let mut gate = AnyGate::new_input_pin(self.id);
        gate.set_input(0, Some(false));
        let gate_id = gate.get_id().gate_id;
        self.components.insert(gate_id, LogicComponent::Gate(gate));
        self.input_gates.insert(pin, gate_id);
        self.pins_dirty = true; 
        gate_id
    }

    pub fn add_output(&mut self) -> GateId {
        let gate = AnyGate::new_output(self.id);
        let gate_id = gate.get_id().gate_id;
        self.components.insert(gate_id, LogicComponent::Gate(gate));
        gate_id
    }

    pub fn add_output_pin(&mut self, pin: usize) -> GateId {
        let gate = AnyGate::new_output(self.id); 
        let gate_id = gate.get_id().gate_id;
        self.components.insert(gate_id, LogicComponent::Gate(gate));
        self.output_gates.insert(pin, gate_id);
        self.pins_dirty = true; 
        gate_id
    }

    pub fn add_identity(&mut self) -> GateId {
        let gate = AnyGate::new_identity(self.id);
        let gate_id = gate.get_id().gate_id;
        self.components.insert(gate_id, LogicComponent::Gate(gate));
        gate_id
    }

    pub fn add_not(&mut self) -> GateId {
        let gate = AnyGate::new_not(self.id);
        let gate_id = gate.get_id().gate_id;
        self.components.insert(gate_id, LogicComponent::Gate(gate));
        gate_id
    }

    pub fn add_or(&mut self) -> GateId {
        let gate = AnyGate::new_or(self.id);
        let gate_id = gate.get_id().gate_id;
        self.components.insert(gate_id, LogicComponent::Gate(gate));
        gate_id
    }

    pub fn add_and(&mut self) -> GateId {
        let gate = AnyGate::new_and(self.id);
        let gate_id = gate.get_id().gate_id;
        self.components.insert(gate_id, LogicComponent::Gate(gate));
        gate_id
    }

    pub fn add_oscillator(&mut self, rate: usize) -> GateId {
        let gate = AnyGate::new_oscillator(self.id, rate);
        let gate_id = gate.get_id().gate_id;
        self.components.insert(gate_id, LogicComponent::Gate(gate));
        gate_id
    }
}

impl Default for Circuit {
    fn default() -> Self {
        Circuit::new()
    }
}
