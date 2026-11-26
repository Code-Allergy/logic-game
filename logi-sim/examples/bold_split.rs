//! Measurements behind `docs/bold-ideas.md`, idea 3 (how cleanly the NES
//! circuit splits into a CPU side, a PPU side and the mapper) and the
//! cost side of idea 4.
//!
//! ```sh
//! cargo run -p logi-sim --profile profiling --features jit --example bold_split -- \
//!     <workspace> [--ticks 3000000] [--no-run] [--warm 9000000] [--sample 180000]
//! ```
//!
//! 1. **Crossing signals.** Every root-level state element or chip is
//!    assigned to a side (CPU: `cpu6502`, DMA, pads, RAM, bus latch,
//!    clock divider; PPU: `ppu_ref`, `ppu_timing`, palette, VRAM, screen;
//!    mapper: `cart_*`); stateless root glue (buses, decoders, splits) is
//!    transparent. A crossing signal is a pair (output pin of one side's
//!    chip, input pin of another's) connected through glue; its width is
//!    the narrower of the two pins. The table is by direction.
//! 2. **State size per side**: state words and bits, writable memory
//!    words (`Sim` keeps one `u64` per memory word, whatever its width).
//! 3. **Op share per side** of the compiled program (default options):
//!    each instruction and commit of every clock-phase variant is
//!    attributed to the side whose chip owns the slot it writes (the
//!    deepest instance among the nets sharing the slot); glue and slots
//!    no authored net names are "unattributed".
//! 4. **Throughput with a side removed** (unless `--no-run`): the same
//!    root with every chip of one side deleted (its sinks read 0),
//!    compiled with default options, run with the JIT. This is not a
//!    clean cost split (the remaining side sees zero inputs and may idle:
//!    the CPU polls forever, the PPU never renders), so it is reported
//!    next to the op counts, not instead of them.
//! 5. **Snapshot cost**: copying the PPU side's state words and memories
//!    (the cost of a raw snapshot), timed.
//!
//! Nothing here changes the simulator.

mod bold_common;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

use bold_common::Loaded;
use logi_sim::compile::{Commit, NodePath, Options, Program, StateLoc, compile};
use logi_sim::netlist::{DefId, Design, NodeId, NodeKind};
use logi_sim::sim::{Sim, SimOptions};

const SIDES: [&str; 3] = ["CPU", "PPU", "mapper"];

/// Side of a root-level node by its group name (`Loaded::group_of`).
fn side_of_group(g: &str) -> Option<usize> {
    match g {
        "cpu6502" | "oam_dma_ref" | "joypad" | "dffe" | "reg8" | "por" | "div3" | "Ram<-mux2x8" => {
            Some(0)
        }
        "ppu_ref" | "ppu_timing" | "nes_palette" | "Screen" | "Ram<-ppu_ref" => Some(1),
        "cart_mmc3" | "cart_nrom" | "cart_mmc1" | "cart_cnrom" => Some(2),
        _ => None,
    }
}

fn node_side(l: &Loaded, node: NodeId) -> Option<usize> {
    side_of_group(&l.group_of(&NodePath {
        instances: Vec::new(),
        node,
    }))
}

fn port_name(d: &Design, def: DefId, node: NodeId, pin: usize, out: bool) -> String {
    let Ok(parent) = d.def(def) else {
        return format!("pin{pin}");
    };
    if let Some(NodeKind::Instance(child)) = parent.nodes.get(node).map(|n| &n.kind)
        && let Ok(c) = d.def(*child)
    {
        let ports = if out { &c.outputs } else { &c.inputs };
        if let Some(n) = ports.get(pin).and_then(|&p| c.name_of(p)) {
            return format!("{}.{}", c.name, n);
        }
        return format!("{}.#{pin}", c.name);
    }
    format!("node.#{pin}")
}

fn crossings(l: &Loaded) {
    let d = &l.ws.design;
    let root = d.root;
    let def = d.def(root).expect("root");
    let sides: HashMap<NodeId, usize> = def
        .nodes
        .keys()
        .filter_map(|n| node_side(l, n).map(|s| (n, s)))
        .collect();
    // (src node, src pin, dst node, dst pin) -> width
    let mut pairs: BTreeMap<(usize, usize, String, String), u32> = BTreeMap::new();
    let mut seen_pairs: HashSet<(NodeId, u8, NodeId, u8)> = HashSet::new();
    // Distinct destination pins per direction: what an interface between
    // the sides would have to carry (the pair list counts a bus once per
    // driver, and the CPU and the DMA unit share the buses through muxes).
    let mut dst_pins: HashMap<(usize, usize), HashMap<(NodeId, u8), u32>> = HashMap::new();
    for (&src, &ss) in &sides {
        let node = &def.nodes[src];
        for (pin, &net) in node.outputs.iter().enumerate() {
            let sw = u32::from(def.nets[net].width);
            let mut stack = vec![net];
            let mut seen_nets: HashSet<_> = HashSet::new();
            while let Some(n) = stack.pop() {
                if !seen_nets.insert(n) {
                    continue;
                }
                for (sink, spin) in def.sinks(n) {
                    if let Some(&ds) = sides.get(&sink) {
                        if !seen_pairs.insert((src, pin as u8, sink, spin)) {
                            continue;
                        }
                        let dw = d
                            .pin_widths(root, sink)
                            .ok()
                            .and_then(|(ins, _)| ins.get(spin as usize).copied())
                            .map_or(1, u32::from);
                        if ds != ss {
                            dst_pins
                                .entry((ss, ds))
                                .or_default()
                                .insert((sink, spin), dw);
                            let key = (
                                ss,
                                ds,
                                port_name(d, root, src, pin, true),
                                port_name(d, root, sink, spin as usize, false),
                            );
                            *pairs.entry(key).or_default() += sw.min(dw);
                        }
                    } else {
                        // Glue: through to every output.
                        stack.extend(def.nodes[sink].outputs.iter().copied());
                    }
                }
            }
        }
    }
    println!("### Signals crossing between sides (root level)\n");
    println!(
        "| from -> to | pin pairs | bits over pairs | distinct destination pins | distinct destination bits | pairs (output pin -> input pin, bits) |"
    );
    println!("|---|---:|---:|---:|---:|---|");
    for a in 0..3 {
        for b in 0..3 {
            if a == b {
                continue;
            }
            let rows: Vec<_> = pairs.iter().filter(|(k, _)| k.0 == a && k.1 == b).collect();
            if rows.is_empty() {
                continue;
            }
            let bits: u32 = rows.iter().map(|(_, w)| *w).sum();
            let list = rows
                .iter()
                .map(|(k, w)| format!("{} -> {} ({w})", k.2, k.3))
                .collect::<Vec<_>>()
                .join("; ");
            let dp = &dst_pins[&(a, b)];
            println!(
                "| {} -> {} | {} | {bits} | {} | {} | {list} |",
                SIDES[a],
                SIDES[b],
                rows.len(),
                dp.len(),
                dp.values().sum::<u32>()
            );
        }
    }
    println!();
}

/// The signals between the sub-chips of the PPU (`ppu_ref`): the same
/// analysis as [`crossings`] one level down, every sub-chip its own side.
fn ppu_internal(l: &Loaded) {
    let d = &l.ws.design;
    let root = d.def(d.root).expect("root");
    let Some(ppu_def) = root.nodes.values().find_map(|n| match n.kind {
        NodeKind::Instance(c) if l.ws.def_names.get(&c).map(String::as_str) == Some("ppu_ref") => {
            Some(c)
        }
        _ => None,
    }) else {
        return;
    };
    let def = d.def(ppu_def).expect("ppu_ref");
    let name_of = |n: NodeId| -> Option<String> {
        match def.nodes.get(n)?.kind {
            NodeKind::Instance(c) => l.ws.def_names.get(&c).cloned(),
            _ => None,
        }
    };
    let chips: Vec<NodeId> = def.nodes.keys().filter(|&n| name_of(n).is_some()).collect();
    let mut names: Vec<String> = chips.iter().filter_map(|&n| name_of(n)).collect();
    names.sort();
    names.dedup();
    let idx = |n: NodeId| name_of(n).and_then(|s| names.iter().position(|x| *x == s));
    let mut dst_pins: HashMap<(usize, usize), HashMap<(NodeId, u8), u32>> = HashMap::new();
    for &src in &chips {
        let si = idx(src).unwrap();
        for &net in &def.nodes[src].outputs {
            let mut stack = vec![net];
            let mut seen: HashSet<_> = HashSet::new();
            while let Some(n) = stack.pop() {
                if !seen.insert(n) {
                    continue;
                }
                for (sink, spin) in def.sinks(n) {
                    if let Some(di) = idx(sink) {
                        if di != si {
                            let dw = d
                                .pin_widths(ppu_def, sink)
                                .ok()
                                .and_then(|(ins, _)| ins.get(spin as usize).copied())
                                .map_or(1, u32::from);
                            dst_pins
                                .entry((si, di))
                                .or_default()
                                .insert((sink, spin), dw);
                        }
                    } else {
                        stack.extend(def.nodes[sink].outputs.iter().copied());
                    }
                }
            }
        }
    }
    println!("### Signals between the sub-chips of the PPU\n");
    println!(
        "Distinct destination bits from row to column (a bus counted once per receiving pin; glue is transparent; the PPU's own ports and its register-file writes are not sub-chip pins).\n"
    );
    print!("| from \\ to |");
    for n in &names {
        print!(" {n} |");
    }
    println!(" total out |");
    print!("|---|");
    for _ in 0..=names.len() {
        print!("---:|");
    }
    println!();
    for (a, n) in names.iter().enumerate() {
        print!("| {n} |");
        let mut tot = 0;
        for b in 0..names.len() {
            let bits: u32 = dst_pins.get(&(a, b)).map_or(0, |m| m.values().sum());
            tot += bits;
            if bits == 0 {
                print!(" . |");
            } else {
                print!(" {bits} |");
            }
        }
        println!(" {tot} |");
    }
    println!();
}

/// Slot -> side of the chip that owns it (deepest instance wins).
fn slot_sides(l: &Loaded, p: &Program) -> HashMap<u32, usize> {
    let dm = &p.debug;
    let mut best: HashMap<u32, (usize, usize)> = HashMap::new();
    for (inst, map) in dm.net_slots.iter().enumerate() {
        let path = dm.path(inst as u32);
        let Some(&first) = path.first() else {
            continue;
        };
        let Some(side) = node_side(l, first) else {
            continue;
        };
        for (_, &slot) in map {
            let e = best.entry(slot).or_insert((0, side));
            if path.len() >= e.0 {
                *e = (path.len(), side);
            }
        }
    }
    best.into_iter().map(|(s, (_, side))| (s, side)).collect()
}

/// Executed operations of a guarded body on settled slots, per side:
/// its commits, and every instruction outside a guard or in one whose
/// conditions hold (as `spec_keys` counts them).
fn executed_by_side(
    body: &logi_sim::compile::Body,
    slots: &[u64],
    side: &HashMap<u32, usize>,
    out: &mut [f64; 4],
) {
    let run = |lo: usize, hi: usize, out: &mut [f64; 4]| {
        for b in &body.blocks[lo..hi] {
            for i in &body.instrs[b.start as usize..(b.start + b.len) as usize] {
                out[side.get(&i.dst).copied().unwrap_or(3)] += 1.0;
            }
        }
    };
    for c in &body.commits {
        let k = match *c {
            Commit::Copy { state, .. } => side.get(&state).copied().unwrap_or(3),
            _ => 3,
        };
        out[k] += 1.0;
    }
    let mut at = 0usize;
    for g in &body.guards {
        run(at, g.start as usize, out);
        if g.holds(slots) {
            run(g.start as usize, g.end as usize, out);
        }
        at = g.end as usize;
    }
    run(at, body.blocks.len(), out);
}

/// Slot -> name of the PPU sub-chip owning it (`ppu_ref/<chip>`), for slots
/// of the PPU side; deepest instance wins.
fn slot_subchips(l: &Loaded, p: &Program) -> HashMap<u32, String> {
    let dm = &p.debug;
    let design = &l.ws.design;
    let root = design.def(design.root).expect("root");
    let mut best: HashMap<u32, (usize, String)> = HashMap::new();
    for (inst, map) in dm.net_slots.iter().enumerate() {
        let path = dm.path(inst as u32);
        if path.len() < 2 {
            continue;
        }
        let Some(NodeKind::Instance(top)) = root.nodes.get(path[0]).map(|n| &n.kind) else {
            continue;
        };
        if l.ws.def_names.get(top).map(String::as_str) != Some("ppu_ref") {
            continue;
        }
        let Some(NodeKind::Instance(sub)) = design
            .def(*top)
            .ok()
            .and_then(|d| d.nodes.get(path[1]))
            .map(|n| &n.kind)
        else {
            continue;
        };
        let name = l.ws.def_names.get(sub).cloned().unwrap_or_default();
        for (_, &slot) in map {
            let e = best.entry(slot).or_insert((0, name.clone()));
            if path.len() >= e.0 {
                *e = (path.len(), name.clone());
            }
        }
    }
    best.into_iter().map(|(s, (_, n))| (s, n)).collect()
}

/// Executed instructions of the PPU side by sub-chip.
fn executed_by_sub(
    body: &logi_sim::compile::Body,
    slots: &[u64],
    sub: &HashMap<u32, String>,
    out: &mut BTreeMap<String, f64>,
) {
    let mut run = |lo: usize, hi: usize| {
        for b in &body.blocks[lo..hi] {
            for i in &body.instrs[b.start as usize..(b.start + b.len) as usize] {
                if let Some(n) = sub.get(&i.dst) {
                    *out.entry(n.clone()).or_default() += 1.0;
                }
            }
        }
    };
    let mut at = 0usize;
    for g in &body.guards {
        run(at, g.start as usize);
        if g.holds(slots) {
            run(g.start as usize, g.end as usize);
        }
        at = g.end as usize;
    }
    run(at, body.blocks.len());
}

fn ops_share(l: &Loaded, warm: u64, sample: u64) -> Result<(), String> {
    use logi_sim::compile::guard::{MIN_REGION, guard_with};
    use logi_sim::compile::phases;
    let program =
        Arc::new(compile(&l.ws.design, &Options::default()).map_err(|e| format!("{e:?}"))?);
    let ss = slot_sides(l, &program);
    let ph = program.phases.as_ref().ok_or("no clock-phase variants")?;
    let mut count = [0f64; 4];
    for v in &ph.variants {
        for i in &v.instrs {
            let k = ss.get(&i.dst).copied().unwrap_or(3);
            count[k] += 1.0;
        }
        for c in &v.commits {
            let slot = match *c {
                Commit::Copy { state, .. } => Some(state),
                _ => None,
            };
            let k = slot.and_then(|s| ss.get(&s).copied()).unwrap_or(3);
            count[k] += 1.0;
        }
    }
    let per = f64::from(ph.period);
    let total: f64 = count.iter().sum();
    // Executed: the fused bodies, guarded as the runtime guards them, on
    // sampled settled states.
    let bodies: Vec<_> = ph
        .variants
        .iter()
        .map(|v| {
            let fused = phases::prune(&program, v).unwrap_or_else(|| v.clone());
            guard_with(program.slot_count as usize, &fused, None, MIN_REGION).unwrap_or(fused)
        })
        .collect();
    let mut sim = Sim::new(Arc::clone(&program));
    sim.run(warm);
    let sub = slot_subchips(l, &program);
    let mut sub_exec: BTreeMap<String, f64> = BTreeMap::new();
    let mut exec = [0f64; 4];
    // Executed ops by place in the frame: visible lines, vblank, other.
    let mut exec_region = [[0f64; 4]; 3];
    let mut n_region = [0f64; 3];
    let names = bold_common::slot_names(&program, &l.ws);
    let line_slot = bold_common::find_slot(&names, "ppu_ref/line");
    let mut slots = vec![0u64; program.slot_count as usize];
    let mut n = 0.0;
    // Bursts of 1000 ticks, 17,341 ticks apart: the bursts walk through the
    // frame (the period is not a divisor of a frame).
    for t in 0..sample {
        if t % 1000 == 0 {
            sim.run(17_341);
        }
        sim.tick();
        for (s, v) in slots.iter_mut().enumerate() {
            *v = sim.read(s as u32);
        }
        let k = (sim.tick_count() % u64::from(ph.period)) as usize;
        let before = exec;
        executed_by_side(&bodies[k], &slots, &ss, &mut exec);
        executed_by_sub(&bodies[k], &slots, &sub, &mut sub_exec);
        if let Some(ls) = line_slot {
            let r = match sim.read(ls) {
                0..=239 => 0,
                241..=260 => 1,
                _ => 2,
            };
            n_region[r] += 1.0;
            for j in 0..4 {
                exec_region[r][j] += exec[j] - before[j];
            }
        }
        n += 1.0;
    }
    let etotal: f64 = exec.iter().sum();
    println!("### Operations per tick by side (default compile)\n");
    println!(
        "{} slots; {:.0} ops per tick in the unguarded variants (instructions plus commits, mean over the {} clock phases); {:.0} executed per tick in the guarded fused bodies over {} sampled ticks (bursts of 1000 spread over about {} frames) from tick {}.\n",
        program.slot_count,
        total / per,
        ph.period,
        etotal / n,
        sample,
        sample / 1000 * 18_341 / 89_342,
        warm
    );
    println!("| side | static ops per tick | share | executed per tick | share |");
    println!("|---|---:|---:|---:|---:|");
    for (k, name) in SIDES.iter().chain(["unattributed"].iter()).enumerate() {
        println!(
            "| {name} | {:.0} | {:.0}% | {:.0} | {:.0}% |",
            count[k] / per,
            100.0 * count[k] / total,
            exec[k] / n,
            100.0 * exec[k] / etotal
        );
    }
    println!();
    println!(
        "Executed operations per tick inside the PPU side, by sub-chip (instructions only, `?` = not in a sub-chip):\n"
    );
    let ptot: f64 = sub_exec.values().sum();
    let mut rows: Vec<_> = sub_exec.iter().collect();
    rows.sort_by(|a, b| b.1.total_cmp(a.1));
    println!(
        "{}\n",
        rows.iter()
            .map(|(k, v)| format!("{k} {:.0} ({:.0}%)", *v / n, 100.0 * *v / ptot))
            .collect::<Vec<_>>()
            .join(", ")
    );
    if line_slot.is_some() {
        println!("Executed operations per tick by place in the frame:\n");
        println!("| lines | share of ticks | CPU | PPU | mapper | unattributed |");
        println!("|---|---:|---:|---:|---:|---:|");
        for (r, name) in ["visible 0-239", "vblank 241-260", "240 and 261"]
            .iter()
            .enumerate()
        {
            if n_region[r] == 0.0 {
                continue;
            }
            println!(
                "| {name} | {:.1}% | {:.0} | {:.0} | {:.0} | {:.0} |",
                100.0 * n_region[r] / n,
                exec_region[r][0] / n_region[r],
                exec_region[r][1] / n_region[r],
                exec_region[r][2] / n_region[r],
                exec_region[r][3] / n_region[r]
            );
        }
        println!();
    }
    Ok(())
}

fn state_sizes(l: &Loaded) {
    let p = &l.program;
    let mut words = [0u64; 3];
    let mut bits = [0u64; 3];
    let mut mem_words = [0u64; 3];
    let mut mem_bits = [0u64; 3];
    let mut rows: Vec<String> = Vec::new();
    for k in &p.state_keys {
        let g = l.group_of(&k.path);
        let s = side_of_group(&g).unwrap_or(1);
        match k.loc {
            StateLoc::Slot(_) => {
                words[s] += 1;
                bits[s] += u64::from(k.width);
            }
            StateLoc::Mem(m) => {
                let d = &p.mems[m as usize];
                if d.writable {
                    mem_words[s] += 1u64 << d.addr_bits;
                    mem_bits[s] += (1u64 << d.addr_bits) * u64::from(d.width);
                    rows.push(format!(
                        "{g}[{}x{}b] ({})",
                        1u64 << d.addr_bits,
                        d.width,
                        SIDES[s]
                    ));
                }
            }
        }
    }
    println!("### State per side\n");
    println!(
        "| side | state words | state bits | writable memory words | memory bits | raw snapshot bytes (`u64` per word) |"
    );
    println!("|---|---:|---:|---:|---:|---:|");
    for s in 0..3 {
        println!(
            "| {} | {} | {} | {} | {} | {} |",
            SIDES[s],
            words[s],
            bits[s],
            mem_words[s],
            mem_bits[s],
            (words[s] + mem_words[s]) * 8
        );
    }
    println!("\nWritable memories: {}.\n", rows.join(", "));
}

fn snapshot_cost(l: &Loaded) {
    // Raw copy of the PPU side: its state words (a gather of slots) and
    // its writable memories except the screen (see the note).
    let p = &l.program;
    let mut sim = Sim::new(Arc::clone(&l.program));
    sim.run(20_000);
    let mut slots: Vec<u32> = Vec::new();
    let mut mems: Vec<u32> = Vec::new();
    let mut screen_words = 0u64;
    for k in &p.state_keys {
        let g = l.group_of(&k.path);
        if side_of_group(&g) != Some(1) {
            continue;
        }
        match k.loc {
            StateLoc::Slot(s) => slots.push(s),
            StateLoc::Mem(m) if p.mems[m as usize].writable => {
                if g == "Screen" {
                    screen_words += 1u64 << p.mems[m as usize].addr_bits;
                } else {
                    mems.push(m);
                }
            }
            _ => {}
        }
    }
    let mem_words: usize = mems.iter().map(|&m| sim.mem(m).len()).sum();
    let mut buf: Vec<u64> = vec![0; slots.len() + mem_words];
    let reps = 20_000;
    let t = Instant::now();
    let mut sink = 0u64;
    for _ in 0..reps {
        let mut at = 0;
        for &s in &slots {
            buf[at] = sim.read(std::hint::black_box(s));
            at += 1;
        }
        for &m in &mems {
            let src = std::hint::black_box(sim.mem(m));
            buf[at..at + src.len()].copy_from_slice(src);
            at += src.len();
        }
        sink ^= std::hint::black_box(&buf)[0];
    }
    let per = t.elapsed().as_secs_f64() / f64::from(reps);
    let t = Instant::now();
    for _ in 0..200 {
        std::hint::black_box(sim.export_state());
    }
    let export = t.elapsed().as_secs_f64() / 200.0;
    println!("### Snapshot cost\n");
    println!(
        "PPU side without the screen: {} state words and {} memory words = {} KiB; a raw copy (state by `Sim::read`, memories by `copy_from_slice`) takes {:.1} us ({}). The screen's {} words are not copied: replaying a frame rewrites them. Whole-machine `Sim::export_state` (HashMap keyed by path) takes {:.0} us.\n",
        slots.len(),
        mem_words,
        (slots.len() + mem_words) * 8 / 1024,
        per * 1e6,
        sink & 1,
        screen_words,
        export * 1e6
    );
}

/// `design` with the root nodes whose group is in `groups` deleted. The
/// clock divider stays: it holds the `Clock` nodes the phase variants
/// need.
fn without(l: &Loaded, groups: &[&str]) -> Design {
    let mut d = l.ws.design.clone();
    let root = d.root;
    let victims: Vec<NodeId> = d
        .def(root)
        .expect("root")
        .nodes
        .keys()
        .filter(|&n| {
            let g = l.group_of(&NodePath {
                instances: Vec::new(),
                node: n,
            });
            groups.contains(&g.as_str())
        })
        .collect();
    for n in victims {
        d.remove_node(root, n).expect("remove");
    }
    d
}

fn bench(d: &Design, label: &str, warm: u64, ticks: u64) -> Result<(f64, f64), String> {
    let program = Arc::new(compile(d, &Options::default()).map_err(|e| format!("{e:?}"))?);
    #[cfg(feature = "jit")]
    let o = SimOptions::jit();
    #[cfg(not(feature = "jit"))]
    let o = SimOptions::default();
    let ops = program.phases.as_ref().map_or(0.0, |p| p.ops_per_tick());
    let mut sim = Sim::with_options(Arc::clone(&program), o);
    sim.run(warm);
    let mut best = f64::MAX;
    for _ in 0..3 {
        let t = Instant::now();
        sim.run(ticks);
        best = best.min(t.elapsed().as_secs_f64());
    }
    let rate = ticks as f64 / best;
    println!(
        "| {label} | {ops:.0} | {:.2} | {:.0} |",
        rate / 1e6,
        1e9 / rate
    );
    Ok((rate, ops))
}

fn main() -> Result<(), String> {
    let mut folder = String::new();
    let mut ticks = 3_000_000u64;
    let mut run = true;
    let mut warm = 9_000_000u64;
    let mut sample = 180_000u64;
    let mut it = std::env::args().skip(1);
    while let Some(x) = it.next() {
        match x.as_str() {
            "--ticks" => ticks = it.next().and_then(|v| v.parse().ok()).ok_or("--ticks N")?,
            "--no-run" => run = false,
            "--warm" => warm = it.next().and_then(|v| v.parse().ok()).ok_or("--warm N")?,
            "--sample" => sample = it.next().and_then(|v| v.parse().ok()).ok_or("--sample N")?,
            f if folder.is_empty() => folder = f.to_string(),
            o => return Err(format!("unknown argument {o}")),
        }
    }
    if folder.is_empty() {
        return Err(
            "usage: bold_split <workspace> [--ticks N] [--no-run] [--warm N] [--sample N]".into(),
        );
    }
    let l = bold_common::load(&folder)?;
    println!("## {folder}\n");
    crossings(&l);
    ppu_internal(&l);
    state_sizes(&l);
    ops_share(&l, warm, sample)?;
    snapshot_cost(&l);
    if run {
        println!("### Throughput with one side removed (JIT, `Sim::run`)\n");
        println!("| root | ops per tick | M ticks/s | ns per tick |");
        println!("|---|---:|---:|---:|");
        let (full, _) = bench(&l.ws.design, "whole console", warm, ticks)?;
        const PPU: [&str; 5] = [
            "ppu_ref",
            "ppu_timing",
            "nes_palette",
            "Screen",
            "Ram<-ppu_ref",
        ];
        const CPU: [&str; 4] = ["cpu6502", "oam_dma_ref", "joypad", "Ram<-mux2x8"];
        let both: Vec<&str> = PPU.iter().chain(CPU.iter()).copied().collect();
        let (cpu, _) = bench(
            &without(&l, &PPU),
            "PPU side removed (CPU + mapper)",
            warm,
            ticks,
        )?;
        let (ppu, _) = bench(
            &without(&l, &CPU),
            "CPU side removed (PPU + mapper)",
            warm,
            ticks,
        )?;
        let (map, _) = bench(&without(&l, &both), "mapper and glue only", warm, ticks)?;
        println!(
            "\nns per tick: whole {:.0}; CPU+mapper {:.0}; PPU+mapper {:.0}; mapper alone {:.0}. Two threads, no synchronization or rollback cost: ceiling {:.2}x (whole / max of the two sides).\n",
            1e9 / full,
            1e9 / cpu,
            1e9 / ppu,
            1e9 / map,
            (1e9 / full) / (1e9 / cpu).max(1e9 / ppu)
        );
    }
    Ok(())
}
