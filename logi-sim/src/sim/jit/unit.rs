//! One body compiled on its own (issue #117, adaptive specialization):
//! a [`JitUnit`] owns a `JITModule` holding just that body's settle and
//! commit functions, and frees its code when dropped. A [`JitProgram`]
//! keeps every body in one module that lives and dies as a whole; a unit
//! can come and go while the others keep running (specialized bodies are
//! compiled when a key turns hot and freed when it is evicted).
//!
//! The emission is the program's: both call `compile_body`, so a unit
//! computes exactly what the same body does inside a `JitProgram`.
//!
//! # Lifetimes
//!
//! The function pointers are private and used only through `&self`, so
//! they cannot outlive the module that owns the code. The code keeps no
//! pointer between calls: slots, memories and write counters are passed to
//! every call. The caller must hand it the buffers of the `Sim` it was
//! compiled for, and refresh a cached memory-pointer table whenever a
//! memory may have been reallocated (`Sim::mems_touched`); the checked
//! entry points take the buffers themselves and build the table per call.
//!
//! [`JitProgram`]: super::JitProgram

use std::sync::Mutex;

use cranelift_frontend::FunctionBuilderContext;
use cranelift_module::Module;
use smallvec::SmallVec;

use super::{
    BodyRef, CommitFn, JitBody, JitError, JitSuper, MAX_BODY, Owned, Part, SettleFn, SuperFn,
    SuperInfo, SuperRef, compile_body, define_superstep, new_module,
};
use crate::compile::Commit;
use crate::compile::Program;
use crate::sim::{Sim, parts};

/// One compiled body with its own code, freed on drop. See the module
/// documentation.
pub struct JitUnit {
    /// Function pointers into `_module`.
    body: JitBody,
    instrs: usize,
    code_bytes: usize,
    /// What the code assumes about the buffers it is called with.
    slot_count: usize,
    mem_lens: Vec<usize>,
    /// Owns the code (behind a `Mutex` only so that the unit is `Sync`; it
    /// is never locked). Declared last: dropped after nothing else needs it.
    _module: Mutex<Owned>,
}

// Units are built on a compile thread and installed on the `Sim`'s, like
// a `JitProgram`.
const _: fn() = || {
    fn shareable<T: Send + Sync>() {}
    shareable::<JitUnit>();
};

/// Compile one body of `p` into a module of its own. Fails for a program
/// or body that does not pass `Sim`'s checks (indices out of range), for a
/// body that is not `used` or is larger than [`super::MAX_BODY`] (the
/// interpreter runs those), and when Cranelift fails.
pub fn compile_unit(p: &Program, body: BodyRef<'_>) -> Result<JitUnit, JitError> {
    let err = |e: &dyn std::fmt::Display| JitError(e.to_string());
    // The checks `Sim` makes before it runs a body: every slot, memory and
    // guard index in range. The safe entry points rely on them (they only
    // check the buffers' sizes against the program's).
    crate::sim::check(p).map_err(|e| JitError(format!("bad program: {}", e.0)))?;
    crate::sim::check_body(
        p,
        body.blocks,
        body.instrs,
        body.imms,
        body.commits,
        body.guards,
    )
    .map_err(|e| JitError(format!("bad body: {}", e.0)))?;
    let mut owned = new_module()?;
    let module = owned.0.as_mut().expect("just created");
    let mut ctx = module.make_context();
    let mut fctx = FunctionBuilderContext::new();
    let (mut n, mut instrs, mut bytes) = (0u32, 0usize, 0usize);
    let ids = compile_body(
        module,
        &mut ctx,
        &mut fctx,
        p,
        &body,
        &mut n,
        &mut instrs,
        &mut bytes,
    )?
    .ok_or_else(|| JitError("body not compiled (unused or too large)".into()))?;
    module.finalize_definitions().map_err(|e| err(&e))?;
    let module = &*module;
    // SAFETY: each id was defined with the matching signature, and the code
    // stays alive as long as `owned`, which the result owns.
    let settle = |id| unsafe {
        std::mem::transmute::<*const u8, SettleFn>(module.get_finalized_function(id))
    };
    let commit = |id| unsafe {
        std::mem::transmute::<*const u8, CommitFn>(module.get_finalized_function(id))
    };
    let (s, c) = ids;
    let body = JitBody {
        settle: s.into_iter().map(settle).collect(),
        commit: c.map(|c| c.into_iter().map(commit).collect()),
    };
    Ok(JitUnit {
        body,
        instrs,
        code_bytes: bytes,
        slot_count: p.slot_count as usize,
        mem_lens: p.mems.iter().map(|m| 1usize << m.addr_bits).collect(),
        _module: Mutex::new(owned),
    })
}

impl JitUnit {
    /// The compiled functions (for `Sim`, which calls them with its
    /// buffers; the borrow keeps them from outliving the unit).
    pub(crate) fn body(&self) -> &JitBody {
        &self.body
    }

    /// Settle chunks.
    pub fn settle_chunks(&self) -> usize {
        self.body.settle_chunks()
    }

    /// Commit chunks (`None`: the interpreter commits this body).
    pub fn commit_chunks(&self) -> Option<usize> {
        self.body.commit_chunks()
    }

    /// Instructions compiled (packed words count one each).
    pub fn instrs(&self) -> usize {
        self.instrs
    }

    /// Bytes of native code, every function included: what a code budget
    /// counts.
    pub fn code_bytes(&self) -> usize {
        self.code_bytes
    }

    /// Run the settle on `slots` and `mems` (a `Sim`'s buffers), at `tick`.
    ///
    /// # Panics
    ///
    /// If the buffers are not the sizes the body was compiled for.
    pub fn settle(&self, slots: &mut [u64], mems: &mut [Vec<u64>], tick: u64) {
        let ptrs = self.pointers(slots, mems);
        // SAFETY: the sizes were checked, and the body passed `check`.
        unsafe { self.settle_raw(slots.as_mut_ptr(), ptrs.as_ptr(), tick) };
    }

    /// Run the commit; `false` (and nothing done) when the interpreter
    /// must commit this body (a `MemClear`). `mem_writes` has one counter
    /// per memory, incremented for every write taken.
    ///
    /// # Panics
    ///
    /// As [`JitUnit::settle`], and if `mem_writes` is not one per memory.
    pub fn commit(&self, slots: &mut [u64], mems: &mut [Vec<u64>], mem_writes: &mut [u64]) -> bool {
        assert_eq!(
            mem_writes.len(),
            self.mem_lens.len(),
            "one counter per memory"
        );
        let ptrs = self.pointers(slots, mems);
        // SAFETY: as in `settle`; `mem_writes` was checked.
        unsafe { self.commit_raw(slots.as_mut_ptr(), ptrs.as_ptr(), mem_writes.as_mut_ptr()) }
    }

    /// [`JitUnit::settle`] on raw pointers, for a caller that keeps a
    /// memory-pointer table (`Sim::mem_ptrs`).
    ///
    /// # Safety
    ///
    /// `slots` must point to at least `slot_count` words, `mems` to one
    /// pointer per memory of the program, each to `2^addr_bits` words, all
    /// valid and unaliased for the call: the buffers of the `Sim` the
    /// program was checked for.
    pub unsafe fn settle_raw(&self, slots: *mut u64, mems: *const *mut u64, tick: u64) {
        for f in &self.body.settle {
            // SAFETY: by the contract above; the code is alive (`&self`).
            unsafe { f(slots, mems, tick) };
        }
    }

    /// [`JitUnit::commit`] on raw pointers.
    ///
    /// # Safety
    ///
    /// As [`JitUnit::settle_raw`], and `mem_writes` must point to one
    /// counter per memory.
    pub unsafe fn commit_raw(
        &self,
        slots: *mut u64,
        mems: *const *mut u64,
        mem_writes: *mut u64,
    ) -> bool {
        let Some(fs) = &self.body.commit else {
            return false;
        };
        for f in fs {
            // SAFETY: as in `settle_raw`.
            unsafe { f(slots, mems, mem_writes) };
        }
        true
    }

    /// The memory-pointer table for `mems`, after checking every size.
    fn pointers(&self, slots: &[u64], mems: &mut [Vec<u64>]) -> SmallVec<[*mut u64; 8]> {
        assert!(slots.len() >= self.slot_count, "slots of another program");
        assert_eq!(
            mems.len(),
            self.mem_lens.len(),
            "memories of another program"
        );
        mems.iter_mut()
            .zip(&self.mem_lens)
            .map(|(m, &len)| {
                assert_eq!(m.len(), len, "memory of another program");
                m.as_mut_ptr()
            })
            .collect()
    }
}

/// A superstep compiled on its own (`Sim`'s superstep when the layout's
/// native code has none: an automatic plan that keys no phase), freed on
/// drop like a [`JitUnit`].
pub struct SuperUnit {
    sup: JitSuper,
    /// Owns the code; declared last.
    _module: Mutex<Owned>,
}

const _: fn() = || {
    fn shareable<T: Send + Sync>() {}
    shareable::<SuperUnit>();
};

/// Compile superstep `s` over `bodies` of `p` into a module of its own.
/// Fails for a program or body that does not pass `Sim`'s checks, for a
/// part naming a body that is missing, unused, larger than [`MAX_BODY`]
/// or has a `MemClear` commit, and when Cranelift fails.
pub fn compile_superstep(
    p: &Program,
    bodies: &[BodyRef<'_>],
    s: &SuperRef<'_>,
) -> Result<SuperUnit, JitError> {
    let err = |e: &dyn std::fmt::Display| JitError(e.to_string());
    crate::sim::check(p).map_err(|e| JitError(format!("bad program: {}", e.0)))?;
    for part in s.parts {
        let (Part::Commit { body } | Part::Settle { body, .. }) = *part;
        let b = bodies
            .get(body)
            .ok_or_else(|| JitError(format!("no body {body}")))?;
        let items: usize = b.blocks.iter().map(|x| x.len as usize).sum();
        if !b.used
            || items > MAX_BODY
            || b.commits
                .iter()
                .any(|c| matches!(c, Commit::MemClear { .. }))
        {
            return Err(JitError(format!("body {body} cannot be in a superstep")));
        }
        crate::sim::check_body(p, b.blocks, b.instrs, b.imms, b.commits, b.guards)
            .map_err(|e| JitError(format!("bad body: {}", e.0)))?;
    }
    if s.live_end.iter().any(|&x| x >= p.slot_count) {
        return Err(JitError("bad live slot".into()));
    }
    let mut owned = new_module()?;
    let module = owned.0.as_mut().expect("just created");
    let mut ctx = module.make_context();
    let mut fctx = FunctionBuilderContext::new();
    let mut n = 0u32;
    let (id, info) = define_superstep(module, &mut ctx, &mut fctx, p, bodies, s, &mut n)?;
    module.finalize_definitions().map_err(|e| err(&e))?;
    // SAFETY: `id` was defined with the `SuperFn` signature, and the code
    // stays alive as long as `owned`, which the result owns.
    let entry =
        unsafe { std::mem::transmute::<*const u8, SuperFn>(module.get_finalized_function(id)) };
    Ok(SuperUnit {
        sup: JitSuper { entry, info },
        _module: Mutex::new(owned),
    })
}

impl SuperUnit {
    /// The superstep (for `Sim`, which calls it with its buffers).
    pub(crate) fn superstep(&self) -> &JitSuper {
        &self.sup
    }

    /// What it is made of.
    pub fn info(&self) -> &SuperInfo {
        &self.sup.info
    }
}

impl Sim {
    /// The bodies of this `Sim`'s layout, numbered as its runs (0 the
    /// program's own, `k` `runs[k - 1]`): what [`Sim::compile_jit`]
    /// compiles. Bodies installed at run time ([`Sim::install_spec`])
    /// have units of their own and are not included.
    pub(crate) fn body_refs(&self) -> Vec<BodyRef<'_>> {
        let p = &*self.program;
        let base = self.spec.base;
        let in_full = |k: usize| self.full.iter().any(|&f| f as usize == k);
        // Specialized bodies, fallbacks and prefixes of the layout's
        // tables run too; a prefix's results are read by the key and the
        // body after it, so all of them are stored (not `fused_only`).
        let mut in_spec = vec![false; base + 1];
        let mut is_prefix = vec![false; base + 1];
        for (r, prefix) in self.spec.base_runs() {
            in_spec[r as usize] = true;
            is_prefix[r as usize] |= prefix;
        }
        let used =
            |k: usize| in_full(k) || in_spec[k] || self.fused.iter().any(|&f| f as usize == k);
        let mut out = vec![BodyRef {
            blocks: &p.blocks,
            instrs: &p.instrs,
            imms: &p.imms,
            commits: &p.commits,
            guards: &[],
            used: used(0),
            fused_only: false,
        }];
        for (k, r) in self.runs[..base].iter().enumerate() {
            let (blocks, instrs, imms) = parts(p, r.body.as_ref());
            out.push(BodyRef {
                blocks,
                instrs,
                imms,
                commits: r.body.as_ref().map_or(&p.commits[..], |b| &b.commits[..]),
                guards: r.body.as_ref().map_or(&[][..], |b| &b.guards[..]),
                used: used(k + 1),
                fused_only: !in_full(k + 1) && !is_prefix[k + 1],
            });
        }
        out
    }

    /// Bodies of this `Sim`'s layout: 0 the program's own, `k`
    /// `runs[k - 1]` (not those installed at run time).
    pub fn body_count(&self) -> usize {
        self.spec.base + 1
    }

    /// Body `run` as a [`JitUnit`] of its own (`None`: not used, or too
    /// large for native code). Independent of the `Sim`'s state, like
    /// [`Sim::compile_jit`].
    pub fn compile_unit_of(&self, run: usize) -> Option<Result<JitUnit, JitError>> {
        let b = self.body_refs().into_iter().nth(run)?;
        let items: usize = b.blocks.iter().map(|x| x.len as usize).sum();
        (b.used && items <= super::MAX_BODY).then(|| compile_unit(&self.program, b))
    }
}
