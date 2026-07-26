use smallvec::SmallVec;

use super::wire::WireId;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InputPin {
    pub name: String,
    pub wire: Option<WireId>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutputPin {
    pub name: String,
    pub wires: SmallVec<[WireId; 4]>,
}
