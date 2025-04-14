use std::collections::HashMap;

#[derive(Debug, Copy, Clone, PartialEq)]
pub enum GateType {
    And,
    Or,
    Not,
    Input,
    Output,
}

#[derive(Debug, Clone, Copy)]
pub struct GateId(pub usize);

#[derive(Debug)]
pub struct Gate {
    pub id: GateId,
    pub gate_type: GateType,
    pub inputs: Vec<Option<bool>>,
    pub output: Option<bool>,
}

#[derive(Debug)]
pub struct Connection {
    source: usize,
    target: usize,
    target_input: usize, // ID of the pin on the target gate
}

#[derive(Debug)]
pub struct Circuit {
    gates: Vec<Gate>,
    connections: Vec<Connection>,
}

impl Circuit {
    pub fn new() -> Self {
        Circuit {
            gates: Vec::new(),
            connections: Vec::new(),
        }
    }

    pub fn connect(&mut self, source: &Gate, destination: &Gate, target_input: usize) {
        self.connections.push(Connection {
            source: source.id.0,
            target: destination.id.0,
            target_input,
        });
    }

    pub fn add_gate(&mut self, gate: Gate) {
        self.gates.insert(gate.id.0, gate);
    }

    pub fn add_connection(&mut self, source: GateId, target: GateId, target_input: usize) {
        self.connections.push(Connection {
            source: source.0,
            target: target.0,
            target_input,
        });
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

    pub fn tick(&mut self) {
        // Ensure you have an up-to-date topological order.
        // If not, generate and cache one.
        let order = self.topological_sort().expect("Failed to get TOPO sort");
        // get cached version later

        // Traverse the nodes in the defined order
        for gate_id in order.iter() {
            // First, compute the output for the current gate
            let gate_output = if let Some(gate) = self.gates.get_mut(*gate_id) {
                // Validate that all inputs are available
                if gate.inputs.iter().any(|i| i.is_none()) {
                    panic!("Gate {} has uninitialized inputs!", gate_id);
                }

                // Compute the gate's output (currently just using the first input for testing)
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
            dbg!(&gate_id);
            dbg!(&updates_needed);
            // Now apply all the updates
            for (target_id, target_input) in updates_needed {
                if let Some(target_gate) = self.gates.get_mut(target_id) {
                    target_gate.inputs[target_input] = gate_output;
                }
            }
        }
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

impl Gate {
    pub fn new(
        id: usize,
        gate_type: GateType,
        inputs: Vec<Option<bool>>,
        output: Option<bool>,
    ) -> Self {
        Gate {
            id: GateId(id),
            gate_type,
            inputs,
            output,
        }
    }

    pub fn new_input(id: GateId, initial_value: Option<bool>) -> Self {
        Gate::new(id.0, GateType::Input, vec![initial_value], None)
    }

    pub fn new_output(id: GateId) -> Self {
        Gate::new(id.0, GateType::Output, vec![None], None)
    }

    pub fn new_or(id: GateId) -> Self {
        Gate::new(id.0, GateType::Or, vec![None, None], None)
    }

    pub fn new_and(id: GateId) -> Self {
        Gate::new(id.0, GateType::And, vec![None, None], None)
    }

    pub fn new_not(id: GateId) -> Self {
        Gate::new(id.0, GateType::Not, vec![None], None)
    }
}

pub fn compute_gate_logic(gate_type: GateType, inputs: Vec<bool>) -> bool {
    debug_assert!(!inputs.is_empty(), "Inputs should not be empty");

    match gate_type {
        GateType::And => {
            debug_assert_eq!(inputs.len(), 2, "AND gate requires exactly 2 inputs");
            debug_assert!(inputs.len() <= 2, "AND gate cannot have more than 2 inputs");
            inputs.into_iter().all(|value| value)
        }
        GateType::Or => {
            debug_assert_eq!(inputs.len(), 2, "OR gate requires exactly 2 inputs");
            debug_assert!(inputs.len() <= 2, "OR gate cannot have more than 2 inputs");
            inputs.into_iter().any(|value| value)
        }
        GateType::Not => {
            debug_assert_eq!(inputs.len(), 1, "NOT gate requires exactly 1 input");
            debug_assert!(inputs.len() <= 1, "NOT gate cannot have more than 1 input");
            !inputs[0]
        }
        GateType::Input => {
            debug_assert_eq!(inputs.len(), 1, "Input gate requires exactly 1 input");
            debug_assert!(
                inputs.len() <= 1,
                "Input gate cannot have more than 1 input"
            );
            inputs[0]
        }
        GateType::Output => {
            debug_assert_eq!(inputs.len(), 1, "Output gate requires exactly 1 input");
            debug_assert!(
                inputs.len() <= 1,
                "Output gate cannot have more than 1 input"
            );
            inputs[0]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compute_gate_logic_and() {
        assert_eq!(
            compute_gate_logic(GateType::And, vec![true, true]),
            true,
            "AND gate should return true when both inputs are true"
        );
        assert_eq!(
            compute_gate_logic(GateType::And, vec![true, false]),
            false,
            "AND gate should return false when one input is false"
        );
        assert_eq!(
            compute_gate_logic(GateType::And, vec![false, false]),
            false,
            "AND gate should return false when both inputs are false"
        );
        assert_eq!(
            compute_gate_logic(GateType::And, vec![false, false]),
            false,
            "AND gate should return false when both inputs are false"
        );
    }

    #[test]
    fn test_compute_gate_logic_or() {
        assert_eq!(
            compute_gate_logic(GateType::Or, vec![true, true]),
            true,
            "OR gate should return true when both inputs are true"
        );
        assert_eq!(
            compute_gate_logic(GateType::Or, vec![true, false]),
            true,
            "OR gate should return true when one input is true"
        );
        assert_eq!(
            compute_gate_logic(GateType::Or, vec![false, true]),
            true,
            "OR gate should return true when one input is true"
        );
        assert_eq!(
            compute_gate_logic(GateType::Or, vec![false, false]),
            false,
            "OR gate should return false when both inputs are false"
        );
    }

    #[test]
    fn test_compute_gate_logic_not() {
        assert_eq!(
            compute_gate_logic(GateType::Not, vec![true]),
            false,
            "NOT gate should return false when input is true"
        );
        assert_eq!(
            compute_gate_logic(GateType::Not, vec![false]),
            true,
            "NOT gate should return true when input is false"
        );
    }

    #[test]
    fn test_compute_gate_logic_input() {
        assert_eq!(
            compute_gate_logic(GateType::Input, vec![true]),
            true,
            "Input gate should return true when input is true"
        );
        assert_eq!(
            compute_gate_logic(GateType::Input, vec![false]),
            false,
            "Input gate should return false when input is false"
        );
    }

    #[test]
    fn test_compute_gate_logic_output() {
        assert_eq!(
            compute_gate_logic(GateType::Output, vec![true]),
            true,
            "Output gate should return true when input is true"
        );
        assert_eq!(
            compute_gate_logic(GateType::Output, vec![false]),
            false,
            "Output gate should return false when input is false"
        );
    }

    #[test]
    fn test_simple_circuit_input_output() {
        let mut circuit = Circuit::new();
        let mut input_gate = Gate::new_input(GateId(0), None);
        let mut output_gate = Gate::new_output(GateId(1));
        let input_id = input_gate.id;
        let output_id = output_gate.id;
        circuit.add_gate(input_gate);
        circuit.add_gate(output_gate);

        circuit.add_connection(input_id, output_id, 0);
        circuit.set_input(input_id, true);
        circuit.tick();

        assert_eq!(
            circuit.read_output(output_id),
            Some(true),
            "Circuit should return true when input is true"
        );

        circuit.set_input(input_id, false);
        circuit.tick();

        assert_eq!(
            circuit.read_output(output_id),
            Some(false),
            "Circuit should return false when input is false"
        );
    }

    #[test]
    fn test_simple_circuit_not() {
        let mut circuit = Circuit::new();
        let mut input_gate = Gate::new_input(GateId(0), None);
        let mut output_gate = Gate::new_output(GateId(1));
        let mut not_gate = Gate::new_not(GateId(2));
        let input_id = input_gate.id;
        let output_id = output_gate.id;
        let not_id = not_gate.id;

        circuit.add_gate(input_gate);
        circuit.add_gate(output_gate);
        circuit.add_gate(not_gate);

        circuit.add_connection(input_id, not_id, 0);
        circuit.add_connection(not_id, output_id, 0);
        circuit.set_input(input_id, true);
        circuit.tick();

        assert_eq!(
            circuit.read_output(output_id),
            Some(false),
            "Circuit should return false when input is true"
        );

        circuit.set_input(input_id, false);
        circuit.tick();

        assert_eq!(
            circuit.read_output(output_id),
            Some(true),
            "Circuit should return true when input is false"
        );
    }

    #[test]
    fn test_simple_circuit_or() {
        let mut circuit = Circuit::new();
        let mut input_gate = Gate::new_input(GateId(0), None);
        let mut output_gate = Gate::new_output(GateId(1));
        let mut or_gate = Gate::new_or(GateId(2));
        let mut input_gate2 = Gate::new_input(GateId(3), None);
        let input_id = input_gate.id;
        let input_id2 = input_gate2.id;
        let output_id = output_gate.id;
        let or_id = or_gate.id;

        circuit.add_gate(input_gate);
        circuit.add_gate(output_gate);
        circuit.add_gate(or_gate);
        circuit.add_gate(input_gate2);

        circuit.add_connection(input_id, or_id, 0);
        circuit.add_connection(input_id2, or_id, 1);
        circuit.add_connection(or_id, output_id, 0);
        circuit.set_input(input_id, true);
        circuit.set_input(input_id2, true);
        circuit.tick();

        assert_eq!(
            circuit.read_output(output_id),
            Some(true),
            "Circuit should return true when input is true, true: {:?}",
            circuit
        );

        circuit.set_input(input_id, false);
        circuit.set_input(input_id2, true);
        circuit.tick();

        assert_eq!(
            circuit.read_output(output_id),
            Some(true),
            "Circuit should return true when input is false, true: {:?}",
            circuit
        );

        circuit.set_input(input_id, true);
        circuit.set_input(input_id2, false);
        circuit.tick();

        assert_eq!(
            circuit.read_output(output_id),
            Some(true),
            "Circuit should return true when input is true, false: {:?}",
            circuit
        );

        circuit.set_input(input_id, false);
        circuit.set_input(input_id2, false);
        circuit.tick();

        assert_eq!(
            circuit.read_output(output_id),
            Some(false),
            "Circuit should return false when input is false, false: {:?}",
            circuit
        );
    }

    #[test]
    fn test_simple_circuit_and() {
        let mut circuit = Circuit::new();
        let mut input_gate = Gate::new_input(GateId(0), None);
        let mut output_gate = Gate::new_output(GateId(1));
        let mut and_gate = Gate::new_and(GateId(2));
        let mut input_gate2 = Gate::new_input(GateId(3), None);
        let input_id = input_gate.id;
        let input_id2 = input_gate2.id;
        let output_id = output_gate.id;
        let and_id = and_gate.id;

        circuit.add_gate(input_gate);
        circuit.add_gate(output_gate);
        circuit.add_gate(and_gate);
        circuit.add_gate(input_gate2);

        circuit.add_connection(input_id, and_id, 0);
        circuit.add_connection(input_id2, and_id, 1);
        circuit.add_connection(and_id, output_id, 0);
        circuit.set_input(input_id, true);
        circuit.set_input(input_id2, true);
        circuit.tick();

        assert_eq!(
            circuit.read_output(output_id),
            Some(true),
            "Circuit should return true when input is true, true: {:?}",
            circuit
        );

        circuit.set_input(input_id, false);
        circuit.set_input(input_id2, true);
        circuit.tick();

        assert_eq!(
            circuit.read_output(output_id),
            Some(false),
            "Circuit should return false when input is false, true: {:?}",
            circuit
        );

        circuit.set_input(input_id, true);
        circuit.set_input(input_id2, false);
        circuit.tick();

        assert_eq!(
            circuit.read_output(output_id),
            Some(false),
            "Circuit should return false when input is true, false: {:?}",
            circuit
        );

        circuit.set_input(input_id, false);
        circuit.set_input(input_id2, false);
        circuit.tick();

        assert_eq!(
            circuit.read_output(output_id),
            Some(false),
            "Circuit should return false when input is false, false: {:?}",
            circuit
        );
    }
}
