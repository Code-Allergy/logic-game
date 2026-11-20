pub mod circuit;
pub mod compiler;
pub mod engine;
pub mod fixtures;
pub mod graphviz;
pub mod netlist;
pub mod reference;
pub mod runner;

pub mod prelude {
    pub use crate::circuit::{
        BitPackedCircuit, Circuit, EventDrivenCircuit, FlattenedCircuit, InputPin, InputRef, Node,
        NodeId, NodeKind, OutputPin, OutputRef, SimdAdder, SimdBinaryInstruction, SimdBlock,
        SimdClock, SimdCounterInstruction, SimdDemuxInstruction, SimdIndex, SimdOp, SimdProgram,
        SimdPrograms, SimdTernaryInstruction, SimdUnaryInstruction, Wire, WireId,
    };
    pub use crate::compiler::Compiler;
    pub use crate::engine::{SharedState, SimCommand, SimEvent, SimulationEngine};
    pub use crate::runner::{
        EventDrivenRunner, FlatRunner, InspectableRunner, NaiveRunner, Runner,
    };
    #[cfg(not(target_arch = "wasm32"))]
    pub use crate::runner::{SimdRunner, ThreadedFlatRunner};
}
