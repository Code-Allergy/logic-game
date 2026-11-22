//! Routing every circuit of the demos and the gallery: the router's
//! invariants hold (on the grid, clear of the tiles, connected trees with
//! junctions, no collinear overlaps between nets), no search falls back,
//! and the result is deterministic. A circuit whose tiles overlap (a
//! layout bug, reported by `render_svg`) is only checked for connected
//! trees; a few densely packed ones ([`CONGESTED`]) may cut through a tile
//! or two where the gaps between tiles leave no free track.

use super::*;
use crate::sim::CircuitLibrary;
use crate::sim::layout::overlaps;
use crate::sim::routing::{
    check_invariants, check_invariants_but_boxes, check_trees, document_nets, route_all,
    tile_crossings,
};

/// Circuits packed so tightly that some wires find no clear way between
/// the tiles (facing tiles 20 or 30 units apart, with every track between
/// them taken by pin approaches): there, at most [`MAX_CROSSED`] wires may
/// cut through a tile. Everything else about them is checked in full.
const CONGESTED: [&str; 4] = [
    "register_file/regfile4x8",
    "cpu/cpu",
    "priority_encoder/prio8",
    "alu/alu8",
];
/// Circuits whose router may report one fallback: the 16-bit CPU's top
/// level.
const MAY_FALL_BACK: [&str; 1] = ["cpu16/cpu16"];
const MAX_CROSSED: usize = 4;

#[test]
fn every_demo_and_gallery_circuit_routes_cleanly() {
    let mut failures = Vec::new();
    let (mut circuits, mut sinks, mut took) = (0, 0, Duration::ZERO);
    let mut overlapping = Vec::new();
    for demo in demos().all() {
        let mut library = CircuitLibrary {
            definitions: demo.workspace.definitions.clone(),
        };
        for (_, doc) in &demo.circuits {
            library.definitions.insert(doc.id, Arc::clone(doc));
        }
        for (name, doc) in &demo.circuits {
            let nets = document_nets(doc, &library);
            let start = Instant::now();
            let routing = route_all(&nets.problem);
            took += start.elapsed();
            circuits += 1;
            sinks += nets
                .problem
                .nets
                .iter()
                .map(|n| n.sinks.len())
                .sum::<usize>();
            let id = format!("{}/{name}", demo.id);
            assert_eq!(route_all(&nets.problem), routing, "{id}: deterministic");
            let boxes: Vec<(Vec2, Vec2)> = doc
                .nodes
                .iter()
                .map(|n| (Vec2::new(n.x, n.y), n.kind.size()))
                .collect();
            if overlaps(&boxes).is_some() {
                overlapping.push(id.clone());
                if let Err(e) = check_trees(&nets.problem, &routing) {
                    failures.push(format!("{id}: {e}"));
                }
                continue;
            }
            let checked = if CONGESTED.contains(&id.as_str()) {
                let mut crossed: Vec<(usize, usize)> = tile_crossings(&nets.problem, &routing)
                    .into_iter()
                    .map(|(net, sink, _)| (net, sink))
                    .collect();
                crossed.dedup();
                eprintln!("{id}: {} wire(s) cross a tile", crossed.len());
                if crossed.len() > MAX_CROSSED {
                    failures.push(format!("{id}: wires cross tiles {crossed:?}"));
                }
                check_invariants_but_boxes(&nets.problem, &routing)
            } else {
                check_invariants(&nets.problem, &routing)
            };
            if let Err(e) = checked {
                failures.push(format!("{id}: {e}"));
            }
            if routing.stats.fallbacks > usize::from(MAY_FALL_BACK.contains(&id.as_str())) {
                failures.push(format!("{id}: {} fallback(s)", routing.stats.fallbacks));
            }
            // Every branch of a net leaves its tree at a junction.
            for net in &routing.nets {
                let branches = net.sinks.iter().filter(|s| s.parent.is_some()).count();
                assert_eq!(net.junctions().count(), branches, "{id}");
                assert!(net.sinks.len() <= 1 || net.sinks.iter().any(|s| s.parent.is_none()));
            }
        }
    }
    eprintln!(
        "{circuits} circuits, {sinks} sinks routed in {took:?}; overlapping tiles: {overlapping:?}"
    );
    assert!(overlapping.len() <= 3, "{overlapping:?}");
    assert!(circuits > 50, "the demos and the gallery are all there");
    assert!(failures.is_empty(), "{failures:#?}");
}

/// The rubber band of a wire drag ([`band_route`]) is routed like a wire
/// with only the tiles near its ends: it joins the two pins (or a pin and
/// the pointer on the grid) with horizontal and vertical runs, in the
/// biggest circuit too. (Timing is printed with `--nocapture`: it is
/// routed once per target pin or grid cell the pointer enters.)
#[test]
fn the_rubber_band_routes_between_the_pins_of_any_net() {
    use crate::sim::wiring::band_route;
    let demo = demos().get("cpu16").expect("the 16-bit CPU");
    let mut library = CircuitLibrary {
        definitions: demo.workspace.definitions.clone(),
    };
    for (_, doc) in &demo.circuits {
        library.definitions.insert(doc.id, Arc::clone(doc));
    }
    let (_, doc) = demo
        .circuits
        .iter()
        .find(|(name, _)| name == "cpu16")
        .expect("the top level");
    let nets = document_nets(doc, &library);
    let (mut bands, mut worst, mut total) = (0, Duration::ZERO, Duration::ZERO);
    let pointer = |p: Vec2| ((p + Vec2::new(130.0, -70.0)) / 20.0).round() * 20.0;
    for net in &nets.problem.nets {
        for sink in net.sinks.iter().copied().chain([pointer(net.driver)]) {
            let start = Instant::now();
            let path = band_route(&nets.problem.boxes, net.driver, sink);
            let took = start.elapsed();
            worst = worst.max(took);
            total += took;
            bands += 1;
            assert_eq!(path.first(), Some(&net.driver));
            assert_eq!(path.last(), Some(&sink));
            for w in path.windows(2) {
                assert!(w[0].x == w[1].x || w[0].y == w[1].y, "{path:?}");
            }
        }
    }
    assert!(bands > 50);
    eprintln!(
        "{bands} bands over {} tiles: worst {worst:?}, mean {:?}",
        nets.problem.boxes.len(),
        total / bands
    );
}
