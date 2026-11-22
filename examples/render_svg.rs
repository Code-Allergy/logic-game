//! Render circuits of version 6 files as SVG previews (a development tool
//! for laying out demos and gallery items).
//!
//! ```sh
//! cargo run --example render_svg -- counter                    # a demo or gallery id
//! cargo run --example render_svg -- path/to/file.json          # any version 6 file
//! cargo run --example render_svg -- seven_segment --out /tmp/p --circuit main --ortho
//! ```
//!
//! Every circuit of each file is written to `<out>/<file>.<circuit>.svg`
//! (`--out` defaults to `target/previews`). The picture shows the expanded
//! circuit as the editor has it: one box per node (the editor's size, and
//! the colour of its family, the label above it, the kind or value inside), generated
//! `Split`/`Merge` adapters included, on the 20-unit grid. Wires are drawn
//! straight from centre to centre; with `--ortho` they are routed as the
//! editor routes them (`gate_quest::sim::routing`, the same router and the
//! same pins): cased, from pin to pin in horizontal and vertical runs with
//! rounded corners, a dot where a net branches and a gap where one wire
//! passes over another. Word wires are thicker.
//!
//! For each circuit it prints the node count and the layout checks:
//! overlapping boxes, and straight wires that pass through a box that is
//! not one of their ends (with `--ortho`: routing problems, and the time
//! the routing took).

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use bevy::math::Vec2;
use gate_quest::sim::art::family;
use gate_quest::sim::art::{BIT_CASING, BIT_CORE, BUS_CASING, BUS_CORE};
use gate_quest::sim::demos::{DEMO_DIR, DemoCatalog, GALLERY_DIR};
use gate_quest::sim::layout::overlaps;
use gate_quest::sim::routing::{self, check_invariants, document_nets};
use gate_quest::sim::shapes::round_corners;
use gate_quest::sim::source;
use gate_quest::sim::visuals::{WIRE_CORNER, wire_path};
use gate_quest::sim::{CircuitDocument, CircuitLibrary, GateKind};
use gate_quest::theme::{Family, Palette};

const MARGIN: f32 = 60.0;

fn main() -> ExitCode {
    let mut out = PathBuf::from("target/previews");
    let mut only: Option<String> = None;
    let mut ortho = false;
    let mut inputs: Vec<String> = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--out" => out = args.next().expect("--out DIR").into(),
            "--circuit" => only = Some(args.next().expect("--circuit NAME")),
            "--ortho" => ortho = true,
            _ => inputs.push(arg),
        }
    }
    if inputs.is_empty() {
        eprintln!("usage: render_svg <id | file.json>... [--out DIR] [--circuit NAME] [--ortho]");
        return ExitCode::FAILURE;
    }
    let catalog = match DemoCatalog::from_dir(Path::new(DEMO_DIR), Path::new(GALLERY_DIR)) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("cannot load the demos (run from the repository root): {e}");
            return ExitCode::FAILURE;
        }
    };
    std::fs::create_dir_all(&out).expect("create the output folder");
    let mut failed = false;
    for input in inputs {
        let (stem, circuits, library) = match load(&catalog, &input) {
            Ok(x) => x,
            Err(e) => {
                eprintln!("{input}: {e}");
                failed = true;
                continue;
            }
        };
        for (name, doc) in circuits {
            if only.as_ref().is_some_and(|o| *o != name) {
                continue;
            }
            let (svg, report) = render(&doc, &library, ortho);
            let path = out.join(format!("{stem}.{name}.svg"));
            std::fs::write(&path, svg).expect("write the SVG");
            println!("{} ({} nodes){report}", path.display(), doc.nodes.len());
        }
    }
    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

type Circuits = Vec<(String, Arc<CircuitDocument>)>;

/// The circuits of a demo or gallery item (by id) or of a file.
fn load(catalog: &DemoCatalog, input: &str) -> Result<(String, Circuits, CircuitLibrary), String> {
    let path = Path::new(input);
    let (stem, circuits, definitions) = if path.is_file() {
        let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        let file = source::parse(&text).map_err(|e| e.to_string())?;
        let stem = path
            .file_stem()
            .map_or("file".into(), |s| s.to_string_lossy().into_owned());
        let expansion = source::expand(&file, &format!("render/{stem}"), &catalog.externals())
            .map_err(|e| e.to_string())?;
        (stem, expansion.circuits, expansion.workspace.definitions)
    } else {
        let demo = catalog
            .get(input)
            .ok_or("not a file, a demo or a gallery item")?;
        (
            demo.id.clone(),
            demo.circuits.clone(),
            demo.workspace.definitions.clone(),
        )
    };
    let mut library = CircuitLibrary { definitions };
    for (_, doc) in &circuits {
        library.definitions.insert(doc.id, Arc::clone(doc));
    }
    Ok((stem, circuits, library))
}

/// Box colour: the strong colour of the part's family in the Workshop
/// day palette (ink for logic, violet for state, sky for I/O).
fn colour(kind: &GateKind) -> String {
    let palette = Palette::DAY;
    hex(match family(kind) {
        Family::Logic => palette.ink,
        Family::State => palette.state,
        Family::Io => palette.io,
    })
}

fn hex(c: bevy::color::Color) -> String {
    let c = c.to_srgba();
    let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!(
        "#{:02x}{:02x}{:02x}",
        byte(c.red),
        byte(c.green),
        byte(c.blue)
    )
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// What is written inside a box.
fn inner_text(kind: &GateKind, library: &CircuitLibrary) -> String {
    match kind {
        GateKind::Const { value, w } if *w > 4 => format!("{value:#x}"),
        GateKind::Const { value, .. } => format!("{value}"),
        GateKind::Split { lo, len: 1, .. } => format!("[{lo}]"),
        GateKind::Split { lo, len, .. } => format!("[{lo}:{}]", lo + len - 1),
        GateKind::Merge { widths } => format!("M{}", widths.len()),
        GateKind::Clock { period, .. } => format!("CLK/{period}"),
        GateKind::SubCircuit { definition } => library
            .get(*definition)
            .map_or("?".into(), |d| d.name.clone()),
        GateKind::Input { w } | GateKind::Output { w } | GateKind::Display { w } if *w > 1 => {
            format!("{} {w}b", kind.display_name())
        }
        _ => {
            let w = kind.width().unwrap_or(1);
            if w > 1 {
                format!("{} {w}", kind.display_name())
            } else {
                kind.display_name().to_string()
            }
        }
    }
}

/// Whether the segment `a`-`b` passes through the box (centre `p`, size
/// `s`, shrunk by a little so grazing a corner does not count).
fn segment_hits_box(a: Vec2, b: Vec2, p: Vec2, s: Vec2) -> bool {
    let half = s / 2.0 - Vec2::splat(4.0);
    let (lo, hi) = (p - half, p + half);
    // Liang-Barsky clipping.
    let d = b - a;
    let (mut t0, mut t1) = (0.0f32, 1.0f32);
    for (pv, qv) in [
        (-d.x, a.x - lo.x),
        (d.x, hi.x - a.x),
        (-d.y, a.y - lo.y),
        (d.y, hi.y - a.y),
    ] {
        if pv == 0.0 {
            if qv < 0.0 {
                return false;
            }
            continue;
        }
        let t = qv / pv;
        if pv < 0.0 {
            t0 = t0.max(t);
        } else {
            t1 = t1.min(t);
        }
        if t0 > t1 {
            return false;
        }
    }
    true
}

fn render(doc: &CircuitDocument, library: &CircuitLibrary, ortho: bool) -> (String, String) {
    let pos = |i: usize| Vec2::new(doc.nodes[i].x, doc.nodes[i].y);
    let size = |i: usize| doc.nodes[i].kind.size();
    let index: std::collections::HashMap<_, _> = doc
        .nodes
        .iter()
        .enumerate()
        .map(|(i, n)| (n.id, i))
        .collect();

    // Bounds (label text above boxes included).
    let (mut lo, mut hi) = (Vec2::splat(f32::MAX), Vec2::splat(f32::MIN));
    for i in 0..doc.nodes.len() {
        lo = lo.min(pos(i) - size(i) / 2.0);
        hi = hi.max(pos(i) + size(i) / 2.0 + Vec2::new(0.0, 16.0));
    }
    if doc.nodes.is_empty() {
        (lo, hi) = (Vec2::ZERO, Vec2::ZERO);
    }
    lo -= Vec2::splat(MARGIN);
    hi += Vec2::new(MARGIN, MARGIN + 30.0);
    let (width, height) = (hi.x - lo.x, hi.y - lo.y);
    // Screen coordinates: x right, y down (the editor's y points up).
    let sx = |x: f32| x - lo.x;
    let sy = |y: f32| hi.y - y;

    let mut svg = String::new();
    let (bench, grid_line) = if ortho {
        (hex(Palette::DAY.bench), hex(Palette::DAY.bench_dot))
    } else {
        ("#14171c".to_string(), "#262b33".to_string())
    };
    let _ = writeln!(
        svg,
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}" viewBox="0 0 {width} {height}" font-family="DejaVu Sans, Arial, sans-serif">"##
    );
    let _ = writeln!(
        svg,
        r##"<defs><pattern id="g" width="20" height="20" patternUnits="userSpaceOnUse" x="{}" y="{}"><path d="M 20 0 L 0 0 0 20" fill="none" stroke="{grid_line}" stroke-width="1"/></pattern></defs>"##,
        sx(0.0).rem_euclid(20.0),
        sy(0.0).rem_euclid(20.0)
    );
    let _ = writeln!(
        svg,
        r##"<rect width="100%" height="100%" fill="{bench}"/><rect width="100%" height="100%" fill="url(#g)"/>"##
    );
    let _ = writeln!(
        svg,
        r##"<text x="12" y="22" fill="{}" font-size="15" font-weight="bold">{} ({} nodes)</text>"##,
        if ortho { "#263046" } else { "#d8dee9" },
        escape(&doc.name),
        doc.nodes.len()
    );

    // Wires under the boxes.
    let mut through = Vec::new();
    let mut routed = String::new();
    if ortho {
        routed = draw_routed(&mut svg, doc, library, &sx, &sy, &mut through);
    } else {
        for wire in &doc.wires {
            let (Some(&a), Some(&b)) = (index.get(&wire.from.node), index.get(&wire.to.node))
            else {
                continue;
            };
            let w = library
                .pins(&doc.nodes[a].kind)
                .and_then(|(_, outs)| outs.get(usize::from(wire.from.pin)).copied())
                .unwrap_or(1);
            let stroke = if w > 1 { 3.0 } else { 1.5 };
            let colour = if w > 1 { "#7fb4e6" } else { "#9aa5b1" };
            let (pa, pb) = (pos(a), pos(b));
            for k in 0..doc.nodes.len() {
                if k != a && k != b && segment_hits_box(pa, pb, pos(k), size(k)) {
                    through.push(format!(
                        "{} -> {} crosses {}",
                        name(doc, a),
                        name(doc, b),
                        name(doc, k)
                    ));
                }
            }
            let _ = writeln!(
                svg,
                r##"<line x1="{:.1}" y1="{:.1}" x2="{:.1}" y2="{:.1}" stroke="{colour}" stroke-width="{stroke}"/>"##,
                sx(pa.x),
                sy(pa.y),
                sx(pb.x),
                sy(pb.y)
            );
        }
    }

    // Boxes, inner text and labels.
    for (i, node) in doc.nodes.iter().enumerate() {
        let (p, s) = (pos(i), size(i));
        let generated = node.label.is_none()
            && matches!(node.kind, GateKind::Split { .. } | GateKind::Merge { .. });
        let _ = writeln!(
            svg,
            r##"<rect x="{:.1}" y="{:.1}" width="{}" height="{}" rx="4" fill="{}" stroke="{}" stroke-width="1"{}/>"##,
            sx(p.x - s.x / 2.0),
            sy(p.y + s.y / 2.0),
            s.x,
            s.y,
            colour(&node.kind),
            if generated { "#8c8c3c" } else { "#c8ccd4" },
            if generated {
                r#" stroke-dasharray="3 2""#
            } else {
                ""
            }
        );
        let inner = inner_text(&node.kind, library);
        let font = if inner.len() > 8 { 8 } else { 10 };
        let _ = writeln!(
            svg,
            r##"<text x="{:.1}" y="{:.1}" fill="#ffffff" font-size="{font}" text-anchor="middle" dominant-baseline="middle">{}</text>"##,
            sx(p.x),
            sy(p.y),
            escape(&inner)
        );
        if let Some(label) = &node.label {
            let _ = writeln!(
                svg,
                r##"<text x="{:.1}" y="{:.1}" fill="#f0d878" font-size="11" text-anchor="middle">{}</text>"##,
                sx(p.x),
                sy(p.y + s.y / 2.0) - 4.0,
                escape(label)
            );
        }
    }
    svg.push_str("</svg>\n");

    let boxes: Vec<(Vec2, Vec2)> = (0..doc.nodes.len()).map(|i| (pos(i), size(i))).collect();
    let mut report = routed;
    if let Some((a, b)) = overlaps(&boxes) {
        let _ = write!(report, "\n  OVERLAP: {} and {}", name(doc, a), name(doc, b));
    }
    if !through.is_empty() {
        let what = if ortho {
            "routing problem(s)"
        } else {
            "wire(s) through boxes"
        };
        let _ = write!(report, "\n  {} {what}:", through.len());
        for t in through.iter().take(12) {
            let _ = write!(report, "\n    {t}");
        }
    }
    (svg, report)
}

/// A readable name of node `i`: its label, or its kind and position.
fn name(doc: &CircuitDocument, i: usize) -> String {
    let n = &doc.nodes[i];
    match &n.label {
        Some(l) => format!("\"{l}\""),
        None => format!("{} at ({}, {})", n.kind.display_name(), n.x, n.y),
    }
}

/// Draw the wires of `doc` routed as the editor routes them: per net, the
/// gaps it cuts in the wires below, then its casings, cores and junction
/// dots. Nets are stacked in routing order (a later net passes over an
/// earlier one). Problems (fallbacks, broken invariants) go to `report`;
/// returns a line on what was routed and how long it took.
fn draw_routed(
    svg: &mut String,
    doc: &CircuitDocument,
    library: &CircuitLibrary,
    sx: &dyn Fn(f32) -> f32,
    sy: &dyn Fn(f32) -> f32,
    report: &mut Vec<String>,
) -> String {
    let palette = Palette::DAY;
    let nets = document_nets(doc, library);
    let start = std::time::Instant::now();
    let routing = routing::route_all(&nets.problem);
    let took = start.elapsed();
    let info = format!(
        "\n  routed {} nets in {took:.2?} ({} searches, {} states expanded)",
        nets.problem.nets.len(),
        routing.stats.searched,
        routing.stats.expanded
    );
    if routing.stats.fallbacks > 0 {
        report.push(format!("{} fallback path(s)", routing.stats.fallbacks));
    }
    if let Err(e) = check_invariants(&nets.problem, &routing) {
        report.push(e);
    }
    let point = |p: bevy::math::Vec2| format!("{:.1},{:.1}", sx(p.x), sy(p.y));
    let (ink, bench) = (hex(palette.ink), hex(palette.bench));
    for (i, net) in routing.nets.iter().enumerate() {
        let bus = nets.widths[i] > 1;
        let (core, casing) = if bus {
            (BUS_CORE, BUS_CASING)
        } else {
            (BIT_CORE, BIT_CASING)
        };
        let colour = hex(if bus { palette.bus } else { palette.off });
        // Gaps in the wires this one passes over.
        for c in &routing.crossings {
            let (h, v) = (c.horizontal.0, c.vertical.0);
            if h.max(v) != i {
                continue;
            }
            let lower_bus = nets.widths[h.min(v)] > 1;
            let along = if lower_bus { BUS_CASING } else { BIT_CASING } * 0.5 + 1.0;
            let across = casing * 0.5 + 4.0;
            let half = if h == i {
                bevy::math::Vec2::new(along, across)
            } else {
                bevy::math::Vec2::new(across, along)
            };
            let _ = writeln!(
                svg,
                r##"<rect x="{:.1}" y="{:.1}" width="{:.1}" height="{:.1}" fill="{bench}"/>"##,
                sx(c.at.x - half.x),
                sy(c.at.y + half.y),
                half.x * 2.0,
                half.y * 2.0
            );
        }
        let paths: Vec<String> = (0..net.sinks.len())
            .map(|s| {
                let round = round_corners(&wire_path(net, s, None).points, WIRE_CORNER);
                round
                    .iter()
                    .map(|p| point(*p))
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect();
        for (width, stroke) in [(casing, &ink), (core, &colour)] {
            for (s, path) in net.sinks.iter().zip(&paths) {
                let dash = if s.routed {
                    ""
                } else {
                    r#" stroke-dasharray="6 4""#
                };
                let _ = writeln!(
                    svg,
                    r##"<polyline points="{path}" fill="none" stroke="{stroke}" stroke-width="{width}" stroke-linecap="round" stroke-linejoin="round"{dash}/>"##
                );
            }
        }
        for j in net.junctions() {
            let _ = writeln!(
                svg,
                r##"<circle cx="{:.1}" cy="{:.1}" r="{:.1}" fill="{colour}" stroke="{ink}" stroke-width="2.5"/>"##,
                sx(j.x),
                sy(j.y),
                casing * 0.5 + 4.25
            );
        }
    }
    info
}
