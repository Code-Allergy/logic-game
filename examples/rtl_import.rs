//! Import a Yosys JSON netlist as a workspace, laid out automatically.
//!
//! ```sh
//! yosys -p 'read_verilog top.v; synth -run :fine; opt -full; write_json top.json'
//! cargo run --example rtl_import -- examples/rtl/top.json out.json
//! cargo run --example rtl_import -- in.json out.json --v5 --top top --name "My CPU"
//! ```
//!
//! Writes save format version 6 (the hand-editable form, canonical text)
//! by default, or version 5 with `--v5`. Either can be put into a
//! workspace folder as `workspaces/<name>/manifest.json` and opened in the
//! editor. See `docs/rtl-import.md`.

use std::process::ExitCode;

use gate_quest::editor::workspace::workspace_to_json;
use gate_quest::sim::rtl::{ImportOptions, import_yosys_json};
use gate_quest::sim::source;

const USAGE: &str = "usage: rtl_import IN.json OUT.json [--v5] [--top MODULE] [--name NAME]";

fn main() -> ExitCode {
    let mut files = Vec::new();
    let mut v5 = false;
    let mut options = ImportOptions::default();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--v5" => v5 = true,
            "--top" => options.top = args.next(),
            "--name" => options.name = args.next(),
            "-h" | "--help" => {
                println!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            _ => files.push(arg),
        }
    }
    let [input, output] = files.as_slice() else {
        eprintln!("{USAGE}");
        return ExitCode::FAILURE;
    };
    let text = match std::fs::read_to_string(input) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("{input}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let import = match import_yosys_json(&text, &options) {
        Ok(i) => i,
        Err(e) => {
            eprintln!("{input}: {e}");
            return ExitCode::FAILURE;
        }
    };
    for warning in &import.warnings {
        eprintln!("warning: {warning}");
    }
    let out = if v5 {
        match workspace_to_json(&import.name, &import.workspace) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("{e}");
                return ExitCode::FAILURE;
            }
        }
    } else {
        source::to_text(&import.source)
    };
    if let Err(e) = std::fs::write(output, out) {
        eprintln!("{output}: {e}");
        return ExitCode::FAILURE;
    }
    let nodes: usize = import.source.circuits.iter().map(|c| c.nodes.len()).sum();
    println!(
        "{output}: workspace \"{}\", {} circuit(s), {nodes} node(s), version {}",
        import.name,
        import.source.circuits.len(),
        if v5 { 5 } else { 6 }
    );
    ExitCode::SUCCESS
}
