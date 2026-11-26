//! On wasm32 the JIT's wasm backend (feature `jit`) puts the functions it
//! generates in the module's function table, which must be able to grow:
//! link the crate's own binaries and examples with `--growable-table`.
//! (A binary of another crate that runs the wasm JIT passes it itself, as
//! the game's build script does; without it, compiling fails and the
//! interpreter runs.)

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("wasm32") {
        println!("cargo::rustc-link-arg-bins=--growable-table");
        println!("cargo::rustc-link-arg-examples=--growable-table");
    }
}
