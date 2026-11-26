//! Measurement hooks for experiments (off unless asked for, at no cost
//! to the code generated otherwise):
//!
//! - `LOGI_JIT_COUNT=1` (read once): the generated code counts its own
//!   slot loads, slot stores and function entries in [`COUNTS`] (an
//!   increment of a global per access, so the code runs slower and its
//!   registers are allocated differently; only the counts are meant).
//! - `LOGI_PERF_MAP=dir` (read once): every compiled function is listed
//!   in `/tmp/perf-<pid>.map` (for `perf report`), and its machine code
//!   written to `dir/<address>.bin` (for an instruction-level breakdown).

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

use cranelift_codegen::ir::types::I64;
use cranelift_codegen::ir::{InstBuilder, MemFlagsData};
use cranelift_frontend::FunctionBuilder;
use cranelift_module::FuncId;

/// Counters: slot loads, slot stores, settle entries, commit entries,
/// superstep chunk entries.
pub static COUNTS: [AtomicU64; 5] = [const { AtomicU64::new(0) }; 5];
pub const LOADS: usize = 0;
pub const STORES: usize = 1;
pub const SETTLES: usize = 2;
pub const COMMITS: usize = 3;
pub const SUPERS: usize = 4;

/// Whether the generated code counts (`LOGI_JIT_COUNT`).
pub fn counting() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("LOGI_JIT_COUNT").is_some_and(|v| v != "0"))
}

/// Emit `COUNTS[k] += 1` when counting.
pub(super) fn count(b: &mut FunctionBuilder<'_>, k: usize) {
    if !counting() {
        return;
    }
    let at = b.ins().iconst(I64, COUNTS[k].as_ptr() as i64);
    let n = b.ins().load(I64, MemFlagsData::trusted(), at, 0);
    let n = b.ins().iadd_imm_u(n, 1);
    b.ins().store(MemFlagsData::trusted(), n, at, 0);
}

/// Snapshot and reset of the counts.
pub fn take() -> [u64; 5] {
    std::array::from_fn(|k| COUNTS[k].swap(0, Ordering::Relaxed))
}

fn perf_dir() -> Option<&'static str> {
    static DIR: OnceLock<Option<String>> = OnceLock::new();
    DIR.get_or_init(|| std::env::var("LOGI_PERF_MAP").ok())
        .as_deref()
}

thread_local! {
    /// Functions defined on this thread and not yet finalized: id, size,
    /// name.
    static PENDING: std::cell::RefCell<Vec<(FuncId, usize, String)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Note a function just defined (when mapping).
pub(super) fn defined(id: FuncId, size: usize, name: &str) {
    if perf_dir().is_some() {
        PENDING.with(|p| p.borrow_mut().push((id, size, name.to_string())));
    }
}

/// After `finalize_definitions`: list the pending functions of `module`.
pub(super) fn finalized(module: &cranelift_jit::JITModule) {
    let Some(dir) = perf_dir() else {
        return;
    };
    use std::io::Write;
    let pending = PENDING.with(|p| std::mem::take(&mut *p.borrow_mut()));
    let path = format!("/tmp/perf-{}.map", std::process::id());
    let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    else {
        return;
    };
    for (id, size, name) in pending {
        let at = module.get_finalized_function(id);
        let _ = writeln!(f, "{:x} {:x} {name}", at as usize, size);
        // SAFETY: the function's code is `size` bytes at `at`, finalized.
        let code = unsafe { std::slice::from_raw_parts(at, size) };
        let _ = std::fs::write(format!("{dir}/{:x}.bin", at as usize), code);
    }
}

/// Items per settle chunk: [`super::CHUNK`], or `LOGI_JIT_CHUNK` (read
/// once) for sweeps.
pub fn chunk() -> usize {
    static N: OnceLock<usize> = OnceLock::new();
    *N.get_or_init(|| {
        std::env::var("LOGI_JIT_CHUNK")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(super::CHUNK)
    })
}
