//! Function matching of subcircuit definitions (issue #117, stage 2).
//!
//! Players build the same function in many ways: a flip-flop from NANDs or
//! NORs, an adder ripple-carry or carry-lookahead. This pass recognises a
//! definition by what it computes, not how: per definition (children
//! first), it flattens the definition ([`flat`]), fingerprints it by random
//! simulation, matches the outputs against a catalogue of word functions
//! and proves the match ([`matcher`]). A proven match becomes a
//! [`replace::Replacement`]: a few word primitives wired to the
//! definition's ports. `emit` lowers every instance of a matched
//! definition (outside another substituted instance, and not the root) to
//! its replacement instead of its gates, before simplify and word lifting,
//! so everything downstream sees RTL-like structure. The instance's
//! internal nets stay readable as shadow slots (see
//! [`crate::compile::Shadow`]).
//!
//! The analysis of a definition depends only on its content and its
//! children's, so it is cached by a content key ([`MatchCache`]): an edit
//! re-analyses only the definitions it changed.

pub(crate) mod flat;
pub(crate) mod matcher;
pub(crate) mod replace;

use std::sync::{Arc, OnceLock};

use slotmap::SecondaryMap;

use super::FastMap;
use crate::netlist::{DefId, Design, NodeKind};
use flat::{Flat, NoFlat};
pub use matcher::Proof;
use replace::Replacement;

/// Largest definition (flattened steps) analysed.
const MAX_STEPS: usize = 4096;

/// Cached entries kept beyond those the last compile used.
const MAX_CACHED: usize = 2048;

/// A definition's content key: BLAKE3 over a canonical serialization of
/// the definition (node and net ids, kinds, wiring, ports) with each
/// child definition replaced by its own key. 256 bits, so a wrong hit,
/// which would substitute the wrong function, is not a practical concern;
/// and stable across builds for a given [`KEY_FORMAT`].
pub type ContentKey = [u8; 32];

/// Version of the serialization behind [`ContentKey`].
pub const KEY_FORMAT: &[u8] = b"logi-sim func-match key v1";

/// What the analysis found for one definition.
#[derive(Debug)]
pub struct DefInfo {
    pub(crate) repl: Option<Arc<Replacement>>,
    /// How the match was proven.
    pub proof: Option<Proof>,
    /// What it matched as, or why it did not.
    pub note: String,
    /// The full gate-level flat, built on first inspection.
    inspect: OnceLock<Option<Arc<Flat>>>,
}

impl DefInfo {
    pub fn matched(&self) -> bool {
        self.repl.is_some()
    }

    /// The gate-level flat of `def` for reading internal nets.
    pub(crate) fn inspect(&self, design: &Design, def: DefId) -> Option<Arc<Flat>> {
        self.inspect
            .get_or_init(|| {
                flat::build(design, def, &|_| None, true, usize::MAX)
                    .ok()
                    .map(Arc::new)
            })
            .clone()
    }
}

/// Analysis results by [`ContentKey`], kept across compiles (an engine
/// holds one), so an edit re-analyses only the definitions it changed.
#[derive(Debug, Default)]
pub struct MatchCache {
    map: FastMap<ContentKey, (Arc<DefInfo>, u64)>,
    generation: u64,
    /// Definitions analysed (misses) and reused (hits), over the cache's
    /// life.
    pub analysed: u64,
    pub reused: u64,
}

impl MatchCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

/// The analysis of every definition reachable from the root except the
/// root itself (which is never substituted).
pub(crate) type Analysis = SecondaryMap<DefId, Arc<DefInfo>>;

/// Does `kind` make a definition unanalysable (not a function of its
/// ports and state)?
fn unsupported(kind: &NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Clock { .. }
            | NodeKind::Rom { .. }
            | NodeKind::Ram { .. }
            | NodeKind::Ram2 { .. }
            | NodeKind::Screen { .. }
    )
}

/// The content key of `def`, given its children's (`None` if a child is
/// unsupported or `def` contains an unsupported node).
fn content_key(
    design: &Design,
    def: DefId,
    keys: &SecondaryMap<DefId, Option<ContentKey>>,
) -> Option<ContentKey> {
    use std::fmt::Write;
    let d = &design.defs[def];
    let mut h = blake3::Hasher::new();
    h.update(KEY_FORMAT);
    let mut text = String::new();
    let id = |k: slotmap::KeyData| k.as_ffi().to_le_bytes();
    use slotmap::Key;
    for (nid, node) in &d.nodes {
        h.update(b"N");
        h.update(&id(nid.data()));
        match &node.kind {
            NodeKind::Instance(child) => {
                h.update(b"I");
                h.update(&(*keys.get(*child)?)?);
            }
            k if unsupported(k) => return None,
            k => {
                text.clear();
                let _ = write!(text, "{k:?}");
                h.update(&(text.len() as u32).to_le_bytes());
                h.update(text.as_bytes());
            }
        }
        h.update(&(node.inputs.len() as u32).to_le_bytes());
        for i in &node.inputs {
            h.update(&i.map_or(u64::MAX, |n| n.data().as_ffi()).to_le_bytes());
        }
        h.update(&(node.outputs.len() as u32).to_le_bytes());
        for o in &node.outputs {
            h.update(&id(o.data()));
        }
    }
    for (nid, net) in &d.nets {
        h.update(b"W");
        h.update(&id(nid.data()));
        h.update(&[net.width]);
    }
    h.update(b"P");
    for p in d.inputs.iter().chain(&d.outputs) {
        h.update(&id(p.data()));
    }
    h.update(&(d.inputs.len() as u32).to_le_bytes());
    Some(*h.finalize().as_bytes())
}

/// Analyse every definition reachable from the root (children first),
/// reusing `cache`.
pub(crate) fn analyze(design: &Design, cache: &mut MatchCache) -> Analysis {
    cache.generation += 1;
    let generation = cache.generation;
    let mut keys: SecondaryMap<DefId, Option<ContentKey>> = SecondaryMap::new();
    let mut out: Analysis = SecondaryMap::new();
    for d in design.defs_bottom_up() {
        let key = content_key(design, d, &keys);
        keys.insert(d, key);
        if d == design.root {
            continue;
        }
        let Some(key) = key else { continue };
        if let Some((info, used)) = cache.map.get_mut(&key) {
            *used = generation;
            cache.reused += 1;
            out.insert(d, Arc::clone(info));
            continue;
        }
        cache.analysed += 1;
        let info = Arc::new(analyse_def(design, d, &out));
        cache.map.insert(key, (Arc::clone(&info), generation));
        out.insert(d, info);
    }
    if cache.map.len() > MAX_CACHED {
        cache.map.retain(|_, (_, used)| *used == generation);
    }
    out
}

/// Fingerprint, match and prove one definition.
fn analyse_def(design: &Design, def: DefId, done: &Analysis) -> DefInfo {
    let abstr = |c: DefId| {
        done.get(c)
            .and_then(|i| i.repl.clone())
            .filter(|r| !r.is_sequential())
    };
    let info = |repl: Option<Replacement>, proof, note: String| DefInfo {
        repl: repl.map(Arc::new),
        proof,
        note,
        inspect: OnceLock::new(),
    };
    let f = match flat::build(design, def, &abstr, false, MAX_STEPS) {
        Ok(f) => f,
        Err(NoFlat::Unsupported) => return info(None, None, "clock or memory inside".into()),
        Err(NoFlat::TooLarge) => return info(None, None, "too large".into()),
        Err(NoFlat::Cycle) => return info(None, None, "combinational cycle".into()),
    };
    let result = if f.states.is_empty() {
        let cost = f
            .steps
            .iter()
            .filter(|s| !matches!(s.kind, NodeKind::Const { .. }))
            .count();
        matcher::match_comb(&f, cost)
    } else {
        matcher::match_seq(&f)
    };
    match result {
        Ok((r, proof)) => {
            let note = r.desc.clone();
            info(Some(r), Some(proof), note)
        }
        Err(why) => info(None, None, why),
    }
}

/// Per definition: name, whether it matched, and the note (diagnostics,
/// `perf-metrics`).
pub fn report(design: &Design, cache: &mut MatchCache) -> Vec<(String, bool, String)> {
    let a = analyze(design, cache);
    let mut v: Vec<_> = a
        .iter()
        .map(|(d, i)| (design.defs[d].name.clone(), i.matched(), i.note.clone()))
        .collect();
    v.sort();
    v
}

#[cfg(test)]
mod tests;
