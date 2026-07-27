with open("logi-sim/src/engine/mod.rs", "r") as f:
    content = f.read()

content = content.replace(
"""    pub fn stop_worker(&mut self) {""",
"""    pub fn set_runner(&mut self, runner_type: RunnerType) {
        self.runner_type = runner_type;
        if let Some(tx) = &self.command_tx {
            let _ = tx.send(SimCommand::SetRunner(runner_type));
        }
    }

    pub fn stop_worker(&mut self) {""")

with open("logi-sim/src/engine/mod.rs", "w") as f:
    f.write(content)
