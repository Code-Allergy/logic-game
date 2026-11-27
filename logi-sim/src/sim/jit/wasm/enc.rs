//! A WebAssembly binary encoder for the few instructions the wasm backend
//! emits: function bodies over `i32`/`i64` locals, and a module of such
//! functions that imports one memory and exports some of them.
//!
//! See the WebAssembly core specification, "Binary Format": every integer
//! is LEB128, a section is its id and its byte length, and a function body
//! is its local declarations, its instructions and `end`.

/// Value types.
pub const I32: u8 = 0x7F;
pub const I64: u8 = 0x7E;

/// Opcodes without immediates.
pub mod op {
    pub const ELSE: u8 = 0x05;
    pub const END: u8 = 0x0B;
    pub const SELECT: u8 = 0x1B;
    pub const I32_EQZ: u8 = 0x45;
    pub const I64_EQZ: u8 = 0x50;
    pub const I64_EQ: u8 = 0x51;
    pub const I64_LT_U: u8 = 0x54;
    pub const I32_ADD: u8 = 0x6A;
    pub const I32_AND: u8 = 0x71;
    pub const I32_OR: u8 = 0x72;
    pub const I32_SHL: u8 = 0x74;
    pub const I64_POPCNT: u8 = 0x7B;
    pub const I64_ADD: u8 = 0x7C;
    pub const I64_SUB: u8 = 0x7D;
    pub const I64_REM_U: u8 = 0x82;
    pub const I64_AND: u8 = 0x83;
    pub const I64_OR: u8 = 0x84;
    pub const I64_XOR: u8 = 0x85;
    pub const I64_SHL: u8 = 0x86;
    pub const I64_SHR_U: u8 = 0x88;
    pub const I32_WRAP_I64: u8 = 0xA7;
    pub const I64_EXTEND_I32_U: u8 = 0xAD;
}

// Deliberate bugs for the tests to catch (0: none): 1, `i64.and` is
// encoded as `i64.or`; 2, a signed constant ends without its sign bit
// (64 encodes as -64).
#[cfg(any(test, feature = "selftest"))]
thread_local! {
    pub(crate) static MUTATION: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
}

pub fn uleb(out: &mut Vec<u8>, mut v: u64) {
    loop {
        let b = (v & 0x7F) as u8;
        v >>= 7;
        if v == 0 {
            out.push(b);
            return;
        }
        out.push(b | 0x80);
    }
}

pub fn sleb(out: &mut Vec<u8>, mut v: i64) {
    loop {
        let b = (v & 0x7F) as u8;
        v >>= 7;
        let done = (v == 0 && b & 0x40 == 0) || (v == -1 && b & 0x40 != 0);
        #[cfg(any(test, feature = "selftest"))]
        let done = done || (MUTATION.get() == 2 && (v == 0 || v == -1));
        if done {
            out.push(b);
            return;
        }
        out.push(b | 0x80);
    }
}

fn name(out: &mut Vec<u8>, s: &str) {
    uleb(out, s.len() as u64);
    out.extend_from_slice(s.as_bytes());
}

fn section(out: &mut Vec<u8>, id: u8, body: &[u8]) {
    out.push(id);
    uleb(out, body.len() as u64);
    out.extend_from_slice(body);
}

/// One function being built: parameters, then locals in the order they
/// were allocated, then code.
pub struct Func {
    params: u32,
    locals: Vec<u8>,
    code: Vec<u8>,
}

impl Func {
    pub fn new(params: u32) -> Self {
        Func {
            params,
            locals: Vec::new(),
            code: Vec::with_capacity(1024),
        }
    }

    /// A new local of type `ty` (zero until set).
    pub fn local(&mut self, ty: u8) -> u32 {
        self.locals.push(ty);
        self.params + self.locals.len() as u32 - 1
    }

    /// An instruction without immediates (see [`op`]).
    pub fn op(&mut self, b: u8) {
        #[cfg(any(test, feature = "selftest"))]
        let b = if b == op::I64_AND && MUTATION.get() == 1 {
            op::I64_OR
        } else {
            b
        };
        self.code.push(b);
    }

    fn op_u(&mut self, b: u8, v: u32) {
        self.code.push(b);
        uleb(&mut self.code, u64::from(v));
    }

    pub fn get(&mut self, l: u32) {
        self.op_u(0x20, l);
    }

    pub fn set(&mut self, l: u32) {
        self.op_u(0x21, l);
    }

    pub fn call(&mut self, f: u32) {
        self.op_u(0x10, f);
    }

    pub fn i64_const(&mut self, v: i64) {
        self.code.push(0x42);
        sleb(&mut self.code, v);
    }

    pub fn i32_const(&mut self, v: i32) {
        self.code.push(0x41);
        sleb(&mut self.code, i64::from(v));
    }

    fn mem(&mut self, b: u8, align: u32, offset: u32) {
        self.code.push(b);
        uleb(&mut self.code, u64::from(align));
        uleb(&mut self.code, u64::from(offset));
    }

    /// `i64.load` at the address on the stack plus `offset`.
    pub fn i64_load(&mut self, offset: u32) {
        self.mem(0x29, 3, offset);
    }

    /// `i64.store` of the value on the stack to the address under it plus
    /// `offset`.
    pub fn i64_store(&mut self, offset: u32) {
        self.mem(0x37, 3, offset);
    }

    pub fn i32_load(&mut self, offset: u32) {
        self.mem(0x28, 2, offset);
    }

    /// `if` with no result, on the `i32` on the stack.
    pub fn if_(&mut self) {
        self.code.extend_from_slice(&[0x04, 0x40]);
    }

    /// The encoded body (locals, code, `end`).
    pub fn finish(mut self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.code.len() + 16);
        // Runs of one type, in local order.
        let mut runs: Vec<(u32, u8)> = Vec::new();
        for &t in &self.locals {
            match runs.last_mut() {
                Some((n, ty)) if *ty == t => *n += 1,
                _ => runs.push((1, t)),
            }
        }
        uleb(&mut out, runs.len() as u64);
        for (n, t) in runs {
            uleb(&mut out, u64::from(n));
            out.push(t);
        }
        self.code.push(op::END);
        out.append(&mut self.code);
        out
    }
}

/// A module: `types` (parameter lists, no results), the functions (type
/// index and encoded body), and the exports (name, function index). It
/// imports `env.memory`.
pub fn module(types: &[&[u8]], funcs: &[(u32, Vec<u8>)], exports: &[(String, u32)]) -> Vec<u8> {
    let mut out = vec![0x00, 0x61, 0x73, 0x6D, 0x01, 0x00, 0x00, 0x00];
    let mut s = Vec::new();
    uleb(&mut s, types.len() as u64);
    for params in types {
        s.push(0x60);
        uleb(&mut s, params.len() as u64);
        s.extend_from_slice(params);
        s.push(0);
    }
    section(&mut out, 1, &s);
    s.clear();
    // Import section: `env.memory`, a memory of at least 0 pages.
    uleb(&mut s, 1);
    name(&mut s, "env");
    name(&mut s, "memory");
    s.extend_from_slice(&[0x02, 0x00, 0x00]);
    section(&mut out, 2, &s);
    s.clear();
    uleb(&mut s, funcs.len() as u64);
    for (t, _) in funcs {
        uleb(&mut s, u64::from(*t));
    }
    section(&mut out, 3, &s);
    s.clear();
    uleb(&mut s, exports.len() as u64);
    for (n, f) in exports {
        name(&mut s, n);
        s.push(0x00);
        uleb(&mut s, u64::from(*f));
    }
    section(&mut out, 7, &s);
    s.clear();
    uleb(&mut s, funcs.len() as u64);
    for (_, body) in funcs {
        uleb(&mut s, body.len() as u64);
        s.extend_from_slice(body);
    }
    section(&mut out, 10, &s);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leb128_matches_the_specification() {
        let u = |v| {
            let mut o = Vec::new();
            uleb(&mut o, v);
            o
        };
        let s = |v| {
            let mut o = Vec::new();
            sleb(&mut o, v);
            o
        };
        assert_eq!(u(0), [0]);
        assert_eq!(u(127), [0x7F]);
        assert_eq!(u(128), [0x80, 0x01]);
        assert_eq!(u(624_485), [0xE5, 0x8E, 0x26]);
        assert_eq!(s(0), [0]);
        assert_eq!(s(63), [0x3F]);
        assert_eq!(s(64), [0xC0, 0x00]);
        assert_eq!(s(-1), [0x7F]);
        assert_eq!(s(-64), [0x40]);
        assert_eq!(s(-65), [0xBF, 0x7F]);
        assert_eq!(s(-123_456), [0xC0, 0xBB, 0x78]);
        assert_eq!(s(i64::MIN).len(), 10);
        assert_eq!(s(i64::MAX).len(), 10);
    }

    #[test]
    fn locals_are_declared_in_runs() {
        let mut f = Func::new(2);
        assert_eq!(f.local(I64), 2);
        assert_eq!(f.local(I64), 3);
        assert_eq!(f.local(I32), 4);
        assert_eq!(f.local(I64), 5);
        assert_eq!(f.finish(), [3, 2, I64, 1, I32, 1, I64, op::END]);
    }
}
