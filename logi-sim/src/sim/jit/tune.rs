//! How generated code is cut into functions and laid out, with overrides
//! from the environment for measurements (read once per process; a value
//! that does not parse is ignored):
//!
//! - `LOGI_JIT_CHUNK`: items per settle chunk ([`super::CHUNK`]).
//! - `LOGI_JIT_COMMIT_CHUNK`: commits per commit chunk
//!   ([`super::COMMIT_CHUNK`]).
//! - `LOGI_JIT_ALIGN`: bytes each native function is aligned to (a power
//!   of two up to 4096; Cranelift alone aligns to 1).
//! - `LOGI_JIT_ARENA`: `0` puts native code where `cranelift-jit` puts it
//!   by default (a page-rounded mapping per function that does not fit
//!   the last one), `1` in contiguous arenas (see `clif::CodeMemory`).
//! - `LOGI_JIT_ENTRY`: `1` gives a body of several settle chunks an entry
//!   function that calls each directly (the caller then makes one call per
//!   settle), `0` has the caller call each chunk ([`super::SETTLE_ENTRY`]).
//! - `LOGI_JIT_NARROW`: `0` computes every value in 64-bit operations
//!   (native code; by default values that fit 32 bits use 32-bit ones,
//!   see `bits.rs`).
//! - `LOGI_JIT_REMAT`: native code loads a slot's value again once it went
//!   unused for more than this many items (`0`: never), see
//!   [`super::REMAT`].
//!
//! The superstep's chunk is a `SimOptions` field
//! (`SimOptions::superstep_chunk`); the benches read it from
//! `SUPERSTEP_CHUNK`.

use std::sync::OnceLock;

/// The values in effect.
#[derive(Debug, Clone, Copy)]
pub struct Tune {
    pub settle_chunk: usize,
    pub commit_chunk: usize,
    #[cfg_attr(target_arch = "wasm32", expect(dead_code))]
    pub align: u64,
    #[cfg_attr(target_arch = "wasm32", expect(dead_code))]
    pub arena: bool,
    pub settle_entry: bool,
    #[cfg_attr(target_arch = "wasm32", expect(dead_code))]
    pub narrow: bool,
    #[cfg_attr(target_arch = "wasm32", expect(dead_code))]
    pub remat: u32,
}

fn env<T: std::str::FromStr>(name: &str) -> Option<T> {
    std::env::var(name).ok()?.trim().parse().ok()
}

/// The values in effect (the defaults, or the environment's).
pub fn get() -> &'static Tune {
    static T: OnceLock<Tune> = OnceLock::new();
    T.get_or_init(|| Tune {
        settle_chunk: env("LOGI_JIT_CHUNK")
            .filter(|&n| n > 0)
            .unwrap_or(super::CHUNK),
        commit_chunk: env("LOGI_JIT_COMMIT_CHUNK")
            .filter(|&n| n > 0)
            .unwrap_or(super::COMMIT_CHUNK),
        align: env("LOGI_JIT_ALIGN")
            .filter(|&n: &u64| n.is_power_of_two() && n <= 4096)
            .unwrap_or(super::FN_ALIGN),
        arena: env::<u8>("LOGI_JIT_ARENA").map_or(super::ARENA, |v| v != 0),
        settle_entry: env::<u8>("LOGI_JIT_ENTRY").map_or(super::SETTLE_ENTRY, |v| v != 0),
        narrow: env::<u8>("LOGI_JIT_NARROW").is_none_or(|v| v != 0),
        remat: env("LOGI_JIT_REMAT").unwrap_or(super::REMAT),
    })
}
