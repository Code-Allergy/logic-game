//! Multithreaded settle (issue #41, [`super::SimOptions::threads`]).
//!
//! # Schedule
//!
//! Built once when the [`super::Sim`] is created. The settle's blocks are
//! grouped by rank. A rank whose work (one unit per streamed instruction,
//! three per instruction run from the full `Instr`, and for a packed word
//! four plus its gathers and scatters: roughly nanoseconds) reaches
//! [`super::SimOptions::par_min`] becomes a **parallel phase**: its
//! instructions are cut into `threads` contiguous pieces of about equal
//! work, possibly splitting blocks. A program laid out for that many
//! threads (`Options::partitions`, `Program::block_parts`) is split by its
//! partitions instead, so a thread mostly reads values it computed itself. Runs of smaller ranks become **serial
//! phases**, run by the calling thread (thread 0) alone. A program with no
//! parallel phase starts no threads.
//!
//! # Synchronization
//!
//! There is no global barrier between phases. Each thread publishes a
//! progress counter after each phase in which it wrote something. Before a
//! phase, a thread waits only for the threads whose earlier writes it reads
//! in that phase, up to the phase of the last such write (the wait lists
//! are computed with the schedule). So workers skip serial phases unless
//! they need their results, and a thread can run ahead of threads it does
//! not read from. At the end of a settle the calling thread waits for every
//! worker to finish.
//!
//! # Soundness
//!
//! Threads share the slot array through a raw pointer. The schedule is only
//! used after checking, slot by slot over the whole settle, that:
//!
//! 1. every slot is written by at most one instruction;
//! 2. no instruction of a parallel phase reads a slot written in the same
//!    phase;
//! 3. no slot is read in a phase before or in the phase where another
//!    thread writes it.
//!
//! Every compiled program passes (each slot has one producer, and ranks only
//! read earlier ranks); otherwise the `Sim` runs single-threaded. Given 1-3,
//! two threads access the same slot only as "writer in phase `p`, reader in
//! a later phase", and the reader's wait on the writer's progress counter
//! (release store after phase `p`, acquire load before reading) orders the
//! write before the read. Slots written outside the settle (inputs,
//! constants, state, by commits) and memory contents are written only by
//! the calling thread between settles: before it starts one (the epoch
//! bump is a release that workers acquire) and after it has seen every
//! worker's final progress (acquire). The calling thread blocks in
//! [`Pool::settle`] until then, so the buffers it lends out stay alive and
//! unmoved while workers use them.
//!
//! # Waiting
//!
//! Waits spin briefly and then yield. Between settles a worker spins for a
//! while (back-to-back ticks are common) and then sleeps on a condition
//! variable; [`Pool::settle`] wakes sleepers only when there are any.

use std::hint::spin_loop;
use std::ops::Range;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;

use super::code::{Code, Env, Form, XBlock, exec};
use crate::compile::{Op, Program};

/// One piece of a parallel phase: instructions `lo..hi` of `code.blocks[block]`.
#[derive(Debug, Clone, Copy)]
pub(super) struct Seg {
    pub block: u32,
    pub lo: u32,
    pub hi: u32,
}

#[derive(Debug, Clone)]
pub(super) enum Phase {
    /// Blocks run in full by thread 0.
    Serial(Range<u32>),
    /// Thread `t` runs `segs[offsets[t]..offsets[t + 1]]`.
    Parallel { offsets: Vec<u32> },
}

#[derive(Debug, Default)]
pub(super) struct Schedule {
    pub threads: usize,
    pub phases: Vec<Phase>,
    pub segs: Vec<Seg>,
    /// Before phase `p`, thread `t` waits for `(thread, phase)` in
    /// `waits[wait_off[p * threads + t]..wait_off[p * threads + t + 1]]`:
    /// that thread must have finished that phase.
    pub waits: Vec<(u32, u32)>,
    pub wait_off: Vec<u32>,
}

impl Schedule {
    pub fn parallel_phases(&self) -> usize {
        self.phases
            .iter()
            .filter(|p| matches!(p, Phase::Parallel { .. }))
            .count()
    }
}

/// Work units of item `k` of block `b` (see the module docs).
fn weight(p: &Program, b: &XBlock, k: u32) -> u64 {
    match b.form {
        Form::Packed { start } => {
            let pk = &p.packs[(start + k) as usize];
            4 + (pk.a.gather_len + pk.b.gather_len + pk.c.gather_len + pk.scatter_len) as u64
        }
        Form::Plain { .. } => 3,
        _ => 1,
    }
}

/// Slots written and read by item `k` of block `b`, passed to `w` and `r`.
fn access(p: &Program, b: &XBlock, k: u32, mut w: impl FnMut(u32), mut r: impl FnMut(u32)) {
    match b.form {
        Form::Packed { start } => {
            let pk = &p.packs[(start + k) as usize];
            w(pk.dst);
            for x in &p.scatters[pk.scatter_start as usize..][..pk.scatter_len as usize] {
                w(x.slot);
            }
            for o in [&pk.a, &pk.b, &pk.c] {
                r(o.base);
                r(o.splat);
                for g in &p.gathers[o.gather_start as usize..][..o.gather_len as usize] {
                    r(g.slot);
                }
            }
        }
        Form::Seq { dst0, .. } => {
            // Operands are the instruction's own, in program order.
            let ins = &p.instrs[b.src as usize + k as usize];
            debug_assert_eq!(ins.dst, dst0 + k);
            w(ins.dst);
            operands(b.op, ins, r);
        }
        Form::Plain { start } => {
            let ins = &p.instrs[(start + k) as usize];
            w(ins.dst);
            operands(b.op, ins, r);
        }
    }
}

fn operands(op: Op, ins: &crate::compile::Instr, mut r: impl FnMut(u32)) {
    let (sa, sb, sc) = op.slot_operands();
    for (is, s) in [(sa, ins.a), (sb, ins.b), (sc, ins.c)] {
        if is {
            r(s);
        }
    }
}

impl Schedule {
    /// Build the schedule for `threads` threads; ranks with less than
    /// `par_min` work run serially.
    pub fn new(p: &Program, code: &Code, threads: usize, par_min: u64) -> Self {
        let mut sched = Schedule {
            threads,
            ..Schedule::default()
        };
        // Stamp per slot: the last phase id that wrote it.
        let mut stamp = vec![u32::MAX; p.slot_count as usize];
        let blocks = &code.blocks;
        let mut lo = 0usize;
        while lo < blocks.len() {
            let rank = blocks[lo].rank;
            let mut hi = lo;
            while hi < blocks.len() && blocks[hi].rank == rank {
                hi += 1;
            }
            let work: u64 = (lo..hi)
                .map(|b| {
                    (0..blocks[b].len)
                        .map(|k| weight(p, &blocks[b], k))
                        .sum::<u64>()
                })
                .sum();
            let id = sched.phases.len() as u32;
            let parallel = threads > 1 && work >= par_min.max(1) && {
                // Distinct writes, and no read of this phase's writes.
                let mut ok = true;
                for b in &blocks[lo..hi] {
                    for k in 0..b.len {
                        access(
                            p,
                            b,
                            k,
                            |s| {
                                ok &= stamp[s as usize] != id;
                                stamp[s as usize] = id;
                            },
                            |_| {},
                        );
                    }
                }
                for b in &blocks[lo..hi] {
                    for k in 0..b.len {
                        access(p, b, k, |_| {}, |s| ok &= stamp[s as usize] != id);
                    }
                }
                ok
            };
            let parts = &p.block_parts;
            let by_part = parts.len() == blocks.len()
                && parts.iter().map(|&x| x as usize + 1).max() == Some(threads);
            if parallel && by_part {
                // The compiler balanced the partitions: thread `t` runs the
                // blocks of partition `t` in full.
                let mut offsets = vec![sched.segs.len() as u32];
                for t in 0..threads {
                    for bi in lo..hi {
                        if parts[bi] as usize == t {
                            sched.segs.push(Seg {
                                block: bi as u32,
                                lo: 0,
                                hi: blocks[bi].len,
                            });
                        }
                    }
                    offsets.push(sched.segs.len() as u32);
                }
                sched.phases.push(Phase::Parallel { offsets });
            } else if parallel {
                let mut offsets = vec![sched.segs.len() as u32];
                let (mut t, mut acc) = (0usize, 0u64);
                let cut = |t: usize| work * (t as u64 + 1) / threads as u64;
                for (bi, b) in blocks.iter().enumerate().take(hi).skip(lo) {
                    let mut seg_lo = 0;
                    for k in 0..b.len {
                        acc += weight(p, b, k);
                        if acc >= cut(t) && t + 1 < threads {
                            sched.segs.push(Seg {
                                block: bi as u32,
                                lo: seg_lo,
                                hi: k + 1,
                            });
                            seg_lo = k + 1;
                            offsets.push(sched.segs.len() as u32);
                            t += 1;
                        }
                    }
                    if seg_lo < b.len {
                        sched.segs.push(Seg {
                            block: bi as u32,
                            lo: seg_lo,
                            hi: b.len,
                        });
                    }
                }
                while offsets.len() < threads + 1 {
                    offsets.push(sched.segs.len() as u32);
                }
                sched.phases.push(Phase::Parallel { offsets });
            } else {
                match sched.phases.last_mut() {
                    Some(Phase::Serial(r)) if r.end == lo as u32 => r.end = hi as u32,
                    _ => sched.phases.push(Phase::Serial(lo as u32..hi as u32)),
                }
            }
            lo = hi;
        }
        if sched.parallel_phases() > 0 && !sched.plan_waits(p, code) {
            // Not provably race-free: run single-threaded.
            sched.phases.clear();
        }
        sched
    }

    /// Items of phase `ph` run by thread `t`, as `(block, lo..hi)`.
    fn parts(&self, code: &Code, ph: &Phase, t: usize) -> Vec<(u32, Range<u32>)> {
        match ph {
            Phase::Serial(r) if t == 0 => (r.start..r.end)
                .map(|b| (b, 0..code.blocks[b as usize].len))
                .collect(),
            Phase::Serial(_) => Vec::new(),
            Phase::Parallel { offsets } => self.segs[offsets[t] as usize..offsets[t + 1] as usize]
                .iter()
                .map(|s| (s.block, s.lo..s.hi))
                .collect(),
        }
    }

    /// Check conditions 1-3 of the module docs and compute the wait lists.
    fn plan_waits(&mut self, p: &Program, code: &Code) -> bool {
        const NONE: u32 = u32::MAX;
        let n = self.threads;
        let mut writer = vec![NONE; p.slot_count as usize]; // phase * n + thread
        for (pi, ph) in self.phases.iter().enumerate() {
            for t in 0..n {
                for (b, r) in self.parts(code, ph, t) {
                    let b = &code.blocks[b as usize];
                    for k in r {
                        let mut ok = true;
                        access(
                            p,
                            b,
                            k,
                            |s| {
                                ok &= writer[s as usize] == NONE;
                                writer[s as usize] = (pi * n + t) as u32;
                            },
                            |_| {},
                        );
                        if !ok {
                            return false;
                        }
                    }
                }
            }
        }
        self.wait_off.push(0);
        // Per thread: the last phase of each other thread already waited for
        // in this settle (+1; 0 = none).
        let mut done = vec![vec![0u32; n]; n];
        for (pi, ph) in self.phases.iter().enumerate() {
            for (t, done) in done.iter_mut().enumerate() {
                let mut need = vec![0u32; n];
                let mut ok = true;
                for (b, r) in self.parts(code, ph, t) {
                    let b = &code.blocks[b as usize];
                    for k in r {
                        access(
                            p,
                            b,
                            k,
                            |_| {},
                            |s| {
                                let w = writer[s as usize];
                                if w == NONE {
                                    return;
                                }
                                let (wp, u) = (w as usize / n, w as usize % n);
                                if u != t {
                                    ok &= wp < pi;
                                    need[u] = need[u].max(wp as u32 + 1);
                                } else if wp == pi && matches!(ph, Phase::Parallel { .. }) {
                                    ok = false;
                                }
                            },
                        );
                    }
                }
                if !ok {
                    return false;
                }
                for (u, (&need, done)) in need.iter().zip(done.iter_mut()).enumerate() {
                    if need > *done {
                        self.waits.push((u as u32, need - 1));
                        *done = need;
                    }
                }
                self.wait_off.push(self.waits.len() as u32);
            }
        }
        true
    }
}

/// Spin until `c >= v`, then yield.
#[inline]
fn wait_for(c: &AtomicU64, v: u64) {
    let mut i = 0u32;
    while c.load(Ordering::Acquire) < v {
        if i < SPINS {
            spin_loop();
            i += 1;
        } else {
            std::thread::yield_now();
        }
    }
}

const SPINS: u32 = 1 << 14;
/// Spins (about 50-100 us) before an idle worker sleeps.
const IDLE_SPINS: u32 = 1 << 16;

/// Run every phase as thread `t` in the settle whose progress values start
/// above `base`, then publish `base + phases`.
///
/// # Safety
///
/// See the module docs: `s` and `env.mems` stay valid until every thread
/// has published its final progress, and the schedule passed
/// `Schedule::plan_waits`.
unsafe fn run_phases(sh: &Shared, env: &Env<'_>, s: *mut u64, t: usize, base: u64) {
    let sched = &sh.schedule;
    let blocks = &env.code.blocks;
    let n = sched.threads;
    for (pi, ph) in sched.phases.iter().enumerate() {
        let w = pi * n + t;
        for &(u, wp) in &sched.waits[sched.wait_off[w] as usize..sched.wait_off[w + 1] as usize] {
            wait_for(&sh.progress[u as usize], base + wp as u64 + 1);
        }
        let wrote = match ph {
            Phase::Serial(r) => {
                if t == 0 {
                    for b in &blocks[r.start as usize..r.end as usize] {
                        // SAFETY: only thread 0 runs serial phases; readers
                        // of its writes wait for its progress.
                        unsafe { exec(env, b, 0, b.len, s) };
                    }
                }
                t == 0
            }
            Phase::Parallel { offsets } => {
                let segs = &sched.segs[offsets[t] as usize..offsets[t + 1] as usize];
                for seg in segs {
                    // SAFETY: conditions 1-3 of the module docs.
                    unsafe { exec(env, &blocks[seg.block as usize], seg.lo, seg.hi, s) };
                }
                !segs.is_empty()
            }
        };
        if wrote {
            sh.progress[t].store(base + pi as u64 + 1, Ordering::Release);
        }
    }
    sh.progress[t].store(base + sched.phases.len() as u64, Ordering::Release);
}

#[repr(align(128))]
struct CachePadded<T>(T);

impl<T> std::ops::Deref for CachePadded<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0
    }
}

struct Shared {
    program: Arc<Program>,
    code: Arc<Code>,
    schedule: Schedule,
    threads: usize,
    /// Per thread: phases finished, counting from `base` of the current
    /// settle (see `run_phases`).
    progress: Vec<CachePadded<AtomicU64>>,
    /// Bumped by the caller to start a settle.
    epoch: AtomicU64,
    shutdown: AtomicBool,
    sleepers: AtomicUsize,
    lock: Mutex<()>,
    wake: Condvar,
    // The current settle's inputs, written before `epoch` is bumped.
    slots: AtomicPtr<u64>,
    mems: AtomicPtr<Vec<u64>>,
    mems_len: AtomicUsize,
    tick: AtomicU64,
}

/// Worker threads owned by a [`super::Sim`].
pub(super) struct Pool {
    shared: Arc<Shared>,
    handles: Vec<JoinHandle<()>>,
}

impl Pool {
    /// Start `schedule.threads - 1` workers; `None` if the schedule has no
    /// parallel phase or a thread cannot be spawned.
    pub fn new(
        program: Arc<Program>,
        code: Arc<Code>,
        schedule: Schedule,
        threads: usize,
    ) -> Option<Self> {
        if cfg!(target_arch = "wasm32") || threads < 2 || schedule.parallel_phases() == 0 {
            return None;
        }
        let shared = Arc::new(Shared {
            program,
            code,
            schedule,
            threads,
            progress: (0..threads)
                .map(|_| CachePadded(AtomicU64::new(0)))
                .collect(),
            epoch: AtomicU64::new(0),
            shutdown: AtomicBool::new(false),
            sleepers: AtomicUsize::new(0),
            lock: Mutex::new(()),
            wake: Condvar::new(),
            slots: AtomicPtr::new(std::ptr::null_mut()),
            mems: AtomicPtr::new(std::ptr::null_mut()),
            mems_len: AtomicUsize::new(0),
            tick: AtomicU64::new(0),
        });
        let mut handles = Vec::with_capacity(threads - 1);
        for t in 1..threads {
            let sh = Arc::clone(&shared);
            let h = std::thread::Builder::new()
                .name(format!("logi-sim-{t}"))
                .spawn(move || worker(&sh, t));
            match h {
                Ok(h) => handles.push(h),
                Err(_) => {
                    let pool = Pool { shared, handles };
                    drop(pool); // stops the workers already started
                    return None;
                }
            }
        }
        Some(Pool { shared, handles })
    }

    pub fn threads(&self) -> usize {
        self.shared.threads
    }

    pub fn schedule(&self) -> &Schedule {
        &self.shared.schedule
    }

    /// Settle with every thread. `slots` must have the program's
    /// `slot_count` entries.
    pub fn settle(&self, slots: &mut [u64], mems: &[Vec<u64>], tick: u64) {
        let sh = &*self.shared;
        debug_assert_eq!(slots.len(), sh.program.slot_count as usize);
        let s = slots.as_mut_ptr();
        sh.slots.store(s, Ordering::Relaxed);
        sh.mems
            .store(mems.as_ptr() as *mut Vec<u64>, Ordering::Relaxed);
        sh.mems_len.store(mems.len(), Ordering::Relaxed);
        sh.tick.store(tick, Ordering::Relaxed);
        // SeqCst pairs with the worker's `sleepers` increment and `epoch`
        // load: either it sees the new epoch or we see it sleeping.
        let epoch = sh.epoch.fetch_add(1, Ordering::SeqCst) + 1;
        if sh.sleepers.load(Ordering::SeqCst) > 0 {
            let _g = sh.lock.lock().unwrap_or_else(|e| e.into_inner());
            sh.wake.notify_all();
        }
        let env = Env {
            program: &sh.program,
            code: &sh.code,
            mems,
            tick,
        };
        let base = base(epoch, &sh.schedule);
        // SAFETY: `slots` and `mems` are borrowed until we return, which is
        // after every worker's final progress; see the module docs.
        unsafe { run_phases(sh, &env, s, 0, base) };
        let end = base + sh.schedule.phases.len() as u64;
        for c in &sh.progress[1..] {
            wait_for(c, end);
        }
    }
}

impl Drop for Pool {
    fn drop(&mut self) {
        let sh = &*self.shared;
        sh.shutdown.store(true, Ordering::SeqCst);
        sh.epoch.fetch_add(1, Ordering::SeqCst);
        {
            let _g = sh.lock.lock().unwrap_or_else(|e| e.into_inner());
            sh.wake.notify_all();
        }
        for h in self.handles.drain(..) {
            let _ = h.join();
        }
    }
}

/// Progress values of settle `epoch` lie in `base + 1..=base + phases`,
/// above every value of earlier settles.
fn base(epoch: u64, sched: &Schedule) -> u64 {
    epoch * (sched.phases.len() as u64 + 1)
}

fn worker(sh: &Shared, t: usize) {
    let mut seen = 0u64;
    loop {
        // Wait for the next settle: spin, then sleep.
        let mut i = 0u32;
        while sh.epoch.load(Ordering::Acquire) == seen && i < IDLE_SPINS {
            spin_loop();
            i += 1;
        }
        if sh.epoch.load(Ordering::Acquire) == seen {
            let mut g = sh.lock.lock().unwrap_or_else(|e| e.into_inner());
            sh.sleepers.fetch_add(1, Ordering::SeqCst);
            while sh.epoch.load(Ordering::SeqCst) == seen {
                g = sh.wake.wait(g).unwrap_or_else(|e| e.into_inner());
            }
            sh.sleepers.fetch_sub(1, Ordering::SeqCst);
        }
        seen = sh.epoch.load(Ordering::Acquire);
        let base = base(seen, &sh.schedule);
        if sh.shutdown.load(Ordering::SeqCst) {
            return;
        }
        let s = sh.slots.load(Ordering::Relaxed);
        // SAFETY: the caller of `settle` published these before bumping the
        // epoch (ordered by the SeqCst/Acquire pair) and keeps them alive
        // and unchanged until every worker published its final progress.
        let mems = unsafe {
            std::slice::from_raw_parts(
                sh.mems.load(Ordering::Relaxed) as *const Vec<u64>,
                sh.mems_len.load(Ordering::Relaxed),
            )
        };
        let env = Env {
            program: &sh.program,
            code: &sh.code,
            mems,
            tick: sh.tick.load(Ordering::Relaxed),
        };
        // SAFETY: as above.
        unsafe { run_phases(sh, &env, s, t, base) };
    }
}
