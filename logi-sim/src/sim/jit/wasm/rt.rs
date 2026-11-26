//! Instantiating generated modules through the host's WebAssembly API, and
//! the function-table entries that make their functions callable through
//! Rust function pointers (see the parent module).
//!
//! Everything here runs on the module's one thread: table entries are
//! recycled through a thread-local free list, and pending instantiations
//! live in a thread-local registry, so the handles (`Entries`,
//! `PendingModule`) are plain indices and stay `Send` like the rest of a
//! `Sim`.

use std::cell::RefCell;
use std::rc::Rc;

use js_sys::{Function, Object, Promise, Reflect, Uint8Array, WebAssembly};
use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast, JsValue};

use super::{Finished, FnId};
use crate::sim::jit::JitError;

thread_local! {
    /// Table entries freed by dropped modules, for reuse.
    static FREE: RefCell<Vec<u32>> = const { RefCell::new(Vec::new()) };
    /// Instantiations in flight, by `PendingModule::id`.
    static PENDING: RefCell<Vec<Option<Slot>>> = const { RefCell::new(Vec::new()) };
}

/// The export name of function `f`.
pub(super) fn name(f: FnId) -> String {
    format!("f{f}")
}

fn js_err(what: &str, e: &JsValue) -> JitError {
    let msg = e
        .dyn_ref::<js_sys::Error>()
        .map(|e| String::from(e.message()))
        .or_else(|| e.as_string())
        .unwrap_or_else(|| format!("{e:?}"));
    JitError(format!("{what}: {msg}"))
}

fn table() -> WebAssembly::Table {
    wasm_bindgen::function_table().unchecked_into()
}

/// `{ env: { memory } }`: the main module's memory.
fn imports() -> Result<Object, JitError> {
    let env = Object::new();
    Reflect::set(&env, &"memory".into(), &wasm_bindgen::memory())
        .map_err(|e| js_err("imports", &e))?;
    let imports = Object::new();
    Reflect::set(&imports, &"env".into(), &env).map_err(|e| js_err("imports", &e))?;
    Ok(imports)
}

/// Table entries holding the functions of one module; cleared and
/// recycled when dropped.
pub struct Entries(Vec<u32>);

impl Entries {
    pub(super) fn indices(&self) -> &[u32] {
        &self.0
    }
}

impl Drop for Entries {
    fn drop(&mut self) {
        let t = table();
        for &i in &self.0 {
            // The instance is collected once nothing refers to it.
            let _ = t.set_raw(i, &JsValue::NULL);
        }
        FREE.with_borrow_mut(|f| f.extend_from_slice(&self.0));
    }
}

/// Put `exports` of `instance` in the function table.
fn install(instance: &WebAssembly::Instance, exports: &[FnId]) -> Result<Entries, JitError> {
    let ex = instance.exports();
    let mut fns = Vec::with_capacity(exports.len());
    for &f in exports {
        let v = Reflect::get(&ex, &name(f).into()).map_err(|e| js_err("exports", &e))?;
        fns.push(
            v.dyn_into::<Function>()
                .map_err(|_| JitError(format!("export {f} is not a function")))?,
        );
    }
    let t = table();
    let mut idx: Vec<u32> = FREE.with_borrow_mut(|free| {
        let k = free.len().saturating_sub(fns.len());
        free.drain(k..).collect()
    });
    let more = (fns.len() - idx.len()) as u32;
    if more > 0 {
        // Fails when the table is not growable (linked without
        // `--growable-table`).
        let start = t.grow(more).map_err(|e| {
            // Back to the free list: nothing holds these.
            FREE.with_borrow_mut(|free| free.extend_from_slice(&idx));
            js_err("growing the function table", &e)
        })?;
        idx.extend(start..start + more);
    }
    let entries = Entries(idx);
    for (&i, f) in entries.0.iter().zip(&fns) {
        t.set(i, f).map_err(|e| js_err("function table", &e))?;
    }
    Ok(entries)
}

/// Compile and instantiate `bytes` synchronously, and put `exports` in the
/// table.
pub(super) fn instantiate(bytes: &[u8], exports: &[FnId]) -> Result<Entries, JitError> {
    let buf = Uint8Array::from(bytes);
    let module = WebAssembly::Module::new(&buf.into()).map_err(|e| js_err("compiling", &e))?;
    let instance = WebAssembly::Instance::new(&module, &imports()?)
        .map_err(|e| js_err("instantiating", &e))?;
    install(&instance, exports)
}

/// What an instantiation's promise settled with, once it did.
type Outcome = Rc<RefCell<Option<Result<JsValue, JsValue>>>>;
type Callback = Closure<dyn FnMut(JsValue)>;

/// An instantiation in flight: its outcome once the promise settled, and
/// the callbacks (kept alive until then).
struct Slot {
    outcome: Outcome,
    /// Its `PendingModule` was dropped: the slot goes once it settled.
    abandoned: bool,
    exports: Vec<FnId>,
    _callbacks: (Callback, Callback),
}

/// A module being compiled and instantiated asynchronously
/// ([`super::Builder::finish_async`]). Dropping it abandons the result.
pub struct PendingModule {
    id: usize,
}

/// Start compiling and instantiating `bytes`.
pub(super) fn instantiate_async(
    bytes: &[u8],
    exports: Vec<FnId>,
) -> Result<PendingModule, JitError> {
    let promise: Promise = WebAssembly::instantiate_buffer(bytes, &imports()?);
    let outcome: Outcome = Rc::new(RefCell::new(None));
    let (o1, o2) = (Rc::clone(&outcome), Rc::clone(&outcome));
    let ok = Closure::new(move |v: JsValue| {
        *o1.borrow_mut() = Some(Ok(v));
    });
    let fail = Closure::new(move |e: JsValue| {
        *o2.borrow_mut() = Some(Err(e));
    });
    let _ = promise.then2(&ok, &fail);
    let slot = Slot {
        outcome,
        abandoned: false,
        exports,
        _callbacks: (ok, fail),
    };
    let id = PENDING.with_borrow_mut(|p| {
        // Abandoned instantiations that settled: their callbacks have run.
        for s in p.iter_mut() {
            if s.as_ref()
                .is_some_and(|s| s.abandoned && s.outcome.borrow().is_some())
            {
                *s = None;
            }
        }
        match p.iter().position(Option::is_none) {
            Some(i) => {
                p[i] = Some(slot);
                i
            }
            None => {
                p.push(Some(slot));
                p.len() - 1
            }
        }
    });
    Ok(PendingModule { id })
}

impl PendingModule {
    /// The finished module once instantiated (its functions in the table),
    /// or why it failed; `None` while it is still in flight. Returns
    /// `Some` once.
    pub(in crate::sim::jit) fn poll(&mut self) -> Option<Result<Finished, JitError>> {
        let done = PENDING.with_borrow(|p| {
            p.get(self.id)
                .and_then(Option::as_ref)
                .is_none_or(|s| s.outcome.borrow().is_some())
        });
        if !done {
            return None;
        }
        // Settled: the callbacks have run (and are not running now).
        let slot = PENDING.with_borrow_mut(|p| p.get_mut(self.id).and_then(Option::take))?;
        let outcome = slot.outcome.borrow_mut().take().expect("settled");
        Some(match outcome {
            Ok(v) => Reflect::get(&v, &"instance".into())
                .map_err(|e| js_err("instance", &e))
                .and_then(|i| {
                    let instance: WebAssembly::Instance = i.unchecked_into();
                    install(&instance, &slot.exports)
                })
                .map(|entries| Finished::new(&slot.exports, entries)),
            Err(e) => Err(js_err("instantiating", &e)),
        })
    }
}

impl Drop for PendingModule {
    fn drop(&mut self) {
        // The callbacks must outlive the promise: a settled slot goes now,
        // one in flight when the next instantiation starts after it
        // settled.
        PENDING.with_borrow_mut(|p| {
            if let Some(s) = p.get_mut(self.id) {
                match s {
                    Some(x) if x.outcome.borrow().is_none() => x.abandoned = true,
                    _ => *s = None,
                }
            }
        });
    }
}

/// Entries in the function table (for the tests).
#[cfg(feature = "selftest")]
pub(super) fn table_len() -> u32 {
    table().length()
}
