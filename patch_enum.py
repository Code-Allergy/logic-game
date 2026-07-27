import re

with open("logi-sim/src/engine/mod.rs", "r") as f:
    content = f.read()

enum_def = """
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RunnerType {
    Naive,
    NaiveOptimized,
    Flat,
    #[default]
    FlatOptimized,
    EventDriven,
    EventDrivenOptimized,
    Simd,
    SimdOptimized,
}

/// Commands sent to the simulation worker thread.
"""

content = content.replace("/// Commands sent to the simulation worker thread.\n", enum_def)

with open("logi-sim/src/engine/mod.rs", "w") as f:
    f.write(content)
