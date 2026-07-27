with open("logi-sim/src/engine/mod.rs", "r") as f:
    content = f.read()

import re

# Update build_runner to use the returned wire map directly.
old_build = """    if is_optimized {
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
        }"""
new_build = """    if is_optimized {
        let (opt_circuit, node_map, wire_map) = crate::compiler::dead_code::cull_dead_nodes_with_map(circuit);

        let mut reverse_wire_map = std::collections::HashMap::new();
        for (raw, opt) in &wire_map {
            reverse_wire_map.insert(*opt, *raw);
        }"""

content = content.replace(old_build, new_build)

with open("logi-sim/src/engine/mod.rs", "w") as f:
    f.write(content)
