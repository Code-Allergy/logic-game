pub mod circuit;
pub mod fixtures;
pub mod graphviz;
pub mod runner;

pub mod prelude {
    pub use crate::circuit::{
        Circuit, InputPin, InputRef, Node, NodeId, NodeKind, OutputPin, OutputRef, Wire, WireId,
    };
}
