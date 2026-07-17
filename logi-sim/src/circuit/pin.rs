use smallvec::SmallVec;

use super::wire::WireId;

pub struct InputPin {
    pub name: String,
    pub wire: Option<WireId>,
}

pub struct OutputPin {
    pub name: String,
    pub wires: SmallVec<[WireId; 4]>,
}
