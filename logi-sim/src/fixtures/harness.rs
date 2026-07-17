use crate::circuit::NodeId;
use crate::runner::Runner;

pub struct RunnerHarness<R: Runner> {
    runner: R,
}

impl<R: Runner> RunnerHarness<R> {
    pub fn tick(&mut self) {
        self.runner.tick();
    }

    pub fn set(&mut self, input: NodeId, pin: u8, value: bool) {
        self.runner.set_input(input, pin, value);
    }

    pub fn output(&self, output: NodeId, pin: u8) -> bool {
        self.runner.output(output, pin)
    }
}
