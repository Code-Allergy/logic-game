//! List the named nets of a workspace's compiled program whose name
//! contains a substring, with slot and width: how the `bold_*` tools find
//! their signals.
//!
//! ```sh
//! cargo run -p logi-sim --profile profiling --example bold_names -- <workspace> <substring>...
//! ```

mod bold_common;

fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let folder = args
        .first()
        .ok_or("usage: bold_names <workspace> <substring>...")?;
    let l = bold_common::load(folder)?;
    let p = &l.program;
    println!(
        "{} slots, {} state keys, {} mems",
        p.slot_count,
        p.state_keys.len(),
        p.mems.len()
    );
    let mut rows: Vec<(String, u32)> = Vec::new();
    for (&s, v) in &l.names {
        for n in v {
            if args[1..].iter().any(|a| n.contains(a.as_str())) {
                rows.push((n.clone(), s));
            }
        }
    }
    rows.sort();
    for (n, s) in rows {
        println!("{n}  slot {s}");
    }
    Ok(())
}
