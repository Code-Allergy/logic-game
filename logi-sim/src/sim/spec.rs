//! State-keyed specialization, runtime (issue #117, packages P4 and P5b of
//! `docs/adaptive-spec.md`): keys per clock phase, and bodies specialized
//! for key values.
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
//! Two kinds of tables:
//!
//! - **fixed** ([`SpecOptions::fixed`], P4): the plan names the keys and
//!   the key values; every body is built with the `Sim`'s layout, and its
//!   native code is part of the program's `JitProgram`;
//! - **adaptive** (P5b, the default): the plan names the keys (or is
//!   chosen at run time from a window of sampled states with
//!   [`crate::compile::spec::choose_plan`]); misses are counted per key
//!   value, a value that turns hot is requested
//!   ([`super::Sim::spec_requests`]), built anywhere ([`build_spec`]:
//!   pure, any thread; with the JIT, a `JitUnit` of its own) and
//!   installed ([`super::Sim::install_spec`]) between calls, evicting the
//!   least recently hit body under the caps. Installed bodies are runs
//!   past the layout's (`Spec::base`): the program's native code does not
//!   change when they come and go.
//!
//! Exactness: a specialized body only runs on a tick whose key, computed
//! this tick, is the one it was built for; under that assumption it
//! commits what the general fused body commits (`compile::spec`); fused
//! ticks are observed by nothing but their commit. The compile side folds
//! ROM contents, so a ROM write turns specialization off with the
//! known-bits bodies (`Sim::rom_changed`) until `Sim::reset`; builds
//! requested before carry an older generation and are refused.
//!
//! Supersteps (`jit::superstep`) run the general fused bodies, so `run`
//! uses none while a phase is keyed. An automatic plan is chosen after the
//! layout's native code was compiled (without a superstep, since keys
//! were to come); a plan that keys no phase therefore brings the layout's
//! superstep, compiled with the plan ([`Spec::superstep`]). Supersteps
//! *keyed* per period were measured and not built: on the NES console the
//! keys of whole periods hardly repeat (`docs/adaptive-spec.md`, "P8 as
//! built").

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};

use crate::compile::guard::guard_with;
use crate::compile::spec::{KeyLayout, PlanOptions, SpecContext, choose_plan, pext};
use crate::compile::{Body, FastMap, Program, phases};
use crate::netlist::mask;

use super::{Run, Sim, SimOptions};

/// State-keyed specialization ([`SimOptions::spec`]). Changes speed only,
/// never results.
///
/// [`SpecOptions::default`] is adaptive with an automatic plan: the first
/// `plan_window` fused ticks of each phase are sampled, the keys are chosen
/// from the samples ([`crate::compile::spec::choose_plan`]; fixed for the
/// life of the `Sim`), then key values that turn hot get bodies, at most
/// `max_per_phase` per phase.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpecOptions {
    /// Keys per clock phase, and key values to build bodies for first.
    /// `None`: chosen at run time (adaptive only; a fixed table without a
    /// plan keys nothing).
    pub plan: Option<Arc<SpecPlan>>,
    /// Adaptive tables: count misses, build hot key values, evict cold
    /// ones under the caps below. Off: the plan's key values get bodies
    /// when the `Sim` is built and nothing changes later (P4); the caps
    /// do not apply.
    pub adaptive: bool,
    /// Specialized bodies per phase at most. Past a few dozen the
    /// instruction cache, not the op count, decides (on an MMC3 game 16
    /// per phase ran faster than 64).
    pub max_per_phase: usize,
    /// Specialized bodies over all phases at most.
    pub max_bodies: usize,
    /// Bytes of native code of the specialized bodies at most (the
    /// prefixes, fallbacks and superstep of an automatic plan are not
    /// counted: they are never evicted).
    pub code_budget: usize,
    /// Fused ticks of a phase per counting window: miss counters restart
    /// every window, and a body not hit for a window is cold (may be
    /// evicted).
    pub window: u64,
    /// Misses of one key value within a window that make it hot.
    pub hot: u32,
    /// Automatic plan: the first fused ticks of each phase, over which
    /// the samples are spread. Long enough to get past a boot sequence: on
    /// an MMC3 game, plans from the first 2^16 ticks per phase executed
    /// 610 ops per tick in play, from the first 2^20 485 (a plan profiled
    /// over 4.5M ticks: 475).
    pub plan_window: u64,
    /// Automatic plan: sampled states per phase.
    pub samples: usize,
    /// Automatic plan: key bits per phase at most.
    pub max_key_bits: u32,
    /// Automatic plan: drop key bits worth fewer executed ops per settle
    /// (`SpecContext::prune_bits`).
    pub min_loss: u32,
    /// Build requested bodies on the calling thread at the end of each
    /// [`super::Sim::run`] (tests, wasm, a `Sim` without a compile
    /// thread). Off: the caller drains [`super::Sim::spec_requests`],
    /// builds ([`build_spec`]) and installs.
    pub inline_build: bool,
}

impl Default for SpecOptions {
    fn default() -> Self {
        SpecOptions {
            plan: None,
            adaptive: true,
            max_per_phase: 16,
            max_bodies: 48,
            code_budget: 1 << 20,
            window: 1 << 16,
            hot: 256,
            plan_window: 1 << 20,
            samples: 512,
            max_key_bits: 32,
            min_loss: 4,
            inline_build: true,
        }
    }
}

impl SpecOptions {
    /// A fixed plan (P4): bodies for the plan's key values, built with the
    /// `Sim`; nothing counted, built or evicted later.
    pub fn fixed(plan: SpecPlan) -> Self {
        SpecOptions {
            plan: Some(Arc::new(plan)),
            adaptive: false,
            ..SpecOptions::default()
        }
    }

    /// Adaptive tables on the keys of `plan` (its key values are built
    /// first, as if hot).
    pub fn with_plan(plan: SpecPlan) -> Self {
        SpecOptions {
            plan: Some(Arc::new(plan)),
            ..SpecOptions::default()
        }
    }

    /// The plan choice's options.
    fn plan_options(&self) -> PlanOptions {
        PlanOptions {
            max_key_bits: self.max_key_bits.min(64),
            min_loss: f64::from(self.min_loss),
            max_bodies: self.max_per_phase.max(1),
            ..PlanOptions::default()
        }
    }

    /// Whether the keys are chosen at run time.
    pub(super) fn automatic(&self) -> bool {
        self.adaptive && self.plan.is_none()
    }
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

/// The key is packed with a shift per contiguous field mask and `pext`
/// otherwise.
fn fields_of(layout: &KeyLayout) -> Vec<Field> {
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
    fields
}

/// A body in a table: its run and when it was last hit (the phase's
/// clock, `hits + misses`).
#[derive(Debug, Clone, Copy)]
struct Entry {
    run: u16,
    last: u64,
}

/// Miss counters per phase: direct-mapped by key hash, a majority vote per
/// counter (a colliding key wears the count down before taking it over).
const HOT_SLOTS: usize = 1024;
/// Hot keys waiting for `spec_requests` per phase at most.
const MAX_WANTED: usize = 64;

/// The adaptive part of a phase's table.
#[derive(Debug, Clone)]
struct Adapt {
    cx: Arc<SpecContext>,
    pre: Arc<[bool]>,
    layout: KeyLayout,
    hot: u32,
    window: u64,
    /// Clock at which the counters restart.
    next_window: u64,
    tag: Box<[u64]>,
    count: Box<[u32]>,
    /// Hot keys not yet requested (no repeats: each is also pending).
    wanted: Vec<u64>,
    /// Keys wanted or requested and not installed, with the clock at which
    /// they were wanted: a key is wanted again only once it is neither.
    /// A request the caller dropped expires after `PENDING_WINDOWS`
    /// windows (one still waiting in `wanted` never does).
    pending: FastMap<u64, u64>,
}

/// Windows after which a requested key that was never installed or
/// refused may be wanted again (the caller dropped the request).
const PENDING_WINDOWS: u64 = 4;

impl Adapt {
    fn new(cx: Arc<SpecContext>, pre: Arc<[bool]>, layout: KeyLayout, o: &SpecOptions) -> Self {
        let window = o.window.max(1);
        Adapt {
            cx,
            pre,
            layout,
            hot: o.hot.max(1),
            window,
            next_window: window,
            tag: vec![0; HOT_SLOTS].into_boxed_slice(),
            count: vec![0; HOT_SLOTS].into_boxed_slice(),
            wanted: Vec::new(),
            pending: FastMap::default(),
        }
    }

    /// Count a miss of `key` at `clock`.
    #[inline]
    fn miss(&mut self, key: u64, clock: u64) {
        if clock >= self.next_window {
            self.count.fill(0);
            let (wanted, expiry) = (&self.wanted, PENDING_WINDOWS * self.window);
            self.pending
                .retain(|k, t| clock - *t < expiry || wanted.contains(k));
            self.next_window = clock + self.window;
        }
        let h = (key.wrapping_mul(0x9e37_79b9_7f4a_7c15) >> 54) as usize & (HOT_SLOTS - 1);
        let c = &mut self.count[h];
        if self.tag[h] == key {
            *c += 1;
        } else if *c == 0 {
            self.tag[h] = key;
            *c = 1;
        } else {
            *c -= 1;
            return;
        }
        if *c >= self.hot {
            *c = 0;
            self.want(key, clock);
        }
    }

    /// Want `key` unless it is wanted, requested or full.
    fn want(&mut self, key: u64, clock: u64) {
        if self.wanted.len() < MAX_WANTED && !self.pending.contains_key(&key) {
            self.pending.insert(key, clock);
            self.wanted.push(key);
        }
    }
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
    /// Key -> body.
    table: FastMap<u64, Entry>,
    /// Fused ticks that found a body, and that did not.
    pub hits: u64,
    pub misses: u64,
    /// `None`: a fixed table.
    adapt: Option<Box<Adapt>>,
}

#[cfg(test)]
thread_local! {
    /// Mutations for the tests: 1 builds each body for a wrong key (the
    /// key with bit 0 flipped); 2 skips the prefix (the key is packed
    /// from the slots as they are, a miss settles with the fused body);
    /// 3 installs each built body under a wrong key (bit 0 flipped);
    /// 4 evicts a body but leaves its table entry (pointing at the freed
    /// run, which the next install reuses); 5 fails every body's check in
    /// `build_spec`; 6 runs every fused tick of a keyed `run` through
    /// `Sim::keyed_ticks` (the inputs never applied).
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
    #[cfg_attr(not(test), expect(dead_code))]
    #[inline]
    pub fn lookup(&self, key: u64) -> u16 {
        self.table.get(&key).map_or(self.fallback, |e| e.run)
    }

    /// The run for `key` in a fused tick: [`PhaseSpec::lookup`], counting
    /// the hit (and when) or the miss (and, adaptive, the key's heat).
    #[inline]
    pub fn dispatch(&mut self, key: u64) -> u16 {
        let clock = self.hits + self.misses;
        if let Some(e) = self.table.get_mut(&key) {
            e.last = clock;
            self.hits += 1;
            return e.run;
        }
        self.misses += 1;
        if let Some(a) = &mut self.adapt {
            a.miss(key, clock);
        }
        self.fallback
    }

    /// The key's fields as words (slot, mask per field), for telling run
    /// layouts apart: bodies for the same keys but other fields assume
    /// other bits.
    fn field_words(&self) -> Vec<u64> {
        self.fields
            .iter()
            .flat_map(|f| [u64::from(f.slot), f.mask])
            .collect()
    }

    /// Specialized bodies.
    pub fn bodies(&self) -> usize {
        self.table.len()
    }

    /// Key values with a body, and their runs (sorted by key).
    pub fn entries(&self) -> Vec<(u64, u16)> {
        let mut v: Vec<(u64, u16)> = self.table.iter().map(|(&k, e)| (k, e.run)).collect();
        v.sort_unstable();
        v
    }

    /// Whether hot keys wait for a request.
    fn wants(&self) -> bool {
        self.adapt.as_ref().is_some_and(|a| !a.wanted.is_empty())
    }

    /// The bodies that may be evicted (not hit for a window): age, key
    /// and run of each.
    fn cold(&self) -> Vec<(u64, u64, u16)> {
        let Some(a) = self.adapt.as_ref() else {
            return Vec::new();
        };
        let clock = self.hits + self.misses;
        self.table
            .iter()
            .map(|(&k, e)| (clock - e.last.min(clock), k, e.run))
            .filter(|&(age, _, _)| age >= a.window)
            .collect()
    }

    /// Whether some body may be evicted.
    fn has_cold(&self) -> bool {
        let Some(a) = self.adapt.as_ref() else {
            return false;
        };
        let clock = self.hits + self.misses;
        self.table
            .values()
            .any(|e| clock - e.last.min(clock) >= a.window)
    }

    /// Build the phase's runs for `plan` with the context `cx` of its
    /// fused body. `fused` is the phase's fused run (a miss settles with
    /// it when the key needs no prefix: the fallback is then the general
    /// body). `add` adds a run (`None` when no run index is left),
    /// `guard` guards a body as the fused bodies are. With `adaptive`,
    /// the table starts empty and the plan's keys are wanted (built by
    /// the first requests). `None` when the plan does not fit the program.
    pub fn build(
        p: &Program,
        cx: &Arc<SpecContext>,
        plan: &PhasePlan,
        fused: u16,
        adaptive: Option<&SpecOptions>,
        add: &mut dyn FnMut(Body) -> Option<u16>,
        guard: &dyn Fn(Body) -> Body,
    ) -> Option<Self> {
        let layout = &plan.layout;
        if !layout.fits() || layout.fields.iter().any(|f| f.slot >= p.slot_count) {
            return None;
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
        let keys = plan
            .keys
            .iter()
            .copied()
            .filter(move |&k| k & !mask(width as u8) == 0);
        let mut ps = PhaseSpec {
            fields: fields_of(layout),
            prefix,
            fallback,
            table: FastMap::default(),
            hits: 0,
            misses: 0,
            adapt: None,
        };
        if let Some(o) = adaptive {
            let mut a = Adapt::new(Arc::clone(cx), pre.into(), layout.clone(), o);
            for key in keys {
                a.want(key, 0);
            }
            ps.adapt = Some(Box::new(a));
            return Some(ps);
        }
        for key in keys {
            if ps.table.contains_key(&key) {
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
            ps.table.insert(key, Entry { run, last: 0 });
        }
        Some(ps)
    }
}

/// A body's native code: a `JitUnit` with the JIT, nothing without.
#[cfg(all(feature = "jit", not(target_arch = "wasm32")))]
pub(super) type Unit = super::jit::JitUnit;
#[cfg(not(all(feature = "jit", not(target_arch = "wasm32"))))]
pub(super) enum Unit {}
#[cfg(not(all(feature = "jit", not(target_arch = "wasm32"))))]
impl Unit {
    fn code_bytes(&self) -> usize {
        match *self {}
    }
}

/// A superstep's native code: a `SuperUnit` with the JIT.
#[cfg(all(feature = "jit", not(target_arch = "wasm32")))]
pub(super) type SuperCode = super::jit::SuperUnit;
#[cfg(not(all(feature = "jit", not(target_arch = "wasm32"))))]
pub(super) enum SuperCode {}

/// A superstep compiled after the layout was built (see
/// [`Spec::superstep`]): its code, and the run each of its bodies is
/// (its parts name bodies by their index in `runs`).
pub(super) struct SpecSuper {
    #[cfg_attr(
        not(all(feature = "jit", not(target_arch = "wasm32"))),
        expect(dead_code)
    )]
    pub code: SuperCode,
    #[cfg_attr(
        not(all(feature = "jit", not(target_arch = "wasm32"), feature = "counters")),
        expect(dead_code)
    )]
    pub runs: Vec<u16>,
    bytes: usize,
}

/// What a superstep request carries: the bodies (owned, so that the build
/// needs nothing of the `Sim`), the run of each, and the superstep.
#[cfg(all(feature = "jit", not(target_arch = "wasm32")))]
pub(super) struct SuperBuild {
    pub bodies: Vec<Body>,
    pub runs: Vec<u16>,
    pub parts: Vec<super::jit::Part>,
    pub live_end: Vec<u32>,
    pub ticks: u64,
    pub chunk: usize,
}

/// A run installed after the layout was built.
pub(super) struct Dyn {
    /// Its native code (run only while the `Sim` runs native code).
    #[cfg_attr(
        not(any(test, all(feature = "jit", not(target_arch = "wasm32")))),
        expect(dead_code)
    )]
    pub unit: Option<Unit>,
    /// A prefix or fallback of a plan chosen at run time: never evicted.
    pinned: bool,
    bytes: usize,
}

/// Rows of settled slots per phase for an automatic plan (the first
/// window of fused ticks, every `stride`-th of each phase).
pub(super) struct Sampler {
    rows: Vec<Vec<Vec<u64>>>,
    /// Fused ticks of each phase seen.
    seen: Vec<u64>,
    stride: u64,
    want: usize,
}

impl Sampler {
    fn new(phases: usize, o: &SpecOptions) -> Box<Self> {
        let want = o.samples.max(1);
        Box::new(Sampler {
            rows: vec![Vec::new(); phases],
            seen: vec![0; phases],
            stride: (o.plan_window / want as u64).max(1),
            want,
        })
    }

    /// Whether this fused tick of phase `k` is sampled.
    #[inline]
    fn take(&mut self, k: usize) -> bool {
        let s = self.seen[k];
        self.seen[k] += 1;
        s.is_multiple_of(self.stride) && self.rows[k].len() < self.want
    }

    fn done(&self, contexts: &[Option<Arc<SpecContext>>]) -> bool {
        contexts
            .iter()
            .zip(&self.rows)
            .all(|(cx, r)| cx.is_none() || r.len() >= self.want)
    }
}

/// Everything specialization adds to a `Sim`'s layout (set aside with it
/// by a ROM write).
#[derive(Default)]
pub(super) struct Spec {
    /// Per phase, its key and table; empty when no phase is keyed.
    pub phases: Vec<Option<PhaseSpec>>,
    /// Some fused tick goes through `Sim::spec_fused` (a keyed phase, or
    /// sampling).
    pub hooked: bool,
    /// Automatic plan: per phase, the context of its fused body (until the
    /// plan is installed).
    contexts: Vec<Option<Arc<SpecContext>>>,
    /// Automatic plan: sampling.
    sampler: Option<Box<Sampler>>,
    /// Automatic plan: the samples, kept until the plan is installed.
    rows: Option<Arc<Vec<Vec<Vec<u64>>>>>,
    /// Automatic plan: requested, and fused ticks since (asked again
    /// after `plan_window` fused ticks per phase without a plan: the
    /// caller dropped the request, or the plan was refused).
    plan_asked: Option<u64>,
    /// Automatic plan: no plan installed yet (no superstep meanwhile).
    pub awaiting_plan: bool,
    /// What the layout's native code depends on (`jit::RunKey::spec`):
    /// the runs built with the layout, not those installed later.
    pub key: Vec<u64>,
    /// Runs built with the layout: `Sim::runs[..base]`.
    pub base: usize,
    /// Per run past `base` (`run - base - 1`): `None` when free.
    dynamic: Vec<Option<Dyn>>,
    /// Free runs past `base`.
    free: Vec<u16>,
    /// Native bytes of the specialized bodies installed, and of the
    /// pinned runs (and the superstep).
    body_bytes: usize,
    pinned_bytes: usize,
    /// The superstep of an automatic plan that keys no phase (the layout's
    /// native code was compiled without one while the plan was awaited):
    /// run by `Sim::run` while native code is installed.
    pub superstep: Option<Arc<SpecSuper>>,
}

impl Spec {
    /// The layout's specialization: `phases` (fixed and adaptive tables
    /// built), `contexts` for an automatic plan (`options` given), `base`
    /// runs.
    pub fn new(
        phases: Vec<Option<PhaseSpec>>,
        contexts: Vec<Option<Arc<SpecContext>>>,
        automatic: Option<&SpecOptions>,
        base: usize,
    ) -> Self {
        let mut s = Spec {
            phases,
            base,
            ..Spec::default()
        };
        if let Some(o) = automatic.filter(|_| contexts.iter().any(Option::is_some)) {
            s.sampler = Some(Sampler::new(contexts.len(), o));
            s.awaiting_plan = true;
            s.contexts = contexts;
        }
        s.key = s.layout_key();
        s.refresh();
        s
    }

    fn refresh(&mut self) {
        self.hooked = !self.phases.is_empty() || self.sampler.is_some() || self.rows.is_some();
    }

    /// Whether the layout folds ROM contents (a keyed phase, or a plan
    /// still to come).
    pub fn folds(&self) -> bool {
        !self.phases.is_empty() || self.awaiting_plan
    }

    /// The specialized runs of the layout, for telling layouts apart: per
    /// keyed phase, its index, prefix, fallback and every key with its
    /// run; then whether a plan is still to come.
    fn layout_key(&self) -> Vec<u64> {
        let mut out = Vec::new();
        for (k, ps) in self.phases.iter().enumerate() {
            let Some(ps) = ps else { continue };
            out.extend([k as u64, ps.prefix.map_or(u64::MAX, u64::from)]);
            out.push(u64::from(ps.fallback));
            let f = ps.field_words();
            out.push(f.len() as u64);
            out.extend(f);
            let e = ps.entries();
            out.push(e.len() as u64);
            for (key, run) in e {
                out.extend([key, u64::from(run)]);
            }
        }
        out.push(u64::from(self.awaiting_plan));
        out
    }

    /// The native code of installed run `run`.
    #[cfg(all(feature = "jit", not(target_arch = "wasm32")))]
    #[inline]
    pub fn unit(&self, run: u16) -> Option<&Unit> {
        let i = (run as usize).checked_sub(self.base + 1)?;
        self.dynamic.get(i)?.as_ref()?.unit.as_ref()
    }

    /// Whether requests wait (`Sim::spec_requests` would return some).
    pub fn has_work(&self) -> bool {
        (self.rows.is_some() && self.plan_asked.is_none())
            || self.phases.iter().flatten().any(PhaseSpec::wants)
    }

    fn live(&self) -> usize {
        self.phases.iter().flatten().map(PhaseSpec::bodies).sum()
    }

    /// Runs of the layout's tables: the prefix, the fallback and the
    /// specialized bodies of fixed tables (not those installed later).
    #[cfg(all(feature = "jit", not(target_arch = "wasm32")))]
    pub fn base_runs(&self) -> Vec<(u16, bool)> {
        let mut out = Vec::new();
        for ps in self.phases.iter().flatten() {
            if let Some(r) = ps.prefix {
                out.push((r, true));
            }
            out.push((ps.fallback, false));
            out.extend(ps.table.values().map(|e| (e.run, false)));
        }
        out.retain(|&(r, _)| r as usize <= self.base);
        out
    }
}

/// Lifetime counts of a `Sim`'s specialization (not set aside by a ROM
/// write) and its generation.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct Life {
    /// Requests and builds of another generation are refused.
    pub generation: u64,
    requested: u64,
    installed: u64,
    evicted: u64,
    stale: u64,
    dropped: u64,
}

/// Generations are unique over all `Sim`s, so a build requested by one
/// `Sim` is refused by every other.
static NEXT_GEN: AtomicU64 = AtomicU64::new(1);

impl Life {
    /// A new `Sim`'s: a generation of its own.
    pub fn new() -> Self {
        Life {
            generation: next_gen(),
            ..Life::default()
        }
    }
}

pub(super) fn next_gen() -> u64 {
    NEXT_GEN.fetch_add(1, Ordering::Relaxed)
}

/// What specialization did in a `Sim` ([`super::Sim::spec_stats`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SpecStats {
    /// Phases with a key now (none while a ROM write has switched
    /// specialization off, or before an automatic plan is installed).
    pub keyed_phases: usize,
    /// Specialized bodies live.
    pub bodies: usize,
    /// Fused ticks of keyed phases that ran a specialized body, and that
    /// ran the fallback, since the `Sim` was built (a ROM write's
    /// switch-off and [`super::Sim::reset`] keep them).
    pub hits: u64,
    pub misses: u64,
    /// Requests handed out ([`super::Sim::spec_requests`]; a plan counts
    /// one), builds installed (a plan counts one), bodies evicted.
    pub requested: u64,
    pub installed: u64,
    pub evicted: u64,
    /// Builds refused: requested by another `Sim` or before a ROM write
    /// or the `reset` after one (stale); for want of room (every body
    /// hit within the window), for a key with a body already, or a key no
    /// longer wanted at request time (dropped).
    pub stale: u64,
    pub dropped: u64,
    /// Native bytes of the bodies installed at run time (specialized
    /// bodies, and the prefixes and fallbacks of an automatic plan), and
    /// of the superstep a plan that keys nothing brings.
    pub code_bytes: usize,
    /// An automatic plan is still being sampled or built.
    pub awaiting_plan: bool,
}

/// A build the `Sim` asks for ([`super::Sim::spec_requests`]): a body for a
/// hot key value, or the automatic plan. Self-contained: [`build_spec`]
/// needs nothing else but the program.
pub struct SpecRequest {
    program: Weak<Program>,
    generation: u64,
    /// Compile native code (`SimOptions::jit`).
    jit: bool,
    /// Guard like the fused bodies (`Some(guard_min)`).
    guard: Option<usize>,
    work: Work,
}

enum Work {
    Body {
        phase: usize,
        key: u64,
        cx: Arc<SpecContext>,
        pre: Arc<[bool]>,
        layout: KeyLayout,
    },
    Plan {
        contexts: Vec<Option<Arc<SpecContext>>>,
        rows: Arc<Vec<Vec<Vec<u64>>>>,
        options: PlanOptions,
        /// The layout's superstep, compiled when the plan keys nothing.
        #[cfg(all(feature = "jit", not(target_arch = "wasm32")))]
        superstep: Option<SuperBuild>,
    },
}

impl SpecRequest {
    /// The phase and key value of a body request (`None`: the plan).
    pub fn body(&self) -> Option<(usize, u64)> {
        match &self.work {
            Work::Body { phase, key, .. } => Some((*phase, *key)),
            Work::Plan { .. } => None,
        }
    }

    /// Whether [`build_spec`] compiles native code (a `JitUnit`) for the
    /// bodies: by default when the `Sim` asked for the JIT or runs native
    /// code. A tiered `Sim` (interpreted until native code is installed,
    /// [`SimOptions::tier_layout`]) wants it on, so that its bodies run
    /// natively once the program's code is in.
    pub fn set_native(&mut self, native: bool) {
        self.jit = native;
    }
}

impl std::fmt::Debug for SpecRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.body() {
            Some((phase, key)) => write!(f, "SpecRequest(phase {phase}, key {key:#x})"),
            None => write!(f, "SpecRequest(plan)"),
        }
    }
}

/// A body ready to install, with its native code.
struct Built {
    body: Body,
    unit: Option<Unit>,
}

/// One phase of a plan built at run time.
struct BuiltPhase {
    layout: KeyLayout,
    cx: Arc<SpecContext>,
    pre: Arc<[bool]>,
    prefix: Option<Built>,
    /// `None` when there is no prefix (the fused body is the fallback).
    fallback: Option<Built>,
}

/// The result of [`build_spec`], for [`super::Sim::install_spec`].
pub struct SpecBuilt {
    program: Weak<Program>,
    generation: u64,
    out: Out,
}

enum Out {
    /// `body` is `None` when it failed `Sim`'s check (never expected).
    Body {
        phase: usize,
        key: u64,
        body: Option<Box<Built>>,
    },
    /// The keyed phases, and the superstep when none is keyed.
    Plan(Vec<Option<BuiltPhase>>, Option<Box<SpecSuper>>),
}

impl SpecBuilt {
    /// The phase and key value of a body (`None`: a plan).
    pub fn body(&self) -> Option<(usize, u64)> {
        match &self.out {
            Out::Body { phase, key, .. } => Some((*phase, *key)),
            Out::Plan(..) => None,
        }
    }

    /// Bytes of native code built.
    pub fn code_bytes(&self) -> usize {
        let b = |x: Option<&Built>| x.and_then(|b| b.unit.as_ref()).map_or(0, Unit::code_bytes);
        match &self.out {
            Out::Body { body, .. } => b(body.as_deref()),
            Out::Plan(ph, sup) => {
                ph.iter()
                    .flatten()
                    .map(|x| b(x.prefix.as_ref()) + b(x.fallback.as_ref()))
                    .sum::<usize>()
                    + sup.as_ref().map_or(0, |s| s.bytes)
            }
        }
    }

    /// Keyed phases of a plan (`None`: a body).
    pub fn plan_phases(&self) -> Option<usize> {
        match &self.out {
            Out::Body { .. } => None,
            Out::Plan(ph, _) => Some(ph.iter().flatten().count()),
        }
    }
}

impl std::fmt::Debug for SpecBuilt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.body() {
            Some((phase, key)) => write!(f, "SpecBuilt(phase {phase}, key {key:#x})"),
            None => write!(f, "SpecBuilt(plan)"),
        }
    }
}

/// Build what `r` asks for, for the program `p` it was requested with
/// (any other `p` builds nothing, and the result is refused by every
/// `Sim`; pure; any thread): specialize, guard, check and (with the JIT) compile
/// a body; or choose the plan from the samples and build each keyed
/// phase's prefix and fallback.
pub fn build_spec(p: &Program, r: &SpecRequest) -> SpecBuilt {
    if !std::ptr::eq(p, r.program.as_ptr()) {
        // Built against another program: refused (another program's
        // slots, memories or ROM folds), so not built at all.
        return SpecBuilt {
            program: Weak::new(),
            generation: r.generation,
            out: match &r.work {
                Work::Body { phase, key, .. } => Out::Body {
                    phase: *phase,
                    key: *key,
                    body: None,
                },
                Work::Plan { .. } => Out::Plan(Vec::new(), None),
            },
        };
    }
    let n = p.slot_count as usize;
    let guard = |b: Body| match r.guard {
        Some(min) => guard_with(n, &b, None, min).unwrap_or(b),
        None => b,
    };
    let out = match &r.work {
        Work::Body {
            phase,
            key,
            cx,
            pre,
            layout,
        } => {
            let body = guard(cx.specialize(p, &layout.assume(*key), pre));
            Out::Body {
                phase: *phase,
                key: *key,
                body: finish(p, body, true, r.jit).map(Box::new),
            }
        }
        Work::Plan {
            contexts,
            rows,
            options,
            #[cfg(all(feature = "jit", not(target_arch = "wasm32")))]
            superstep,
        } => {
            let cxs: Vec<Option<&SpecContext>> = contexts.iter().map(Option::as_deref).collect();
            let samples: Vec<&[Vec<u64>]> = rows.iter().map(Vec::as_slice).collect();
            let plan = choose_plan(p, &cxs, &samples, options);
            let phases: Vec<Option<BuiltPhase>> = contexts
                .iter()
                .enumerate()
                .map(|(k, cx)| {
                    let cx = cx.as_ref()?;
                    let layout = plan.phases.get(k)?.as_ref()?.layout.clone();
                    if !layout.fits() || layout.fields.iter().any(|f| f.slot >= p.slot_count) {
                        return None;
                    }
                    let slots: Vec<u32> = layout.fields.iter().map(|f| f.slot).collect();
                    let (prefix, pre) = cx.prefix(&slots);
                    let (prefix, fallback) = if prefix.instrs.is_empty() {
                        (None, None)
                    } else {
                        let fallback = guard(cx.fallback(p, &pre));
                        (
                            Some(finish(p, prefix, false, r.jit)?),
                            Some(finish(p, fallback, true, r.jit)?),
                        )
                    };
                    Some(BuiltPhase {
                        layout,
                        cx: Arc::clone(cx),
                        pre: pre.into(),
                        prefix,
                        fallback,
                    })
                })
                .collect();
            // Keying nothing: the superstep the layout would have had.
            #[cfg(all(feature = "jit", not(target_arch = "wasm32")))]
            let sup = superstep
                .as_ref()
                .filter(|_| r.jit && phases.iter().all(Option::is_none))
                .and_then(|sb| build_superstep(p, sb))
                .map(Box::new);
            #[cfg(not(all(feature = "jit", not(target_arch = "wasm32"))))]
            let sup = None;
            Out::Plan(phases, sup)
        }
    };
    SpecBuilt {
        program: r.program.clone(),
        generation: r.generation,
        out,
    }
}

/// Compile the superstep of `sb` (`None` when it cannot be compiled).
#[cfg(all(feature = "jit", not(target_arch = "wasm32")))]
fn build_superstep(p: &Program, sb: &SuperBuild) -> Option<SpecSuper> {
    use super::jit::{BodyRef, SuperRef, compile_superstep};
    let bodies: Vec<BodyRef<'_>> = sb
        .bodies
        .iter()
        .map(|b| BodyRef {
            blocks: &b.blocks,
            instrs: &b.instrs,
            imms: &b.imms,
            commits: &b.commits,
            guards: &b.guards,
            used: true,
            fused_only: true,
        })
        .collect();
    let s = SuperRef {
        parts: &sb.parts,
        ticks: sb.ticks,
        live_end: &sb.live_end,
        chunk: sb.chunk,
    };
    let code = compile_superstep(p, &bodies, &s).ok()?;
    Some(SpecSuper {
        bytes: code.info().code_bytes,
        code,
        runs: sb.runs.clone(),
    })
}

/// Check `body` as `Sim` checks its bodies and compile its unit.
/// `fused_only`: every body but a prefix (whose results the key and the
/// body after it read).
#[cfg_attr(
    not(all(feature = "jit", not(target_arch = "wasm32"))),
    expect(unused_variables)
)]
fn finish(p: &Program, body: Body, fused_only: bool, jit: bool) -> Option<Built> {
    #[cfg(test)]
    if MUTATION.get() == 5 {
        return None;
    }
    super::check_body(
        p,
        &body.blocks,
        &body.instrs,
        &body.imms,
        &body.commits,
        &body.guards,
    )
    .ok()?;
    #[cfg(all(feature = "jit", not(target_arch = "wasm32")))]
    let unit = {
        let items: usize = body.blocks.iter().map(|b| b.len as usize).sum();
        (jit && items <= super::jit::MAX_BODY)
            .then(|| {
                super::jit::compile_unit(
                    p,
                    super::jit::BodyRef {
                        blocks: &body.blocks,
                        instrs: &body.instrs,
                        imms: &body.imms,
                        commits: &body.commits,
                        guards: &body.guards,
                        used: true,
                        fused_only,
                    },
                )
                .ok()
            })
            .flatten()
    };
    #[cfg(not(all(feature = "jit", not(target_arch = "wasm32"))))]
    let unit = None;
    Some(Built { body, unit })
}

impl Sim {
    /// A fused tick of phase `k` while specialization is hooked in: a
    /// sample for the automatic plan, a keyed settle, or the fused body.
    #[inline]
    pub(super) fn spec_fused(&mut self, k: usize) {
        if let Some(s) = &mut self.spec.sampler
            && s.take(k)
        {
            // A full settle computes every slot the plan may read; the
            // commit after it is the full body's.
            self.settle(self.full[k]);
            let s = self.spec.sampler.as_mut().expect("sampling");
            s.rows[k].push(self.slots.clone());
            if s.done(&self.spec.contexts) {
                let s = self.spec.sampler.take().expect("sampling");
                self.spec.rows = Some(Arc::new(s.rows));
                self.spec.refresh();
            }
            return;
        }
        if self.spec.rows.is_some() {
            // The plan is awaited: ask again after a plan window.
            if let Some(n) = &mut self.spec.plan_asked {
                *n += 1;
                let o = self.options.spec.as_ref().expect("spec options");
                if *n >= o.plan_window.saturating_mul(self.period) {
                    self.spec.plan_asked = None;
                }
            }
            self.settle(self.fused[k]);
            return;
        }
        if self.spec.phases.get(k).is_some_and(Option::is_some) {
            self.spec_settle(k);
        } else {
            self.settle(self.fused[k]);
        }
    }

    /// Builds the `Sim` wants: hot key values that fit under the caps (a
    /// key that does not fit now is dropped and counted again), and the
    /// automatic plan once its samples are in. Build them with
    /// [`build_spec`] (on any thread) and hand the results to
    /// [`Sim::install_spec`]. With [`SpecOptions::inline_build`], `run`
    /// does all of this itself.
    pub fn spec_requests(&mut self) -> Vec<SpecRequest> {
        let mut out = Vec::new();
        let Some(o) = self.options.spec.clone() else {
            return out;
        };
        // Native code for a `Sim` that runs it or will (`SimOptions::jit`;
        // a tiered `Sim` asks with `SpecRequest::set_native`).
        let jit = self.options.jit || self.jit_active();
        let guards = self.options.guard && (self.options.jit || self.options.guard_interpreter);
        let guard = guards.then_some(self.options.guard_min);
        let program = Arc::downgrade(&self.program);
        let generation = self.spec_life.generation;
        if let Some(rows) = self
            .spec
            .rows
            .as_ref()
            .filter(|_| self.spec.plan_asked.is_none())
        {
            let rows = Arc::clone(rows);
            self.spec.plan_asked = Some(0);
            out.push(SpecRequest {
                program: program.clone(),
                generation,
                jit,
                guard,
                work: Work::Plan {
                    contexts: self.spec.contexts.clone(),
                    rows,
                    options: o.plan_options(),
                    #[cfg(all(feature = "jit", not(target_arch = "wasm32")))]
                    superstep: if jit { self.super_build() } else { None },
                },
            });
        }
        let live = self.spec.live();
        let any_cold = self.spec.phases.iter().flatten().any(PhaseSpec::has_cold);
        let mut asked = 0;
        for (k, ps) in self.spec.phases.iter_mut().enumerate() {
            let Some(ps) = ps else { continue };
            let cold = ps.has_cold();
            let bodies = ps.bodies();
            let Some(a) = ps.adapt.as_mut() else { continue };
            let mut asked_here = 0;
            for key in std::mem::take(&mut a.wanted) {
                let phase_room = bodies + asked_here < o.max_per_phase || cold;
                let room = live + asked < o.max_bodies || any_cold;
                if ps.table.contains_key(&key) || !(phase_room && room) {
                    a.pending.remove(&key);
                    self.spec_life.dropped += 1;
                    continue;
                }
                asked += 1;
                asked_here += 1;
                out.push(SpecRequest {
                    program: program.clone(),
                    generation,
                    jit,
                    guard,
                    work: Work::Body {
                        phase: k,
                        key,
                        cx: Arc::clone(&a.cx),
                        pre: Arc::clone(&a.pre),
                        layout: a.layout.clone(),
                    },
                });
            }
        }
        self.spec_life.requested += out.len() as u64;
        out
    }

    /// Install a build from [`build_spec`]. `false` when it is refused:
    /// requested by another `Sim`, or before a ROM write or the `reset`
    /// that undid one (another generation); for a key that has a body
    /// already, or for want of room (every body of the phase hit within
    /// the window). Call it between calls of `run` and `tick` (never
    /// while a body runs: an install may evict one).
    pub fn install_spec(&mut self, b: SpecBuilt) -> bool {
        let fresh = std::ptr::eq(b.program.as_ptr(), Arc::as_ptr(&self.program))
            && b.generation == self.spec_life.generation;
        if !fresh {
            self.spec_life.stale += 1;
            if b.generation == self.spec_life.generation
                && let Out::Body { phase, key, .. } = b.out
            {
                self.unpend(phase, key);
            }
            return false;
        }
        let ok = match b.out {
            Out::Plan(phases, sup) => self.install_plan(phases, sup),
            Out::Body { phase, key, body } => {
                let ok = body.is_some_and(|body| self.install_body(phase, key, *body));
                if !ok {
                    // Refused (or failed its check): the key may be wanted
                    // again.
                    self.unpend(phase, key);
                }
                ok
            }
        };
        if ok {
            self.spec_life.installed += 1;
        } else {
            self.spec_life.dropped += 1;
        }
        ok
    }

    /// `key` of phase `k` is no longer requested.
    fn unpend(&mut self, k: usize, key: u64) {
        if let Some(a) = self
            .spec
            .phases
            .get_mut(k)
            .and_then(Option::as_mut)
            .and_then(|ps| ps.adapt.as_mut())
        {
            a.pending.remove(&key);
        }
    }

    fn install_plan(
        &mut self,
        phases: Vec<Option<BuiltPhase>>,
        sup: Option<Box<SpecSuper>>,
    ) -> bool {
        let Some(o) = self.options.spec.clone() else {
            return false;
        };
        if !self.spec.awaiting_plan || self.spec.rows.is_none() || self.spec.sampler.is_some() {
            return false;
        }
        // Room for every run first: a plan is installed whole or not at
        // all (asked again later).
        let needed = phases
            .iter()
            .enumerate()
            .filter(|(k, ph)| {
                *k < self.fused.len() && ph.as_ref().is_some_and(|x| x.prefix.is_some())
            })
            .count()
            * 2;
        if needed > self.spec.free.len() + self.runs_left() {
            return false;
        }
        let mut table: Vec<Option<PhaseSpec>> = Vec::new();
        for (k, ph) in phases.into_iter().enumerate() {
            let Some(ph) = ph else { continue };
            if k >= self.fused.len() {
                break;
            }
            let (prefix, fallback) = match (ph.prefix, ph.fallback) {
                (Some(pre), Some(fb)) => {
                    let pre = self.add_run(pre, true).expect("room checked");
                    let fb = self.add_run(fb, true).expect("room checked");
                    (Some(pre), fb)
                }
                _ => (None, self.fused[k]),
            };
            table.resize(k + 1, None);
            table[k] = Some(PhaseSpec {
                fields: fields_of(&ph.layout),
                prefix,
                fallback,
                table: FastMap::default(),
                hits: 0,
                misses: 0,
                adapt: Some(Box::new(Adapt::new(ph.cx, ph.pre, ph.layout, &o))),
            });
        }
        if table.iter().any(Option::is_some) {
            table.resize(self.fused.len(), None);
        } else {
            table.clear();
            // Nothing keyed: the layout's superstep.
            if let Some(sup) = sup {
                self.spec.pinned_bytes += sup.bytes;
                self.spec.superstep = Some(Arc::from(sup));
            }
        }
        self.spec.phases = table;
        self.spec.contexts.clear();
        self.spec.rows = None;
        self.spec.plan_asked = None;
        self.spec.awaiting_plan = false;
        // A plan change: nothing requested before it fits the new tables.
        self.spec_life.generation = next_gen();
        self.spec.refresh();
        true
    }

    /// A run index past the layout's for `b` (a free one first); `None`
    /// when every `u16` run index is taken (the install is refused).
    fn add_run(&mut self, b: Built, pinned: bool) -> Option<u16> {
        let run = match self.spec.free.pop() {
            Some(r) => r,
            None => {
                if self.runs_left() == 0 {
                    return None;
                }
                self.runs
                    .push(Run::new(&self.program, Some(Body::default()), false));
                self.spec.dynamic.push(None);
                self.runs.len() as u16
            }
        };
        let bytes = b.unit.as_ref().map_or(0, Unit::code_bytes);
        if pinned {
            self.spec.pinned_bytes += bytes;
        } else {
            self.spec.body_bytes += bytes;
        }
        self.runs[run as usize - 1] = Run::new(&self.program, Some(b.body), self.options.stream);
        self.spec.dynamic[run as usize - self.spec.base - 1] = Some(Dyn {
            unit: b.unit,
            pinned,
            bytes,
        });
        Some(run)
    }

    /// Run indices never used yet (`u16` indices, `runs.len() + 1` the
    /// next).
    fn runs_left(&self) -> usize {
        (usize::from(u16::MAX) - 1).saturating_sub(self.runs.len())
    }

    /// Free installed run `run`, its native code with it.
    fn free_run(&mut self, run: u16) {
        debug_assert!(run != self.last, "the body that settled the slots");
        let i = run as usize - self.spec.base - 1;
        // (Under mutation 4 a run can be freed twice.)
        let Some(d) = self.spec.dynamic[i].take() else {
            return;
        };
        debug_assert!(!d.pinned);
        self.spec.body_bytes -= d.bytes;
        self.runs[run as usize - 1] = Run::new(&self.program, Some(Body::default()), false);
        self.spec.free.push(run);
    }

    /// Evict the body of `key` in phase `k`.
    fn evict(&mut self, k: usize, key: u64) {
        let ps = self.spec.phases[k].as_mut().expect("a keyed phase");
        let e = ps.table.remove(&key).expect("a body");
        #[cfg(test)]
        if MUTATION.get() == 4 {
            // Mutation: the entry stays (as if just hit), its run is freed.
            let clock = ps.hits + ps.misses;
            ps.table.insert(
                key,
                Entry {
                    run: e.run,
                    last: clock,
                },
            );
        }
        self.free_run(e.run);
        self.spec_life.evicted += 1;
    }

    fn install_body(&mut self, k: usize, key: u64, b: Built) -> bool {
        let Some(o) = self.options.spec.clone() else {
            return false;
        };
        let Some(ps) = self.spec.phases.get_mut(k).and_then(Option::as_mut) else {
            return false;
        };
        let Some(a) = ps.adapt.as_mut() else {
            return false;
        };
        a.pending.remove(&key);
        if key & !mask(a.layout.width() as u8) != 0 || ps.table.contains_key(&key) {
            return false;
        }
        let bytes = b.unit.as_ref().map_or(0, Unit::code_bytes);
        if bytes > o.code_budget {
            return false;
        }
        // Choose the bodies to evict (the coldest first: of this phase
        // while it is full, of any phase while all are) before evicting
        // any, so that a body that cannot fit evicts nothing.
        let mut cold: Vec<(u64, u64, usize, usize)> = Vec::new();
        for (pk, ps) in self.spec.phases.iter().enumerate() {
            let Some(ps) = ps else { continue };
            for (age, vkey, run) in ps.cold() {
                let i = run as usize - self.spec.base - 1;
                let b = self.spec.dynamic[i].as_ref().map_or(0, |d| d.bytes);
                cold.push((age, vkey, pk, b));
            }
        }
        cold.sort_unstable_by_key(|c| std::cmp::Reverse((c.0, c.1, c.2)));
        let (mut here, mut live, mut used) = (
            self.spec.phases[k].as_ref().expect("keyed").bodies(),
            self.spec.live(),
            self.spec.body_bytes,
        );
        let mut evict = Vec::new();
        loop {
            let full_here = here >= o.max_per_phase;
            if !full_here && live < o.max_bodies && used + bytes <= o.code_budget {
                break;
            }
            let Some(i) = cold.iter().position(|c| !full_here || c.2 == k) else {
                return false;
            };
            let (_, vkey, vk, vb) = cold.remove(i);
            // Saturating: the counts and the table can disagree under a
            // mutation (a test's eviction that keeps its entry).
            here = here.saturating_sub(usize::from(vk == k));
            live = live.saturating_sub(1);
            used = used.saturating_sub(vb);
            evict.push((vk, vkey));
        }
        if evict.is_empty() && self.spec.free.is_empty() && self.runs_left() == 0 {
            return false;
        }
        for (vk, vkey) in evict {
            self.evict(vk, vkey);
        }
        let run = self.add_run(b, false).expect("a run is free");
        #[cfg(test)]
        let key = if MUTATION.get() == 3 { key ^ 1 } else { key };
        let ps = self.spec.phases[k].as_mut().expect("keyed");
        let last = ps.hits + ps.misses;
        ps.table.insert(key, Entry { run, last });
        true
    }

    /// Build and install every request now (`SpecOptions::inline_build`).
    pub(super) fn spec_build_inline(&mut self) {
        while self.spec.has_work() {
            let p = Arc::clone(&self.program);
            for r in self.spec_requests() {
                let b = build_spec(&p, &r);
                self.install_spec(b);
            }
        }
    }

    /// A ROM write switched specialization off, or `reset` back on: builds
    /// requested before are refused, and the keys they were for may be
    /// wanted again; a plan whose samples went out is sampled again.
    pub(super) fn spec_new_generation(&mut self) {
        self.spec_life.generation = next_gen();
        for a in self
            .spec
            .phases
            .iter_mut()
            .flatten()
            .filter_map(|ps| ps.adapt.as_mut())
        {
            a.pending.clear();
            a.wanted.clear();
        }
        // A plan in flight is refused now: ask again.
        self.spec.plan_asked = None;
        if self.spec.awaiting_plan && self.spec.sampler.is_none() && self.spec.rows.is_none() {
            let o = self.options.spec.clone().unwrap_or_default();
            self.spec.sampler = Some(Sampler::new(self.spec.contexts.len(), &o));
            self.spec.refresh();
        }
    }

    /// Keyed phases, bodies, hits and misses, and what the adaptive
    /// tables did ([`SimOptions::spec`]).
    pub fn spec_stats(&self) -> SpecStats {
        let l = &self.spec_life;
        let mut s = SpecStats {
            requested: l.requested,
            installed: l.installed,
            evicted: l.evicted,
            stale: l.stale,
            dropped: l.dropped,
            ..SpecStats::default()
        };
        for ps in self.spec.phases.iter().flatten() {
            s.keyed_phases += 1;
            s.bodies += ps.bodies();
        }
        // Set aside by a ROM write, the tables keep their counts.
        let stashed = self.stash.as_ref().map(|st| &st.spec);
        for sp in std::iter::once(&self.spec).chain(stashed) {
            s.code_bytes += sp.body_bytes + sp.pinned_bytes;
            s.awaiting_plan |= sp.awaiting_plan;
            for ps in sp.phases.iter().flatten() {
                s.hits += ps.hits;
                s.misses += ps.misses;
            }
        }
        s
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
mod adaptive_tests;
#[cfg(test)]
mod tests;
