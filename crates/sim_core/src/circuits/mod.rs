pub mod builtin;

use std::collections::HashMap;

use uuid::Uuid;

use crate::gates::{Connection, Gate, GateId, GateType, compute_gate_logic};

#[derive(Debug, Copy, Clone, PartialEq)]
pub enum TickError {
    CycleDetected,
}

#[derive(Debug)]
pub struct Circuit {
    pub id: Uuid,
    pub gates: Vec<Gate>,
    pub connections: Vec<Connection>,
    topological_order: Option<Vec<usize>>,
    topological_dirty: bool,
    input_gates: HashMap<PinId, GateId>,
    output_gates: HashMap<PinId, GateId>,
}

pub trait CircuitTemplate {
    fn template() -> Circuit;
}

pub trait Port {
    fn port_id(&self) -> usize;
}

pub type PinId = usize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PortId {
    pub circuit_id: Uuid,
    pub gate_id: GateId,
}

impl PortId {
    pub fn new(circuit_id: Uuid, gate_id: GateId) -> Self {
        PortId {
            circuit_id,
            gate_id,
        }
    }

    pub fn new_usize(circuit_id: Uuid, gate_id: usize) -> Self {
        PortId {
            circuit_id,
            gate_id: GateId(gate_id),
        }
    }
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

    fn set_input(&mut self, pin: usize, value: bool);
    fn read_output(&self, pin: usize) -> Option<bool>;
}

impl Circuit {
    pub fn new() -> Self {
        Circuit {
            id: Uuid::new_v4(),
            gates: Vec::new(),
            connections: Vec::new(),
            topological_order: None,
            topological_dirty: true,
            input_gates: HashMap::new(),
            output_gates: HashMap::new(),
        }
    }

    pub fn id(&self) -> Uuid {
        self.id
    }

    pub fn connect(&mut self, source: &Gate, destination: &Gate, target_input: usize) {
        self.connections.push(Connection::new(
            source.id.gate_id.0,
            destination.id.gate_id.0,
            target_input,
        ));
    }

    pub fn add_gate(&mut self, gate: Gate) {
        self.gates.insert(gate.id.gate_id.0, gate);
    }

    pub fn add_connection(&mut self, source: GateId, target: GateId, target_input: usize) {
        self.connections
            .push(Connection::new(source.0, target.0, target_input));
    }

    pub fn get_topological_order(&mut self) -> Option<Vec<usize>> {
        if self.topological_dirty {
            self.topological_order = self.topological_sort();
            self.topological_dirty = false;
        }
        self.topological_order.clone()
    }

    pub fn topological_sort(&self) -> Option<Vec<usize>> {
        use std::collections::VecDeque;
        // Compute in-degree for each gate
        let mut in_degree = HashMap::new();
        for gate in self.gates.iter() {
            in_degree.insert(gate.id.gate_id.0, 0);
        }
        for connection in self.connections.iter() {
            if let Some(degree) = in_degree.get_mut(&connection.target) {
                *degree += 1;
            }
        }
        // Initialize queue with nodes having 0 in-degree
        let mut queue: VecDeque<usize> = in_degree
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
        if sorted.len() == self.gates.len() {
            Some(sorted)
        } else {
            // There is a cycle in the graph
            None
        }
    }

    pub fn tick(&mut self) -> Result<(), TickError> {
        let result: Option<Vec<usize>> = self.get_topological_order();
        let order = match result {
            Some(order) => order,
            None => return Err(TickError::CycleDetected),
        };

        // Traverse the nodes in the defined order
        for gate_id in order.iter() {
            // First, compute the output for the current gate
            let gate_output = if let Some(gate) = self.gates.get_mut(*gate_id) {
                // Validate that all inputs are available
                if gate.inputs.iter().any(|i| i.is_none()) {
                    panic!("Gate {} has uninitialized inputs!", gate_id);
                }

                gate.output = Some(compute_gate_logic(
                    gate.gate_type,
                    gate.inputs
                        .iter()
                        .map(|&v| v.unwrap())
                        .collect::<Vec<bool>>(),
                ));
                gate.output // Store the output for later use
            } else {
                continue; // Skip if gate not found
            };

            // Then, find all connections that need updating
            let updates_needed: Vec<(usize, usize)> = self
                .connections
                .iter()
                .filter(|c| c.source == *gate_id)
                .map(|c| (c.target, c.target_input))
                .collect();

            // Now apply all the updates
            for (target_id, target_input) in updates_needed {
                if let Some(target_gate) = self.gates.get_mut(target_id) {
                    debug_assert!(
                        target_gate.inputs.len() > target_input,
                        "Target gate {} has gate {} updating itself, but has an expected input port of {}",
                        target_gate.id.gate_id.0,
                        gate_id,
                        target_input
                    );
                    target_gate.inputs[target_input] = gate_output;
                }
            }
        }

        Ok(())
    }

    pub fn set_input(&mut self, id: GateId, value: bool) {
        if let Some(gate) = self.gates.get_mut(id.0) {
            debug_assert!(
                gate.gate_type == GateType::Input,
                "Attempted to set input on non-input gate"
            );
            gate.inputs[0] = Some(value);
        }
    }

    pub fn set_input_pin(&mut self, id: usize, value: bool) {
        if let Some(gate_id) = self.input_gates.get_mut(&id) {
            if let Some(gate) = self.gates.get_mut(gate_id.0) {
                debug_assert!(
                    gate.gate_type == GateType::Input,
                    "Attempted to set input on non-input gate"
                );
                gate.inputs[0] = Some(value);
            }
        }
    }

    pub fn read_output(&self, id: GateId) -> Option<bool> {
        if let Some(gate) = self.gates.get(id.0) {
            debug_assert!(
                gate.gate_type == GateType::Output,
                "Attempted to read output from non-output gate"
            );
            gate.output
        } else {
            None
        }
    }

    pub fn read_output_pin(&self, id: usize) -> Option<bool> {
        if let Some(gate_id) = self.output_gates.get(&id) {
            if let Some(gate) = self.gates.get(gate_id.0) {
                debug_assert!(
                    gate.gate_type == GateType::Output,
                    "Attempted to read output from non-output gate"
                );
                return gate.output;
            }
        }
        None
    }
}

impl GateTrait for Circuit {
    fn add_input(&mut self, initial: Option<bool>) -> GateId {
        let gate = Gate::new_input(self.id(), GateId(self.gates.len()), initial);
        let gate_id = gate.id.gate_id;
        self.add_gate(gate);
        gate_id
    }

    fn add_input_pin(&mut self, initial: Option<bool>, pin: usize) -> GateId {
        let gate = Gate::new_input(self.id(), GateId(pin), initial);
        let gate_id = gate.id.gate_id;
        self.add_gate(gate);
        self.input_gates.insert(pin, gate_id);
        gate_id
    }

    fn add_output(&mut self) -> GateId {
        let gate = Gate::new_output(self.id(), GateId(self.gates.len()));
        let gate_id = gate.id.gate_id;
        self.add_gate(gate);
        gate_id
    }

    fn add_output_pin(&mut self, pin: usize) -> GateId {
        let gate = Gate::new_output(self.id(), GateId(self.gates.len()));
        let gate_id = gate.id.gate_id;
        self.add_gate(gate);
        self.output_gates.insert(pin, gate_id);
        gate_id
    }

    fn add_identity(&mut self) -> GateId {
        let gate = Gate::new_identity(self.id(), GateId(self.gates.len()));
        let gate_id = gate.id.gate_id;
        self.add_gate(gate);
        gate_id
    }

    fn add_not(&mut self) -> GateId {
        let gate = Gate::new_not(self.id(), GateId(self.gates.len()));
        let gate_id = gate.id.gate_id;
        self.add_gate(gate);
        gate_id
    }

    fn add_or(&mut self) -> GateId {
        let gate = Gate::new_or(self.id(), GateId(self.gates.len()));
        let gate_id = gate.id.gate_id;
        self.add_gate(gate);
        gate_id
    }

    fn add_and(&mut self) -> GateId {
        let gate = Gate::new_and(self.id(), GateId(self.gates.len()));
        let gate_id = gate.id.gate_id;
        self.add_gate(gate);
        gate_id
    }

    fn set_input(&mut self, pin: usize, value: bool) {
        if let Some(gate) = self.gates.get_mut(pin) {
            gate.inputs[0] = Some(value);
        }
    }

    fn read_output(&self, pin: usize) -> Option<bool> {
        if let Some(gate) = self.gates.get(pin) {
            gate.output
        } else {
            None
        }
    }
}
