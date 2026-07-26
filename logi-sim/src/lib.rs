pub mod circuit;
pub mod compiler;
pub mod engine;
pub mod fixtures;
pub mod graphviz;
pub mod runner;

#[cfg(feature = "bevy")]
pub mod bevy_integration;

pub mod prelude {
    pub use crate::circuit::{
        Circuit, FlattenedCircuit, InputPin, InputRef, Node, NodeId, NodeKind, OutputPin,
        OutputRef, Wire, WireId,
    };
    pub use crate::compiler::Compiler;
    pub use crate::engine::{SharedState, SimulationEngine};
    pub use crate::runner::{FlatRunner, InspectableRunner, NaiveRunner, Runner};


    #[cfg(feature = "bevy")]
    pub use crate::bevy_integration::LogiSimPlugin;
}
