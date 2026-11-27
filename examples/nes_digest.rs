//! Per-frame state digests of a workspace, for checking that two
//! simulator builds (or two configurations of one build) compute the same
//! thing: an SMB3 (or any NES console) run from power-on, with the press
//! script below on pad 1, hashing the exported state every 100 frames.
//!
//! Each checkpoint prints `frame <f> words <hash> mems <hash>`: `words`
//! hashes every exported state register by its path, `mems` every memory
//! (RAM, VRAM, OAM, palette, CHR, PRG RAM) and the screen. The last line
//! counts the state words. Two runs agree when their `frame` lines are
//! identical; the halves tell register differences from memory ones.
//!
//! ```sh
//! nes_digest <workspace folder> [config] [frames]
//! ```
//!
//! - `config`: `interp` (`SimOptions::default`, the default), `jit`
//!   (`SimOptions::jit`), or `interactive` (`SimOptions::interactive`, the
//!   game's configuration: the JIT with adaptive specialization).
//! - `frames`: how many frames to run (default 3,000, which reaches 1-1
//!   and walks right at frame 1899).
//!
//! The press script (frame, button, frames held): Start at 451 and 657,
//! Right at 1063 (12), A at 1175, Up at 1481 (12), A at 1593, Right at
//! 1899 (200): title, the file menu and the map into World 1-1.
//!
//! Comparing a build against an older one (another worktree; there,
//! `interactive` may not exist yet, so compare `interp` or `jit`):
//!
//! ```sh
//! git worktree add ../logi-base <older commit>
//! cp examples/nes_digest.rs ../logi-base/examples/
//! (cd ../logi-base && cargo build --release --features jit --example nes_digest)
//! cargo build --release --features jit --example nes_digest
//! WS=.gatequest/workspaces/nes
//! ../logi-base/target/release/examples/nes_digest $WS interp > base.txt
//! target/release/examples/nes_digest $WS interactive > head.txt
//! diff <(grep ^frame base.txt) <(grep ^frame head.txt) && echo identical
//! ```

use std::path::Path;
use std::sync::Arc;

use gate_quest::mcp::suite;
use gate_quest::sim::netlist::design_of;
use logi_sim::compile::{Options, compile};
use logi_sim::sim::{Sim, SimOptions};

const FRAME: u64 = 89_342;
const PRESSES: &[(u64, &str, u64)] = &[
    (451, "p1_start", 6),
    (657, "p1_start", 6),
    (1063, "p1_right", 12),
    (1175, "p1_a", 6),
    (1481, "p1_up", 12),
    (1593, "p1_a", 6),
    (1899, "p1_right", 200),
];
const BUTTONS: &[&str] = &["p1_start", "p1_right", "p1_a", "p1_up"];

fn fold(h: &mut u64, x: u64) {
    *h = (*h ^ x).wrapping_mul(0x100_0000_01b3).rotate_left(29);
}

fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let folder = args
        .first()
        .ok_or("usage: nes_digest <workspace folder> [interp|jit|interactive] [frames]")?;
    let config = args.get(1).map(String::as_str).unwrap_or("interp");
    let frames: u64 = args.get(2).and_then(|v| v.parse().ok()).unwrap_or(3000);
    let design = design_of(&suite::read_workspace(Path::new(folder))?)?;
    let program = Arc::new(compile(&design, &Options::default()).map_err(|e| format!("{e:?}"))?);
    let options = match config {
        "interp" => SimOptions::default(),
        "jit" => SimOptions::jit(),
        "interactive" => SimOptions::interactive(),
        other => return Err(format!("unknown config {other}")),
    };
    let screen = program
        .mems
        .iter()
        .position(|m| m.writable && m.addr_bits == 16 && m.width == 8)
        .ok_or("no screen")? as u32;
    let mut sim = Sim::with_options(Arc::clone(&program), options);
    let mut words_n = 0;
    for f in 1..=frames {
        for b in BUTTONS {
            let on = PRESSES
                .iter()
                .any(|(from, i, c)| i == b && (*from..from + c).contains(&f));
            sim.set(b, u64::from(on));
        }
        sim.run(FRAME);
        if f % 100 == 0 || f == frames {
            let st = sim.export_state();
            let mut words: Vec<(String, u64)> = st
                .words
                .iter()
                .map(|(k, v)| (format!("{k:?}"), *v))
                .collect();
            words.sort();
            words_n = words.len();
            let mut hw = 0xcbf2_9ce4_8422_2325u64;
            for (k, v) in &words {
                for b in k.bytes() {
                    fold(&mut hw, u64::from(b));
                }
                fold(&mut hw, *v);
            }
            let mut mems: Vec<(String, &Vec<u64>)> = st
                .mems
                .iter()
                .map(|(k, v)| (format!("{k:?}"), &v.2))
                .collect();
            mems.sort();
            let mut hm = 0xcbf2_9ce4_8422_2325u64;
            for (_, m) in &mems {
                for &x in m.iter() {
                    fold(&mut hm, x);
                }
            }
            for &x in sim.mem(screen) {
                fold(&mut hm, x);
            }
            println!("frame {f} words {hw:016x} mems {hm:016x}");
        }
    }
    println!("{config}: {words_n} state words");
    Ok(())
}
