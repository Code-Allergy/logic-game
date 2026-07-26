use smallvec::SmallVec;

use super::wire::WireId;

#[derive(Clone, Debug)]
pub struct InputPin {
    pub name: String,
    pub wire: Option<WireId>,
}

#[derive(Clone, Debug)]
pub struct OutputPin {
    pub name: String,
    pub wires: SmallVec<[WireId; 4]>,
}

