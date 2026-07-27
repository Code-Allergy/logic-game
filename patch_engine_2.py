import re

with open("logi-sim/src/engine/mod.rs", "r") as f:
    content = f.read()

# Add builder fn
builder = """
fn build_runner(circuit: &Circuit, runner_type: RunnerType) -> Box<dyn InspectableRunner> {
    use crate::compiler::Compiler;
    use crate::circuit::{FlattenedCircuit, EventDrivenCircuit, BitPackedCircuit};
    use crate::runner::{FlatRunner, EventDrivenRunner, SimdRunner};
    match runner_type {
        RunnerType::Naive => Box::new(NaiveRunner::new(circuit.clone())),
        RunnerType::NaiveOptimized => {
            let opt = Compiler::optimize(circuit);
            Box::new(NaiveRunner::new(opt))
        }
        RunnerType::Flat => {
            let flat = FlattenedCircuit::compile(circuit);
            Box::new(FlatRunner::new(flat))
        }
        RunnerType::FlatOptimized => {
            let opt = Compiler::optimize(circuit);
            let flat = FlattenedCircuit::compile(&opt);
            Box::new(FlatRunner::new(flat))
        }
        RunnerType::EventDriven => {
            let evt = EventDrivenCircuit::compile(circuit);
            Box::new(EventDrivenRunner::new(evt))
        }
        RunnerType::EventDrivenOptimized => {
            let opt = Compiler::optimize(circuit);
            let evt = EventDrivenCircuit::compile(&opt);
            Box::new(EventDrivenRunner::new(evt))
        }
        RunnerType::Simd => {
            let bp = BitPackedCircuit::compile(circuit);
            Box::new(SimdRunner::new(bp))
        }
        RunnerType::SimdOptimized => {
            let opt = Compiler::optimize(circuit);
            let bp = BitPackedCircuit::compile(&opt);
            Box::new(SimdRunner::new(bp))
        }
    }
}
"""

content = content.replace("impl SimulationEngine {", builder + "\nimpl SimulationEngine {")

# Update start_worker
start_worker_orig = """
        let shared = Arc::clone(&self.shared_state);
        let mut circuit = self.circuit.clone();

        let handle = spawn(move || {
            let mut runner = NaiveRunner::new(circuit.clone());
            let mut running = true;
"""

start_worker_new = """
        let shared = Arc::clone(&self.shared_state);
        let mut circuit = self.circuit.clone();
        let mut current_runner_type = self.runner_type;

        let handle = spawn(move || {
            let mut runner = build_runner(&circuit, current_runner_type);
            let mut running = true;
            let mut needs_rebuild = false;
"""

content = content.replace(start_worker_orig, start_worker_new)


# Worker loop replace `runner = NaiveRunner::new(circuit.clone());` with `needs_rebuild = true;`
content = content.replace("runner = NaiveRunner::new(circuit.clone());", "needs_rebuild = true;")
# Remove unused `_runner_type`
content = content.replace("SimCommand::SetRunner(runner_type) => {\n                            // Handled later\n                        }", "SimCommand::SetRunner(rt) => {\n                            current_runner_type = rt;\n                            needs_rebuild = true;\n                        }")

tick_orig = """
                    if should_tick {
                        let _ = tx_evt.send(SimEvent::TickStarted {
                            tick_count: shared.tick_count.load(Ordering::Relaxed),
                        });
                        runner.tick();
"""

tick_new = """
                    if should_tick {
                        if needs_rebuild {
                            runner = build_runner(&circuit, current_runner_type);
                            needs_rebuild = false;
                        }
                        let _ = tx_evt.send(SimEvent::TickStarted {
                            tick_count: shared.tick_count.load(Ordering::Relaxed),
                        });
                        runner.tick();
"""

content = content.replace(tick_orig, tick_new)

# step sync
sync_orig = """    pub fn step_sync(&mut self) {
        let mut runner = NaiveRunner::new(self.circuit.clone());
        runner.tick();"""

sync_new = """    pub fn step_sync(&mut self) {
        let mut runner = build_runner(&self.circuit, self.runner_type);
        runner.tick();"""

content = content.replace(sync_orig, sync_new)

# Rebuild circuit before step cmd
cmd_step_orig = """                        SimCommand::Step => {
                            runner.tick();"""
cmd_step_new = """                        SimCommand::Step => {
                            if needs_rebuild {
                                runner = build_runner(&circuit, current_runner_type);
                                needs_rebuild = false;
                            }
                            runner.tick();"""

content = content.replace(cmd_step_orig, cmd_step_new)

# Add set_runner func
add_set_runner = """
    /// Stop the background worker thread if active.
    pub fn stop_worker(&mut self) {
"""

add_set_runner_new = """
    pub fn set_runner(&mut self, runner_type: RunnerType) {
        self.runner_type = runner_type;
        if let Some(tx) = &self.command_tx {
            let _ = tx.send(SimCommand::SetRunner(runner_type));
        }
    }

    /// Stops the background worker thread if active.
    pub fn stop_worker(&mut self) {
"""
content = content.replace(add_set_runner, add_set_runner_new)

with open("logi-sim/src/engine/mod.rs", "w") as f:
    f.write(content)
