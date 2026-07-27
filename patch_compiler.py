with open("logi-sim/src/compiler/mod.rs", "r") as f:
    content = f.read()

import re

old = """    pub fn optimize_with_map(
        circuit: &Circuit,
    ) -> (
        Circuit,
        std::collections::HashMap<crate::circuit::NodeId, crate::circuit::NodeId>,
    ) {
        dead_code::cull_dead_nodes(circuit)
    }"""
new = """    pub fn optimize_with_map(
        circuit: &Circuit,
    ) -> (
        Circuit,
        std::collections::HashMap<crate::circuit::NodeId, crate::circuit::NodeId>,
    ) {
        let (c, m, _) = dead_code::cull_dead_nodes_with_map(circuit);
        (c, m)
    }"""
content = content.replace(old, new)

with open("logi-sim/src/compiler/mod.rs", "w") as f:
    f.write(content)
