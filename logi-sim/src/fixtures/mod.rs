use crate::fixtures::builder::{Fixture, FixtureBuilder};

mod builder;
mod harness;

pub fn and_gate() -> Fixture {
    let mut b = FixtureBuilder::new();

    let a = b.input("A");
    let b_in = b.input("B");

    let and = b.and();

    let out = b.output("Y");

    b.circuit.connect((a, 0), (and, 0));
    b.circuit.connect((b_in, 0), (and, 1));
    b.circuit.connect((and, 0), (out, 0));

    b.build()
}
pub fn or_gate() -> Fixture {
    let mut b = FixtureBuilder::new();

    let a = b.input("A");
    let b_in = b.input("B");

    let or = b.or();

    let out = b.output("Y");

    b.circuit.connect((a, 0), (or, 0));
    b.circuit.connect((b_in, 0), (or, 1));
    b.circuit.connect((or, 0), (out, 0));

    b.build()
}
pub fn not_gate() -> Fixture {
    let mut b = FixtureBuilder::new();

    let a = b.input("A");
    let not = b.not();

    let out = b.output("Y");

    b.circuit.connect((a, 0), (not, 0));
    b.circuit.connect((not, 0), (out, 0));

    b.build()
}
