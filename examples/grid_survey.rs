//! Survey the demos and gallery for the editor grid (a development tool
//! for choosing the snap grid and the pin layout).
//!
//! ```sh
//! cargo run --example grid_survey
//! ```
//!
//! For every circuit of every demo and gallery item (expanded, as the
//! editor has it) it counts:
//! - parts whose centre is off a 5, 10 or 20-unit lattice (they would
//!   move if snapped to it);
//! - pins whose row is off the 20-unit routing grid (the router gives
//!   each such row its own track);
//! - wires that a snap of 5, 10 or 20 units lets run straight: the
//!   driver's and the sink's pin offsets (from their part's centre) differ
//!   by a multiple of the snap, so the parts can be placed with both pins
//!   on one row;
//! - the routed wires (as the editor routes them): sink paths, straight
//!   ones, bends, fallbacks;
//! - the same after a greedy tidy-up a player could do with each snap:
//!   left to right, a part moves by at most one routing track (20 units)
//!   so its first input lines up with its driver's pin, when the snap
//!   allows that exact offset and the move overlaps nothing.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use bevy::math::Vec2;
use gate_quest::sim::demos::{DEMO_DIR, DemoCatalog, GALLERY_DIR};
use gate_quest::sim::layout::overlaps;
use gate_quest::sim::routing::{self, document_nets, pin_layout};
use gate_quest::sim::{CircuitDocument, CircuitLibrary};

const SNAPS: [f32; 3] = [5.0, 10.0, 20.0];
const ROUTING_GRID: f32 = 20.0;

fn on(v: f32, step: f32) -> bool {
    (v / step - (v / step).round()).abs() < 1e-3
}

#[derive(Default, Clone, Copy)]
struct Routed {
    paths: usize,
    straight: usize,
    bends: usize,
    fallbacks: usize,
    moved: usize,
}

impl Routed {
    fn add(&mut self, doc: &CircuitDocument, library: &CircuitLibrary) {
        let nets = document_nets(doc, library);
        let routed = routing::route(&nets.problem, &[], true);
        self.fallbacks += routed.stats.fallbacks;
        for net in &routed.nets {
            for sink in &net.sinks {
                self.paths += 1;
                let bends = sink.points.len().saturating_sub(2);
                self.bends += bends;
                if bends == 0 {
                    self.straight += 1;
                }
            }
        }
    }

    fn print(&self, what: &str) {
        let pc = |n: usize, of: usize| 100.0 * n as f32 / of.max(1) as f32;
        println!(
            "{what}: {} sink paths, {} straight ({:.1}%), {:.2} bends per path, {} fallbacks, {} parts moved",
            self.paths,
            self.straight,
            pc(self.straight, self.paths),
            self.bends as f32 / self.paths.max(1) as f32,
            self.fallbacks,
            self.moved
        );
    }
}

/// Move parts (left to right) to line their first input up with its
/// driver, by at most one track, when `snap` allows the offset exactly.
fn tidy(doc: &CircuitDocument, library: &CircuitLibrary, snap: f32) -> (CircuitDocument, usize) {
    let mut doc = doc.clone();
    let mut order: Vec<usize> = (0..doc.nodes.len()).collect();
    order.sort_by(|&a, &b| doc.nodes[a].x.total_cmp(&doc.nodes[b].x));
    let mut moved = 0;
    for i in order {
        let id = doc.nodes[i].id;
        let Some(wire) = doc.wires.iter().find(|w| w.to.node == id && w.to.pin == 0) else {
            continue;
        };
        let Some(d) = doc.nodes.iter().position(|n| n.id == wire.from.node) else {
            continue;
        };
        let driver_y = doc.nodes[d].y
            + pin_layout(&doc.nodes[d].kind, library)
                .output(wire.from.pin)
                .y;
        let sink_y = doc.nodes[i].y + pin_layout(&doc.nodes[i].kind, library).input(0).y;
        let dy = driver_y - sink_y;
        if dy == 0.0 || dy.abs() > ROUTING_GRID + 1e-3 || !on(dy, snap) || d == i {
            continue;
        }
        doc.nodes[i].y += dy;
        let boxes: Vec<_> = doc
            .nodes
            .iter()
            .map(|n| (Vec2::new(n.x, n.y), n.kind.size()))
            .collect();
        if overlaps(&boxes).is_some() {
            doc.nodes[i].y -= dy;
        } else {
            moved += 1;
        }
    }
    (doc, moved)
}

#[derive(Default)]
struct Tally {
    circuits: usize,
    parts: usize,
    off_lattice: [usize; 3],
    pins: usize,
    pins_off_grid: usize,
    pin_offsets: BTreeMap<i64, usize>,
    wires: usize,
    alignable: [usize; 3],
    paths: usize,
    straight: usize,
    bends: usize,
    fallbacks: usize,
    overlaps: usize,
    tidied: [Routed; 3],
    squeezed: BTreeMap<String, usize>,
}

fn main() {
    let catalog = DemoCatalog::from_dir(Path::new(DEMO_DIR), Path::new(GALLERY_DIR))
        .expect("run from the repository root");
    let mut t = Tally::default();
    for demo in catalog.all() {
        let mut library = CircuitLibrary {
            definitions: demo.workspace.definitions.clone(),
        };
        for (_, doc) in &demo.circuits {
            library.definitions.insert(doc.id, doc.clone());
        }
        for (_, doc) in &demo.circuits {
            t.circuits += 1;
            let mut offsets = HashMap::new();
            for node in &doc.nodes {
                t.parts += 1;
                for (i, s) in SNAPS.iter().enumerate() {
                    if !(on(node.x, *s) && on(node.y, *s)) {
                        t.off_lattice[i] += 1;
                    }
                }
                let layout = pin_layout(&node.kind, &library);
                for side in [&layout.inputs, &layout.outputs] {
                    if side.len() > 1 && (side[0].y - side[1].y) < 19.99 {
                        let name = format!("{} {}", node.kind.display_name(), side.len());
                        *t.squeezed.entry(name).or_default() += 1;
                    }
                }
                for p in layout.inputs.iter().chain(&layout.outputs) {
                    t.pins += 1;
                    *t.pin_offsets
                        .entry((p.y * 10.0).round() as i64)
                        .or_default() += 1;
                    if !on(node.y + p.y, ROUTING_GRID) {
                        t.pins_off_grid += 1;
                    }
                }
                offsets.insert(node.id, layout);
            }
            for wire in &doc.wires {
                let (Some(a), Some(b)) = (offsets.get(&wire.from.node), offsets.get(&wire.to.node))
                else {
                    continue;
                };
                t.wires += 1;
                let d = a.output(wire.from.pin).y - b.input(wire.to.pin).y;
                for (i, s) in SNAPS.iter().enumerate() {
                    if on(d, *s) {
                        t.alignable[i] += 1;
                    }
                }
            }
            let boxes: Vec<_> = doc
                .nodes
                .iter()
                .map(|n| (Vec2::new(n.x, n.y), n.kind.size()))
                .collect();
            if overlaps(&boxes).is_some() {
                t.overlaps += 1;
            }
            let nets = document_nets(doc, &library);
            let routed = routing::route(&nets.problem, &[], true);
            t.fallbacks += routed.stats.fallbacks;
            for net in &routed.nets {
                for sink in &net.sinks {
                    t.paths += 1;
                    let bends = sink.points.len().saturating_sub(2);
                    t.bends += bends;
                    if bends == 0 {
                        t.straight += 1;
                    }
                }
            }
            for (i, s) in SNAPS.iter().enumerate() {
                let (tidied, moved) = tidy(doc, &library, *s);
                t.tidied[i].moved += moved;
                t.tidied[i].add(&tidied, &library);
            }
        }
    }
    let pc = |n: usize, of: usize| 100.0 * n as f32 / of.max(1) as f32;
    println!(
        "{} circuits ({} with overlapping parts), {} parts, {} wires",
        t.circuits, t.overlaps, t.parts, t.wires
    );
    let squeezed: Vec<String> = t.squeezed.iter().map(|(k, n)| format!("{k}:{n}")).collect();
    println!(
        "sides with pins closer than 20 (kind pins:count): {}",
        squeezed.join(" ")
    );
    for (i, s) in SNAPS.iter().enumerate() {
        println!(
            "snap {s:>2}: {:>4} parts off the lattice ({:.1}%), {:>4} wires can run straight ({:.1}%)",
            t.off_lattice[i],
            pc(t.off_lattice[i], t.parts),
            t.alignable[i],
            pc(t.alignable[i], t.wires),
        );
    }
    println!(
        "pins: {} ({} off the {ROUTING_GRID}-unit routing grid, {:.1}%)",
        t.pins,
        t.pins_off_grid,
        pc(t.pins_off_grid, t.pins)
    );
    let offsets: Vec<String> = t
        .pin_offsets
        .iter()
        .map(|(o, n)| format!("{}:{n}", *o as f32 / 10.0))
        .collect();
    println!(
        "pin row offsets from the centre (offset:count): {}",
        offsets.join(" ")
    );
    println!(
        "routed: {} sink paths, {} straight ({:.1}%), {} bends ({:.2} per path), {} fallbacks",
        t.paths,
        t.straight,
        pc(t.straight, t.paths),
        t.bends,
        t.bends as f32 / t.paths.max(1) as f32,
        t.fallbacks
    );
    for (i, s) in SNAPS.iter().enumerate() {
        t.tidied[i].print(&format!("tidied with snap {s:>2}"));
    }
}
