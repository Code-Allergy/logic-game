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

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use crate::compile::{CompileError, NodePath, Options, Program, compile};
use crate::netlist::{Design, NetId, NodeId};
use crate::sim::{Sim, SimOptions};

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
}

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
        let mut sim = Sim::with_options(program, self.sim_options.clone());
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
        self.resolve_probes();
        self.mem_requested = true;
        self.dirty = true;
        event
    }

    /// Compile and install inline (the threadless path).
    pub fn load(&mut self, design: &Design, revision: u64) -> EngineEvent {
        match compile(design, &self.options) {
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
    use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
    use std::sync::{Arc, Mutex};
    use std::thread::JoinHandle;
    use std::time::{Duration, Instant};

    use super::*;

    /// Time budget per batch: commands are drained between batches.
    const BATCH: Duration = Duration::from_millis(1);

    enum Msg {
        SetInput(NodeId, u64),
        SetRate(Rate),
        Step(u64),
        SetProbes(Vec<Probe>),
        SetMemProbes(Vec<MemProbe>),
        RequestMems,
        Load(Arc<Design>, u64),
        Compiled(u64, Result<Arc<Program>, Vec<CompileError>>),
        Sync(Sender<()>),
        Shutdown,
    }

    struct Shared {
        frame: Mutex<Arc<ProbeFrame>>,
        tick: AtomicU64,
        revision: AtomicU64,
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
            let (tx, rx) = channel::<Msg>();
            let (ev_tx, events) = channel();
            let (compile_tx, compile_rx) = channel::<(Arc<Design>, u64)>();
            let shared = Arc::new(Shared {
                frame: Mutex::new(Arc::new(ProbeFrame::default())),
                tick: AtomicU64::new(0),
                revision: AtomicU64::new(0),
            });

            let back = tx.clone();
            let compile_options = options.clone();
            let compiler = std::thread::Builder::new()
                .name("sim-compile".into())
                .spawn(move || {
                    while let Ok(mut job) = compile_rx.recv() {
                        // Latest revision wins.
                        while let Ok(newer) = compile_rx.try_recv() {
                            job = newer;
                        }
                        let (design, rev) = job;
                        let result = compile(&design, &compile_options).map(Arc::new);
                        if back.send(Msg::Compiled(rev, result)).is_err() {
                            break;
                        }
                    }
                })
                .expect("spawn compiler thread");

            let worker_shared = Arc::clone(&shared);
            let worker = std::thread::Builder::new()
                .name("sim-worker".into())
                .spawn(move || worker_loop(options, rx, compile_tx, ev_tx, worker_shared))
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
        options: Options,
        rx: Receiver<Msg>,
        compile_tx: Sender<(Arc<Design>, u64)>,
        events: Sender<EngineEvent>,
        shared: Arc<Shared>,
    ) {
        let origin = Instant::now();
        let clock = move || origin.elapsed();
        let mut core = EngineCore::new(options);
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
        };

        // Returns false on shutdown.
        let handle = |msg: Msg, core: &mut EngineCore, syncs: &mut Vec<Sender<()>>| -> bool {
            match msg {
                Msg::SetInput(n, v) => core.set_input(n, v),
                Msg::SetRate(r) => core.set_rate(r),
                Msg::Step(n) => core.step(n),
                Msg::SetProbes(p) => core.set_probes(p),
                Msg::SetMemProbes(p) => core.set_mem_probes(p),
                Msg::RequestMems => core.request_mems(),
                Msg::Load(d, rev) => {
                    let _ = compile_tx.send((d, rev));
                }
                Msg::Compiled(rev, Ok(p)) => {
                    let _ = events.send(core.install(p, rev));
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
            core.run(BATCH, &clock);
            if core.dirty() || core.mems_stale() || !syncs.is_empty() {
                publish(&mut core, &mut spare);
            }
            for s in syncs.drain(..) {
                let _ = s.send(());
            }
            // Wait for the next due tick or the next command.
            let msg = match core.wait_hint(&clock) {
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
