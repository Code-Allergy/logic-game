//! State-keyed specialization, runtime (issue #117, package P4 of
//! `docs/adaptive-spec.md`): a fixed plan of keys per clock phase, and
//! bodies built for given key values when the `Sim` is built.
//!
//! Per fused tick of a keyed phase ([`super::Sim::run`]), the runtime
//! applies the inputs, runs the key's *prefix* (the fused body's own
//! instructions computing the key slots; unguarded), packs the key from
//! the slots and looks it up: a hit runs the body specialized for that
//! key ([`SpecContext::specialize`]), a miss the *fallback*
//! ([`SpecContext::fallback`]: the general body without the prefix's
//! instructions). Both are guarded like the fused body they replace. The
//! commit after the settle is the settled body's own (`Sim::last`).
//!
//! Exactness: a specialized body only runs on a tick whose key, computed
//! this tick, is the one it was built for; under that assumption it
//! commits what the general fused body commits (`compile::spec`); fused
//! ticks are observed by nothing but their commit. The compile side folds
//! ROM contents, so a ROM write turns specialization off with the
//! known-bits bodies (`Sim::rom_changed`) until `Sim::reset`.
//!
//! Adaptive tables (hot keys built in the background, eviction) are P5b;
//! an automatic plan is P6. Here a plan is given ([`SpecOptions::plan`])
//! and every body is built inline.

use std::sync::Arc;

use crate::compile::spec::{KeyLayout, SpecContext, pext};
use crate::compile::{Body, FastMap, Program, phases};
use crate::netlist::mask;

use super::SimOptions;

/// State-keyed specialization ([`SimOptions::spec`]). Changes speed only,
/// never results.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SpecOptions {
    /// Fixed keys, and the key values to build bodies for, per clock
    /// phase. `None`: no specialization (an automatic plan is not
    /// implemented yet).
    pub plan: Option<Arc<SpecPlan>>,
}

/// What each clock phase is keyed on: `phases[k]` for the fused ticks of
/// phase `k` (tick residue `k` of the clock-phase period; phase 0 for a
/// program run without variants). A missing or `None` entry runs the
/// phase's fused body.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SpecPlan {
    pub phases: Vec<Option<PhasePlan>>,
}

/// One phase's key and the key values that get a body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhasePlan {
    /// The key: slots (state, inputs, or values the fused body computes)
    /// and their bits. A layout wider than 64 bits, or naming a slot past
    /// the program's, leaves the phase unkeyed.
    pub layout: KeyLayout,
    /// Packed key values ([`KeyLayout::pack`]) to build specialized bodies
    /// for. Values with bits past the layout's width never occur and are
    /// ignored, as are repeats.
    pub keys: Vec<u64>,
}

/// One key field, ready to pack.
#[derive(Debug, Clone, Copy)]
struct Field {
    slot: u32,
    mask: u64,
    /// Where the field's bits start in the key.
    at: u32,
    /// The mask's lowest bit when its bits are contiguous (a shift packs
    /// them); `None`: a `pext`.
    shift: Option<u32>,
}

/// A keyed phase: its prefix, its fallback and its table.
#[derive(Debug, Clone)]
pub(super) struct PhaseSpec {
    fields: Vec<Field>,
    /// The run computing the key slots the body computes (`None`: every
    /// key slot is state, an input or a constant).
    pub prefix: Option<u16>,
    /// The run a miss settles with.
    pub fallback: u16,
    /// Key -> run.
    table: FastMap<u64, u16>,
    /// Fused ticks that found a body, and that did not.
    pub hits: u64,
    pub misses: u64,
}

/// What specialization did in a `Sim` ([`super::Sim::spec_stats`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SpecStats {
    /// Phases with a key now (none while a ROM write has switched
    /// specialization off).
    pub keyed_phases: usize,
    /// Specialized bodies in use.
    pub bodies: usize,
    /// Fused ticks of keyed phases that ran a specialized body, and that
    /// ran the fallback, since the `Sim` was built (a ROM write's
    /// switch-off and [`super::Sim::reset`] keep them).
    pub hits: u64,
    pub misses: u64,
}

#[cfg(test)]
thread_local! {
    /// Mutations for the tests: 1 builds each body for a wrong key (the
    /// key with bit 0 flipped); 2 skips the prefix (the key is packed
    /// from the slots as they are, a miss settles with the fused body).
    pub(crate) static MUTATION: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
}

impl PhaseSpec {
    /// The key of the slots now.
    #[inline]
    pub fn key(&self, slots: &[u64]) -> u64 {
        let mut key = 0;
        for f in &self.fields {
            let v = slots[f.slot as usize] & f.mask;
            key |= match f.shift {
                Some(lo) => v >> lo,
                None => pext(v, f.mask),
            } << f.at;
        }
        key
    }

    /// The run for `key`: its specialized body, or the fallback.
    #[inline]
    pub fn lookup(&self, key: u64) -> u16 {
        self.table.get(&key).copied().unwrap_or(self.fallback)
    }

    /// Specialized bodies.
    pub fn bodies(&self) -> usize {
        self.table.len()
    }

    /// Key values with a body, and their runs (sorted by key).
    #[cfg(all(feature = "jit", not(target_arch = "wasm32")))]
    pub fn entries(&self) -> Vec<(u64, u16)> {
        let mut v: Vec<(u64, u16)> = self.table.iter().map(|(&k, &r)| (k, r)).collect();
        v.sort_unstable();
        v
    }

    /// Build the phase's runs for `plan` with the context `cx` of its
    /// fused body. `fused` is the phase's fused run (a miss settles with
    /// it when the key needs no prefix: the fallback is then the general
    /// body). `add` adds a run (`None` when no run index is left),
    /// `guard` guards a body as the fused bodies are. `None` when the plan
    /// does not fit the program.
    pub fn build(
        p: &Program,
        cx: &SpecContext,
        plan: &PhasePlan,
        fused: u16,
        add: &mut dyn FnMut(Body) -> Option<u16>,
        guard: &dyn Fn(Body) -> Body,
    ) -> Option<Self> {
        let layout = &plan.layout;
        if !layout.fits() || layout.fields.iter().any(|f| f.slot >= p.slot_count) {
            return None;
        }
        let mut fields = Vec::new();
        let mut at = 0;
        for f in layout.fields.iter().filter(|f| f.mask != 0) {
            let lo = f.mask.trailing_zeros();
            let contiguous = (f.mask >> lo).wrapping_add(1) & (f.mask >> lo) == 0;
            fields.push(Field {
                slot: f.slot,
                mask: f.mask,
                at,
                shift: contiguous.then_some(lo),
            });
            at += f.mask.count_ones();
        }
        let slots: Vec<u32> = layout.fields.iter().map(|f| f.slot).collect();
        let (prefix, pre) = cx.prefix(&slots);
        let prefix = if prefix.instrs.is_empty() {
            None
        } else {
            Some(add(prefix)?)
        };
        let fallback = match prefix {
            Some(_) => add(guard(cx.fallback(p, &pre)))?,
            None => fused,
        };
        let width = layout.width();
        let mut table = FastMap::default();
        for &key in &plan.keys {
            if key & !mask(width as u8) != 0 || table.contains_key(&key) {
                continue;
            }
            #[cfg(test)]
            let built_for = if MUTATION.get() == 1 && width > 0 {
                key ^ 1
            } else {
                key
            };
            #[cfg(not(test))]
            let built_for = key;
            let body = cx.specialize(p, &layout.assume(built_for), &pre);
            let Some(run) = add(guard(body)) else {
                break;
            };
            table.insert(key, run);
        }
        Some(PhaseSpec {
            fields,
            prefix,
            fallback,
            table,
            hits: 0,
            misses: 0,
        })
    }
}

/// The context of `body` (a clock-phase variant or the program's own body)
/// as a fused-run body: pruned to its commits' cone, prepared for
/// specialization. Also the pruned body (`None`: nothing to prune).
pub(super) fn fused_context(
    p: &Program,
    body: &Body,
    possible: Option<&[u64]>,
    phase: Option<u64>,
) -> (Option<Body>, Option<SpecContext>) {
    let pruned = phases::prune(p, body);
    let base = pruned.as_ref().unwrap_or(body);
    let cx = possible.and_then(|poss| SpecContext::new(p, base, poss, phase));
    (pruned, cx)
}

/// Per phase of a `Sim` of `p` under `options`, the context of the fused
/// body a plan's key would specialize (`None` where specialization does
/// not apply: a packed program, fusion off). Its
/// [`SpecContext::candidates`] are the keys a plan can choose from. (A
/// `Sim` with a thread pool runs no fused bodies at all.)
pub fn spec_contexts(p: &Program, options: &SimOptions) -> Vec<Option<SpecContext>> {
    if !options.fuse {
        return Vec::new();
    }
    let possible = crate::compile::spec::possible_bits(p);
    match p.phases.as_deref() {
        Some(ph) if options.phases => ph
            .variants
            .iter()
            .enumerate()
            .map(|(k, v)| fused_context(p, v, Some(&possible), Some(k as u64)).1)
            .collect(),
        _ if p.packs.is_empty() => {
            let own = Body {
                blocks: p.blocks.clone(),
                instrs: p.instrs.clone(),
                imms: p.imms.clone(),
                commits: p.commits.clone(),
                guards: Vec::new(),
            };
            vec![fused_context(p, &own, Some(&possible), None).1]
        }
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests;
