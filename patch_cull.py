with open("logi-sim/src/compiler/dead_code.rs", "r") as f:
    content = f.read()

import re

# Update return type
content = content.replace("-> (Circuit, std::collections::HashMap<NodeId, NodeId>) {", "-> (Circuit, std::collections::HashMap<NodeId, NodeId>, std::collections::HashMap<WireId, WireId>) {")

# Add wire_map
content = content.replace("let mut node_map = std::collections::HashMap::new();", "let mut node_map = std::collections::HashMap::new();\n    let mut wire_map = std::collections::HashMap::new();")

# Populate wire_map
old_loop = """    for (wire_id, wire) in &circuit.wires {
        if live_wires.contains(&wire_id) {
            if let Some(&new_source) = node_map.get(&wire.source.node) {
                for sink in &wire.sinks {
                    if let Some(&new_sink) = node_map.get(&sink.node) {
                        optimized.connect((new_source, wire.source.pin), (new_sink, sink.pin));
                    }
                }
            }
        }
    }"""
new_loop = """    for (wire_id, wire) in &circuit.wires {
        if live_wires.contains(&wire_id) {
            if let Some(&new_source) = node_map.get(&wire.source.node) {
                for sink in &wire.sinks {
                    if let Some(&new_sink) = node_map.get(&sink.node) {
                        let new_wire_id = optimized.connect((new_source, wire.source.pin), (new_sink, sink.pin));
                        wire_map.insert(*wire_id, new_wire_id);
                    }
                }
            }
        }
    }"""
content = content.replace(old_loop, new_loop)

# Return
content = content.replace("(optimized, node_map)", "(optimized, node_map, wire_map)")

with open("logi-sim/src/compiler/dead_code.rs", "w") as f:
    f.write(content)
