#!/bin/bash
sed -i 's/Clock = 11,//' logi-sim/src/circuit/bit_packed.rs
sed -i '/pub outputs: SmallVec<\[u32; 2\]>,/a \
\
#[derive(Clone, Debug)]\
pub struct SimdClock {\
    pub period_ticks: u32,\
    pub duty_ticks: u32,\
    pub phase: u32,\
    pub outputs: SmallVec<[u32; 2]>,\
}' logi-sim/src/circuit/bit_packed.rs

sed -i '/pub instructions: Vec<SimdGateInstruction>,/a \    pub clocks: Vec<SimdClock>,' logi-sim/src/circuit/bit_packed.rs
sed -i '/let mut instructions = Vec::with_capacity(node_count);/a \        let mut clocks = Vec::new();' logi-sim/src/circuit/bit_packed.rs

# Using awk for the multiline replace of NodeKind::Clock
awk '
/NodeKind::Clock \{ \.\. \} => SimdOp::Clock,/ {
    print "                NodeKind::Clock { period_ticks, duty_ticks, phase } => {"
    print "                    clocks.push(SimdClock {"
    print "                        period_ticks,"
    print "                        duty_ticks,"
    print "                        phase,"
    print "                        outputs: f_node.outputs.iter().map(|&w| w as u32).collect(),"
    print "                    });"
    print "                    continue;"
    print "                }"
    next
}
{ print }
' logi-sim/src/circuit/bit_packed.rs > temp.rs && mv temp.rs logi-sim/src/circuit/bit_packed.rs

sed -i '/instructions,/a \            clocks,' logi-sim/src/circuit/bit_packed.rs
