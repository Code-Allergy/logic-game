pub mod circuit;
pub mod engine;
pub mod fixtures;
pub mod graphviz;
pub mod runner;

#[cfg(feature = "bevy")]
pub mod bevy_integration;

pub mod prelude {
    pub use crate::circuit::{
        Circuit, InputPin, InputRef, Node, NodeId, NodeKind, OutputPin, OutputRef, Wire, WireId,
    };
    pub use crate::engine::{SharedState, SimulationEngine};
    pub use crate::runner::{NaiveRunner, Runner};

    #[cfg(feature = "bevy")]
    pub use crate::bevy_integration::LogiSimPlugin;
}

