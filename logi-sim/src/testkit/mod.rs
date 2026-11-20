//! Test and benchmark inputs for simulation v2: a seeded RNG, a random
//! design generator, and hand-built fixtures (design doc sections 8 and 9).

pub mod fixtures;
pub mod random;
pub mod rng;

#[cfg(test)]
mod tests;

pub use random::{GenConfig, Generated, generate};
pub use rng::Rng;
