#!/bin/bash
sed -i 's/Clock { period_ticks: u32 }/Clock { period_ticks: u32, duty_ticks: u32 }/' src/sim/model.rs
sed -i 's/Clock { period_ticks } => Some(NodeKind::Clock {/Clock { period_ticks, duty_ticks } => Some(NodeKind::Clock {/' src/sim/model.rs
sed -i '/period_ticks,/a \                    duty_ticks: *duty_ticks,' src/sim/model.rs

sed -i 's/GameNodeKind::Clock { period_ticks: 4 },/GameNodeKind::Clock { period_ticks: 4, duty_ticks: 1 },/' src/sim/fixtures.rs
sed -i 's/GameNodeKind::Clock { period_ticks: 10 }/GameNodeKind::Clock { period_ticks: 10, duty_ticks: 5 }/' src/editor.rs
