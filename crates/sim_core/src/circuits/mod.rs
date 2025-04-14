use std::collections::HashMap;

use crate::gates::{Connection, Gate, GateId, GateType, compute_gate_logic};

#[derive(Debug, Copy, Clone, PartialEq)]
pub enum TickError {
    CycleDetected,
}

#[derive(Debug)]
pub struct Circuit {
    pub gates: Vec<Gate>,
    pub connections: Vec<Connection>,
    topological_order: Option<Vec<usize>>,
    topological_dirty: bool,
}

impl Circuit {
    pub fn new() -> Self {
        Circuit {
            gates: Vec::new(),
            connections: Vec::new(),
            topological_order: None,
            topological_dirty: true,
        }
    }

    pub fn connect(&mut self, source: &Gate, destination: &Gate, target_input: usize) {
        self.connections
            .push(Connection::new(source.id.0, destination.id.0, target_input));
    }

    pub fn add_gate(&mut self, gate: Gate) {
        self.gates.insert(gate.id.0, gate);
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
            in_degree.insert(gate.id.0, 0);
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
        let order = match self.get_topological_order() {
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
                        target_gate.id.0,
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
}
