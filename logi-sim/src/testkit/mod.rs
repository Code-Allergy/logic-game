//! Test and benchmark inputs for simulation v2: a seeded RNG, a random
//! design generator, and hand-built fixtures (design doc sections 8 and 9).

pub mod bitblast;
pub mod diff;
pub mod fixtures;
pub mod random;
pub mod rng;

#[cfg(test)]
mod tests;

pub use random::{GenConfig, Generated, OpMix, generate};
pub use rng::Rng;
