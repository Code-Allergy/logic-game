pub mod circuit;
pub mod graphviz;

pub mod prelude {
    pub use crate::circuit::{
        Circuit, InputPin, InputRef, Node, NodeId, NodeKind, OutputPin, OutputRef, Wire, WireId,
    };
}
