#!/bin/bash
awk '
/NodeKind::Clock \{ \.\. \} => SimdOp::Clock,/ {
    print "                NodeKind::Clock { period_ticks, duty_ticks, phase } => {"
    print "                    clocks.push(SimdClock {"
    print "                        period_ticks: *period_ticks,"
    print "                        duty_ticks: *duty_ticks,"
    print "                        phase: *phase,"
    print "                        outputs: f_node.outputs.iter().map(|&w| w as u32).collect(),"
    print "                    });"
    print "                    continue;"
    print "                }"
    next
}
{ print }
' logi-sim/src/circuit/bit_packed.rs > temp.rs && mv temp.rs logi-sim/src/circuit/bit_packed.rs

awk '
/instructions,/ {
    print "            instructions,"
    print "            clocks,"
    next
}
{ print }
' logi-sim/src/circuit/bit_packed.rs > temp.rs && mv temp.rs logi-sim/src/circuit/bit_packed.rs
