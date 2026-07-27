import re

with open("logi-sim/src/engine/mod.rs", "r") as f:
    content = f.read()

# Add MappedRunner struct
mapped_runner = """
struct MappedRunner {
    inner: Box<dyn InspectableRunner>,
    node_map: std::collections::HashMap<NodeId, NodeId>,
    wire_map: std::collections::HashMap<WireId, WireId>,
    reverse_wire_map: std::collections::HashMap<WireId, WireId>,
}

impl Runner for MappedRunner {
    fn set_input(&mut self, node: NodeId, pin: u8, value: bool) {
        if let Some(&mapped_node) = self.node_map.get(&node) {
            self.inner.set_input(mapped_node, pin, value);
        }
    }

    fn tick(&mut self) {
        self.inner.tick();
    }

    fn output(&self, node: NodeId, pin: u8) -> bool {
        if let Some(&mapped_node) = self.node_map.get(&node) {
            self.inner.output(mapped_node, pin)
        } else {
            false
        }
    }

    fn current_tick(&self) -> u64 {
        self.inner.current_tick()
    }
}

impl InspectableRunner for MappedRunner {
    fn wire_value(&self, wire: WireId) -> bool {
        if let Some(&mapped_wire) = self.wire_map.get(&wire) {
            self.inner.wire_value(mapped_wire)
        } else {
            false
        }
    }

    fn for_each_wire(&self, f: &mut dyn FnMut(WireId, bool)) {
        self.inner.for_each_wire(&mut |mapped_wire, value| {
            if let Some(&raw_wire) = self.reverse_wire_map.get(&mapped_wire) {
                f(raw_wire, value);
            }
        });
    }
}

fn build_runner(circuit: &Circuit, runner_type: RunnerType) -> Box<dyn InspectableRunner> {
    use crate::compiler::Compiler;
    use crate::circuit::{FlattenedCircuit, EventDrivenCircuit, BitPackedCircuit};
    use crate::runner::{FlatRunner, EventDrivenRunner, SimdRunner};

    let is_optimized = match runner_type {
        RunnerType::NaiveOptimized | RunnerType::FlatOptimized | RunnerType::EventDrivenOptimized | RunnerType::SimdOptimized => true,
        _ => false,
    };

    if is_optimized {
        let (opt_circuit, node_map) = crate::compiler::dead_code::cull_dead_nodes_with_map(circuit);

        let mut wire_map = std::collections::HashMap::new();
        let mut reverse_wire_map = std::collections::HashMap::new();

        // cull_dead_nodes_with_map preserves wires that are connected. But it creates a new circuit with new WireIds.
        // We need to map raw wire IDs to opt wire IDs.
        // Let's match them based on source node and pin.
        // For each wire in opt_circuit, find its source node and pin. Map back to raw source node, and find raw wire.
        let mut reverse_node_map = std::collections::HashMap::new();
        for (raw_node, opt_node) in &node_map {
            reverse_node_map.insert(*opt_node, *raw_node);
        }

        for (opt_wire_id, opt_wire) in &opt_circuit.wires {
            let opt_source_node = opt_wire.source.node;
            if let Some(&raw_source_node) = reverse_node_map.get(&opt_source_node) {
                if let Some(raw_node_ref) = circuit.nodes.get(raw_source_node) {
                    if let Some(raw_out_pin) = raw_node_ref.outputs.get(opt_wire.source.pin as usize) {
                        if let Some(&raw_wire_id) = raw_out_pin.wires.first() {
                            wire_map.insert(raw_wire_id, opt_wire_id);
                            reverse_wire_map.insert(opt_wire_id, raw_wire_id);
                        }
                    }
                }
            }
        }

        let inner: Box<dyn InspectableRunner> = match runner_type {
            RunnerType::NaiveOptimized => Box::new(NaiveRunner::new(opt_circuit)),
            RunnerType::FlatOptimized => {
                let flat = FlattenedCircuit::compile(&opt_circuit);
                Box::new(FlatRunner::new(flat))
            }
            RunnerType::EventDrivenOptimized => {
                let evt = EventDrivenCircuit::compile(&opt_circuit);
                Box::new(EventDrivenRunner::new(evt))
            }
            RunnerType::SimdOptimized => {
                let bp = BitPackedCircuit::compile(&opt_circuit);
                Box::new(SimdRunner::new(bp))
            }
            _ => unreachable!(),
        };

        Box::new(MappedRunner {
            inner,
            node_map,
            wire_map,
            reverse_wire_map,
        })
    } else {
        match runner_type {
            RunnerType::Naive => Box::new(NaiveRunner::new(circuit.clone())),
            RunnerType::Flat => {
                let flat = FlattenedCircuit::compile(circuit);
                Box::new(FlatRunner::new(flat))
            }
            RunnerType::EventDriven => {
                let evt = EventDrivenCircuit::compile(circuit);
                Box::new(EventDrivenRunner::new(evt))
            }
            RunnerType::Simd => {
                let bp = BitPackedCircuit::compile(circuit);
                Box::new(SimdRunner::new(bp))
            }
            _ => unreachable!(),
        }
    }
}
"""

content = re.sub(r'fn build_runner.*?(?=\nimpl SimulationEngine \{)', mapped_runner, content, flags=re.DOTALL)

# Fix SetInput rebuild missing
set_input_orig = """                        SimCommand::SetInput { node, pin, value } => {
                            runner.set_input(node, pin, value);
                        }"""
set_input_new = """                        SimCommand::SetInput { node, pin, value } => {
                            if needs_rebuild {
                                runner = build_runner(&circuit, current_runner_type);
                                needs_rebuild = false;
                            }
                            runner.set_input(node, pin, value);
                        }"""
content = content.replace(set_input_orig, set_input_new)


with open("logi-sim/src/engine/mod.rs", "w") as f:
    f.write(content)
