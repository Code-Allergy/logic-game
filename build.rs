use std::env;
use std::path::Path;

/// The Windows executable icon, when the packaging files are present.
const ICON_RC: &str = "build-files/windows/icon.rc";

fn main() {
    let target = env::var("TARGET").unwrap();
    // The simulator's wasm JIT (logi-sim feature `jit` on wasm32) puts the
    // functions it generates in the module's function table, which must be
    // able to grow (otherwise the interpreter runs).
    if env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("wasm32") {
        println!("cargo::rustc-link-arg-bins=--growable-table");
        println!("cargo::rustc-link-arg-examples=--growable-table");
    }
    // on windows we set our game icon as icon for the executable. The
    // packaging files are not in the repository at the moment; without
    // this check `rc.exe` fails on the missing file and the Windows build
    // with it.
    if target.contains("windows") && Path::new(ICON_RC).exists() {
        // The icon is cosmetic: skip it when no resource compiler is
        // installed, but fail the build if one is there and fails.
        embed_resource::compile(ICON_RC, embed_resource::NONE)
            .manifest_optional()
            .unwrap();
    }
}
