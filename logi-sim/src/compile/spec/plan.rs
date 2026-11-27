//! Automatic key choice (package P6, pure part): from each phase's
//! [`SpecContext`] and a window of observed slot values, pick the key
//! worth building bodies for.
//!
//! Per phase: every candidate is scored alone (bodies for its most
//! frequent values, the others run the fallback), the best few are
//! combined greedily up to `max_key_bits`, and [`SpecContext::prune_bits`]
//! drops the bits that do not pay. Gains are *executed* operations per
//! tick (instructions plus commits that the guarded bodies run on the
//! sampled states, as the runtime runs them; the doc's cost model) and
//! charge the key's prefix, which is unguarded and runs every tick, hit
//! or miss: a key whose cone the general body would mostly skip does not
//! pay. A phase with no key that saves anything gets no plan. The function is pure: the same
//! inputs give the same plan, and nothing is run.

use super::{Candidate, KeyField, KeyLayout, SpecContext};
use crate::compile::guard::{MIN_REGION, guard_with};
use crate::compile::{Body, FastMap, Program};

/// Limits and thresholds of [`choose_plan`].
#[derive(Debug, Clone, PartialEq)]
pub struct PlanOptions {
    /// Widest key, in bits, of any phase.
    pub max_key_bits: u32,
    /// `prune_bits` threshold: ops per settle a key bit must be worth.
    pub min_loss: f64,
    /// Bodies a phase is assumed to keep (the most frequent keys).
    pub max_bodies: usize,
    /// A key with more distinct values in the samples is not worth keying
    /// (its bodies would rarely be reused).
    pub max_distinct: usize,
    /// Candidates, best alone first, that the greedy combination tries.
    pub pool: usize,
    /// Ops per tick a key (or a greedy step) must save to count.
    pub min_gain: f64,
}

impl Default for PlanOptions {
    fn default() -> Self {
        PlanOptions {
            max_key_bits: 32,
            min_loss: 4.0,
            max_bodies: 24,
            max_distinct: 256,
            pool: 20,
            min_gain: 2.0,
        }
    }
}

/// What a phase is keyed on, with the estimate it was chosen by.
#[derive(Debug, Clone, PartialEq)]
pub struct PhasePlan {
    pub layout: KeyLayout,
    /// Estimated ops saved per tick of this phase.
    pub saved: f64,
    /// Distinct key values in the samples.
    pub distinct: usize,
    /// Share of the samples whose key has a body.
    pub hit: f64,
}

/// A key per clock phase (`None`: the phase runs its fused body).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SpecPlan {
    pub phases: Vec<Option<PhasePlan>>,
}

/// What keying on a layout is worth on a window of samples.
#[derive(Debug, Clone, PartialEq)]
pub struct Estimate {
    /// Executed ops saved per tick: bodies for `keys`, the fallback
    /// elsewhere, the prefix paid every tick.
    pub saved: f64,
    pub distinct: usize,
    pub hit: f64,
    /// The keys that get bodies, most frequent first.
    pub keys: Vec<u64>,
    /// Instructions of the key's prefix.
    pub prefix_ops: usize,
}

/// Operations (instructions plus commits) `body` runs on the settled
/// values `slots`: guarded regions count only when their guard holds.
pub fn executed_ops(body: &Body, slots: &[u64]) -> usize {
    let mut n = body.commits.len();
    let mut at = 0usize;
    let len =
        |r: std::ops::Range<usize>| body.blocks[r].iter().map(|b| b.len as usize).sum::<usize>();
    for g in &body.guards {
        n += len(at..g.start as usize);
        if g.holds(slots) {
            n += len(g.start as usize..g.end as usize);
        }
        at = g.end as usize;
    }
    n + len(at..body.blocks.len())
}

/// `b` as the runtime runs it: guarded.
pub fn guarded(p: &Program, b: Body) -> Body {
    guard_with(p.slot_count as usize, &b, None, MIN_REGION).unwrap_or(b)
}

/// Key frequencies of `rows` under `layout`, most frequent first (ties by
/// key, so the order is deterministic).
fn frequencies(layout: &KeyLayout, rows: &[Vec<u64>]) -> Vec<(u64, u64)> {
    let mut count: FastMap<u64, u64> = FastMap::default();
    for r in rows {
        *count.entry(layout.pack(r)).or_default() += 1;
    }
    let mut v: Vec<(u64, u64)> = count.into_iter().collect();
    v.sort_by_key(|&(k, n)| (std::cmp::Reverse(n), k));
    v
}

/// The worth of keying on `layout` given `rows` (settled slot values,
/// one row per sampled tick of the phase), with bodies for the `bodies`
/// most frequent keys: per row, the guarded general body's executed ops,
/// minus the prefix's (all of them, unguarded) and those of the guarded
/// specialized body (or fallback, on a key without a body).
pub fn estimate(
    p: &Program,
    cx: &SpecContext,
    layout: &KeyLayout,
    rows: &[Vec<u64>],
    bodies: usize,
) -> Estimate {
    let freq = frequencies(layout, rows);
    let slots: Vec<u32> = layout.fields.iter().map(|f| f.slot).collect();
    let (prefix, pre) = cx.prefix(&slots);
    let general = guarded(p, cx.body().clone());
    let prefix_ops = prefix.ops();
    let pay = prefix_ops as f64;
    let fallback = guarded(p, cx.fallback(p, &pre));
    let mut built: FastMap<u64, Body> = FastMap::default();
    for &(key, _) in freq.iter().take(bodies) {
        let b = guarded(p, cx.specialize(p, &layout.assume(key), &pre));
        built.insert(key, b);
    }
    let (mut saved, mut hits) = (0.0, 0usize);
    for row in rows {
        let body = match built.get(&layout.pack(row)) {
            Some(b) => {
                hits += 1;
                b
            }
            None => &fallback,
        };
        saved += executed_ops(&general, row) as f64 - pay - executed_ops(body, row) as f64;
    }
    let total = rows.len().max(1) as f64;
    Estimate {
        saved: saved / total,
        distinct: freq.len(),
        hit: hits as f64 / total,
        keys: freq.iter().take(bodies).map(|x| x.0).collect(),
        prefix_ops,
    }
}

fn single(c: &Candidate) -> KeyField {
    KeyField {
        slot: c.slot,
        mask: c.support,
    }
}

/// The plan of one phase, or `None` when no key pays.
fn choose_phase(
    p: &Program,
    cx: &SpecContext,
    rows: &[Vec<u64>],
    o: &PlanOptions,
) -> Option<PhasePlan> {
    if rows.is_empty() {
        return None;
    }
    let ok = |e: &Estimate| e.distinct <= o.max_distinct;
    let mut scored: Vec<(usize, f64)> = Vec::new();
    let cands = cx.candidates(p);
    for (i, c) in cands.iter().enumerate() {
        let layout = KeyLayout::new(vec![single(c)]);
        if c.support == 0 || layout.width() > o.max_key_bits {
            continue;
        }
        let e = estimate(p, cx, &layout, rows, o.max_bodies);
        if ok(&e) && e.saved > o.min_gain {
            scored.push((i, e.saved));
        }
    }
    // Stable: ties keep the candidates' order.
    scored.sort_by(|x, y| y.1.total_cmp(&x.1));
    let pool: Vec<usize> = scored.iter().take(o.pool).map(|x| x.0).collect();

    let mut fields: Vec<KeyField> = Vec::new();
    let mut best = 0.0;
    loop {
        let mut pick: Option<(usize, f64)> = None;
        for &i in &pool {
            let c = &cands[i];
            if fields.iter().any(|f| f.slot == c.slot) {
                continue;
            }
            let mut f = fields.clone();
            f.push(single(c));
            let layout = KeyLayout::new(f);
            if layout.width() > o.max_key_bits {
                continue;
            }
            let e = estimate(p, cx, &layout, rows, o.max_bodies);
            if ok(&e) && pick.is_none_or(|x| e.saved > x.1) {
                pick = Some((i, e.saved));
            }
        }
        match pick {
            Some((i, s)) if s > best + o.min_gain => {
                fields.push(single(&cands[i]));
                best = s;
            }
            _ => break,
        }
    }
    if fields.is_empty() {
        return None;
    }
    let greedy = KeyLayout::new(fields);
    let samples = frequencies(&greedy, rows);
    let layout = cx.prune_bits(p, &greedy, &samples, o.min_loss);
    if layout.fields.is_empty() {
        return None;
    }
    let e = estimate(p, cx, &layout, rows, o.max_bodies);
    (ok(&e) && e.saved > o.min_gain).then_some(PhasePlan {
        layout,
        saved: e.saved,
        distinct: e.distinct,
        hit: e.hit,
    })
}

/// The plan for a program: per clock phase, the key to build bodies for.
///
/// `contexts[k]` is phase `k`'s fused-body context (`None` where the
/// analysis does not apply) and `samples[k]` its sampled rows: the settled
/// slot values (all of them, indexed by slot) of some ticks of that phase.
/// A phase with no context, no samples or no worthwhile key has no plan.
pub fn choose_plan(
    p: &Program,
    contexts: &[Option<&SpecContext>],
    samples: &[&[Vec<u64>]],
    opts: &PlanOptions,
) -> SpecPlan {
    SpecPlan {
        phases: contexts
            .iter()
            .enumerate()
            .map(|(k, cx)| {
                let rows = samples.get(k)?;
                choose_phase(p, (*cx)?, rows, opts)
            })
            .collect(),
    }
}
