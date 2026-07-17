use std::collections::HashMap;

use crate::circuit::{Circuit, NodeId};
use smallvec::SmallVec;

pub struct Fixture {
    pub circuit: Circuit,
    pub nodes: HashMap<&'static str, NodeId>,
}

pub struct FixtureBuilder {
    pub circuit: Circuit,
    pub nodes: HashMap<&'static str, NodeId>,
}

impl FixtureBuilder {
    pub fn new() -> Self {
        FixtureBuilder {
            circuit: Circuit::new(),
            nodes: HashMap::new(),
        }
    }
    pub fn build(self) -> Fixture {
        Fixture {
            circuit: self.circuit,
            nodes: self.nodes,
        }
    }
}

impl FixtureBuilder {
    pub fn input(&mut self, name: &'static str) -> NodeId {
        let node = self.circuit.input();
        self.nodes.insert(name, node);
        node
    }

    pub fn output(&mut self, name: &'static str) -> NodeId {
        let node = self.circuit.output();
        self.nodes.insert(name, node);
        node
    }

    pub fn and(&mut self) -> NodeId {
        self.circuit.and()
    }

    pub fn or(&mut self) -> NodeId {
        self.circuit.or()
    }

    pub fn not(&mut self) -> NodeId {
        self.circuit.not()
    }
}
