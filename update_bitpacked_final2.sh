#!/bin/bash

# Remove Clock from SimdOp
sed -i 's/Clock = 11,//' logi-sim/src/circuit/bit_packed.rs

# Insert SimdClock struct properly right after SimdGateInstruction struct
awk '
/^pub struct SimdGateInstruction \{/ {
    in_struct = 1
}
in_struct && /^\}/ {
    print "}"
    print ""
    print "#[derive(Clone, Debug)]"
    print "pub struct SimdClock {"
    print "    pub period_ticks: u32,"
    print "    pub duty_ticks: u32,"
    print "    pub phase: u32,"
    print "    pub outputs: SmallVec<[u32; 2]>,"
    print "}"
    in_struct = 0
    next
}
{ print }
' logi-sim/src/circuit/bit_packed.rs > temp.rs && mv temp.rs logi-sim/src/circuit/bit_packed.rs

# Add to BitPackedCircuit properly
awk '
/^pub struct BitPackedCircuit \{/ {
    print "pub struct BitPackedCircuit {"
    print "    pub instructions: Vec<SimdGateInstruction>,"
    print "    pub clocks: Vec<SimdClock>,"
    in_struct = 1
    next
}
in_struct && /pub instructions: Vec<SimdGateInstruction>,/ {
    next
}
in_struct && /^\}/ {
    in_struct = 0
}
{ print }
' logi-sim/src/circuit/bit_packed.rs > temp.rs && mv temp.rs logi-sim/src/circuit/bit_packed.rs

# Add mut clocks in compile_from_flat
sed -i '/let mut instructions = Vec::with_capacity(node_count);/a \        let mut clocks = Vec::new();' logi-sim/src/circuit/bit_packed.rs

# Replace NodeKind::Clock arm
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

# Pass clocks in Self return
awk '
/instructions,/ {
    print "            instructions,"
    print "            clocks,"
    next
}
{ print }
' logi-sim/src/circuit/bit_packed.rs > temp.rs && mv temp.rs logi-sim/src/circuit/bit_packed.rs
