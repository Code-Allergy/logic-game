use logi_sim::circuit::{Circuit, NodeId};
use logi_sim::prelude::*;

pub struct NaiveFactory;
pub struct FlatFactory;

pub trait RunnerFactory {
    type Runner<'a>: Runner
    where
        Self: 'a;

    fn create<'a>(circuit: &'a Circuit) -> Self::Runner<'a>;
}

impl RunnerFactory for NaiveFactory {
    type Runner<'a> = NaiveRunner<'a>;

    fn create<'a>(circuit: &'a Circuit) -> Self::Runner<'a> {
        NaiveRunner::new(circuit)
    }
}

pub struct FlatRunnerWrapper(FlatRunner);

impl Runner for FlatRunnerWrapper {
    fn set_input(&mut self, node: NodeId, pin: u8, value: bool) {
        self.0.set_input(node, pin, value);
    }

    fn tick(&mut self) {
        self.0.tick();
    }

    fn output(&self, node: NodeId, pin: u8) -> bool {
        self.0.output(node, pin)
    }
}

impl RunnerFactory for FlatFactory {
    type Runner<'a> = FlatRunnerWrapper;

    fn create<'a>(circuit: &'a Circuit) -> Self::Runner<'a> {
        let flat_circuit = FlattenedCircuit::compile(circuit);
        FlatRunnerWrapper(FlatRunner::new(flat_circuit))
    }
}

pub struct EventDrivenFactory;

pub struct EventDrivenRunnerWrapper(EventDrivenRunner);

impl Runner for EventDrivenRunnerWrapper {
    fn set_input(&mut self, node: NodeId, pin: u8, value: bool) {
        self.0.set_input(node, pin, value);
    }

    fn tick(&mut self) {
        self.0.tick();
    }

    fn output(&self, node: NodeId, pin: u8) -> bool {
        self.0.output(node, pin)
    }
}

impl RunnerFactory for EventDrivenFactory {
    type Runner<'a> = EventDrivenRunnerWrapper;

    fn create<'a>(circuit: &'a Circuit) -> Self::Runner<'a> {
        let event_circuit = EventDrivenCircuit::compile(circuit);
        EventDrivenRunnerWrapper(EventDrivenRunner::new(event_circuit))
    }
}

pub struct SimdFactory;

pub struct SimdRunnerWrapper(SimdRunner);

impl Runner for SimdRunnerWrapper {
    fn set_input(&mut self, node: NodeId, pin: u8, value: bool) {
        self.0.set_input(node, pin, value);
    }

    fn tick(&mut self) {
        self.0.tick();
    }

    fn output(&self, node: NodeId, pin: u8) -> bool {
        self.0.output(node, pin)
    }
}

impl RunnerFactory for SimdFactory {
    type Runner<'a> = SimdRunnerWrapper;

    fn create<'a>(circuit: &'a Circuit) -> Self::Runner<'a> {
        let bit_packed = BitPackedCircuit::compile(circuit);
        SimdRunnerWrapper(SimdRunner::new(bit_packed))
    }
}




