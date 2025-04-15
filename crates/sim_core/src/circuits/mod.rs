pub mod builtin;

use std::collections::HashMap;

use uuid::Uuid;

use crate::gates::compute_gate_logic;
use crate::{
    Circuit, CircuitId, ComponentId, Connection, Gate, GateId, GateTrait, GateType, PortId,
    TickError,
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
            components: HashMap::new(),
            // gates: Vec::new(),
            connections: Vec::new(),
            topological_order: None,
            topological_dirty: true,
            input_gates: HashMap::new(),
            output_gates: HashMap::new(),
        }
    }

    pub fn connect(&mut self, source: &Gate, destination: &Gate, target_input: usize) {
        self.connections.push(Connection::new(
            source.id.gate_id,
            destination.id.gate_id,
            target_input,
        ));
    }

    pub fn add_connection(
        &mut self,
        source: ComponentId,
        target: ComponentId,
        target_input: usize,
    ) {
        self.connections
            .push(Connection::new(source, target, target_input));
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

    pub fn tick(&mut self) -> Result<(), TickError> {
        let result: Option<Vec<Uuid>> = self.get_topological_order();
        let order = match result {
            Some(order) => order,
            None => return Err(TickError::CycleDetected),
        };

        // Traverse the nodes in the defined order
        for gate_id in order.iter() {
            let component = match self.components.get_mut(gate_id) {
                Some(g) => g,
                None => continue,
            };

            // Validate that all inputs are available
            let inputs = component.get_inputs();
            if inputs.iter().any(|i| i.is_none()) {
                panic!("Gate {:?} has uninitialized inputs!", gate_id);
            }

            let input_bools: Vec<bool> = inputs.iter().map(|&v| v.unwrap()).collect();
            let output = compute_gate_logic(component.get_type(), &input_bools);

            component.set_output(0, Some(output));
            let updates_needed: Vec<(ComponentId, usize)> = self
                .connections
                .iter()
                .filter(|c| c.source == *gate_id)
                .map(|c| (c.target, c.target_input))
                .collect();

            // Apply updates to all connected gates
            for (target_id, target_input) in updates_needed {
                if let Some(target_gate) = self.components.get_mut(&target_id) {
                    target_gate.set_input(target_input, Some(output));
                }
            }
        }

        Ok(())
    }

    // pub fn set_input(&mut self, id: ComponentId, value: bool) {
    //     if let Some(gate) = self.components.get_mut(&id) {
    //         // debug_assert!(
    //         //     gate.gate_type == GateType::Input,
    //         //     "Attempted to set input on non-input gate"
    //         // );
    //         gate.inputs[0] = Some(value);
    //     }
    // }

    // pub fn set_input_pin(&mut self, id: usize, value: bool) {
    //     if let Some(gate_id) = self.input_gates.get_mut(&id) {
    //         if let Some(gate) = self.gates.get_mut(gate_id.0) {
    //             debug_assert!(
    //                 gate.gate_type == GateType::Input,
    //                 "Attempted to set input on non-input gate"
    //             );
    //             gate.inputs[0] = Some(value);
    //         }
    //     }
    // }

    // pub fn read_output(&self, id: GateId) -> Option<bool> {
    //     if let Some(gate) = self.gates.get(id.0) {
    //         debug_assert!(
    //             gate.gate_type == GateType::Output,
    //             "Attempted to read output from non-output gate"
    //         );
    //         gate.output
    //     } else {
    //         None
    //     }
    // }

    // pub fn read_output_pin(&self, id: usize) -> Option<bool> {
    //     if let Some(gate_id) = self.output_gates.get(&id) {
    //         if let Some(gate) = self.gates.get(gate_id.0) {
    //             debug_assert!(
    //                 gate.gate_type == GateType::Output,
    //                 "Attempted to read output from non-output gate"
    //             );
    //             return gate.output;
    //         }
    //     }
    //     None
    // }
}

impl GateTrait for Circuit {
    fn add_input(&mut self, initial: Option<bool>) -> GateId {
        let gate = Gate::new_input(self.id, initial);
        let gate_id = gate.id.gate_id;
        self.components.insert(gate_id, Box::new(gate));
        gate_id
    }

    fn add_input_pin(&mut self, initial: Option<bool>, pin: usize) -> GateId {
        let gate = Gate::new_input(self.id, initial);
        let gate_id = gate.id.gate_id;
        self.components.insert(gate_id, Box::new(gate));
        self.input_gates.insert(pin, gate_id);
        gate_id
    }

    fn add_output(&mut self) -> GateId {
        let gate = Gate::new_output(self.id);
        let gate_id = gate.id.gate_id;
        self.components.insert(gate_id, Box::new(gate));
        gate_id
    }

    fn add_output_pin(&mut self, pin: usize) -> GateId {
        let gate = Gate::new_output(self.id);
        let gate_id = gate.id.gate_id;
        self.components.insert(gate_id, Box::new(gate));
        self.output_gates.insert(pin, gate_id);
        gate_id
    }

    fn add_identity(&mut self) -> GateId {
        let gate = Gate::new_identity(self.id);
        let gate_id = gate.id.gate_id;
        self.components.insert(gate_id, Box::new(gate));
        gate_id
    }

    fn add_not(&mut self) -> GateId {
        let gate = Gate::new_not(self.id);
        let gate_id = gate.id.gate_id;
        self.components.insert(gate_id, Box::new(gate));
        gate_id
    }

    fn add_or(&mut self) -> GateId {
        let gate = Gate::new_or(self.id);
        let gate_id = gate.id.gate_id;
        self.components.insert(gate_id, Box::new(gate));
        gate_id
    }

    fn add_and(&mut self) -> GateId {
        let gate = Gate::new_and(self.id);
        let gate_id = gate.id.gate_id;
        self.components.insert(gate_id, Box::new(gate));
        gate_id
    }

    // fn set_input(&mut self, pin: usize, value: bool) {
    //     if let Some(gate) = self.components.get_mut(pin) {
    //         gate.inputs[0] = Some(value);
    //     }
    // }

    // fn read_output(&self, pin: usize) -> Option<bool> {
    //     if let Some(gate) = self.gates.get(pin) {
    //         gate.output
    //     } else {
    //         None
    //     }
    // }
}
