use logi_sim::circuit::Circuit;
use logi_sim::runner::{NaiveRunner, Runner};

pub struct Factory;

pub trait RunnerFactory {
    type Runner<'a>: Runner
    where
        Self: 'a;

    fn create<'a>(circuit: &'a Circuit) -> Self::Runner<'a>;
}

impl RunnerFactory for Factory {
    type Runner<'a> = NaiveRunner<'a>;

    fn create<'a>(circuit: &'a Circuit) -> Self::Runner<'a> {
        NaiveRunner::new(circuit)
    }
}
