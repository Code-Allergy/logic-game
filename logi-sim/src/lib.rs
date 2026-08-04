pub mod circuit;
pub mod compiler;
pub mod engine;
pub mod fixtures;
pub mod graphviz;
pub mod runner;

pub mod prelude {
    pub use crate::circuit::{
        BitPackedCircuit, Circuit, EventDrivenCircuit, FlattenedCircuit, InputPin, InputRef, Node,
        NodeId, NodeKind, OutputPin, OutputRef, SimdBinaryInstruction, SimdBlock, SimdClock,
        SimdDemuxInstruction, SimdIndex, SimdOp, SimdProgram, SimdPrograms, SimdTernaryInstruction,
        SimdUnaryInstruction, Wire, WireId,
    };
    pub use crate::compiler::Compiler;
    pub use crate::engine::{SharedState, SimCommand, SimEvent, SimulationEngine};
    pub use crate::runner::{
        EventDrivenRunner, FlatRunner, InspectableRunner, NaiveRunner, Runner, SimdRunner,
    };
}
