pub mod builtin;

// Removed unused import: use std::collections::HashMap;
use ahash::AHashMap;
use uuid::Uuid;

use crate::{
    AnyGate, Circuit, CircuitId, ComponentId, Connection, Gate, GateId, LogicComponent, PinId,
    PortId, TickError, HierarchicalUuidId, FlatConnectionInternal, FlatCircuitExecutionView, InternalGateId // Updated for flattening
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
            // Initialize flat_view (Step 7 of current plan)
            flat_view: None,
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
        self.flat_view = None; // Invalidate flat view
        self.topological_dirty = true; // Structural change
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
        self.flat_view = None; // Invalidate flat view
    }

    // Method to get a mutable reference to a gate using HierarchicalUuidId (Updated parameter type)
    fn get_gate_mut_from_hierarchical_id<'a>(&'a mut self, h_uuid_id: &HierarchicalUuidId) -> Option<&'a mut AnyGate> {
        let mut current_circuit_opt: Option<&mut Circuit> = Some(self);

        for instance_id in &h_uuid_id.instance_path {
            match current_circuit_opt {
                Some(current_circuit_ref) => { // Use a temporary variable for the mutable borrow
                    match current_circuit_ref.components.get_mut(instance_id) {
                        Some(LogicComponent::Circuit(sub_circuit)) => {
                            current_circuit_opt = Some(sub_circuit);
                        }
                        _ => return None, // Not a circuit or invalid ID in path
                    }
                }
                None => return None, // Should not happen if logic is structured correctly
            }
        }

        // After iterating through the instance_path, current_circuit_opt should hold the final circuit
        match current_circuit_opt {
            Some(final_circuit) => {
                match final_circuit.components.get_mut(&h_uuid_id.local_gate_id) {
                    Some(LogicComponent::Gate(gate)) => Some(gate),
                    _ => None, // Not a gate or not found
                }
            }
            None => None, // Path was empty or resolution failed
        }
    }

    // Skeleton for compile_flat_view (Updated for InternalGateId)
    fn compile_flat_view(&mut self) {
        let mut collected_hierarchical_gates: Vec<HierarchicalUuidId> = Vec::new();
        // Intermediate structure for connections using HierarchicalUuidId before mapping to InternalGateId
        #[derive(Clone)] // Make cloneable for processing
        struct RawFlatConnection {
            source_gate: HierarchicalUuidId,
            target_gate: HierarchicalUuidId,
            target_input_pin: usize,
            source_output_pin: usize,
        }
        let mut collected_raw_connections: Vec<RawFlatConnection> = Vec::new();

        fn recursive_collect_raw_data(
            circuit_to_flatten: &Circuit,
            current_instance_path: &mut Vec<ComponentId>,
            collected_hierarchical_gates: &mut Vec<HierarchicalUuidId>,
            collected_raw_connections: &mut Vec<RawFlatConnection>,
        ) {
            // TODO: Implement recursive collection of gates using HierarchicalUuidId
            // TODO: Implement collection/remapping of connections into RawFlatConnection
            // (This is the complex part involving pin mapping and hierarchical resolution)
            let _ = (circuit_to_flatten, current_instance_path, collected_hierarchical_gates, collected_raw_connections);
        }
        
        let mut initial_path = Vec::new();
        recursive_collect_raw_data(self, &mut initial_path, &mut collected_hierarchical_gates, &mut collected_raw_connections);

        let mut next_internal_gate_id: InternalGateId = 0;
        let mut hierarchical_to_internal_id_map: AHashMap<HierarchicalUuidId, InternalGateId> = AHashMap::new();
        let mut internal_to_hierarchical_uuid_id_map_builder: AHashMap<InternalGateId, HierarchicalUuidId> = AHashMap::new();
        
        // Assign InternalGateIds (example, actual unique gates needed)
        // This should iterate over unique HierarchicalUuidId found in collected_hierarchical_gates
        // For now, using the placeholder vector directly, assuming it contains unique items for skeleton.
        let mut unique_hierarchical_gates = collected_hierarchical_gates; // Placeholder for actual unique gate collection
        unique_hierarchical_gates.sort_unstable_by_key(|h_id| (h_id.instance_path.clone(), h_id.local_gate_id)); // Ensure consistent ordering for dedup if any
        unique_hierarchical_gates.dedup(); // Remove duplicates if any

        for h_uuid_id in unique_hierarchical_gates {
            let internal_id = next_internal_gate_id;
            hierarchical_to_internal_id_map.insert(h_uuid_id.clone(), internal_id);
            internal_to_hierarchical_uuid_id_map_builder.insert(internal_id, h_uuid_id);
            next_internal_gate_id += 1;
        }

        let mut final_remapped_connections_internal: AHashMap<InternalGateId, Vec<FlatConnectionInternal>> = AHashMap::new();
        for raw_conn in collected_raw_connections {
            if let (Some(&source_internal_id), Some(&target_internal_id)) = (
                hierarchical_to_internal_id_map.get(&raw_conn.source_gate),
                hierarchical_to_internal_id_map.get(&raw_conn.target_gate),
            ) {
                final_remapped_connections_internal
                    .entry(source_internal_id)
                    .or_default()
                    .push(FlatConnectionInternal {
                        target_gate: target_internal_id,
                        target_input_pin: raw_conn.target_input_pin,
                        source_output_pin: raw_conn.source_output_pin,
                    });
            } else {
                // Handle error: Hierarchical ID not found in map (should not happen if collection is correct)
                // For skeleton, we might ignore or panic.
            }
        }
        
        // TODO: Implement global topological sort of InternalGateIds
        // This will use final_remapped_connections_internal to determine dependencies.
        // For now, a placeholder:
        let sorted_internal_gate_ids: Vec<InternalGateId> = 
            (0..next_internal_gate_id).collect(); // Simple sequence, not topologically sorted

        self.flat_view = Some(FlatCircuitExecutionView {
            topologically_sorted_gates: sorted_internal_gate_ids,
            remapped_outgoing_connections: final_remapped_connections_internal,
            internal_to_hierarchical_uuid_id: internal_to_hierarchical_uuid_id_map_builder,
        });
    }

    // Public method to trigger compilation
    pub fn compile(&mut self) {
        self.compile_flat_view();
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
        self.flat_view = None; // Invalidate flat view
        gate_id
    }

    pub fn add_output(&mut self) -> GateId {
        let gate = AnyGate::new_output(self.id);
        let gate_id = gate.get_id().gate_id;
        self.components.insert(gate_id, LogicComponent::Gate(gate));
        self.flat_view = None; // Invalidate flat view
        gate_id
    }

    pub fn add_output_pin(&mut self, pin: usize) -> GateId {
        let gate = AnyGate::new_output(self.id); 
        let gate_id = gate.get_id().gate_id;
        self.components.insert(gate_id, LogicComponent::Gate(gate));
        self.output_gates.insert(pin, gate_id);
        self.pins_dirty = true; 
        self.flat_view = None; // Invalidate flat view
        gate_id
    }

    pub fn add_identity(&mut self) -> GateId {
        let gate = AnyGate::new_identity(self.id);
        let gate_id = gate.get_id().gate_id;
        self.components.insert(gate_id, LogicComponent::Gate(gate));
        self.flat_view = None; // Invalidate flat view
        gate_id
    }

    pub fn add_not(&mut self) -> GateId {
        let gate = AnyGate::new_not(self.id);
        let gate_id = gate.get_id().gate_id;
        self.components.insert(gate_id, LogicComponent::Gate(gate));
        self.flat_view = None; // Invalidate flat view
        gate_id
    }

    pub fn add_or(&mut self) -> GateId {
        let gate = AnyGate::new_or(self.id);
        let gate_id = gate.get_id().gate_id;
        self.components.insert(gate_id, LogicComponent::Gate(gate));
        self.flat_view = None; // Invalidate flat view
        gate_id
    }

    pub fn add_and(&mut self) -> GateId {
        let gate = AnyGate::new_and(self.id);
        let gate_id = gate.get_id().gate_id;
        self.components.insert(gate_id, LogicComponent::Gate(gate));
        self.flat_view = None; // Invalidate flat view
        gate_id
    }

    pub fn add_oscillator(&mut self, rate: usize) -> GateId {
        let gate = AnyGate::new_oscillator(self.id, rate);
        let gate_id = gate.get_id().gate_id;
        self.components.insert(gate_id, LogicComponent::Gate(gate));
        self.flat_view = None; // Invalidate flat view
        gate_id
    }
}

impl Default for Circuit {
    fn default() -> Self {
        Circuit::new()
    }
}
