with open("src/sim/bridge.rs", "r") as f:
    content = f.read()

import re

# We need to import RunnerType from logi_sim::engine
content = content.replace(
    "use logi_sim::engine::{SimCommand, SimEvent, SimulationEngine};",
    "use logi_sim::engine::{RunnerType, SimCommand, SimEvent, SimulationEngine};"
)

# And expose a method to set the runner type
new_method = """
    pub fn set_tick_rate(&self, tick_rate: Option<f64>) {
        self.send_cmd(SimCommand::SetTickRate(tick_rate));
    }

    pub fn set_runner_type(&mut self, runner_type: RunnerType) {
        self.inner().engine.set_runner(runner_type);
    }
"""
content = content.replace("""
    pub fn set_tick_rate(&self, tick_rate: Option<f64>) {
        self.send_cmd(SimCommand::SetTickRate(tick_rate));
    }
""", new_method)

with open("src/sim/bridge.rs", "w") as f:
    f.write(content)
