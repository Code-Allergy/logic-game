//! Simulation engine (design doc section 7; issue #27). Built as
//! `engine_v2` beside the legacy engine, it took the name `engine` in M6.
//!
//! - [`EngineCore`] owns the [`Sim`] and does everything that is not
//!   threading: rate pacing, single steps, batches sized by a time budget,
//!   probe readback, program swaps with state carry-over. It takes its
//!   clock as a parameter, so it is the same on native and wasm.
//! - [`Engine`] (native only) runs a core on a worker thread and compiles
//!   designs on a second thread, off the tick loop.
//! - [`LocalEngine`] runs a core in the caller's thread, compiling inline.
//!   This is the wasm path: call [`LocalEngine::run_frame`] once per frame
//!   with a time budget.
//!
//! **Probes instead of publish-everything.** The caller registers the nets
//! it wants to see as [`Probe`]s. After each batch the core copies just
//! those slots into a dense [`ProbeFrame`], so readback cost scales with
//! the number of probes, not the circuit.
//!
//! **Edits.** Loading a new design revision compiles it, then swaps
//! programs: state is carried across with `export_state`/`import_state`,
//! root inputs are reapplied by port node, and probes are re-resolved. On a
//! compile error the previous program keeps running, and the errors arrive
//! as [`EngineEvent::CompileFailed`].

#[cfg(test)]
mod tests;

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use crate::compile::func::MatchCache;
use crate::compile::{CompileError, NodePath, Options, Program, compile_cached};
use crate::netlist::{Design, NetId, NodeId};
use crate::sim::{Sim, SimOptions, SpecBuilt, SpecRequest};

/// Tick pacing.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Rate {
    /// Only explicit steps run.
    Paused,
    /// Ticks per second.
    Hz(f64),
    /// As fast as possible.
    Unlimited,
}

/// A net to read back: instance path from the root, and the net in that
/// instance's definition.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Probe {
    pub path: Vec<NodeId>,
    pub net: NetId,
}

/// A memory to read back whole (a RAM or a screen): the node at `node`
/// in the instance at `path` from the root.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MemProbe {
    pub path: Vec<NodeId>,
    pub node: NodeId,
}

/// Probe values after a batch.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProbeFrame {
    /// Design revision of the running program (0 before any load).
    pub revision: u64,
    pub tick: u64,
    /// One value per registered probe, in registration order. 0 where the
    /// probe does not resolve (see `present`).
    pub values: Vec<u64>,
    /// Whether each probe resolved to a net in the running program.
    pub present: Vec<bool>,
    /// One copy per registered [`MemProbe`], in registration order; `None`
    /// where it does not resolve. Copies are taken only when asked for
    /// ([`EngineCore::request_mems`]; once per video frame) and only when
    /// the memory changed, so an unchanged memory keeps the same `Arc`
    /// from frame to frame: compare with `Arc::ptr_eq` to skip redrawing.
    pub mems: Vec<Option<Arc<[u64]>>>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum EngineEvent {
    /// A design revision compiled and is now running.
    Loaded {
        revision: u64,
        instrs: usize,
        slots: u32,
    },
    /// A design revision failed to compile; the previous program (if any)
    /// keeps running.
    CompileFailed {
        revision: u64,
        errors: Vec<CompileError>,
    },
}

/// Clock: monotonic time since an arbitrary origin.
pub type Clock<'a> = &'a dyn Fn() -> Duration;

/// Most ticks of debt a paced rate may accumulate before it is dropped
/// (when the circuit cannot keep up): a quarter second's worth.
const MAX_DEBT_SECS: f64 = 0.25;

pub struct EngineCore {
    options: Options,
    /// Runtime options for every `Sim` this core creates (single-threaded
    /// by default).
    sim_options: SimOptions,
    sim: Option<Sim>,
    revision: u64,
    /// Root input values by port node; reapplied after every program swap.
    inputs: HashMap<NodeId, u64>,
    rate: Rate,
    /// Pacing anchor `(time, ticks run under the rate since then)`. `None`
    /// until the next run after a rate change.
    anchor: Option<(Duration, u64)>,
    pending_steps: u64,
    /// Ticks per budget check; adapts so clock reads are rare.
    chunk: u64,
    probes: Vec<Probe>,
    probe_slots: Vec<Option<u32>>,
    mem_probes: Vec<MemProbe>,
    /// Memory index of each mem probe in the running program.
    mem_index: Vec<Option<u32>>,
    /// The last copy of each mem probe, with the `Sim::mem_version` it was
    /// taken at. Cleared when the program changes.
    mem_copies: Vec<Option<(u64, Arc<[u64]>)>>,
    /// The reader asked for fresh memory copies.
    mem_requested: bool,
    /// Something observable changed since the last publish.
    dirty: bool,
    /// Function-matching analyses of earlier compiles: an edit re-analyses
    /// only the definitions it changed.
    match_cache: MatchCache,
    /// JIT tiering (`SimOptions::jit` in the threaded [`Engine`]): a new
    /// program starts interpreted, and its native code is compiled on the
    /// compile thread and swapped in by [`EngineCore::install_jit`].
    jit_tiered: bool,
    /// The tiered program's native code has not arrived yet (or has been
    /// given up on): specialized bodies wait for it, so that each is built
    /// with its native unit ([`SpecRequest::set_native`]).
    jit_wait: bool,
    /// Somebody else builds the `Sim`'s specialized bodies
    /// ([`EngineCore::drive_spec`]): the `Sim`s run with
    /// `SpecOptions::inline_build` off and requests are handed out by
    /// [`EngineCore::spec_jobs`].
    spec_driven: bool,
    /// Requests taken from the `Sim` and not handed out yet (the `Sim`
    /// bounds them by its own caps), plan first.
    spec_queue: VecDeque<SpecRequest>,
    /// Jobs handed out for the running program and not yet installed back.
    spec_inflight: usize,
    /// The automatic plan was handed out for the running program and its
    /// build has not come back: the `Sim` asks again after a plan window,
    /// but one plan build (seconds) at a time is all that is wanted.
    plan_inflight: bool,
}

/// A specialization build to run off the tick loop: `request` was made by
/// the `Sim` running `program`, revision `revision`. Run it with
/// [`crate::sim::build_spec`]`(&job.program, &job.request)` on any thread
/// and give the result to [`EngineCore::install_spec`] with `revision`.
pub struct SpecJob {
    pub revision: u64,
    pub program: Arc<Program>,
    pub request: SpecRequest,
}

impl SpecJob {
    /// Whether it is the automatic plan (the expensive build: seconds on
    /// a console).
    pub fn is_plan(&self) -> bool {
        self.request.body().is_none()
    }

    /// Build it.
    pub fn build(&self) -> SpecBuilt {
        crate::sim::build_spec(&self.program, &self.request)
    }
}

/// Builds in flight at once, at most: bounds what a slow compile thread
/// holds (the rest wait in the core, and in the `Sim`'s own queues).
pub const SPEC_INFLIGHT: usize = 8;

impl EngineCore {
    /// An empty core. `options` are used for every compile; the default
    /// for an editor is [`Options::debug`], which keeps every net probeable.
    pub fn new(options: Options) -> Self {
        Self {
            options,
            sim_options: SimOptions::default(),
            sim: None,
            revision: 0,
            inputs: HashMap::new(),
            rate: Rate::Paused,
            anchor: None,
            pending_steps: 0,
            chunk: 1,
            probes: Vec::new(),
            probe_slots: Vec::new(),
            mem_probes: Vec::new(),
            mem_index: Vec::new(),
            mem_copies: Vec::new(),
            mem_requested: false,
            dirty: true,
            match_cache: MatchCache::new(),
            jit_tiered: false,
            jit_wait: false,
            spec_driven: false,
            spec_queue: VecDeque::new(),
            spec_inflight: 0,
            plan_inflight: false,
        }
    }

    pub fn options(&self) -> &Options {
        &self.options
    }

    /// Runtime options (threads, issue #41) for programs installed from
    /// now on.
    pub fn set_sim_options(&mut self, sim_options: SimOptions) {
        self.sim_options = sim_options;
    }

    pub fn sim_options(&self) -> &SimOptions {
        &self.sim_options
    }

    pub fn sim(&self) -> Option<&Sim> {
        self.sim.as_ref()
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn tick_count(&self) -> u64 {
        self.sim.as_ref().map_or(0, Sim::tick_count)
    }

    pub fn rate(&self) -> Rate {
        self.rate
    }

    /// Change the rate. Setting the current rate again is a no-op: it does
    /// not reset pacing.
    pub fn set_rate(&mut self, rate: Rate) {
        if rate != self.rate {
            self.rate = rate;
            self.anchor = None;
        }
    }

    /// Queue `n` ticks, run regardless of the rate.
    pub fn step(&mut self, n: u64) {
        self.pending_steps = self.pending_steps.saturating_add(n);
    }

    /// Return the running program to power-on (see [`Sim::reset`]): tick
    /// 0, state at its initial values, inputs kept. Queued steps are
    /// dropped; the rate is unchanged. No-op before the first load.
    pub fn reset(&mut self) {
        if let Some(sim) = &mut self.sim {
            sim.reset();
            self.pending_steps = 0;
            self.anchor = None;
            // Memories went back to their initial contents: publish fresh
            // copies (`Sim::reset` moved their versions on).
            self.mem_requested = true;
            self.dirty = true;
        }
    }

    /// Set a root input by its port node. Remembered across edits; ignored
    /// by programs without that port.
    pub fn set_input(&mut self, node: NodeId, value: u64) {
        self.inputs.insert(node, value);
        if let Some(sim) = &mut self.sim
            && let Some(port) = sim.program().ports.input_by_node(node)
        {
            sim.set_input(port, value);
        }
    }

    pub fn set_probes(&mut self, probes: Vec<Probe>) {
        self.probes = probes;
        self.resolve_probes();
        self.dirty = true;
    }

    /// Memories to read back (see [`ProbeFrame::mems`]).
    pub fn set_mem_probes(&mut self, probes: Vec<MemProbe>) {
        self.mem_probes = probes;
        self.resolve_probes();
        self.mem_requested = true;
        self.dirty = true;
    }

    /// Ask for fresh copies of the probed memories at the next publish.
    /// A reader calls this once per frame it draws.
    pub fn request_mems(&mut self) {
        self.mem_requested = true;
    }

    /// Fresh memory copies were asked for and some probed memory changed
    /// since its last copy: worth a publish even if nothing else changed.
    pub fn mems_stale(&self) -> bool {
        self.mem_requested
            && self.sim.as_ref().is_some_and(|sim| {
                self.mem_index
                    .iter()
                    .zip(&self.mem_copies)
                    .any(|(m, copy)| match (m, copy) {
                        (Some(m), Some((v, _))) => sim.mem_version(*m) != *v,
                        (Some(_), None) => true,
                        (None, _) => false,
                    })
            })
    }

    fn resolve_probes(&mut self) {
        self.probe_slots = match &self.sim {
            Some(sim) => {
                let dbg = &sim.program().debug;
                self.probes
                    .iter()
                    .map(|p| dbg.slot(&p.path, p.net))
                    .collect()
            }
            None => vec![None; self.probes.len()],
        };
        self.mem_index = self
            .mem_probes
            .iter()
            .map(|p| {
                let sim = self.sim.as_ref()?;
                sim.mem_index(&NodePath {
                    instances: p.path.clone(),
                    node: p.node,
                })
            })
            .collect();
        self.mem_copies = vec![None; self.mem_probes.len()];
    }

    /// Swap in a compiled program for `revision`, carrying state, inputs and
    /// probes over from the running one.
    pub fn install(&mut self, program: Arc<Program>, revision: u64) -> EngineEvent {
        let snapshot = self.sim.as_ref().map(Sim::export_state);
        // Tiered: native code arrives later (`install_jit`); until then the
        // interpreter runs the layout the native code is compiled from.
        let options = self.sim_run_options();
        let mut sim = Sim::with_options(program, options);
        for (&node, &v) in &self.inputs {
            if let Some(port) = sim.program().ports.input_by_node(node) {
                sim.set_input(port, v);
            }
        }
        match snapshot {
            Some(s) => sim.import_state(&s),
            // A fresh load still applies inputs at the tick-0 settle.
            None => {
                let fresh = sim.export_state();
                sim.import_state(&fresh);
            }
        }
        let event = EngineEvent::Loaded {
            revision,
            instrs: sim.program().instrs.len(),
            slots: sim.program().slot_count,
        };
        self.sim = Some(sim);
        self.revision = revision;
        // Builds in flight were for the old program (their results are
        // dropped by revision, and refused by the `Sim` anyway).
        self.spec_queue.clear();
        self.spec_inflight = 0;
        // A plan building for the old program never comes back here (its
        // revision is stale): the new program may ask for its own.
        self.plan_inflight = false;
        // (Native code only ever arrives with the feature.)
        self.jit_wait = self.jit_tiered && cfg!(all(feature = "jit", not(target_arch = "wasm32")));
        self.resolve_probes();
        self.mem_requested = true;
        self.dirty = true;
        event
    }

    /// The options of the `Sim`s this core creates: tiered when native
    /// code comes later, without inline spec builds when they are driven.
    fn sim_run_options(&self) -> SimOptions {
        let mut options = if self.jit_tiered {
            self.sim_options.tier_layout()
        } else {
            self.sim_options.clone()
        };
        if self.spec_driven
            && let Some(spec) = &mut options.spec
        {
            spec.inline_build = false;
        }
        options
    }

    /// The caller builds the specialized bodies of
    /// [`SimOptions::spec`] (see [`EngineCore::spec_jobs`]) instead of the
    /// `Sim` building them inline. Call it before the first program is
    /// installed. The threaded [`Engine`] does; [`LocalEngine`] (wasm)
    /// does not, so there a `Sim` with `spec` stalls one `run` for the
    /// automatic plan's build: leave `spec` off there.
    pub fn drive_spec(&mut self, on: bool) {
        self.spec_driven = on;
    }

    /// The program's native code is not coming (the compile failed or was
    /// skipped for a newer edit): bodies built from now on need not wait
    /// for it. A no-op for another revision.
    pub fn jit_given_up(&mut self, revision: u64) {
        if self.revision == revision {
            self.jit_wait = false;
        }
    }

    /// Specialization builds to run now, at most [`SPEC_INFLIGHT`] in
    /// flight, the automatic plan before bodies. Call it after each batch;
    /// nothing is handed out while a tiered program awaits its native
    /// code, so that bodies get native units, and the JIT compile is never
    /// queued behind a plan build.
    pub fn spec_jobs(&mut self) -> Vec<SpecJob> {
        let mut out = Vec::new();
        if !self.spec_driven || self.jit_wait {
            return out;
        }
        let Some(sim) = &mut self.sim else {
            return out;
        };
        if self.spec_queue.is_empty() && self.spec_inflight < SPEC_INFLIGHT {
            let mut fresh = sim.spec_requests();
            // A tiered `Sim` builds units only once native code is really
            // installed (not after a failed or refused JIT compile).
            if self.jit_tiered {
                let native = sim.jit_active();
                for r in &mut fresh {
                    r.set_native(native);
                }
            }
            // The plan first (stable: bodies keep their order).
            fresh.sort_by_key(|r| r.body().is_some());
            self.spec_queue.extend(fresh);
        }
        while self.spec_inflight < SPEC_INFLIGHT
            && let Some(request) = self.spec_queue.pop_front()
        {
            if request.body().is_none() {
                if self.plan_inflight {
                    // Asked again while the plan builds: the `Sim` asks
                    // once more after another window if it must.
                    continue;
                }
                self.plan_inflight = true;
            }
            self.spec_inflight += 1;
            out.push(SpecJob {
                revision: self.revision,
                program: Arc::clone(sim.program()),
                request,
            });
        }
        out
    }

    /// Install a build of [`EngineCore::spec_jobs`] into the running
    /// program, if it is still revision `revision` (the `Sim` also checks
    /// its program, ROM generation and caps). Results do not change, only
    /// speed. Returns whether it was installed.
    pub fn install_spec(&mut self, revision: u64, built: SpecBuilt) -> bool {
        if self.revision != revision {
            return false;
        }
        self.spec_inflight = self.spec_inflight.saturating_sub(1);
        if built.body().is_none() {
            self.plan_inflight = false;
        }
        match &mut self.sim {
            Some(sim) => sim.install_spec(built),
            None => false,
        }
    }

    /// Builds installed into the running program so far.
    pub fn spec_installed(&self) -> u64 {
        self.sim.as_ref().map_or(0, |s| s.spec_stats().installed)
    }

    /// Builds handed out and not yet installed.
    pub fn spec_inflight(&self) -> usize {
        self.spec_inflight
    }

    /// Swap JIT-compiled code (`Sim::compile_jit` of a `Sim` with this
    /// revision's program and these `SimOptions`) into the running
    /// program, if it is still revision `revision`. Results do not change,
    /// only speed. Returns whether it was installed.
    #[cfg(all(feature = "jit", not(target_arch = "wasm32")))]
    pub fn install_jit(&mut self, revision: u64, code: Arc<crate::sim::jit::JitProgram>) -> bool {
        match &mut self.sim {
            Some(sim) if self.revision == revision && self.sim_options.jit => {
                self.jit_wait = false;
                sim.install_jit(Some(code))
            }
            _ => false,
        }
    }

    /// Compile and install inline (the threadless path).
    pub fn load(&mut self, design: &Design, revision: u64) -> EngineEvent {
        match compile_cached(design, &self.options, &mut self.match_cache, |_| {}) {
            Ok(p) => self.install(Arc::new(p), revision),
            Err(errors) => EngineEvent::CompileFailed { revision, errors },
        }
    }

    /// Run queued steps, then whatever the rate calls for, for at most about
    /// `budget`. Returns the number of ticks run.
    pub fn run(&mut self, budget: Duration, clock: Clock<'_>) -> u64 {
        if self.sim.is_none() {
            return 0;
        }
        let start = clock();
        let mut ran = 0;

        if self.pending_steps > 0 {
            let n = self.run_for(self.pending_steps, start, budget, clock);
            self.pending_steps -= n;
            ran += n;
        }

        match self.rate {
            Rate::Paused => {}
            Rate::Unlimited => ran += self.run_for(u64::MAX, start, budget, clock),
            Rate::Hz(hz) => {
                let now = clock();
                let (t0, done) = *self.anchor.get_or_insert((now, 0));
                let due = ((now - t0).as_secs_f64() * hz) as u64;
                let max_debt = (hz * MAX_DEBT_SECS).ceil() as u64 + 1;
                let mut done = done;
                if due > done + max_debt {
                    done = due - max_debt; // cannot keep up: drop the debt
                }
                let n = self.run_for(due - done, start, budget, clock);
                self.anchor = Some((t0, done + n));
                ran += n;
            }
        }
        if ran > 0 {
            self.dirty = true;
        }
        ran
    }

    /// Run up to `max` ticks, stopping once `budget` has elapsed since
    /// `start`. Ticks run in chunks between clock reads; the chunk grows
    /// until a chunk takes about 1/16 of the budget.
    fn run_for(&mut self, max: u64, start: Duration, budget: Duration, clock: Clock<'_>) -> u64 {
        let sim = self.sim.as_mut().expect("checked by caller");
        let target = budget / 16;
        let mut ran = 0;
        while ran < max {
            let n = self.chunk.min(max - ran);
            let t = clock();
            sim.run(n);
            ran += n;
            let now = clock();
            if n == self.chunk {
                let took = now - t;
                if took < target / 2 {
                    self.chunk = (self.chunk * 2).min(1 << 24);
                } else if took > target * 2 && self.chunk > 1 {
                    self.chunk /= 2;
                }
            }
            if now - start >= budget {
                break;
            }
        }
        ran
    }

    /// How long the caller may wait before calling [`Self::run`] again:
    /// `None` = until a command arrives, `Some(ZERO)` = immediately.
    pub fn wait_hint(&self, clock: Clock<'_>) -> Option<Duration> {
        self.sim.as_ref()?;
        if self.pending_steps > 0 {
            return Some(Duration::ZERO);
        }
        match self.rate {
            Rate::Paused => None,
            Rate::Unlimited => Some(Duration::ZERO),
            Rate::Hz(hz) => match self.anchor {
                None => Some(Duration::ZERO),
                Some((t0, done)) => {
                    let next = t0 + Duration::from_secs_f64((done + 1) as f64 / hz);
                    Some(next.saturating_sub(clock()))
                }
            },
        }
    }

    /// Whether probe values changed since the last [`Self::publish`].
    pub fn dirty(&self) -> bool {
        self.dirty
    }

    /// Copy probe values into `frame`, reusing its buffers.
    pub fn publish(&mut self, frame: &mut ProbeFrame) {
        frame.revision = self.revision;
        frame.tick = self.tick_count();
        frame.values.clear();
        frame.present.clear();
        for slot in &self.probe_slots {
            let v = match (slot, &self.sim) {
                (Some(s), Some(sim)) => sim.read(*s),
                _ => 0,
            };
            frame.values.push(v);
            frame.present.push(slot.is_some());
        }
        if self.mem_requested
            && let Some(sim) = &self.sim
        {
            for (m, copy) in self.mem_index.iter().zip(&mut self.mem_copies) {
                let Some(m) = *m else { continue };
                let version = sim.mem_version(m);
                if copy.as_ref().is_none_or(|(v, _)| *v != version) {
                    *copy = Some((version, Arc::from(sim.mem(m))));
                }
            }
            self.mem_requested = false;
        }
        frame.mems.clear();
        frame.mems.extend(
            self.mem_copies
                .iter()
                .map(|c| c.as_ref().map(|(_, data)| Arc::clone(data))),
        );
        self.dirty = false;
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Threadless engine (wasm, tests, tools)
// ─────────────────────────────────────────────────────────────────────────────

/// An engine that runs in the caller's thread.
pub struct LocalEngine {
    pub core: EngineCore,
    frame: ProbeFrame,
    events: Vec<EngineEvent>,
}

impl LocalEngine {
    pub fn new(options: Options) -> Self {
        Self {
            core: EngineCore::new(options),
            frame: ProbeFrame::default(),
            events: Vec::new(),
        }
    }

    /// Compile and swap in a design revision (inline).
    pub fn load(&mut self, design: &Design, revision: u64) {
        let e = self.core.load(design, revision);
        self.events.push(e);
    }

    /// Run for at most `budget` (e.g. a few ms per frame) and refresh the
    /// probe frame. Returns ticks run.
    /// Memories are copied at most once per call, when they changed.
    pub fn run_frame(&mut self, budget: Duration, clock: Clock<'_>) -> u64 {
        let n = self.core.run(budget, clock);
        self.core.request_mems();
        if self.core.dirty() || self.core.mems_stale() {
            self.core.publish(&mut self.frame);
        }
        n
    }

    pub fn frame(&self) -> &ProbeFrame {
        &self.frame
    }

    pub fn drain_events(&mut self) -> Vec<EngineEvent> {
        std::mem::take(&mut self.events)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Threaded engine (native)
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(not(target_arch = "wasm32"))]
pub use threaded::Engine;

#[cfg(not(target_arch = "wasm32"))]
mod threaded {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, TryRecvError, channel};
    use std::sync::{Arc, Mutex};
    use std::thread::JoinHandle;
    use std::time::{Duration, Instant};

    use super::*;

    #[cfg(feature = "jit")]
    /// Options for `Sim`s whose spec builds are driven by the engine.
    fn driven(mut o: SimOptions) -> SimOptions {
        if let Some(spec) = &mut o.spec {
            spec.inline_build = false;
        }
        o
    }

    /// Time budget per batch: commands are drained between batches.
    const BATCH: Duration = Duration::from_millis(1);

    enum Msg {
        SetInput(NodeId, u64),
        SetRate(Rate),
        Step(u64),
        Reset,
        SetProbes(Vec<Probe>),
        SetMemProbes(Vec<MemProbe>),
        RequestMems,
        Load(Arc<Design>, u64),
        Compiled(u64, Result<Arc<Program>, Vec<CompileError>>),
        /// Native code for revision `.0` (JIT tiering), or `None` when it
        /// is not coming (compile failure, or skipped for a newer edit).
        #[cfg(feature = "jit")]
        Jitted(u64, Option<Arc<crate::sim::jit::JitProgram>>),
        /// A specialization build for revision `.0` (`EngineCore::spec_jobs`).
        SpecBuilt(u64, SpecBuilt),
        Sync(Sender<()>),
        Shutdown,
    }

    /// A job if one waits; `Err` once the worker is gone.
    pub(super) fn poll(rx: &Receiver<Job>) -> Result<Option<Job>, ()> {
        match rx.try_recv() {
            Ok(job) => Ok(Some(job)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err(()),
        }
    }

    /// Queue a spec build; only the newest plan is kept (an older one is
    /// replaced: the program it was for has been replaced, or the `Sim`
    /// asked again).
    pub(super) fn enqueue(backlog: &mut VecDeque<SpecJob>, job: SpecJob) {
        if job.is_plan() {
            backlog.retain(|j| !j.is_plan());
        }
        backlog.push_back(job);
    }

    /// Work for the compile thread.
    pub(super) enum Job {
        Compile(Arc<Design>, u64),
        Spec(SpecJob),
    }

    struct Shared {
        frame: Mutex<Arc<ProbeFrame>>,
        tick: AtomicU64,
        revision: AtomicU64,
        /// Specialized bodies (and plans) installed so far in the running
        /// program, as of the last publish.
        spec_installed: AtomicU64,
    }

    /// Runs an [`EngineCore`] on a worker thread. Designs compile on a
    /// second thread, so edits never stall the tick loop.
    pub struct Engine {
        tx: Sender<Msg>,
        events: Receiver<EngineEvent>,
        shared: Arc<Shared>,
        worker: Option<JoinHandle<()>>,
        compiler: Option<JoinHandle<()>>,
    }

    impl Engine {
        pub fn spawn(options: Options) -> Self {
            Self::spawn_with(options, SimOptions::default())
        }

        /// [`Engine::spawn`] with runtime options for every `Sim`. With
        /// [`SimOptions::jit`] (and the `jit` feature), JIT compilation is
        /// tiered: every new program starts interpreted at once, and the
        /// compile thread compiles its native code and swaps it in when
        /// ready (unless a newer edit is already waiting).
        ///
        /// With [`SimOptions::spec`], state-keyed specialization is
        /// adaptive and built off the tick loop too: the worker hands the
        /// `Sim`'s requests (at most [`SPEC_INFLIGHT`] in flight) to the
        /// compile thread, which builds them after the JIT compile (the
        /// automatic plan first, then bodies) and sends them back, to be
        /// installed between batches if the revision still runs. An edit
        /// drops the builds of the old revision. Frames never change.
        pub fn spawn_with(options: Options, sim_options: SimOptions) -> Self {
            let origin = Instant::now();
            Self::spawn_with_clock(options, sim_options, Arc::new(move || origin.elapsed()))
        }

        /// [`Engine::spawn_with`] with the worker reading `clock` instead
        /// of the wall clock (tests pace it by hand).
        pub(crate) fn spawn_with_clock(
            options: Options,
            sim_options: SimOptions,
            clock: Arc<dyn Fn() -> Duration + Send + Sync>,
        ) -> Self {
            let (tx, rx) = channel::<Msg>();
            let (ev_tx, events) = channel();
            let (compile_tx, compile_rx) = channel::<Job>();
            let shared = Arc::new(Shared {
                frame: Mutex::new(Arc::new(ProbeFrame::default())),
                tick: AtomicU64::new(0),
                revision: AtomicU64::new(0),
                spec_installed: AtomicU64::new(0),
            });

            let back = tx.clone();
            let compile_options = options.clone();
            #[cfg(feature = "jit")]
            let jit_options = driven(sim_options.clone());
            let compiler = std::thread::Builder::new()
                .name("sim-compile".into())
                .spawn(move || {
                    let mut cache = MatchCache::new();
                    // Specialization builds waiting, and the revision of the
                    // program compiled last (builds for another are stale:
                    // the worker installs that program and drops them).
                    let mut backlog: VecDeque<SpecJob> = VecDeque::new();
                    let mut current = None;
                    #[cfg_attr(not(feature = "jit"), allow(unused_mut))]
                    let mut next: Option<Job> = None;
                    loop {
                        // An edit first, then (when it is compiled) the
                        // JIT, then the plan, then bodies, one at a time.
                        let job = match next.take() {
                            Some(job) => Some(job),
                            None if backlog.is_empty() => match compile_rx.recv() {
                                Ok(job) => Some(job),
                                Err(_) => break,
                            },
                            None => match poll(&compile_rx) {
                                Ok(job) => job,
                                Err(()) => break,
                            },
                        };
                        let Some(job) = job else {
                            let i = backlog
                                .iter()
                                .position(SpecJob::is_plan)
                                .unwrap_or_default();
                            let job = backlog.remove(i).expect("a job waits");
                            if current == Some(job.revision)
                                && back
                                    .send(Msg::SpecBuilt(job.revision, job.build()))
                                    .is_err()
                            {
                                break;
                            }
                            continue;
                        };
                        let (design, rev) = match job {
                            Job::Spec(job) => {
                                enqueue(&mut backlog, job);
                                continue;
                            }
                            Job::Compile(design, rev) => (design, rev),
                        };
                        // Latest revision wins.
                        let (mut design, mut rev) = (design, rev);
                        while let Ok(newer) = compile_rx.try_recv() {
                            match newer {
                                Job::Compile(d, r) => (design, rev) = (d, r),
                                Job::Spec(job) => enqueue(&mut backlog, job),
                            }
                        }
                        let result = compile_cached(&design, &compile_options, &mut cache, |_| {})
                            .map(Arc::new);
                        if result.is_ok() {
                            current = Some(rev);
                        }
                        #[cfg(feature = "jit")]
                        let program = result.as_ref().ok().map(Arc::clone);
                        if back.send(Msg::Compiled(rev, result)).is_err() {
                            break;
                        }
                        // JIT tiering: the program already runs interpreted;
                        // compile its native code unless a newer edit waits.
                        #[cfg(feature = "jit")]
                        if let (Some(p), true) = (program, jit_options.jit) {
                            while let Ok(newer) = compile_rx.try_recv() {
                                match newer {
                                    Job::Spec(job) => enqueue(&mut backlog, job),
                                    edit => next = Some(edit),
                                }
                            }
                            let code = if next.is_none() {
                                let layout = Sim::with_options(p, jit_options.tier_layout());
                                layout.compile_jit().ok().map(Arc::new)
                            } else {
                                None
                            };
                            if back.send(Msg::Jitted(rev, code)).is_err() {
                                break;
                            }
                        }
                    }
                })
                .expect("spawn compiler thread");

            let worker_shared = Arc::clone(&shared);
            let worker = std::thread::Builder::new()
                .name("sim-worker".into())
                .spawn(move || {
                    let mut core = EngineCore::new(options);
                    core.jit_tiered = sim_options.jit;
                    core.drive_spec(sim_options.spec.is_some());
                    core.set_sim_options(sim_options);
                    worker_loop(core, rx, compile_tx, ev_tx, worker_shared, &*clock)
                })
                .expect("spawn worker thread");

            Engine {
                tx,
                events,
                shared,
                worker: Some(worker),
                compiler: Some(compiler),
            }
        }

        /// Compile `design` off the tick loop and swap it in when ready.
        pub fn load(&self, design: Arc<Design>, revision: u64) {
            let _ = self.tx.send(Msg::Load(design, revision));
        }
        pub fn set_input(&self, node: NodeId, value: u64) {
            let _ = self.tx.send(Msg::SetInput(node, value));
        }
        pub fn set_rate(&self, rate: Rate) {
            let _ = self.tx.send(Msg::SetRate(rate));
        }
        pub fn step(&self, n: u64) {
            let _ = self.tx.send(Msg::Step(n));
        }
        /// See [`EngineCore::reset`].
        pub fn reset(&self) {
            let _ = self.tx.send(Msg::Reset);
        }
        pub fn set_probes(&self, probes: Vec<Probe>) {
            let _ = self.tx.send(Msg::SetProbes(probes));
        }
        pub fn set_mem_probes(&self, probes: Vec<MemProbe>) {
            let _ = self.tx.send(Msg::SetMemProbes(probes));
        }
        /// Ask for fresh copies of the probed memories (once per drawn
        /// frame); they are in a frame published soon after, also while
        /// paused.
        pub fn request_mems(&self) {
            let _ = self.tx.send(Msg::RequestMems);
        }
        /// Latest probe frame (a cheap `Arc` clone).
        pub fn frame(&self) -> Arc<ProbeFrame> {
            Arc::clone(&self.shared.frame.lock().expect("frame lock"))
        }
        pub fn tick_count(&self) -> u64 {
            self.shared.tick.load(Ordering::Relaxed)
        }
        pub fn revision(&self) -> u64 {
            self.shared.revision.load(Ordering::Relaxed)
        }
        /// Specialized builds installed into the running program
        /// ([`SimOptions::spec`]), as of the last frame.
        pub fn spec_installed(&self) -> u64 {
            self.shared.spec_installed.load(Ordering::Relaxed)
        }
        pub fn try_event(&self) -> Option<EngineEvent> {
            self.events.try_recv().ok()
        }
        /// Block until the next event (with a timeout).
        pub fn wait_event(&self, timeout: Duration) -> Option<EngineEvent> {
            self.events.recv_timeout(timeout).ok()
        }
        /// Block until every command sent so far has been handled, and the
        /// frame published after it.
        pub fn sync(&self) {
            let (tx, rx) = channel();
            if self.tx.send(Msg::Sync(tx)).is_ok() {
                let _ = rx.recv();
            }
        }
    }

    impl Drop for Engine {
        fn drop(&mut self) {
            let _ = self.tx.send(Msg::Shutdown);
            if let Some(w) = self.worker.take() {
                let _ = w.join();
            }
            // The compiler thread exits once the worker drops its sender.
            if let Some(c) = self.compiler.take() {
                let _ = c.join();
            }
        }
    }

    fn worker_loop(
        mut core: EngineCore,
        rx: Receiver<Msg>,
        compile_tx: Sender<Job>,
        events: Sender<EngineEvent>,
        shared: Arc<Shared>,
        clock: Clock<'_>,
    ) {
        let mut spare: Option<Arc<ProbeFrame>> = None;
        let mut syncs: Vec<Sender<()>> = Vec::new();

        let publish = |core: &mut EngineCore, spare: &mut Option<Arc<ProbeFrame>>| {
            // Reuse the previous frame's buffers once no reader holds it.
            let mut frame = match spare.take() {
                Some(a) if Arc::strong_count(&a) == 1 && Arc::weak_count(&a) == 0 => a,
                _ => Arc::new(ProbeFrame::default()),
            };
            core.publish(Arc::get_mut(&mut frame).expect("unique"));
            let old = std::mem::replace(&mut *shared.frame.lock().expect("frame lock"), frame);
            *spare = Some(old);
            shared.tick.store(core.tick_count(), Ordering::Relaxed);
            shared.revision.store(core.revision(), Ordering::Relaxed);
            shared
                .spec_installed
                .store(core.spec_installed(), Ordering::Relaxed);
        };

        // Returns false on shutdown.
        let handle = |msg: Msg, core: &mut EngineCore, syncs: &mut Vec<Sender<()>>| -> bool {
            match msg {
                Msg::SetInput(n, v) => core.set_input(n, v),
                Msg::SetRate(r) => core.set_rate(r),
                Msg::Step(n) => core.step(n),
                Msg::Reset => core.reset(),
                Msg::SetProbes(p) => core.set_probes(p),
                Msg::SetMemProbes(p) => core.set_mem_probes(p),
                Msg::RequestMems => core.request_mems(),
                Msg::Load(d, rev) => {
                    let _ = compile_tx.send(Job::Compile(d, rev));
                }
                Msg::Compiled(rev, Ok(p)) => {
                    let _ = events.send(core.install(p, rev));
                }
                #[cfg(feature = "jit")]
                Msg::Jitted(rev, Some(code)) => {
                    core.install_jit(rev, code);
                }
                #[cfg(feature = "jit")]
                Msg::Jitted(rev, None) => core.jit_given_up(rev),
                Msg::SpecBuilt(rev, built) => {
                    core.install_spec(rev, built);
                }
                Msg::Compiled(revision, Err(errors)) => {
                    let _ = events.send(EngineEvent::CompileFailed { revision, errors });
                }
                Msg::Sync(tx) => {
                    core.request_mems();
                    syncs.push(tx);
                }
                Msg::Shutdown => return false,
            }
            true
        };

        loop {
            // Drain commands between batches.
            while let Ok(msg) = rx.try_recv() {
                if !handle(msg, &mut core, &mut syncs) {
                    return;
                }
            }
            core.run(BATCH, clock);
            for job in core.spec_jobs() {
                let _ = compile_tx.send(Job::Spec(job));
            }
            if core.dirty() || core.mems_stale() || !syncs.is_empty() {
                publish(&mut core, &mut spare);
            }
            for s in syncs.drain(..) {
                let _ = s.send(());
            }
            // Wait for the next due tick or the next command.
            let msg = match core.wait_hint(clock) {
                Some(d) if d.is_zero() => continue,
                Some(d) => match rx.recv_timeout(d) {
                    Ok(m) => m,
                    Err(RecvTimeoutError::Timeout) => continue,
                    Err(RecvTimeoutError::Disconnected) => return,
                },
                None => match rx.recv() {
                    Ok(m) => m,
                    Err(_) => return,
                },
            };
            if !handle(msg, &mut core, &mut syncs) {
                return;
            }
        }
    }
}
