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
        // LogicComponent now has an id() method that delegates to the Simulatable trait object.
        let component_id = component.id();
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
            if let Some(component) = self.components.get_mut(gate_id) {
                // Input pins of a circuit typically correspond to an internal InputPin gate.
                // The `pin` argument for the component's set_input should be the specific
                // input pin on that component being targeted. For an InputPin gate, this is usually 0.
                component.set_input(0, value);
            }
        }
    }

    pub fn get_output(&self, pin: PinId) -> Option<bool> {
        if let Some(gate_id) = self.output_gates.get(&pin) {
            if let Some(component) = self.components.get(gate_id) {
                // Output pins of a circuit typically correspond to an internal OutputPin gate.
                // The `pin` argument for the component's get_output should be the specific
                // output pin on that component being read. For an OutputPin gate, this is usually 0.
                return component.get_output(0);
            }
        }
        None
    }

    pub fn tick(&mut self) -> Result<(), TickError> {
        let order = self.get_topological_order().ok_or(TickError::CycleDetected)?;

        for component_id in order.iter() {
            // Step 1: Get component, check inputs, run, and collect its outputs.
            let outputs = { // Scoping to release borrow on self.components sooner
                let component = self.components.get_mut(component_id)
                    .expect("Component in topological order should exist but was not found in components map.");

                // Validate that all *expected* inputs are available.
                // Some components like Oscillator might have 0 expected_inputs.
                // Or Input gates might have their input set directly and not through connections.
                let expected_count = component.expected_inputs();
                let current_inputs = component.get_inputs();

                if expected_count > 0 {
                    // Check if any of the *expected* inputs are None.
                    // This assumes inputs are ordered and current_inputs.len() >= expected_count.
                    // A more robust check might involve specific logic per component type
                    // or ensuring current_inputs always reflects expected_inputs in length.
                    let mut all_expected_inputs_set = true;
                    for i in 0..expected_count {
                        if current_inputs.get(i).map_or(true, |opt_val| opt_val.is_none()) {
                            all_expected_inputs_set = false;
                            break;
                        }
                    }

                    if !all_expected_inputs_set {
                        // Only panic if the component is not an input-type gate, as those get inputs externally.
                        // Or if it's an oscillator that doesn't depend on inputs.
                        let comp_type = component.get_type();
                        // For non-circuit components (actual gates), if they expect inputs, they should be set.
                        // InputPin, Input, and Oscillator have special handling (don't rely on all inputs being set by connections).
                        // Circuits will manage their own inputs internally when their run() method is called.
                        if comp_type != GateType::Circuit && comp_type != GateType::Input && comp_type != GateType::InputPin && comp_type != GateType::Oscillator {
                            panic!(
                                "Component {:?} (type: {:?}) has uninitialized inputs. Expected: {}, Got: {:?}",
                                component_id,
                                comp_type, // Use already fetched comp_type
                                expected_count,
                                current_inputs
                            );
                        }
                    }
                }
                
                component.run()?; // This calls component.component.run()
                component.get_all_outputs() // This calls component.component.get_all_outputs()
            };
            
            // Step 2: Propagate outputs to connected components.
            // Iterate over a clone of connections if modifications to self.components occur elsewhere,
            // but here we only modify target_component's inputs, which is fine.
            for connection in self.connections.iter().filter(|c| c.source == *component_id) {
                let output_value = outputs
                    .get(connection.source_output) // source_output is the PinId on the source component
                    .copied()
                    .flatten(); // Correctly handles Option<Option<bool>> from get() and then the Option<bool> itself

                if let Some(target_component) = self.components.get_mut(&connection.target) {
                    target_component.set_input(connection.target_input, output_value);
                } else {
                    // This case should ideally not happen in a well-formed circuit.
                    // Consider logging an error or returning a TickError variant.
                    // For now, matches previous behavior of potentially doing nothing if target not found.
                    eprintln!("Warning: Target component {:?} for connection not found.", connection.target);
                }
            } // End of for connection in self.connections...
        } // End of for component_id in order...

        Ok(())
    }

    // pub fn execute(&mut self, inputs: Vec<Option<bool>>) -> Result<Vec<Option<bool>>, TickError> {
    // }

    fn get_inputs(&self) -> Vec<Option<bool>> {
        let mut pins: Vec<(PinId, Option<bool>)> = self
            .input_gates
            .iter()
            .filter_map(|(&pin_idx, gate_id)| { // Renamed pin to pin_idx to avoid conflict
                if let Some(component) = self.components.get(gate_id) {
                    // Input pins of a circuit are typically InputPin gates.
                    // Their "output" (current state) is what the circuit sees as its input.
                    if component.get_type() == GateType::InputPin || component.get_type() == GateType::Input {
                        return Some((pin_idx, component.get_output(0))); // Pin 0 of the InputPin/Input gate
                    }
                }
                None
            })
            .collect();

        pins.sort_by_key(|&(pin_idx, _)| pin_idx);
        pins.into_iter().map(|(_, value)| value).collect()
    }

    fn get_outputs(&self) -> Vec<Option<bool>> {
        let mut pins: Vec<(PinId, Option<bool>)> = self
            .output_gates
            .iter()
            .filter_map(|(&pin_idx, gate_id)| { // Renamed pin to pin_idx
                if let Some(component) = self.components.get(gate_id) {
                    // Output pins of a circuit are typically OutputPin gates.
                    // The "input" to these OutputPin gates is what the circuit provides as its output.
                    if component.get_type() == GateType::OutputPin || component.get_type() == GateType::Output {
                        // OutputPin's state is its first input
                        return Some((pin_idx, component.get_inputs().get(0).cloned().flatten()));
                    }
                }
                None
            })
            .collect();

        pins.sort_by_key(|&(pin_idx, _)| pin_idx);
        pins.into_iter().map(|(_, value)| value).collect()
    }
}

impl Circuit {
    pub fn get_gate_count(&self) -> usize {
        self.components.values().fold(0, |acc, component_wrapper| {
            if component_wrapper.get_type() == GateType::Circuit {
                // It's a sub-circuit. Try to downcast to get its gate count recursively.
                if let Some(sub_circuit) = component_wrapper.component.as_any().downcast_ref::<Circuit>() {
                    acc + sub_circuit.get_gate_count()
                } else {
                    // Should not happen if get_type() is consistent with the actual type.
                    // Count as 1 if downcast fails for some reason.
                    acc + 1 
                }
            } else {
                // It's a simple gate.
                acc + 1
            }
        })
    }

    // TODO: Convert add_input to this macro? It has an `initial` param.
    pub fn add_input(&mut self, initial: bool) -> GateId {
        let gate = AnyGate::new_input(self.id, initial);
        let gate_id = gate.get_id().gate_id; // Assuming get_id() returns PortId which has gate_id
        self.components.insert(gate_id, LogicComponent::new(Box::new(gate)));
        gate_id
    }

    pub fn add_input_pin(&mut self, pin: usize) -> GateId {
        let mut gate = AnyGate::new_input_pin(self.id);
        gate.set_input(0, Some(false)); // InputPins are initialized to false
        let gate_id = gate.get_id().gate_id;
        self.components.insert(gate_id, LogicComponent::new(Box::new(gate)));
        self.input_gates.insert(pin, gate_id);
        gate_id
    }
    
    // TODO: Convert add_output to this macro?
    pub fn add_output(&mut self) -> GateId {
        let gate = AnyGate::new_output(self.id);
        let gate_id = gate.get_id().gate_id;
        self.components.insert(gate_id, LogicComponent::new(Box::new(gate)));
        gate_id
    }

    pub fn add_output_pin(&mut self, pin: usize) -> GateId {
        let gate = AnyGate::new(GateType::OutputPin, self.id); // Correct way to create an OutputPin
        let gate_id = gate.get_id().gate_id;
        self.components.insert(gate_id, LogicComponent::new(Box::new(gate)));
        self.output_gates.insert(pin, gate_id);
        gate_id
    }
}

macro_rules! impl_circuit_add_gate_methods {
    ($method_name:ident, $new_gate_fn:path) => {
        impl Circuit {
            pub fn $method_name(&mut self) -> GateId {
                let gate = $new_gate_fn(self.id);
                let gate_id = gate.get_id().gate_id; // Assuming get_id() from AnyGate
                self.components.insert(gate_id, LogicComponent::new(Box::new(gate)));
                gate_id
            }
        }
    };
    ($method_name:ident, $new_gate_fn:path, $param_name:ident: $param_type:ty) => {
        impl Circuit {
            pub fn $method_name(&mut self, $param_name: $param_type) -> GateId {
                let gate = $new_gate_fn(self.id, $param_name);
                let gate_id = gate.get_id().gate_id; // Assuming get_id() from AnyGate
                self.components.insert(gate_id, LogicComponent::new(Box::new(gate)));
                gate_id
            }
        }
    };
}

impl_circuit_add_gate_methods!(add_identity, AnyGate::new_identity);
impl_circuit_add_gate_methods!(add_not, AnyGate::new_not);
impl_circuit_add_gate_methods!(add_or, AnyGate::new_or);
impl_circuit_add_gate_methods!(add_and, AnyGate::new_and);
impl_circuit_add_gate_methods!(add_oscillator, AnyGate::new_oscillator, rate: usize);


impl Default for Circuit {
    fn default() -> Self {
        Circuit::new()
    }
}

use crate::{Simulatable, GateType};

impl Simulatable for Circuit {
    fn id(&self) -> ComponentId {
        self.id
    }

    fn get_type(&self) -> GateType {
        GateType::Circuit
    }

    fn set_input(&mut self, pin: PinId, value: Option<bool>) {
        // This existing method on Circuit seems to be the correct implementation
        self.set_input(pin, value)
    }

    fn get_output(&self, pin: PinId) -> Option<bool> {
        // This existing method on Circuit seems to be the correct implementation
        self.get_output(pin)
    }

    fn get_inputs(&self) -> Vec<Option<bool>> {
        // This existing method on Circuit seems to be the correct implementation
        self.get_inputs()
    }

    fn run(&mut self) -> Result<(), TickError> {
        self.tick()
    }

    fn expected_inputs(&self) -> usize {
        // A circuit's "expected_inputs" is dynamic and determined by its input_gates configuration.
        // This might need further refinement based on how it's used.
        // For now, returning the count of configured input pins.
        self.input_gates.len()
    }
    
    fn get_all_outputs(&self) -> Vec<Option<bool>> {
        Circuit::get_outputs(self) // Call the inherent method on Circuit struct
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    // If Simulatable had clone_box:
    // fn clone_box(&self) -> Box<dyn Simulatable> {
    //     Box::new(self.clone()) // Requires Circuit to be Clone
    // }
}
