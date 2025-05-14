pub mod builtin;

use std::collections::HashMap;

use ahash::AHashMap;
use uuid::Uuid;

use crate::{
    AnyGate, Circuit, CircuitId, ComponentId, Connection, Gate, GateId, LogicComponent, PinId,
    PortId, TickError,
};

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
        // Compute in-degree for each gate
        let mut in_degree = HashMap::new();
        for id in self.components.keys() {
            in_degree.insert(*id, 0);
        }
        for connection in self.connections.iter() {
            if let Some(degree) = in_degree.get_mut(&connection.target) {
                *degree += 1;
            }
        }
        // Initialize queue with nodes having 0 in-degree
        let mut queue: VecDeque<ComponentId> = in_degree
            .iter()
            .filter(|(_, deg)| **deg == 0)
            .map(|(&id, _)| id)
            .collect();

        let mut sorted = Vec::new();
        while let Some(id) = queue.pop_front() {
            sorted.push(id);
            // For every node connected from the current one, decrease its in-degree
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
            // There is a cycle in the graph
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
                        // For circuits, we need to get the output from the circuit
                        return circuit.get_output(pin);
                    }
                }
            }
        }
        None
    }

    pub fn tick(&mut self) -> Result<(), TickError> {
        let result: Option<Vec<Uuid>> = self.get_topological_order();
        let order = match result {
            Option::Some(order) => order,
            Option::None => return Err(TickError::CycleDetected),
        };

        // Traverse the nodes in the defined order
        for gate_id in order.iter() {
            let component = match self.components.get_mut(gate_id) {
                Some(g) => g,
                Option::None => continue,
            };

            // Validate that all inputs are available
            let inputs = match component {
                LogicComponent::Gate(gate) => gate.get_inputs(),
                LogicComponent::Circuit(circuit) => circuit.get_inputs(),
            };
            if inputs.iter().any(|i| i.is_none()) {
                panic!("Gate {:?} has uninitialized inputs: {:?}", gate_id, inputs);
            }

            let outputs = match component {
                LogicComponent::Gate(gate) => {
                    gate.compute();
                    vec![gate.get_output()]
                }
                LogicComponent::Circuit(circuit) => {
                    // For circuits, we need to tick the subcircuit
                    circuit.tick()?;
                    // Assume output is on pin 0 (or adjust as needed)
                    let outputs = circuit.get_outputs();
                    if outputs.len() != circuit.output_gates.len() {
                        return Err(TickError::OutputGateMismatch);
                    }
                    for (pin, value) in self.output_gates.iter().zip(outputs.clone()) {
                        if let Some(LogicComponent::Gate(gate)) = self.components.get_mut(pin.1) {
                            gate.set_input(0, value);
                        }
                    }
                    outputs
                }
            };

            for connection in self.connections.iter().filter(|c| c.source == *gate_id) {
                let output_value = outputs
                    .get(connection.source_output)
                    .copied()
                    .unwrap_or(None);
                if let Some(target_gate) = self.components.get_mut(&connection.target) {
                    match target_gate {
                        LogicComponent::Gate(gate) => {
                            gate.set_input(connection.target_input, output_value);
                        }
                        LogicComponent::Circuit(circuit) => {
                            // For circuits, we need to set the input directly
                            circuit.set_input(connection.target_input, output_value);
                        }
                    }
                }
            }
        }

        Ok(())
    }

    // pub fn execute(&mut self, inputs: Vec<Option<bool>>) -> Result<Vec<Option<bool>>, TickError> {
    // }

    fn get_inputs(&self) -> Vec<Option<bool>> {
        let mut pins: Vec<(PinId, Option<bool>)> = self
            .input_gates
            .iter()
            .filter_map(|(&pin, gate_id)| {
                if let Some(LogicComponent::Gate(AnyGate::Input(gate))) =
                    self.components.get(gate_id)
                {
                    return Some((pin, gate.get_input(0)));
                }

                None
            })
            .collect();

        // Sort by pin number if needed
        pins.sort_by_key(|&(pin, _)| pin);

        // Extract just the values
        pins.into_iter().map(|(_, value)| value).collect()
    }

    fn get_outputs(&self) -> Vec<Option<bool>> {
        let mut pins: Vec<(PinId, Option<bool>)> = self
            .output_gates
            .iter()
            .filter_map(|(&pin, gate_id)| {
                if let Some(LogicComponent::Gate(AnyGate::Output(gate))) =
                    self.components.get(gate_id)
                {
                    return Some((pin, gate.get_input(0)));
                }
                None
            })
            .collect();

        // Sort by pin number
        pins.sort_by_key(|&(pin, _)| pin);

        // Extract just the values
        pins.into_iter().map(|(_, value)| value).collect()
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
        // TODO output pins
        let gate_id = gate.get_id().gate_id;
        self.components.insert(gate_id, LogicComponent::Gate(gate));
        self.input_gates.insert(pin, gate_id);
        gate_id
    }

    pub fn add_output(&mut self) -> GateId {
        let gate = AnyGate::new_output(self.id);
        let gate_id = gate.get_id().gate_id;
        self.components.insert(gate_id, LogicComponent::Gate(gate));
        gate_id
    }

    pub fn add_output_pin(&mut self, pin: usize) -> GateId {
        let gate = AnyGate::new_output(self.id); // TODO output pins
        let gate_id = gate.get_id().gate_id;
        self.components.insert(gate_id, LogicComponent::Gate(gate));
        self.output_gates.insert(pin, gate_id);
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
